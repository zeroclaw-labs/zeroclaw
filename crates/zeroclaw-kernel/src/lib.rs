//! Session execution owner. Adapters supply capabilities; history never executes effects.
pub mod session;

pub mod approval;
pub mod approval_gate;
pub mod tool_execution;

pub mod composition;
