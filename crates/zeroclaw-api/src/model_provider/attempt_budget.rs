use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// A cumulative allowance for client-side generation submissions.
///
/// The caller creates one budget for the whole operation, including repairs,
/// adapter retries and provider failover. Clones share its remaining allowance.
/// This does not count remote executions or replace usage accounting.
#[derive(Clone, Debug)]
pub struct GenerationAttemptBudget {
    remaining: Arc<AtomicUsize>,
}

impl GenerationAttemptBudget {
    #[must_use]
    pub fn new(attempts: usize) -> Self {
        Self {
            remaining: Arc::new(AtomicUsize::new(attempts)),
        }
    }

    #[must_use]
    pub fn remaining(&self) -> usize {
        self.remaining.load(Ordering::Relaxed)
    }

    /// Check before preparing another attempt. Only [`Self::try_spend`]
    /// authorizes a submission when several users share this budget.
    pub fn ensure_available(&self) -> Result<(), GenerationAttemptBudgetExhausted> {
        if self.remaining() == 0 {
            Err(GenerationAttemptBudgetExhausted::default())
        } else {
            Ok(())
        }
    }

    /// Spend immediately before starting generation I/O, after request building.
    /// There is no refund: a failed or cancelled send may have reached the server.
    pub fn try_spend(&self) -> Result<(), GenerationAttemptBudgetExhausted> {
        self.remaining
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |remaining| {
                remaining.checked_sub(1)
            })
            .map(|_| ())
            .map_err(|_| GenerationAttemptBudgetExhausted::default())
    }
}

/// Terminal for the entire operation, including every retry/failover wrapper.
#[derive(Debug, Default, thiserror::Error)]
#[error("generation request budget exhausted")]
pub struct GenerationAttemptBudgetExhausted {
    #[source]
    source: Option<Box<dyn std::error::Error + Send + Sync>>,
}

impl GenerationAttemptBudgetExhausted {
    /// Retain the failure that exhausted recovery without losing this typed
    /// terminal marker when another provider wraps the error as a source.
    #[must_use]
    pub fn with_cause(cause: anyhow::Error) -> Self {
        Self {
            source: Some(cause.into()),
        }
    }
}

/// The provider cannot enforce bounded generation and has sent no request.
/// Reliability may advance to another eligible configured candidate.
#[derive(Debug, Clone, Copy, thiserror::Error)]
#[error("provider does not support bounded generation requests")]
pub struct BoundedChatUnsupported;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cloned_budgets_share_a_cumulative_allowance() {
        let budget = GenerationAttemptBudget::new(2);
        budget.try_spend().unwrap();
        budget.clone().try_spend().unwrap();
        assert!(budget.try_spend().is_err());
        assert_eq!(budget.remaining(), 0);
        assert_eq!(GenerationAttemptBudget::new(2).remaining(), 2);
    }

    #[test]
    fn competing_admissions_never_overspend() {
        let budget = GenerationAttemptBudget::new(2);
        let workers: Vec<_> = (0..32)
            .map(|_| {
                let budget = budget.clone();
                std::thread::spawn(move || usize::from(budget.try_spend().is_ok()))
            })
            .collect();
        assert_eq!(
            workers
                .into_iter()
                .map(|worker| worker.join().unwrap())
                .sum::<usize>(),
            2
        );
    }
}
