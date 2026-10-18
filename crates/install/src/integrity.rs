//! Explicit local integrity checks for installed runtime dependencies. Capture
//! reuses repair's metadata and directory authority, but checks an existing
//! asset index before allowing its object list to expand that authority.
//!
//! Verification streams only files with a captured SHA1/size declaration. File
//! descriptors and path stamps bracket every read; all observed leaves are
//! rechecked before returning. This is an informational report, not permission
//! to replace a damaged file. No download, write, Java or processor runs here.
use super::*;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IntegrityProblemKind {
    Missing,
    SizeMismatch,
    Sha1Mismatch,
    Unverifiable,
}

#[derive(Clone, Debug, Serialize)]
pub struct IntegrityProblem {
    pub path: String,
    pub kind: IntegrityProblemKind,
    pub reason: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct IntegrityReport {
    pub id: String,
    pub files_verified: u64,
    pub files_missing: u64,
    pub files_corrupt: u64,
    /// Enumerated files without a usable captured SHA1/size declaration.
    pub files_unverifiable: u64,
    /// False means object coverage is unknown because the required index was
    /// absent, damaged, unverifiable, or could not be parsed safely.
    pub assets_enumerated: bool,
    pub problems: Vec<IntegrityProblem>,
    pub problems_total: usize,
}

impl IntegrityReport {
    fn problem(&mut self, path: &str, kind: IntegrityProblemKind, reason: &str) {
        self.problems_total += 1;
        if self.problems.len() < UNSUPPORTED_LIMIT {
            self.problems.push(IntegrityProblem {
                path: bounded(path),
                kind,
                reason: bounded(reason),
            });
        }
    }
}

/// Separate from MissingRepair so a capture that tolerates a corrupt asset
/// index cannot accidentally authorize the missing-only writer.
pub struct IntegrityCheck {
    captured: MissingRepair,
}

impl IntegrityCheck {
    /// The supplied root FD and canonical display directory are bound exactly
    /// as for MissingRepair. This bounded capture reads metadata only.
    pub fn capture_bound(root: fs::File, root_display: impl AsRef<Path>, id: &str) -> Result<Self> {
        Ok(Self {
            captured: MissingRepair::capture(root, root_display.as_ref(), id, CaptureMode::Verify)?,
        })
    }

    pub fn id(&self) -> &str {
        self.captured.id()
    }

    /// Caller owns task admission. Cheap generations and cancellation are
    /// checked per chunk; full binding checks bracket this read-only operation.
    pub fn verify(
        &self,
        cancel: &AtomicBool,
        fast_guard: &(impl Fn() -> Result<()> + Send + Sync),
        validate_binding: &(impl Fn() -> Result<()> + Send + Sync),
        on_progress: &(impl Fn(Progress) + Send + Sync),
    ) -> Result<IntegrityReport> {
        let mut state = self
            .captured
            .state
            .lock()
            .map_err(|_| "完整性检查捕获锁损坏")?;
        validate(&state, cancel, validate_binding, true)?;
        let mut files = self.captured.files.clone();
        files.extend(self.captured.index.iter().cloned());
        let files = manifest::deduplicate(files)?;
        let mut progress = IntegrityProgress {
            total: files.len() as u64,
            bytes_total: files
                .iter()
                .filter_map(|f| f.download.as_ref())
                .map(|d| d.size)
                .sum(),
            ..Default::default()
        };
        let mut report = IntegrityReport {
            id: self.id().into(),
            files_verified: 0,
            files_missing: 0,
            files_corrupt: 0,
            files_unverifiable: 0,
            assets_enumerated: self.captured.index.is_none()
                || self.captured.captured_index.is_some(),
            problems: Vec::new(),
            problems_total: 0,
        };
        let mut observed = Vec::with_capacity(files.len());
        on_progress(progress.event(false));
        for file in &files {
            check(cancel)?;
            fast_guard()?;
            state.verify_fast()?;
            let Some(path) = &file.relative else {
                report.files_unverifiable += 1;
                report.problem(
                    &file.label,
                    IntegrityProblemKind::Unverifiable,
                    file.unsupported
                        .as_deref()
                        .unwrap_or("所需文件路径无法安全定位"),
                );
                progress.completed += 1;
                on_progress(progress.event(false));
                continue;
            };
            state.filesystem.inspect(path)?;
            let stamp = state.filesystem.stamp(path)?;
            observed.push((path.clone(), stamp));
            match stamp {
                None => {
                    report.files_missing += 1;
                    report.problem(path, IntegrityProblemKind::Missing, "所需文件缺失");
                }
                Some(stamp) => IntegrityReader {
                    state: &state,
                    cancel,
                    fast_guard,
                    on_progress,
                }
                .verify_present(file, stamp, &mut progress, &mut report)?,
            }
            progress.completed += 1;
            on_progress(progress.event(false));
        }
        if !report.assets_enumerated {
            report.problem(
                "assets/objects",
                IntegrityProblemKind::Unverifiable,
                "资源索引缺失、未通过可信校验或无法解析，未能枚举资源对象",
            );
        }
        validate(&state, cancel, validate_binding, true)?;
        for (path, stamp) in observed {
            check(cancel)?;
            fast_guard()?;
            if state.filesystem.stamp(&path)? != stamp {
                return Err("所需文件在完整性检查期间变化，请重新检查".into());
            }
        }
        validate(&state, cancel, validate_binding, false)?;
        on_progress(progress.event(true));
        Ok(report)
    }
}

pub(super) fn trusted_bytes(file: &RequiredFile, bytes: &[u8]) -> bool {
    file.download.as_ref().is_some_and(|download| {
        bytes.len() as u64 == download.size && format!("{:x}", Sha1::digest(bytes)) == download.sha1
    })
}

struct IntegrityReader<'a> {
    state: &'a RepairState,
    cancel: &'a AtomicBool,
    fast_guard: &'a (dyn Fn() -> Result<()> + Send + Sync),
    on_progress: &'a (dyn Fn(Progress) + Send + Sync),
}

impl IntegrityReader<'_> {
    fn verify_present(
        &self,
        required: &RequiredFile,
        stamp: Stamp,
        progress: &mut IntegrityProgress,
        report: &mut IntegrityReport,
    ) -> Result<()> {
        let state = self.state;
        let cancel = self.cancel;
        let fast_guard = self.fast_guard;
        let on_progress = self.on_progress;
        let path = required.relative.as_deref().ok_or("完整性检查路径缺失")?;
        let Some(download) = &required.download else {
            report.files_unverifiable += 1;
            report.problem(
                path,
                IntegrityProblemKind::Unverifiable,
                required
                    .unsupported
                    .as_deref()
                    .unwrap_or("缺少可信 SHA1 和大小声明"),
            );
            return Ok(());
        };
        if stamp.len != download.size {
            report.files_corrupt += 1;
            report.problem(
                path,
                IntegrityProblemKind::SizeMismatch,
                "文件大小与捕获的声明不符",
            );
            return Ok(());
        }
        let mut file = open_file(state, path, stamp)?;
        let mut digest = Sha1::new();
        let mut received = 0u64;
        let mut buffer = [0u8; 64 * 1024];
        let mut last_binding_check = Instant::now();
        while received < download.size {
            check(cancel)?;
            fast_guard()?;
            if last_binding_check.elapsed() >= Duration::from_millis(100) {
                state.verify_fast()?;
                last_binding_check = Instant::now();
            }
            // The captured descriptor, rather than the current on-disk length,
            // bounds total input even if another process keeps appending bytes.
            let remaining = (download.size - received).min(buffer.len() as u64) as usize;
            let length = file.read(&mut buffer[..remaining]).map_err(error)?;
            if length == 0 {
                return Err("所需文件在完整性检查读取期间变化".into());
            }
            digest.update(&buffer[..length]);
            received += length as u64;
            progress.bytes_done += length as u64;
            on_progress(progress.event(false));
        }
        check(cancel)?;
        fast_guard()?;
        if Stamp::of(&file.metadata().map_err(error)?) != stamp
            || state.filesystem.stamp(path)? != Some(stamp)
        {
            return Err("所需文件在完整性检查读取期间变化".into());
        }
        if format!("{:x}", digest.finalize()) == download.sha1 {
            report.files_verified += 1;
        } else {
            report.files_corrupt += 1;
            report.problem(
                path,
                IntegrityProblemKind::Sha1Mismatch,
                "文件 SHA1 与捕获的声明不符",
            );
        }
        Ok(())
    }
}

fn open_file(state: &RepairState, path: &str, stamp: Stamp) -> Result<fs::File> {
    let (parent, leaf) = split(path);
    let parent = state
        .filesystem
        .directories
        .get(parent)
        .ok_or("校验目录未绑定")?;
    let name = cstring(leaf)?;
    let how = OpenHow {
        flags: (libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK) as u64,
        mode: 0,
        resolve: 0x01 | 0x04 | 0x08,
    };
    let fd = unsafe {
        libc::syscall(
            libc::SYS_openat2,
            parent.as_raw_fd(),
            name.as_ptr(),
            &how,
            std::mem::size_of::<OpenHow>(),
        )
    };
    if fd < 0 {
        return Err(format!(
            "无法安全打开校验文件：{}",
            std::io::Error::last_os_error()
        ));
    }
    let file = unsafe { fs::File::from_raw_fd(fd as i32) };
    let metadata = file.metadata().map_err(error)?;
    if !metadata.is_file() || Stamp::of(&metadata) != stamp {
        return Err("所需文件在完整性检查打开期间变化".into());
    }
    Ok(file)
}

#[derive(Default)]
struct IntegrityProgress {
    completed: u64,
    total: u64,
    bytes_done: u64,
    bytes_total: u64,
}

impl IntegrityProgress {
    fn event(&self, complete: bool) -> Progress {
        Progress {
            stage: if complete {
                "complete"
            } else {
                "integrity-verify"
            }
            .into(),
            message: if complete {
                "完整性检查完成"
            } else {
                "正在校验已有依赖"
            }
            .into(),
            completed: self.completed,
            total: self.total,
            bytes_done: self.bytes_done,
            bytes_total: self.bytes_total,
            network_bytes: 0,
            steps: vec![InstallStep {
                id: "integrity-verify".into(),
                label: "校验依赖完整性".into(),
                state: if complete { "complete" } else { "running" }.into(),
                progress: Some(self.completed as f64 / self.total.max(1) as f64),
            }],
        }
    }
}

#[cfg(test)]
#[path = "integrity_tests.rs"]
mod tests;
