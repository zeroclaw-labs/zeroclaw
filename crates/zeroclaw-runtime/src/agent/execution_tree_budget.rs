use std::fmt;
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use tokio::task_local;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExecutionTreeBudgetRole {
    Root,
    Child,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExecutionTreeReservation {
    Iteration,
    FinalCompletion,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExecutionTreeBudgetExhausted;

impl fmt::Display for ExecutionTreeBudgetExhausted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("execution tree iteration budget exhausted")
    }
}

impl std::error::Error for ExecutionTreeBudgetExhausted {}

#[derive(Clone, Debug)]
pub struct ExecutionTreeBudget {
    remaining: Arc<AtomicUsize>,
    role: ExecutionTreeBudgetRole,
}

task_local! {
    static ACTIVE_EXECUTION_TREE_BUDGET: ExecutionTreeBudget;
}

impl ExecutionTreeBudget {
    #[must_use]
    pub fn from_limit(limit: Option<usize>) -> Option<Self> {
        limit.filter(|&limit| limit > 0).map(Self::root)
    }

    #[must_use]
    pub fn root(limit: usize) -> Self {
        assert!(limit > 0, "execution tree budget must be positive");
        Self {
            remaining: Arc::new(AtomicUsize::new(limit)),
            role: ExecutionTreeBudgetRole::Root,
        }
    }

    #[must_use]
    pub fn child(&self) -> Self {
        Self {
            remaining: Arc::clone(&self.remaining),
            role: ExecutionTreeBudgetRole::Child,
        }
    }

    #[must_use]
    pub fn role(&self) -> ExecutionTreeBudgetRole {
        self.role
    }

    #[must_use]
    pub fn remaining(&self) -> usize {
        self.remaining.load(Ordering::Acquire)
    }

    pub fn reserve(&self) -> Result<ExecutionTreeReservation, ExecutionTreeBudgetExhausted> {
        loop {
            let remaining = self.remaining.load(Ordering::Acquire);
            let reservation = match (self.role, remaining) {
                (_, 0) => return Err(ExecutionTreeBudgetExhausted),
                (ExecutionTreeBudgetRole::Child, 1) => {
                    return Err(ExecutionTreeBudgetExhausted);
                }
                (ExecutionTreeBudgetRole::Root, 1) => ExecutionTreeReservation::FinalCompletion,
                _ => ExecutionTreeReservation::Iteration,
            };
            if self
                .remaining
                .compare_exchange(
                    remaining,
                    remaining - 1,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok()
            {
                return Ok(reservation);
            }
        }
    }

    #[must_use]
    pub fn current() -> Option<Self> {
        ACTIVE_EXECUTION_TREE_BUDGET.try_with(Clone::clone).ok()
    }

    pub async fn scope<F>(budget: Self, future: F) -> F::Output
    where
        F: Future,
    {
        ACTIVE_EXECUTION_TREE_BUDGET.scope(budget, future).await
    }

    pub async fn scope_optional<F>(budget: Option<Self>, future: F) -> F::Output
    where
        F: Future,
    {
        match budget {
            Some(budget) => Self::scope(budget, future).await,
            None => future.await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ExecutionTreeBudget, ExecutionTreeBudgetExhausted, ExecutionTreeBudgetRole,
        ExecutionTreeReservation,
    };
    use std::sync::Arc;

    #[test]
    fn omitted_budget_is_disabled() {
        assert!(ExecutionTreeBudget::from_limit(None).is_none());
    }

    #[test]
    fn root_owns_the_final_slot() {
        let root = ExecutionTreeBudget::root(2);
        assert_eq!(root.reserve(), Ok(ExecutionTreeReservation::Iteration));
        assert_eq!(
            root.reserve(),
            Ok(ExecutionTreeReservation::FinalCompletion)
        );
        assert_eq!(root.reserve(), Err(ExecutionTreeBudgetExhausted));
        assert_eq!(root.remaining(), 0);
    }

    #[test]
    fn child_cannot_consume_the_final_slot_or_wrap() {
        let root = ExecutionTreeBudget::root(2);
        let child = root.child();
        assert_eq!(child.reserve(), Ok(ExecutionTreeReservation::Iteration));
        assert_eq!(child.reserve(), Err(ExecutionTreeBudgetExhausted));
        assert_eq!(root.remaining(), 1);
        assert_eq!(root.role(), ExecutionTreeBudgetRole::Root);
    }

    #[test]
    fn concurrent_children_leave_one_slot_for_root() {
        let root = Arc::new(ExecutionTreeBudget::root(32));
        let mut workers = Vec::new();
        for _ in 0..64 {
            let child = root.child();
            workers.push(std::thread::spawn(move || child.reserve().is_ok()));
        }
        let reservations = workers
            .into_iter()
            .filter_map(|worker| worker.join().ok())
            .filter(|reserved| *reserved)
            .count();
        assert_eq!(reservations, 31);
        assert_eq!(root.remaining(), 1);
        assert_eq!(
            root.reserve(),
            Ok(ExecutionTreeReservation::FinalCompletion)
        );
        assert_eq!(root.remaining(), 0);
    }

    #[tokio::test]
    async fn task_local_scope_is_awaited_but_not_detached() {
        let root = ExecutionTreeBudget::root(2);
        assert!(ExecutionTreeBudget::current().is_none());

        ExecutionTreeBudget::scope(root, async {
            assert_eq!(
                ExecutionTreeBudget::current().map(|budget| budget.role()),
                Some(ExecutionTreeBudgetRole::Root)
            );
            let detached = ::zeroclaw_spawn::spawn!(async { ExecutionTreeBudget::current() })
                .await
                .expect("detached budget probe should join");
            assert!(detached.is_none());
        })
        .await;

        assert!(ExecutionTreeBudget::current().is_none());
    }
}
