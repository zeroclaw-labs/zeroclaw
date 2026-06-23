//! The durable run/task control-plane — supervised lifecycle for delegated and
//! subagent runs.
//!
//! A NEW small-module tree (modular-architecture) behind compiler-enforced traits, so
//! the megafiles (`tools/delegate.rs`, `tools/spawn_subagent.rs`, `daemon/mod.rs`)
//! change only as thin wiring at named seams:
//!   * [`task_registry`] — the `TaskRegistry` trait + `TaskRecord`/`TaskKind`/`TaskStatus`.
//!   * [`task_store_sqlite`] — the single SQLite impl, modelled on
//!     `zeroclaw_infra::acp_session_store`.
//!   * [`authority`] — `is_authoritative`: the runtime-authority reclaim guard.
//!   * [`reaper`] — the periodic sweep + one-shot startup crash-recovery pass.
//!   * [`boot`] — the per-run [`ControlPlaneHandle`].
//!   * [`global`] — the process-global accessor producers reach the handle through.
//!
//! Producers (background `delegate`, `spawn_subagent`) register a task before the run
//! and resolve it on completion; the reaper reconciles an abandoned task to a terminal
//! state (`Lost`/`TimedOut`) from OUTSIDE the task body.

pub mod authority;
pub mod boot;
pub mod global;
pub mod reaper;
pub mod task_registry;
pub mod task_store_sqlite;

pub use authority::is_authoritative;
pub use boot::ControlPlaneHandle;
pub use global::{control_plane, init_control_plane};
pub use task_registry::{TaskKind, TaskRecord, TaskRegistry, TaskStatus};
pub use task_store_sqlite::SqliteTaskStore;
