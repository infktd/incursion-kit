//! The loop shared by the workers: run a cycle, sleep, repeat until cancelled, backing off
//! after failures.

use std::future::Future;
use std::time::Duration;

use tokio_util::sync::CancellationToken;

/// Exponential backoff: `initial`, doubling after each consecutive failure up to `max`.
#[derive(Debug, Clone)]
pub(crate) struct Backoff {
    initial: Duration,
    max: Duration,
    next: Duration,
}

impl Backoff {
    pub(crate) fn new(initial: Duration, max: Duration) -> Self {
        Self {
            initial,
            max: max.max(initial),
            next: initial,
        }
    }

    /// The delay after a failure; the following one is twice as long.
    pub(crate) fn failure(&mut self) -> Duration {
        let delay = self.next;
        self.next = (self.next * 2).min(self.max);
        delay
    }

    pub(crate) fn success(&mut self) {
        self.next = self.initial;
    }
}

/// Sleep for `duration`; `false` if cancelled first.
pub(crate) async fn sleep(cancel: &CancellationToken, duration: Duration) -> bool {
    tokio::select! {
        () = cancel.cancelled() => false,
        () = tokio::time::sleep(duration) => true,
    }
}

/// Run `cycle` until `cancel` fires: after a success wait `interval`, after a failure wait the
/// next backoff delay. A cycle in progress is dropped on cancellation (open transactions roll
/// back).
pub(crate) async fn run<F, Fut, E>(
    name: &'static str,
    cancel: CancellationToken,
    interval: Duration,
    mut backoff: Backoff,
    mut cycle: F,
) where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<(), E>>,
    E: std::fmt::Display,
{
    tracing::info!(
        worker = name,
        interval_secs = interval.as_secs_f64(),
        "worker started"
    );
    loop {
        let result = tokio::select! {
            () = cancel.cancelled() => break,
            result = cycle() => result,
        };
        let delay = match result {
            Ok(()) => {
                backoff.success();
                interval
            }
            Err(e) => {
                let delay = backoff.failure();
                tracing::error!(
                    worker = name,
                    error = %e,
                    retry_in_secs = delay.as_secs(),
                    "worker cycle failed"
                );
                delay
            }
        };
        if !sleep(&cancel, delay).await {
            break;
        }
    }
    tracing::info!(worker = name, "worker stopped");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_up_to_the_cap_and_resets() {
        let mut backoff = Backoff::new(Duration::from_secs(30), Duration::from_secs(100));
        assert_eq!(backoff.failure(), Duration::from_secs(30));
        assert_eq!(backoff.failure(), Duration::from_secs(60));
        assert_eq!(backoff.failure(), Duration::from_secs(100));
        assert_eq!(backoff.failure(), Duration::from_secs(100));
        backoff.success();
        assert_eq!(backoff.failure(), Duration::from_secs(30));
    }

    #[tokio::test(start_paused = true)]
    async fn run_stops_when_cancelled() {
        let cancel = CancellationToken::new();
        let runs = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = runs.clone();
        let handle = tokio::spawn(run(
            "test",
            cancel.clone(),
            Duration::from_secs(10),
            Backoff::new(Duration::from_secs(1), Duration::from_secs(1)),
            move || {
                let counter = counter.clone();
                async move {
                    let n = counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    if n.is_multiple_of(2) {
                        Ok(())
                    } else {
                        Err("boom")
                    }
                }
            },
        ));
        tokio::time::sleep(Duration::from_secs(25)).await;
        cancel.cancel();
        handle.await.unwrap();
        // t=0 ok, t=10 err, t=11 ok, t=21 err, t=22 ok.
        assert_eq!(runs.load(std::sync::atomic::Ordering::SeqCst), 5);
    }
}
