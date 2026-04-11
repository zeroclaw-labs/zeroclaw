//! Generic event dispatch and audit subsystem.
//!
//! This module unifies how the application reacts to ambient events from
//! many sources: hardware peripherals (GPIO, sensors, STM32 boards), MQTT
//! messages, HTTP webhooks, cron schedules, and manual triggers.
//!
//! ## Layers
//!
//! - [`condition`] — JSON path + direct comparison DSL for filtering events
//! - [`audit`] — persists events and dispatch results to the memory backend
//! - [`router`] — handler registration and event fan-out
//! - [`types`] — shared event/result types
//!
//! ## Origin
//!
//! Extracted from the unfinished SOP (Standard Operating Procedure) engine
//! that was added to the codebase in commit `1a0e5547` (8,997 lines) but
//! never wired into the build. The SOP execution machine, gates, metrics,
//! and LLM tools were intentionally **not** extracted because:
//!
//! 1. The agent loop already handles sequential reasoning natively.
//! 2. The `WaitingApproval` state machine conflicts with agent-first design.
//! 3. Approval gating is already covered by `src/approval/` and
//!    `src/security/policy.rs` (autonomy modes + risk scoring).
//! 4. SOP-specific metrics have no value without the SOP execution layer.
//!
//! What was extracted is the genuinely reusable substrate: a generic event
//! router, condition evaluator, and audit logger that any subsystem can use.
//!
//! See `docs/ARCHITECTURE.md` §15A.6 for the full extraction rationale and
//! the list of preserved-but-unused SOP files.
//!
//! ## Usage
//!
//! ```ignore
//! use std::sync::Arc;
//! use zeroclaw::dispatch::{
//!     DispatchAuditLogger, DispatchEvent, EventHandler, EventRouter,
//!     EventSource, HandlerOutcome,
//! };
//!
//! // 1. Build the router and audit logger once at startup.
//! let router = Arc::new(EventRouter::new());
//! let audit = Arc::new(DispatchAuditLogger::new(memory.clone()));
//!
//! // 2. Register handlers (notification, agent trigger, ontology update, ...).
//! router.register(Arc::new(MyHandler::new()));
//!
//! // 3. From any subsystem, publish events.
//! let event = DispatchEvent::new(
//!     EventSource::Peripheral,
//!     Some("nucleo/pin_3".into()),
//!     Some("1".into()),
//! );
//! audit.log_event(&event).await?;
//! let result = router.dispatch(event).await;
//! audit.log_result(&result).await?;
//! ```

pub mod audit;
pub mod condition;
pub mod handlers;
#[cfg(feature = "mqtt")]
pub mod mqtt;
pub mod router;
pub mod types;

// These re-exports are part of the public API but the binary crate does not
// use them directly yet. Allow unused so `cargo check` stays clean while the
// library surface remains discoverable.
#[allow(unused_imports)]
pub use audit::DispatchAuditLogger;
#[allow(unused_imports)]
pub use condition::evaluate_condition;
#[allow(unused_imports)]
pub use handlers::{AgentTriggerHandler, EventFilter, NotificationHandler};
#[allow(unused_imports)]
pub use router::{EventHandler, EventRouter};
#[allow(unused_imports)]
pub use types::{DispatchEvent, DispatchResult, EventSource, HandlerOutcome};
