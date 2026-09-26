//! Config operations shared by every surface that serves them.
//!
//! The gateway's HTTP routes and the RPC `config/*` methods call these same
//! functions, so the two surfaces cannot drift apart. Each surface still
//! commits writes through its own config-write path.

pub mod agent_options;
pub mod agent_owned_state;
pub mod context_window;
pub mod delete;
pub mod document;
pub mod drift;
pub mod sections;
