//! Progress follows the turn future; no detached notifier can outlive its owner.
use std::{future::Future, time::Duration};

pub(super) async fn with_progress<T, F, N, NF>(future: F, interval: Duration, mut notify: N) -> T
where
    F: Future<Output = T>,
    N: FnMut(Duration) -> NF,
    NF: Future<Output = ()>,
{
    if interval.is_zero() {
        return future.await;
    }
    tokio::pin!(future);
    let started = tokio::time::Instant::now();
    let mut ticker = tokio::time::interval_at(started + interval, interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            biased;
            result = &mut future => return result,
            _ = ticker.tick() => {
                // A slow transport must not hide turn completion or hang it.
                let notice = tokio::time::timeout(Duration::from_secs(5), crate::paced_channel::scope_progress_send(notify(started.elapsed())));
                tokio::select! {
                    biased;
                    result = &mut future => return result,
                    _ = notice => {}
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    #[tokio::test(start_paused = true)]
    async fn periodic_notices_stop_when_the_turn_finishes() {
        let calls = Arc::new(AtomicUsize::new(0));
        let count = Arc::clone(&calls);
        let value = with_progress(
            async {
                tokio::time::sleep(Duration::from_secs(125)).await;
                7
            },
            Duration::from_secs(60),
            move |_| {
                count.fetch_add(1, Ordering::SeqCst);
                async {}
            },
        )
        .await;
        assert_eq!(value, 7);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        tokio::time::sleep(Duration::from_secs(120)).await;
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }
    #[tokio::test(start_paused = true)]
    async fn slow_notice_does_not_delay_completed_turn() {
        let start = tokio::time::Instant::now();
        with_progress(
            tokio::time::sleep(Duration::from_secs(61)),
            Duration::from_secs(60),
            |_| std::future::pending(),
        )
        .await;
        assert_eq!(start.elapsed(), Duration::from_secs(61));
    }
    #[tokio::test]
    async fn disabled_and_immediately_finished_turns_send_nothing() {
        for interval in [Duration::ZERO, Duration::from_secs(60)] {
            let value = with_progress(async { 3 }, interval, |_| async {
                panic!("unexpected notice")
            })
            .await;
            assert_eq!(value, 3);
        }
    }
}
