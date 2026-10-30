//! Process-local serialization point between Goal-owned operations.
//!
//! Surface adapters register control tickets before acknowledging deferred
//! commands. The Goal worker drains those tickets between admitted operations,
//! in registration order, while retaining its surface-owned execution lease.
//! Durable intent storage and authorization remain with the host/control plane.

use std::{
    collections::VecDeque,
    sync::{Arc, Mutex, MutexGuard},
};

use tokio::sync::{Mutex as AsyncMutex, Notify, OwnedMutexGuard, oneshot};
use uuid::Uuid;

/// One process-local checkpoint shared by a session's Goal worker and control
/// submissions. A new instance defines a new generation; tickets from one
/// generation can never be confused with tickets from a replacement worker.
#[derive(Clone)]
pub struct GoalParentTurnCheckpoint {
    inner: Arc<CheckpointInner>,
}

struct CheckpointInner {
    generation: Uuid,
    state: Mutex<CheckpointState>,
    /// Serializes workers attempting to enter the same checkpoint. The lock
    /// stays held for the admitted operation and is paired with `state` so
    /// ticket registration and next-operation admission have one atomic order.
    operation_gate: Arc<AsyncMutex<()>>,
    closed_notify: Notify,
}

#[derive(Default)]
struct CheckpointState {
    next_sequence: u64,
    operation_active: bool,
    closed: bool,
    pending: VecDeque<QueuedTicket>,
}

struct QueuedTicket {
    scheduled: oneshot::Sender<()>,
    settled: oneshot::Receiver<()>,
}

/// Stable identity for one deferred control registration within a process
/// generation. It is diagnostic/correlation context, not durable authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GoalParentTurnTicketId {
    generation: Uuid,
    sequence: u64,
}

impl GoalParentTurnTicketId {
    pub fn generation(self) -> Uuid {
        self.generation
    }

    pub fn sequence(self) -> u64 {
        self.sequence
    }
}

/// A deferred control's place in the next parent-turn boundary.
///
/// The submitter waits until the worker grants the ticket, commits its
/// revalidated transition, then drops or completes the ticket. Dropping it
/// also releases the worker, so cancellation cannot strand the checkpoint.
pub struct GoalDeferredControlTicket {
    id: GoalParentTurnTicketId,
    checkpoint: GoalParentTurnCheckpoint,
    scheduled: Option<oneshot::Receiver<()>>,
    settled: Option<oneshot::Sender<()>>,
}

impl std::fmt::Debug for GoalDeferredControlTicket {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GoalDeferredControlTicket")
            .field("id", &self.id)
            .field("scheduled", &self.scheduled.is_none())
            .finish()
    }
}

impl GoalDeferredControlTicket {
    pub fn id(&self) -> GoalParentTurnTicketId {
        self.id
    }

    /// Wait until the worker reaches this ticket at an operation boundary.
    /// The caller must commit only after this returns successfully.
    pub async fn wait_until_turn(&mut self) -> Result<(), GoalCheckpointClosed> {
        let Some(scheduled) = self.scheduled.take() else {
            return Err(GoalCheckpointClosed);
        };
        scheduled.await.map_err(|_| GoalCheckpointClosed)
    }

    /// Observe closure of the issuing generation, including after scheduling.
    /// A control waiting for command serialization must select against this
    /// future so a lifecycle fence can release the worker without deadlock.
    pub async fn wait_closed(&self) {
        self.checkpoint.wait_closed().await;
    }

    /// Mark this ticket's transition as settled and release the worker.
    pub fn complete(self) {}
}

impl Drop for GoalDeferredControlTicket {
    fn drop(&mut self) {
        if let Some(settled) = self.settled.take() {
            let _ = settled.send(());
        }
    }
}

/// The worker was dropped or replaced before this ticket reached its boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GoalCheckpointClosed;

impl std::fmt::Display for GoalCheckpointClosed {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .write_str("Goal checkpoint generation closed before the control ticket was admitted")
    }
}

impl std::error::Error for GoalCheckpointClosed {}

/// Exclusive admission for one Goal-owned operation, including its ordinary
/// output persistence and accounting settlement.
pub struct GoalParentTurnOperation {
    inner: Arc<CheckpointInner>,
    _operation_gate: OwnedMutexGuard<()>,
}

impl std::fmt::Debug for GoalParentTurnOperation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GoalParentTurnOperation")
            .field("generation", &self.inner.generation)
            .finish_non_exhaustive()
    }
}

impl Drop for GoalParentTurnOperation {
    fn drop(&mut self) {
        let mut state = self.inner.lock_state();
        debug_assert!(state.operation_active, "operation guard lost its admission");
        state.operation_active = false;
    }
}

impl std::fmt::Debug for GoalParentTurnCheckpoint {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GoalParentTurnCheckpoint")
            .field("generation", &self.inner.generation)
            .finish_non_exhaustive()
    }
}

impl Default for GoalParentTurnCheckpoint {
    fn default() -> Self {
        Self::new()
    }
}

impl GoalParentTurnCheckpoint {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(CheckpointInner {
                generation: Uuid::new_v4(),
                state: Mutex::new(CheckpointState::default()),
                operation_gate: Arc::new(AsyncMutex::new(())),
                closed_notify: Notify::new(),
            }),
        }
    }

    pub fn generation(&self) -> Uuid {
        self.inner.generation
    }

    /// Wait until this execution epoch can no longer admit work.
    ///
    /// Session adapters use this only while waiting to acquire the ordinary
    /// foreground/session lease. Closing a checkpoint therefore lets a
    /// lifecycle fence withdraw an unadmitted waiter without interrupting a
    /// lease or model operation that was already admitted.
    pub async fn wait_closed(&self) {
        loop {
            let notified = self.inner.closed_notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.inner.lock_state().closed {
                return;
            }
            notified.await;
        }
    }

    /// Register a deferred control atomically against the worker's next
    /// operation admission. Tickets are processed FIFO at that boundary.
    pub fn register_control(&self) -> Result<GoalDeferredControlTicket, GoalCheckpointClosed> {
        let (scheduled_tx, scheduled_rx) = oneshot::channel();
        let (settled_tx, settled_rx) = oneshot::channel();
        let mut state = self.inner.lock_state();
        if state.closed {
            return Err(GoalCheckpointClosed);
        }
        let sequence = state.next_sequence;
        state.next_sequence = state
            .next_sequence
            .checked_add(1)
            .expect("Goal checkpoint ticket sequence exhausted");
        let id = GoalParentTurnTicketId {
            generation: self.inner.generation,
            sequence,
        };
        state.pending.push_back(QueuedTicket {
            scheduled: scheduled_tx,
            settled: settled_rx,
        });
        Ok(GoalDeferredControlTicket {
            id,
            checkpoint: self.clone(),
            scheduled: Some(scheduled_rx),
            settled: Some(settled_tx),
        })
    }

    /// Enter the next Goal-owned operation. Any controls registered since the
    /// prior admission are granted and settled in FIFO order first. The state
    /// lock makes a registration either part of this boundary or part of the
    /// following one; there is no gap in which a command can slip past an
    /// operation that has not yet been admitted.
    pub async fn admit_operation(&self) -> Result<GoalParentTurnOperation, GoalCheckpointClosed> {
        let operation_gate = Arc::clone(&self.inner.operation_gate).lock_owned().await;
        loop {
            let queued = {
                let mut state = self.inner.lock_state();
                if state.closed {
                    return Err(GoalCheckpointClosed);
                } else if let Some(ticket) = state.pending.pop_front() {
                    Some(ticket)
                } else {
                    debug_assert!(
                        !state.operation_active,
                        "checkpoint admitted overlapping Goal operations"
                    );
                    state.operation_active = true;
                    None
                }
            };
            let Some(ticket) = queued else {
                return Ok(GoalParentTurnOperation {
                    inner: Arc::clone(&self.inner),
                    _operation_gate: operation_gate,
                });
            };
            if ticket.scheduled.send(()).is_ok() {
                // An immediate fence must not depend on the control owner
                // finishing acknowledgement or acquiring the command lock.
                // No operation has been admitted while this ticket is pending.
                tokio::select! {
                    biased;
                    () = self.wait_closed() => return Err(GoalCheckpointClosed),
                    _ = ticket.settled => {}
                }
            }
        }
    }

    /// Reject tickets that have not reached a boundary. This is called when
    /// the worker settles or is fenced so no deferred command waits forever
    /// for an operation that will never be admitted.
    pub fn close(&self) {
        let pending = {
            let mut state = self.inner.lock_state();
            state.closed = true;
            std::mem::take(&mut state.pending)
        };
        self.inner.closed_notify.notify_waiters();
        drop(pending);
    }
}

impl CheckpointInner {
    fn lock_state(&self) -> MutexGuard<'_, CheckpointState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn close_releases_a_worker_waiting_for_a_scheduled_control() {
        let checkpoint = GoalParentTurnCheckpoint::new();
        let mut ticket = checkpoint.register_control().unwrap();
        let worker_checkpoint = checkpoint.clone();
        let worker =
            zeroclaw_spawn::spawn!(async move { worker_checkpoint.admit_operation().await });
        tokio::time::timeout(std::time::Duration::from_secs(1), ticket.wait_until_turn())
            .await
            .unwrap()
            .unwrap();
        checkpoint.close();
        tokio::time::timeout(std::time::Duration::from_secs(1), ticket.wait_closed())
            .await
            .unwrap();
        // The owner stays alive throughout the drain: closure, not owner Drop,
        // must release an operation waiting on this already-scheduled ticket.
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(1), worker)
                .await
                .expect("checkpoint closure must release the fenced worker")
                .unwrap()
                .is_err()
        );
        drop(ticket);
    }
    use tokio::sync::oneshot;

    #[tokio::test]
    async fn controls_registered_before_admission_run_fifo() {
        let checkpoint = GoalParentTurnCheckpoint::new();
        let mut first = checkpoint.register_control().unwrap();
        let mut second = checkpoint.register_control().unwrap();
        let (admitted_tx, mut admitted_rx) = oneshot::channel();
        let worker_checkpoint = checkpoint.clone();
        let worker = zeroclaw_spawn::spawn!(async move {
            let operation = worker_checkpoint.admit_operation().await.unwrap();
            let _ = admitted_tx.send(());
            operation
        });

        first.wait_until_turn().await.unwrap();
        assert_eq!(first.id().sequence(), 0);
        assert!(admitted_rx.try_recv().is_err());
        first.complete();

        second.wait_until_turn().await.unwrap();
        assert_eq!(second.id().sequence(), 1);
        assert!(admitted_rx.try_recv().is_err());
        second.complete();

        admitted_rx.await.unwrap();
        drop(worker.await.unwrap());
    }

    #[tokio::test]
    async fn registration_during_operation_is_admitted_at_next_boundary() {
        let checkpoint = GoalParentTurnCheckpoint::new();
        let first_operation = checkpoint.admit_operation().await.unwrap();
        let mut ticket = checkpoint.register_control().unwrap();
        let (next_operation_tx, mut next_operation_rx) = oneshot::channel();
        let worker_checkpoint = checkpoint.clone();
        let worker = zeroclaw_spawn::spawn!(async move {
            let operation = worker_checkpoint.admit_operation().await.unwrap();
            let _ = next_operation_tx.send(());
            operation
        });

        assert!(next_operation_rx.try_recv().is_err());
        drop(first_operation);
        ticket.wait_until_turn().await.unwrap();
        assert!(next_operation_rx.try_recv().is_err());
        ticket.complete();
        next_operation_rx.await.unwrap();
        drop(worker.await.unwrap());
    }

    #[tokio::test]
    async fn dropped_control_ticket_does_not_strand_operation_admission() {
        let checkpoint = GoalParentTurnCheckpoint::new();
        let ticket = checkpoint.register_control().unwrap();
        drop(ticket);
        let _operation = checkpoint.admit_operation().await.unwrap();
    }

    #[test]
    fn replacement_checkpoint_has_a_distinct_generation() {
        let first = GoalParentTurnCheckpoint::new();
        let replacement = GoalParentTurnCheckpoint::new();
        assert_ne!(first.generation(), replacement.generation());
        assert_ne!(
            first.register_control().unwrap().id().generation(),
            replacement.register_control().unwrap().id().generation()
        );
    }

    #[tokio::test]
    async fn closed_generation_rejects_new_tickets_and_operation_admission() {
        let checkpoint = GoalParentTurnCheckpoint::new();
        let mut ticket = checkpoint.register_control().unwrap();
        checkpoint.close();
        assert!(ticket.wait_until_turn().await.is_err());
        assert!(checkpoint.register_control().is_err());
        assert!(checkpoint.admit_operation().await.is_err());
    }

    #[tokio::test]
    async fn close_wakes_unadmitted_lease_waiters_without_releasing_active_operation() {
        let checkpoint = GoalParentTurnCheckpoint::new();
        let operation = checkpoint.admit_operation().await.unwrap();
        let waiting_checkpoint = checkpoint.clone();
        let waiter = zeroclaw_spawn::spawn!(async move {
            waiting_checkpoint.wait_closed().await;
        });

        tokio::task::yield_now().await;
        assert!(!waiter.is_finished());
        checkpoint.close();
        waiter.await.unwrap();

        // The active operation remains owned across checkpoint closure. Once
        // it settles, no following operation may be admitted.
        drop(operation);
        assert!(checkpoint.admit_operation().await.is_err());
    }
}
