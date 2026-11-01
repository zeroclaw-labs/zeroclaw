//! A2A task executor — the host-provided business entry point.
//!
//! When the server receives a `SendMessage` for an alias, it hands the message to
//! that agent's executor, which returns a [`Task`]. Multiple agents each register
//! their own executor implementation (see [`super::handler::A2aServerState`]).
//!
//! The open-source implementation is synchronous: an executor returns a completed
//! `Task` (reply text in `artifacts`). Async task lifecycle / streaming are out of

use crate::a2a::a2a_wire::{Artifact, Message, Part, Task, TaskState, TaskStatus};

/// A2A task executor — host business entry point.
#[async_trait::async_trait]
pub trait A2aExecutor: Send + Sync {
    /// Handle an inbound message and return the corresponding [`Task`].
    async fn execute(&self, message: Message) -> anyhow::Result<Task>;
}

/// Build a synchronously-completed `Task` (executor convenience helper).
///
/// `context_id` links multi-turn continuations; an executor usually passes the
/// request message's `context_id` (or a generated value when absent).
#[must_use]
pub fn completed_task_with_text(context_id: String, text: String) -> Task {
    Task {
        id: uuid::Uuid::new_v4().to_string(),
        context_id,
        status: TaskStatus {
            state: TaskState::TaskStateCompleted,
            message: None,
            timestamp: None,
        },
        artifacts: vec![Artifact {
            artifact_id: uuid::Uuid::new_v4().to_string(),
            name: None,
            description: None,
            parts: vec![Part::text_str(text)],
        }],
        history: vec![],
        metadata: None,
    }
}
