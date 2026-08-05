//! A minimal periodic-task runner: Atlas's first background job scheduler.
//!
//! Deliberately small — one thing, done once, for the first real consumer
//! ([`crate::integrations::github::poll`]) rather than a general job-queue framework nothing
//! yet needs. [`spawn`] starts one `tokio::spawn`ed task per [`Job`], each on its own
//! `tokio::time::interval`; [`Handle::stop`] ends them all. Nothing here persists a job's
//! state across a restart — a missed tick during downtime is simply the next tick, which is
//! exactly right for the two consumers this exists for (Phase 12's GitHub poll fallback,
//! Phase 10's daily cycle snapshot): both re-derive their answer from current state rather
//! than from what happened while Atlas was down.
//!
//! # Why a job cannot bring the process down
//!
//! [`run_job`] catches a panicking job with [`FutureExt::catch_unwind`] rather than letting it
//! unwind through the `tokio::spawn`ed task. A bare `.await` here would mean one bad tick ends
//! that job's task *permanently* — nothing restarts it — silently turning "runs every 5
//! minutes" into "ran once". A caught panic is logged and the loop carries on to the next
//! tick instead.

use std::fmt;
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use futures_util::FutureExt;
use tokio::sync::oneshot;

type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// One periodic job: run `run`, every `interval`, until stopped.
#[derive(Clone)]
pub struct Job {
    /// Named for its log lines — the only way to tell two jobs' ticks apart once several are
    /// running.
    pub name: &'static str,
    pub interval: Duration,
    pub run: Arc<dyn Fn() -> BoxFuture<'static, ()> + Send + Sync>,
}

impl fmt::Debug for Job {
    // `Arc<dyn Fn() -> BoxFuture<...>>` is not `Debug`, and does not need to be — this exists
    // only so `Job` (and anything holding one) satisfies `missing_debug_implementations`,
    // which `-D warnings` promotes to a hard error.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Job")
            .field("name", &self.name)
            .field("interval", &self.interval)
            .field("run", &"Arc<dyn Fn() -> BoxFuture<...>>")
            .finish()
    }
}

/// The running jobs. Dropping this without calling [`stop`](Handle::stop) leaves every job
/// running — a `Handle` is not itself a stop-on-drop guard, since Atlas's own shutdown path
/// wants to decide exactly when background work ends relative to closing the database pools.
#[derive(Debug)]
pub struct Handle {
    stops: Vec<oneshot::Sender<()>>,
}

impl Handle {
    /// Ends every job. A no-op for a job whose task has already exited on its own — sending on
    /// a closed channel is simply ignored.
    pub fn stop(self) {
        for stop in self.stops {
            let _ = stop.send(());
        }
    }
}

/// Starts every job, each on its own task and its own interval.
#[must_use]
pub fn spawn(jobs: Vec<Job>) -> Handle {
    let mut stops = Vec::with_capacity(jobs.len());
    for job in jobs {
        let (stop_tx, stop_rx) = oneshot::channel();
        stops.push(stop_tx);
        tokio::spawn(run_job(job, stop_rx));
    }
    Handle { stops }
}

/// One job's loop: wait for the next tick or a stop signal, whichever comes first.
///
/// `tokio::time::interval`'s first tick fires immediately (not after the first `interval`),
/// which is the right default here — a poll fallback or a snapshot job should not sit idle
/// for its whole interval before doing anything the first time.
async fn run_job(job: Job, mut stop: oneshot::Receiver<()>) {
    let mut ticker = tokio::time::interval(job.interval);
    loop {
        tokio::select! {
            biased;
            _ = &mut stop => return,
            _ = ticker.tick() => {
                tracing::debug!(job = job.name, "running scheduled job");
                let result = AssertUnwindSafe((job.run)()).catch_unwind().await;
                if let Err(panic) = result {
                    tracing::error!(
                        job = job.name,
                        panic = %describe_panic(&panic),
                        "a scheduled job panicked; it will run again on its next tick"
                    );
                }
            }
        }
    }
}

/// Turns a caught panic payload into a loggable string — panics carry `Box<dyn Any>`, which
/// is almost always either a `&str` or a `String` (from `panic!("...")`/`.unwrap()`), and
/// anything else is reported by its type name rather than dropped silently.
fn describe_panic(panic: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = panic.downcast_ref::<&str>() {
        (*s).to_owned()
    } else if let Some(s) = panic.downcast_ref::<String>() {
        s.clone()
    } else {
        "non-string panic payload".to_owned()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tokio::time::{Duration, timeout};

    use super::*;

    #[tokio::test]
    async fn a_job_runs_on_its_interval_until_stopped() {
        let count = Arc::new(AtomicUsize::new(0));
        let counted = count.clone();
        let handle = spawn(vec![Job {
            name: "test-job",
            interval: Duration::from_millis(5),
            run: Arc::new(move || {
                let count = counted.clone();
                Box::pin(async move {
                    count.fetch_add(1, Ordering::SeqCst);
                })
            }),
        }]);

        timeout(Duration::from_secs(2), async {
            while count.load(Ordering::SeqCst) < 3 {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .expect("the job must have ticked at least 3 times well within 2s");

        handle.stop();

        // Give the stop a moment to land, then confirm ticking has actually ended rather than
        // merely slowed — the count must not keep climbing.
        tokio::time::sleep(Duration::from_millis(20)).await;
        let after_stop = count.load(Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(
            count.load(Ordering::SeqCst),
            after_stop,
            "a stopped job must not tick again"
        );
    }

    #[tokio::test]
    async fn a_panicking_tick_does_not_end_the_job() {
        let count = Arc::new(AtomicUsize::new(0));
        let counted = count.clone();
        let handle = spawn(vec![Job {
            name: "flaky-job",
            interval: Duration::from_millis(5),
            run: Arc::new(move || {
                let count = counted.clone();
                Box::pin(async move {
                    let n = count.fetch_add(1, Ordering::SeqCst);
                    // Panics on its first tick only, then behaves — proving the *next* tick
                    // still happens rather than the job's task having died with it.
                    assert_ne!(n, 0, "deliberate panic on the first tick");
                })
            }),
        }]);

        timeout(Duration::from_secs(2), async {
            while count.load(Ordering::SeqCst) < 3 {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .expect("later ticks must still run after an earlier one panicked");

        handle.stop();
    }

    #[tokio::test]
    async fn independent_jobs_tick_independently() {
        let a = Arc::new(AtomicUsize::new(0));
        let b = Arc::new(AtomicUsize::new(0));
        let (ac, bc) = (a.clone(), b.clone());
        let handle = spawn(vec![
            Job {
                name: "a",
                interval: Duration::from_millis(5),
                run: Arc::new(move || {
                    let a = ac.clone();
                    Box::pin(async move {
                        a.fetch_add(1, Ordering::SeqCst);
                    })
                }),
            },
            Job {
                name: "b",
                interval: Duration::from_secs(1),
                run: Arc::new(move || {
                    let b = bc.clone();
                    Box::pin(async move {
                        b.fetch_add(1, Ordering::SeqCst);
                    })
                }),
            },
        ]);

        timeout(Duration::from_secs(2), async {
            while a.load(Ordering::SeqCst) < 5 {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .expect("the fast job must tick well ahead of the slow one");
        // The slow job has a 1s interval and this waited well under that for the fast job's 5
        // ticks, so it should have fired at most once (its immediate first tick).
        assert!(b.load(Ordering::SeqCst) <= 1);

        handle.stop();
    }
}
