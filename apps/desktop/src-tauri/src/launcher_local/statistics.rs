//! Statistics have their own schema/file/lock, independent of preferences,
//! accounts and game directories. Loading defaults is read-only. Each actual
//! event rereads under the process lock and commits with a checked file exchange.
use super::filesystem::{optional_snapshot, revision, Scope};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    path::PathBuf,
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};
const NAME: &str = "statistics.json";
const LIMIT: u64 = 16 * 1024;

#[derive(Clone, Copy)]
pub enum StatEvent {
    LauncherOpened,
    GameSpawned,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Counts {
    schema_version: u32,
    launcher_opens: u64,
    game_spawns: u64,
    last_launcher_open: Option<u64>,
    last_game_spawn: Option<u64>,
}
impl Default for Counts {
    fn default() -> Self {
        Self {
            schema_version: 1,
            launcher_opens: 0,
            game_spawns: 0,
            last_launcher_open: None,
            last_game_spawn: None,
        }
    }
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatsView {
    pub schema_version: u32,
    pub launcher_opens: u64,
    pub game_spawns: u64,
    pub last_launcher_open: Option<u64>,
    pub last_game_spawn: Option<u64>,
    pub revision: String,
}

pub struct StatsStore {
    project: PathBuf,
    writer: Mutex<()>,
}
impl StatsStore {
    pub fn new(project: PathBuf) -> Self {
        Self {
            project,
            writer: Mutex::new(()),
        }
    }
    pub fn load(&self) -> Result<StatsView, String> {
        let scope = Scope::open(&self.project, "launcher-local")?;
        let (counts, revision) = read(&scope)?;
        scope.recheck()?;
        Ok(view(counts, revision))
    }
    /// Call only after successful launcher setup or successful game spawn. A
    /// returned error never means that an already spawned game should be killed.
    pub fn increment(&self, event: StatEvent) -> Result<StatsView, String> {
        let _writer = self.writer.lock().map_err(|_| "启动统计状态暂时不可用")?;
        let scope = Scope::create(&self.project, "launcher-local")?;
        let folder = scope.folder.as_ref().unwrap();
        let _lock = folder.lock(".statistics.lock")?;
        let snapshot = optional_snapshot(folder, NAME, LIMIT)?;
        let mut counts = match &snapshot {
            Some(snapshot) => parse(&snapshot.header)?,
            None => Counts::default(),
        };
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        match event {
            StatEvent::LauncherOpened => {
                counts.launcher_opens = counts
                    .launcher_opens
                    .checked_add(1)
                    .ok_or("启动器打开次数已达到上限")?;
                counts.last_launcher_open = Some(now);
            }
            StatEvent::GameSpawned => {
                counts.game_spawns = counts
                    .game_spawns
                    .checked_add(1)
                    .ok_or("游戏启动次数已达到上限")?;
                counts.last_game_spawn = Some(now);
            }
        }
        if counts.launcher_opens > 9_007_199_254_740_991
            || counts.game_spawns > 9_007_199_254_740_991
        {
            return Err("启动统计次数已达到可表示上限".into());
        }
        scope.recheck()?;
        folder.replace(
            NAME,
            snapshot.as_ref(),
            &serde_json::to_vec_pretty(&counts).map_err(|_| "启动统计无法序列化")?,
            LIMIT,
        )?;
        scope.recheck()?;
        let (counts, revision) = read(&scope)?;
        Ok(view(counts, revision))
    }
}
fn parse(bytes: &[u8]) -> Result<Counts, String> {
    let counts: Counts =
        serde_json::from_slice(bytes).map_err(|_| "启动统计文件无效，已保留原文件")?;
    if counts.schema_version != 1
        || counts.launcher_opens > 9_007_199_254_740_991
        || counts.game_spawns > 9_007_199_254_740_991
    {
        return Err("启动统计文件版本或计数不受支持，已保留原文件".into());
    }
    Ok(counts)
}
fn read(scope: &Scope) -> Result<(Counts, String), String> {
    let snapshot = scope
        .folder
        .as_ref()
        .map(|folder| optional_snapshot(folder, NAME, LIMIT))
        .transpose()?
        .flatten();
    match snapshot {
        Some(snapshot) => Ok((
            parse(&snapshot.header)?,
            revision(&scope.path().join(NAME), &snapshot)?,
        )),
        None => Ok((
            Counts::default(),
            format!(
                "{:x}",
                Sha256::digest(format!("missing-statistics:{}", scope.path().display()))
            ),
        )),
    }
}
fn view(counts: Counts, revision: String) -> StatsView {
    StatsView {
        schema_version: counts.schema_version,
        launcher_opens: counts.launcher_opens,
        game_spawns: counts.game_spawns,
        last_launcher_open: counts.last_launcher_open,
        last_game_spawn: counts.last_game_spawn,
        revision,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        sync::{
            atomic::{AtomicU64, Ordering},
            Arc,
        },
    };
    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = super::super::filesystem::fixture_path(&format!(
                "stats-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn load_never_creates_files_and_actual_events_are_separate() {
        let f = Fixture::new();
        let store = StatsStore::new(f.0.clone());
        assert_eq!(store.load().unwrap().launcher_opens, 0);
        assert!(!f.0.join(".pcl-rust").exists());
        store.increment(StatEvent::LauncherOpened).unwrap();
        let second = store.increment(StatEvent::GameSpawned);
        let view = second.unwrap();
        assert_eq!(view.launcher_opens, 1);
        assert_eq!(view.game_spawns, 1);
        assert!(view.last_game_spawn.is_some());
        assert!(f
            .0
            .join(".pcl-rust/launcher-local/statistics.json")
            .is_file());
        assert!(!f.0.join(".pcl-rust/settings.json").exists());
        assert!(!f.0.join(".pcl-rust/accounts.json").exists());
    }
    #[test]
    fn threaded_counts_do_not_overwrite_each_other() {
        let f = Fixture::new();
        let store = Arc::new(StatsStore::new(f.0.clone()));
        let workers: Vec<_> = (0..4)
            .map(|_| {
                let store = store.clone();
                std::thread::spawn(move || {
                    let mut succeeded = 0;
                    for _ in 0..8 {
                        let result = store.increment(StatEvent::GameSpawned);
                        if result.is_ok() {
                            succeeded += 1;
                        }
                        result.unwrap();
                    }
                    succeeded
                })
            })
            .collect();
        let succeeded: u64 = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .sum();
        assert_eq!(succeeded, 32);
        assert_eq!(store.load().unwrap().game_spawns, succeeded);
    }
    #[test]
    fn malformed_future_and_symlink_files_are_retained() {
        let f = Fixture::new();
        let scope = Scope::create(&f.0, "launcher-local").unwrap();
        let path = scope.path().join(NAME);
        let store = StatsStore::new(f.0.clone());
        for bytes in [b"invalid private data".as_slice(),br#"{"schema_version":2,"launcher_opens":0,"game_spawns":0,"last_launcher_open":null,"last_game_spawn":null}"#] {fs::write(&path,bytes).unwrap();assert!(store.increment(StatEvent::LauncherOpened).is_err());assert_eq!(fs::read(&path).unwrap(),bytes);}
        fs::remove_file(&path).unwrap();
        let retained = f.0.join("retained.json");
        fs::write(&retained, b"retain").unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&retained, &path).unwrap();
            assert!(store.increment(StatEvent::LauncherOpened).is_err());
            assert_eq!(fs::read(retained).unwrap(), b"retain");
        }
    }
}
