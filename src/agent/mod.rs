#[allow(clippy::module_inception)]
pub mod agent;
pub mod analytics;
pub mod classifier;
pub mod conversation;
pub mod dispatcher;
pub mod handoff;
pub mod loop_;
pub mod memory_loader;
pub mod prompt;

#[cfg(test)]
mod tests;

#[allow(unused_imports)]
pub use agent::{Agent, AgentBuilder};
#[allow(unused_imports)]
pub use loop_::{process_message, run};
