//! Session-bound stop requests for the monitor that owns the actual GameChild.
//!
//! This port never signals a PID. The private game-monitor directory and the
//! complete monitor/game identities authorize one fixed request document. The
//! owning monitor may then stop only its own retained child. Old requests are
//! preserved under distinct session keys; no history or user original is deleted.
use crate::launcher_local::filesystem::{optional_snapshot, Dir, Scope, Snapshot, MAX_ENTRIES};
use pcl_core::{
    game_process::Identity,
    retained_fs::{
        self,
        stage::{PublishOutcome, Stage},
        Directory, Leaf,
    },
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::Write;

const FOLDER: &str = "game-monitor";
const LIMIT: u64 = 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StopRequest {
    schema_version: u32,
    monitor: Identity,
    game: Identity,
}
impl StopRequest {
    fn new(monitor: &Identity, game: &Identity) -> Result<Self, String> {
        if !monitor.valid() || !game.valid() || monitor.pid == game.pid {
            return Err("停止请求的监控或游戏身份无效".into());
        }
        Ok(Self {
            schema_version: 1,
            monitor: monitor.clone(),
            game: game.clone(),
        })
    }
    fn bytes(&self) -> Result<Vec<u8>, String> {
        let bytes = serde_json::to_vec(self).map_err(|_| "无法编码游戏停止请求")?;
        if bytes.len() as u64 > LIMIT {
            return Err("游戏停止请求超过大小上限".into());
        }
        Ok(bytes)
    }
    fn name(&self) -> String {
        // Deliberately identical to the monitor process::session_id input.
        // Fixed-width numeric fields precede the already validated boot UUID.
        let mut hash = Sha256::new();
        hash.update(self.monitor.pid.to_be_bytes());
        hash.update(self.monitor.start_time.to_be_bytes());
        hash.update(self.monitor.boot_id.as_bytes());
        format!(".stop-{:x}.json", hash.finalize())
    }
}

fn folder(scope: &Scope) -> Result<&Dir, String> {
    if scope.folder_name != FOLDER {
        return Err("停止请求只允许已捕获的游戏监控目录".into());
    }
    scope.recheck()?;
    let folder = scope
        .folder
        .as_ref()
        .ok_or("游戏监控目录不存在，未创建停止请求")?;
    folder.require_private()?;
    Ok(folder)
}
fn native(folder: &Dir) -> Result<Directory, String> {
    Directory::from_file(folder.0.try_clone().map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())
}
fn matching(scope: &Scope, expected: &StopRequest) -> Result<Option<Snapshot>, String> {
    let folder = folder(scope)?;
    let name = expected.name();
    let Some(snapshot) = optional_snapshot(folder, &name, LIMIT)? else {
        scope.recheck()?;
        return Ok(None);
    };
    retained_fs::metadata(&snapshot.file)
        .and_then(|m| m.require_private())
        .map_err(|_| "游戏停止请求不是当前用户独占的普通文件")?;
    let request: StopRequest =
        serde_json::from_slice(&snapshot.header).map_err(|_| "游戏停止请求格式无效，已保留文件")?;
    if request != *expected || snapshot.header != expected.bytes()? {
        return Err("游戏停止请求与当前监控会话或游戏身份不匹配，已保留文件".into());
    }
    snapshot.recheck(folder, &name)?;
    scope.recheck()?;
    Ok(Some(snapshot))
}

/// Write only into an already captured private folder. Existing documents are
/// idempotent only when their complete canonical bytes match this exact pair.
/// An uncertain publication is an error; callers must not delete or retry it.
/// Cooperative callers leave at least one slot below MAX_ENTRIES for native
/// named staging. At the directory bound, history is retained and writes stop.
pub(super) fn request(scope: &Scope, monitor: &Identity, game: &Identity) -> Result<(), String> {
    let expected = StopRequest::new(monitor, game)?;
    if matching(scope, &expected)?.is_some() {
        return Ok(());
    }
    let folder = folder(scope)?;
    if folder.names()?.len() >= MAX_ENTRIES.saturating_sub(1) {
        return Err("游戏监控目录达到数量上限，停止请求与历史文件均已保留".into());
    }
    let parent = native(folder)?;
    let name = Leaf::new(expected.name()).map_err(|e| e.to_string())?;
    let bytes = expected.bytes()?;
    // Recheck immediately before mutation; the actual publish still targets
    // this held directory if its visible ancestors change concurrently.
    scope.recheck()?;
    let mut stage = Stage::new(&parent).map_err(|e| format!("无法创建私有游戏停止请求：{e}"))?;
    stage
        .file_mut()
        .write_all(&bytes)
        .map_err(|e| format!("停止请求写入失败，命名租约（如有）已保留：{e}"))?;
    let sealed = stage
        .seal(LIMIT)
        .map_err(|e| format!("停止请求封存失败，命名租约（如有）已保留：{e}"))?;
    match sealed
        .publish_new(&parent, &name)
        .map_err(|e| format!("停止请求发布前检查失败，命名租约（如有）已保留：{e}"))?
    {
        PublishOutcome::Published { .. } => {}
        PublishOutcome::Exists { .. } => {
            // A concurrent identical requester may win. Inspect its exact
            // document; no replacement, second publication or cleanup follows.
        }
        PublishOutcome::MayHavePublished { error, .. } => {
            return Err(format!(
                "停止请求可能已发布，但复核或持久化失败；文件已保留，请重新检查：{error}"
            ));
        }
    }
    if matching(scope, &expected)?.is_none() {
        return Err("游戏停止请求在发布后消失，未确认停止".into());
    }
    Ok(())
}

pub(super) struct Controller {
    scope: Scope,
    expected: StopRequest,
}
impl Controller {
    /// Retain copies of the admitted scope handles without borrowing Session,
    /// so the monitor can continue its durable phase updates independently.
    pub(super) fn for_session(
        scope: &Scope,
        monitor: &Identity,
        game: &Identity,
    ) -> Result<Self, String> {
        let expected = StopRequest::new(monitor, game)?;
        folder(scope)?;
        let clone = |dir: &Dir| dir.0.try_clone().map(Dir).map_err(|e| e.to_string());
        let held = Scope {
            path: scope.path.clone(),
            root: clone(&scope.root)?,
            owner: scope.owner.as_ref().map(clone).transpose()?,
            folder: scope.folder.as_ref().map(clone).transpose()?,
            folder_name: scope.folder_name,
        };
        folder(&held)?;
        Ok(Self {
            scope: held,
            expected,
        })
    }
    /// Read-only and bounded. False means no request for this session; malformed
    /// or mismatched evidence is an error, never authority to stop another child.
    pub(super) fn stop_requested(&self) -> Result<bool, String> {
        matching(&self.scope, &self.expected).map(|snapshot| snapshot.is_some())
    }
}
