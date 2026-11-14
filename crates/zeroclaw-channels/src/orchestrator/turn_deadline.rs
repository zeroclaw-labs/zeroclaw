//! Bounded cooperative closeout retains results already produced by a turn.
use std::{future::Future, time::Duration};
use tokio_util::sync::CancellationToken;

pub(super) enum Outcome<T> {
    Completed(T),
    Cancelled,
    TimedOut,
}

pub(super) async fn run<F: Future>(
    future: F,
    cancellation: &CancellationToken,
    budget: Duration,
) -> Outcome<F::Output> {
    if cancellation.is_cancelled() {
        return Outcome::Cancelled;
    }
    if budget.is_zero() {
        cancellation.cancel();
        return Outcome::TimedOut;
    }
    tokio::pin!(future);
    let outcome = tokio::select! {
        biased;
        result = &mut future => {
            return if cancellation.is_cancelled() { Outcome::Cancelled } else { Outcome::Completed(result) };
        },
        () = cancellation.cancelled() => Outcome::Cancelled,
        () = tokio::time::sleep(budget) => Outcome::TimedOut,
    };
    cancellation.cancel();
    // The executor returns completed batch slots when cancellation is observed.
    // A provider/adapter that ignores it must not retain the conversation lane.
    let _ = tokio::time::timeout(Duration::from_secs(5), &mut future).await;
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[tokio::test(start_paused = true)]
    async fn deadline_drains_completed_results_before_returning() {
        let token = CancellationToken::new();
        let completed = Arc::new(Mutex::new(Vec::new()));
        let output = completed.clone();
        let cancel = token.clone();
        let outcome = run(
            async move {
                output.lock().unwrap().push("first operation completed");
                cancel.cancelled().await;
                output.lock().unwrap().push("partial batch closed");
            },
            &token,
            Duration::from_secs(20),
        )
        .await;
        assert!(matches!(outcome, Outcome::TimedOut));
        assert!(token.is_cancelled());
        assert_eq!(
            *completed.lock().unwrap(),
            ["first operation completed", "partial batch closed"]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn uncooperative_future_cannot_hold_the_lane_after_cleanup_budget() {
        let token = CancellationToken::new();
        let start = tokio::time::Instant::now();
        let outcome = run(
            std::future::pending::<()>(),
            &token,
            Duration::from_secs(20),
        )
        .await;
        assert!(matches!(outcome, Outcome::TimedOut));
        assert_eq!(start.elapsed(), Duration::from_secs(25));
    }

    #[tokio::test(start_paused = true)]
    async fn cancellation_drains_without_reporting_success() {
        let token = CancellationToken::new();
        let cancel = token.clone();
        let outcome = run(
            async move {
                cancel.cancel();
                std::future::pending::<()>().await;
            },
            &token,
            Duration::from_secs(20),
        )
        .await;
        assert!(matches!(outcome, Outcome::Cancelled));
    }

    #[tokio::test(start_paused = true)]
    async fn normal_completion_does_not_cancel_the_turn() {
        let token = CancellationToken::new();
        assert!(matches!(
            run(async { 42 }, &token, Duration::from_secs(20)).await,
            Outcome::Completed(42)
        ));
        assert!(!token.is_cancelled());
    }

    #[tokio::test(start_paused = true)]
    async fn cancellation_observed_by_executor_cannot_be_reported_as_completion() {
        let token = CancellationToken::new();
        let cancel = token.clone();
        let outcome = run(
            async move {
                cancel.cancel();
                42
            },
            &token,
            Duration::from_secs(20),
        )
        .await;
        assert!(matches!(outcome, Outcome::Cancelled));
    }

    #[tokio::test(start_paused = true)]
    async fn expired_budget_never_polls_another_operation() {
        let token = CancellationToken::new();
        let mut calls = 0;
        let outcome = run(
            async {
                calls += 1;
            },
            &token,
            Duration::ZERO,
        )
        .await;
        assert!(matches!(outcome, Outcome::TimedOut));
        assert_eq!(calls, 0);
        assert!(token.is_cancelled());
    }
}
