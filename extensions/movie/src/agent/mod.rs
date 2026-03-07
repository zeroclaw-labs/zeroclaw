#[allow(clippy::module_inception)]
pub mod agent;
pub mod classifier;
pub mod dispatcher;
pub mod loop_;
pub mod memory_loader;
pub mod prompt;
pub mod quota_aware;
pub mod research;
pub mod session;
pub mod team_orchestration;

#[cfg(test)]
mod tests;

#[allow(unused_imports)]
pub use agent::{Agent, AgentBuilder};
#[allow(unused_imports)]
pub use loop_::{process_message, process_message_with_session, run, run_tool_call_loop};
