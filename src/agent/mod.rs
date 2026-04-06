#[allow(clippy::module_inception)]
pub mod agent;
pub mod classifier;
pub mod context_analyzer;
pub mod context_compressor;
pub mod cost;
pub mod credentials;
pub mod dispatcher;
pub mod entrypoint;
pub mod eval;
pub mod history;
pub mod history_pruner;
pub mod loop_;
pub mod loop_detector;
pub mod memory_loader;
pub mod model_switch;
pub mod personality;
pub mod prompt;
pub mod streaming;
pub mod thinking;
pub mod tool_call_parser;
pub mod tool_execution;
pub mod tool_filter;

#[cfg(test)]
mod tests;

#[allow(unused_imports)]
pub use agent::{Agent, AgentBuilder, TurnEvent};
#[allow(unused_imports)]
pub use entrypoint::{process_message, run};
