//! Opt-in local diagnostics accepts closed enums rather than user/source text.
//! The log cannot receive URLs, paths, usernames, arguments, tokens or raw error
//! messages through this API. Rotation keeps four bounded validated segments.
use super::filesystem::{optional_snapshot, Scope};
use serde::{Deserialize, Serialize};
use std::{
    path::PathBuf,
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};
const LIMIT: u64 = 1024 * 1024;
#[derive(Clone, Copy, Default)]
pub struct DiagnosticPolicy {
    /// Caller combines explicit local diagnostics and debug preferences here.
    /// Debug only changes the closed stage/event enum detail; no source text or
    /// network transport exists in this service.
    pub enabled: bool,
    pub debug: bool,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticEvent {
    LauncherOpened,
    GameSpawned,
    TaskStarted,
    TaskFinished,
    Stage,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticStage {
    Ready,
    Scan,
    Plan,
    Download,
    Verify,
    Publish,
    Rollback,
    Finished,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticCode {
    Success,
    Cancelled,
    Failed,
    Conflict,
    Unavailable,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiagnosticRecord {
    pub event: DiagnosticEvent,
    pub stage: DiagnosticStage,
    pub duration_ms: Option<u64>,
    pub code: Option<DiagnosticCode>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Line {
    schema_version: u32,
    time: u64,
    sequence: u64,
    event: DiagnosticEvent,
    stage: DiagnosticStage,
    duration_ms: Option<u64>,
    code: Option<DiagnosticCode>,
}
pub struct Diagnostics {
    project: PathBuf,
    writer: Mutex<()>,
}
impl Diagnostics {
    pub fn new(project: PathBuf) -> Self {
        Self {
            project,
            writer: Mutex::new(()),
        }
    }
    pub fn record(&self, policy: DiagnosticPolicy, record: DiagnosticRecord) -> Result<(), String> {
        if !policy.enabled
            || !policy.debug
                && matches!(
                    record.event,
                    DiagnosticEvent::Stage | DiagnosticEvent::TaskStarted
                )
        {
            return Ok(());
        }
        if record
            .duration_ms
            .is_some_and(|duration| duration > 24 * 60 * 60 * 1000)
        {
            return Err("本地诊断持续时间超过上限".into());
        }
        let _writer = self.writer.lock().map_err(|_| "本地诊断状态暂时不可用")?;
        let scope = Scope::create(&self.project, "launcher-logs")?;
        let folder = scope.folder.as_ref().unwrap();
        let _lock = folder.lock(".diagnostics.lock")?;
        let mut segments = Vec::new();
        for index in 0..4 {
            let name = format!("launcher-{index}.log");
            let mut snapshot = optional_snapshot(folder, &name, LIMIT)?;
            let data = if let Some(snapshot) = &mut snapshot {
                let data = snapshot.bytes(folder, &name)?;
                validate(&data)?;
                data
            } else {
                Vec::new()
            };
            segments.push((name, snapshot, data));
        }
        // An explicit sequence orders rotations across same-second events and
        // process restarts. Wall-clock timestamps never choose the active file.
        let newest = segments
            .iter()
            .enumerate()
            .filter_map(|(index, (_, snapshot, data))| {
                snapshot
                    .as_ref()
                    .map(|_| (index, last_sequence(data).unwrap_or(0)))
            })
            .max_by_key(|(_, sequence)| *sequence);
        let active = newest.map(|(index, _)| index).unwrap_or(0);
        let sequence = newest
            .map(|(_, sequence)| sequence)
            .unwrap_or(0)
            .checked_add(1)
            .ok_or("本地诊断序列已达到上限")?;
        let line = Line {
            schema_version: 1,
            time: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            sequence,
            event: record.event,
            stage: record.stage,
            duration_ms: record.duration_ms,
            code: record.code,
        };
        let mut bytes = serde_json::to_vec(&line).map_err(|_| "本地诊断无法序列化")?;
        bytes.push(b'\n');
        let use_active = segments[active]
            .1
            .as_ref()
            .map(|snapshot| snapshot.stamp.bytes + bytes.len() as u64 <= LIMIT)
            .unwrap_or(true);
        let selected = if use_active { active } else { (active + 1) % 4 };
        let (name, snapshot, data) = &segments[selected];
        if use_active {
            if snapshot.is_some() {
                let mut previous = data.clone();
                previous.extend(bytes);
                bytes = previous;
            }
        }
        scope.recheck()?;
        folder.replace(name, snapshot.as_ref(), &bytes, LIMIT)?;
        scope.recheck()
    }
}
fn validate(bytes: &[u8]) -> Result<(), String> {
    if !bytes.ends_with(b"\n") {
        return Err("已有本地诊断日志缺少完整记录边界，已保留原文件".into());
    }
    let mut previous = 0;
    for line in bytes.split(|b| *b == b'\n').filter(|line| !line.is_empty()) {
        let record: Line =
            serde_json::from_slice(line).map_err(|_| "已有本地诊断日志无效，已保留原文件")?;
        if record.schema_version != 1
            || record
                .duration_ms
                .is_some_and(|duration| duration > 24 * 60 * 60 * 1000)
            || record.sequence <= previous
        {
            return Err("已有本地诊断日志版本或字段无效，已保留原文件".into());
        }
        previous = record.sequence;
    }
    if previous == 0 {
        return Err("已有本地诊断日志没有可验证的记录，已保留原文件".into());
    }
    Ok(())
}
fn last_sequence(bytes: &[u8]) -> Option<u64> {
    let line = bytes
        .split(|b| *b == b'\n')
        .rfind(|line| !line.is_empty())?;
    serde_json::from_slice::<Line>(line)
        .ok()
        .map(|line| line.sequence)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        sync::atomic::{AtomicU64, Ordering},
    };
    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let p = super::super::filesystem::fixture_path(&format!(
                "diag-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&p).unwrap();
            Self(p)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn event() -> DiagnosticRecord {
        DiagnosticRecord {
            event: DiagnosticEvent::GameSpawned,
            stage: DiagnosticStage::Ready,
            duration_ms: Some(42),
            code: Some(DiagnosticCode::Success),
        }
    }
    #[test]
    fn disabled_is_read_only_and_logger_rejects_source_text_fields() {
        let f = Fixture::new();
        let logger = Diagnostics::new(f.0.clone());
        logger.record(DiagnosticPolicy::default(), event()).unwrap();
        assert!(!f.0.join(".pcl-rust").exists());
        logger
            .record(
                DiagnosticPolicy {
                    enabled: true,
                    debug: false,
                },
                event(),
            )
            .unwrap();
        let bytes = fs::read(f.0.join(".pcl-rust/launcher-logs/launcher-0.log")).unwrap();
        validate(&bytes).unwrap();
        assert!(!String::from_utf8(bytes)
            .unwrap()
            .contains(f.0.to_str().unwrap()));
        assert!(serde_json::from_str::<DiagnosticRecord>(r#"{"event":"game_spawned","stage":"ready","duration_ms":null,"code":null,"token":"fake"}"#).is_err());
    }
    #[test]
    fn debug_stage_requires_debug_and_foreign_log_is_retained() {
        let f = Fixture::new();
        let logger = Diagnostics::new(f.0.clone());
        let mut record = event();
        record.event = DiagnosticEvent::Stage;
        logger
            .record(
                DiagnosticPolicy {
                    enabled: true,
                    debug: false,
                },
                record,
            )
            .unwrap();
        assert!(!f.0.join(".pcl-rust").exists());
        logger
            .record(
                DiagnosticPolicy {
                    enabled: true,
                    debug: true,
                },
                record,
            )
            .unwrap();
        let path = f.0.join(".pcl-rust/launcher-logs/launcher-0.log");
        fs::write(&path, b"foreign raw private error").unwrap();
        assert!(logger
            .record(
                DiagnosticPolicy {
                    enabled: true,
                    debug: true
                },
                record
            )
            .is_err());
        assert_eq!(fs::read(path).unwrap(), b"foreign raw private error");
    }
    fn segment(first: u64, target_bytes: usize) -> (Vec<u8>, u64) {
        let mut bytes = Vec::new();
        let mut sequence = first;
        loop {
            let mut line = serde_json::to_vec(&Line {
                schema_version: 1,
                time: 1,
                sequence,
                event: DiagnosticEvent::GameSpawned,
                stage: DiagnosticStage::Ready,
                duration_ms: Some(42),
                code: Some(DiagnosticCode::Success),
            })
            .unwrap();
            line.push(b'\n');
            if bytes.len() + line.len() > target_bytes {
                break;
            }
            bytes.extend(line);
            sequence += 1;
        }
        (bytes, sequence)
    }
    #[test]
    fn appending_large_segment_preserves_bytes_beyond_snapshot_header() {
        let f = Fixture::new();
        let scope = Scope::create(&f.0, "launcher-logs").unwrap();
        let (previous, next) = segment(1, 192 * 1024);
        let path = scope.path().join("launcher-0.log");
        fs::write(&path, &previous).unwrap();
        let result = Diagnostics::new(f.0.clone()).record(
            DiagnosticPolicy {
                enabled: true,
                debug: false,
            },
            event(),
        );
        let saved = fs::read(path).unwrap();
        #[cfg(unix)]
        {
            result.unwrap();
            assert!(saved.starts_with(&previous));
            assert_eq!(last_sequence(&saved), Some(next));
        }
        #[cfg(windows)]
        {
            assert!(result.unwrap_err().contains("尚未实现"));
            assert_eq!(saved, previous);
            assert_eq!(last_sequence(&saved), Some(next - 1));
        }
        validate(&saved).unwrap();
    }
    #[test]
    fn rotation_wrap_and_restart_use_sequence_instead_of_timestamp_or_index() {
        let f = Fixture::new();
        let scope = Scope::create(&f.0, "launcher-logs").unwrap();
        let mut next = 1;
        let mut previous = Vec::new();
        for index in 0..4 {
            let (bytes, after) = segment(next, LIMIT as usize);
            next = after;
            fs::write(scope.path().join(format!("launcher-{index}.log")), &bytes).unwrap();
            previous.push(bytes);
        }
        let policy = DiagnosticPolicy {
            enabled: true,
            debug: false,
        };
        #[cfg(windows)]
        {
            assert!(Diagnostics::new(f.0.clone())
                .record(policy, event())
                .unwrap_err()
                .contains("尚未实现"));
            for index in 0..4 {
                assert_eq!(
                    fs::read(scope.path().join(format!("launcher-{index}.log"))).unwrap(),
                    previous[index]
                );
            }
        }
        #[cfg(unix)]
        {
            Diagnostics::new(f.0.clone())
                .record(policy, event())
                .unwrap();
            // After wrapping to index zero, a new store must continue index zero.
            Diagnostics::new(f.0.clone())
                .record(policy, event())
                .unwrap();
            let wrapped = fs::read(scope.path().join("launcher-0.log")).unwrap();
            assert_eq!(last_sequence(&wrapped), Some(next + 1));
            assert_eq!(
                wrapped
                    .split(|byte| *byte == b'\n')
                    .filter(|line| !line.is_empty())
                    .count(),
                2
            );
            for index in 1..4 {
                assert_eq!(
                    fs::read(scope.path().join(format!("launcher-{index}.log"))).unwrap(),
                    previous[index]
                );
            }
            assert!(fs::read_dir(scope.path())
                .unwrap()
                .filter_map(Result::ok)
                .all(|entry| !entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".local-stage-")));
        }
    }
}
