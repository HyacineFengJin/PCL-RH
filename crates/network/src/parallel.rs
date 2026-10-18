//! Borrowed transfer futures share one bounded owner. Dropping this future or
//! returning an error closes every active response before its caller cleans
//! staging. Completed files are consumed immediately instead of retaining one
//! descriptor per manifest entry. Global connection admission remains the
//! DownloadScheduler's job; this bounds each submission's active work.
use std::future::{poll_fn, Future};
use std::task::Poll;

pub async fn for_each_bounded<I, F, T>(
    jobs: I,
    limit: usize,
    mut complete: impl FnMut(T) -> Result<(), String>,
) -> Result<(), String>
where
    I: IntoIterator<Item = F>,
    F: Future<Output = Result<T, String>>,
{
    if !(1..=64).contains(&limit) {
        return Err("并行下载数量必须为 1..64".into());
    }
    let mut pending = jobs.into_iter();
    let mut active = Vec::with_capacity(limit);
    loop {
        while active.len() < limit {
            let Some(job) = pending.next() else { break };
            active.push(Box::pin(job));
        }
        if active.is_empty() {
            return Ok(());
        }
        let (index, result) = poll_fn(|context| {
            for (index, job) in active.iter_mut().enumerate() {
                if let Poll::Ready(result) = job.as_mut().poll(context) {
                    return Poll::Ready((index, result));
                }
            }
            Poll::Pending
        })
        .await;
        drop(active.swap_remove(index));
        // No spawned worker survives either failure path. `active` and the
        // unstarted iterator are dropped before the caller receives the error.
        complete(result?)?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    use std::time::Duration;

    struct Active(Arc<AtomicUsize>);
    impl Drop for Active {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn bounded_work_is_concurrent_and_consumes_every_completion() {
        let active = Arc::new(AtomicUsize::new(0));
        let peak = AtomicUsize::new(0);
        let mut completed = Vec::new();
        for_each_bounded(
            (0..9).map(|index| {
                let active = active.clone();
                let peak = &peak;
                async move {
                    let count = active.fetch_add(1, Ordering::SeqCst) + 1;
                    let _owned = Active(active);
                    peak.fetch_max(count, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    Ok(index)
                }
            }),
            3,
            |value| {
                completed.push(value);
                Ok(())
            },
        )
        .await
        .unwrap();
        completed.sort();
        assert_eq!(completed, (0..9).collect::<Vec<_>>());
        assert_eq!(peak.load(Ordering::SeqCst), 3);
        assert_eq!(active.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn producer_and_consumer_errors_drop_active_work_before_returning() {
        for producer_error in [true, false] {
            let active = Arc::new(AtomicUsize::new(0));
            let started = AtomicUsize::new(0);
            let error = for_each_bounded(
                (0..8).map(|index| {
                    let active = active.clone();
                    let started = &started;
                    async move {
                        started.fetch_add(1, Ordering::SeqCst);
                        active.fetch_add(1, Ordering::SeqCst);
                        let _owned = Active(active);
                        if index == 2 {
                            if producer_error {
                                Err("producer".into())
                            } else {
                                Ok(index)
                            }
                        } else {
                            std::future::pending::<Result<usize, String>>().await
                        }
                    }
                }),
                3,
                |_| Err("consumer".into()),
            )
            .await
            .unwrap_err();
            assert_eq!(
                error,
                if producer_error {
                    "producer"
                } else {
                    "consumer"
                }
            );
            assert_eq!(started.load(Ordering::SeqCst), 3);
            assert_eq!(active.load(Ordering::SeqCst), 0);
        }
    }

    #[tokio::test]
    async fn dropping_owner_releases_all_active_futures() {
        let active = Arc::new(AtomicUsize::new(0));
        let work = for_each_bounded(
            (0..8).map(|_| {
                let active = active.clone();
                async move {
                    active.fetch_add(1, Ordering::SeqCst);
                    let _owned = Active(active);
                    std::future::pending::<Result<(), String>>().await
                }
            }),
            3,
            |_| Ok(()),
        );
        assert!(tokio::time::timeout(Duration::from_millis(10), work)
            .await
            .is_err());
        assert_eq!(active.load(Ordering::SeqCst), 0);
    }
}
