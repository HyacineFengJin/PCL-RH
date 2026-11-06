//! Immutable artifact transfer admission and aggregate payload pacing.
//!
//! A submission retains one Arc snapshot. All transfers using that snapshot
//! share slots and one byte clock, including blocking consumers. A permit must
//! span headers/body and be dropped after the response stops. Metadata callers
//! deliberately bypass this artifact policy. TLS, URL/hash authority, verified
//! cache reuse and publication remain the caller's responsibility.
use serde::{Deserialize, Serialize};
use std::{
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex, OnceLock, RwLock,
    },
    time::{Duration, Instant},
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

pub const DOWNLOAD_SLICE_BYTES: usize = 64 * 1024;
const CANCEL_POLL: Duration = Duration::from_millis(40);
// Tokio timers round short waits to millisecond ticks. Recover at most this
// much missed service time, shared across all readers; never bank idle time
// without a bound or grant credit before the first payload reservation.
const MAX_PACING_CATCHUP: Duration = Duration::from_millis(2);
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DownloadPolicy {
    pub max_concurrent_transfers: u16,
    pub total_rate_limit_mib_per_second: u32,
    pub forbid_cross_root_cache_copy: bool,
}
impl Default for DownloadPolicy {
    fn default() -> Self {
        Self {
            max_concurrent_transfers: 4,
            total_rate_limit_mib_per_second: 0,
            forbid_cross_root_cache_copy: false,
        }
    }
}
impl DownloadPolicy {
    pub fn validate(&self) -> Result<(), String> {
        if !(1..=64).contains(&self.max_concurrent_transfers) {
            return Err("并行下载数量必须为 1..64".into());
        }
        if self.total_rate_limit_mib_per_second > 1024 {
            return Err("总下载速度限制必须为 0..1024 MiB/s；0 表示不限速".into());
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize)]
pub struct DownloadStatus {
    pub policy: DownloadPolicy,
    pub active_transfers: u64,
    pub queued_transfers: u64,
    pub paced_payload_bytes: u64,
}
struct Counters {
    active: AtomicU64,
    queued: AtomicU64,
    paced: AtomicU64,
}
pub struct DownloadScheduler {
    policy: DownloadPolicy,
    slots: Arc<Semaphore>,
    // The shared byte clock permits at most rate * 2ms of delayed-service
    // catch-up, with no initial credit. Reservations remain at most 64 KiB,
    // bounding cancelled unused debt to one small slice per active transfer.
    // No mutex survives a sleep or await.
    next_payload: Mutex<PayloadClock>,
    counters: Arc<Counters>,
}
pub struct DownloadPermit {
    _slot: OwnedSemaphorePermit,
    counters: Arc<Counters>,
}
impl Drop for DownloadPermit {
    fn drop(&mut self) {
        self.counters.active.fetch_sub(1, Ordering::Relaxed);
    }
}
struct Queued(Arc<Counters>);
impl Drop for Queued {
    fn drop(&mut self) {
        self.0.queued.fetch_sub(1, Ordering::Relaxed);
    }
}
#[derive(Default)]
struct PayloadClock {
    next: Option<Instant>,
}
impl PayloadClock {
    fn reserve(&mut self, now: Instant, service: Duration) -> Result<Instant, String> {
        let oldest = now.checked_sub(MAX_PACING_CATCHUP).unwrap_or(now);
        let start = self.next.map(|next| next.max(oldest)).unwrap_or(now);
        let deadline = start.checked_add(service).ok_or("下载限速时间超出范围")?;
        self.next = Some(deadline);
        Ok(deadline)
    }
}
impl DownloadScheduler {
    pub fn new(policy: DownloadPolicy) -> Result<Arc<Self>, String> {
        policy.validate()?;
        Ok(Arc::new(Self {
            slots: Arc::new(Semaphore::new(policy.max_concurrent_transfers as usize)),
            policy,
            next_payload: Mutex::new(PayloadClock::default()),
            counters: Arc::new(Counters {
                active: AtomicU64::new(0),
                queued: AtomicU64::new(0),
                paced: AtomicU64::new(0),
            }),
        }))
    }
    pub fn policy(&self) -> &DownloadPolicy {
        &self.policy
    }
    pub fn status(&self) -> DownloadStatus {
        DownloadStatus {
            policy: self.policy.clone(),
            active_transfers: self.counters.active.load(Ordering::Relaxed),
            queued_transfers: self.counters.queued.load(Ordering::Relaxed),
            paced_payload_bytes: self.counters.paced.load(Ordering::Relaxed),
        }
    }
    fn queued(&self) -> Queued {
        self.counters.queued.fetch_add(1, Ordering::Relaxed);
        Queued(self.counters.clone())
    }
    fn permit(&self, slot: OwnedSemaphorePermit) -> DownloadPermit {
        self.counters.active.fetch_add(1, Ordering::Relaxed);
        DownloadPermit {
            _slot: slot,
            counters: self.counters.clone(),
        }
    }
    pub async fn acquire(&self, cancel: &AtomicBool) -> Result<DownloadPermit, String> {
        cancelled(cancel)?;
        let _queued = self.queued();
        tokio::select! {
            biased;
            _=wait_cancelled(cancel)=>Err("下载已取消".into()),
            slot=self.slots.clone().acquire_owned()=>{
                cancelled(cancel)?;
                Ok(self.permit(slot.map_err(|_|"下载调度器已关闭")?))
            }
        }
    }
    /// Blocking workers consume the same semaphore and byte clock. Polling
    /// avoids nesting a Tokio runtime inside reqwest::blocking/Tauri workers.
    pub fn acquire_blocking(&self, cancel: &AtomicBool) -> Result<DownloadPermit, String> {
        let _queued = self.queued();
        loop {
            cancelled(cancel)?;
            match self.slots.clone().try_acquire_owned() {
                Ok(slot) => {
                    cancelled(cancel)?;
                    return Ok(self.permit(slot));
                }
                Err(tokio::sync::TryAcquireError::Closed) => return Err("下载调度器已关闭".into()),
                Err(tokio::sync::TryAcquireError::NoPermits) => std::thread::sleep(CANCEL_POLL),
            }
        }
    }
    fn reserve(&self, bytes: u64) -> Result<Option<Instant>, String> {
        if bytes == 0 || self.policy.total_rate_limit_mib_per_second == 0 {
            return Ok(None);
        }
        let rate = u64::from(self.policy.total_rate_limit_mib_per_second) * 1024 * 1024;
        let nanos = (u128::from(bytes) * 1_000_000_000).div_ceil(u128::from(rate)) as u64;
        let mut clock = self.next_payload.lock().map_err(|_| "下载限速器锁异常")?;
        clock
            .reserve(Instant::now(), Duration::from_nanos(nanos))
            .map(Some)
    }
    pub async fn throttle(&self, mut bytes: u64, cancel: &AtomicBool) -> Result<(), String> {
        while bytes > 0 {
            cancelled(cancel)?;
            let slice = bytes.min(DOWNLOAD_SLICE_BYTES as u64);
            // Already-funded slices must not register a new timer: even a past
            // sub-millisecond deadline can round into the next Tokio tick.
            if let Some(deadline) = self.reserve(slice)?.filter(|time| *time > Instant::now()) {
                tokio::select! {
                    biased;
                    _=wait_cancelled(cancel)=>return Err("下载已取消".into()),
                    _=tokio::time::sleep_until(tokio::time::Instant::from_std(deadline))=>{}
                }
            }
            cancelled(cancel)?;
            self.counters.paced.fetch_add(slice, Ordering::Relaxed);
            bytes -= slice;
        }
        Ok(())
    }
    pub fn throttle_blocking(&self, mut bytes: u64, cancel: &AtomicBool) -> Result<(), String> {
        while bytes > 0 {
            cancelled(cancel)?;
            let slice = bytes.min(DOWNLOAD_SLICE_BYTES as u64);
            if let Some(deadline) = self.reserve(slice)? {
                while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
                    cancelled(cancel)?;
                    std::thread::sleep(remaining.min(CANCEL_POLL));
                }
            }
            cancelled(cancel)?;
            self.counters.paced.fetch_add(slice, Ordering::Relaxed);
            bytes -= slice;
        }
        Ok(())
    }
}
fn cancelled(cancel: &AtomicBool) -> Result<(), String> {
    if cancel.load(Ordering::Acquire) {
        Err("下载已取消".into())
    } else {
        Ok(())
    }
}
async fn wait_cancelled(cancel: &AtomicBool) {
    loop {
        if cancel.load(Ordering::Acquire) {
            return;
        }
        tokio::time::sleep(CANCEL_POLL).await;
    }
}
fn global() -> &'static RwLock<Arc<DownloadScheduler>> {
    static GLOBAL: OnceLock<RwLock<Arc<DownloadScheduler>>> = OnceLock::new();
    GLOBAL.get_or_init(|| {
        RwLock::new(
            DownloadScheduler::new(DownloadPolicy::default())
                .expect("valid default download policy"),
        )
    })
}
pub fn download_snapshot() -> Arc<DownloadScheduler> {
    global().read().unwrap_or_else(|p| p.into_inner()).clone()
}
pub fn install_download_policy(policy: DownloadPolicy) -> Result<(), String> {
    install_download_snapshot(DownloadScheduler::new(policy)?);
    Ok(())
}
pub fn install_download_snapshot(scheduler: Arc<DownloadScheduler>) -> Arc<DownloadScheduler> {
    std::mem::replace(
        &mut *global().write().unwrap_or_else(|p| p.into_inner()),
        scheduler,
    )
}

#[cfg(test)]
#[path = "download_tests.rs"]
mod tests;
