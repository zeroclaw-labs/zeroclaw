//! A2A server (inbound) — protocol layer: JSON-RPC handler + axum routes + abstractions.
//!
//! Hosts register agents through [`A2aServerState`] (an executor plus an optional card
//! source); this crate owns the protocol and routing. The implementation is synchronous

pub mod agent_card;
pub mod error;
pub mod executor;
pub mod handler;
pub mod protocol;

pub use agent_card::{AgentCardProducer, StaticAgentCard, static_card_producer};
pub use error::ServerError;
pub use executor::{A2aExecutor, completed_task_with_text};
pub use handler::{
    A2aAgent, A2aServerState, AdvertisedEndpoint, JsonRpcRequest, a2a_router, handle_jsonrpc,
    server_error_value,
};
pub use protocol::{
    A2A_PROTOCOL_VERSION, jsonrpc_error, task_not_found_error, unsupported_operation_error,
    version_not_supported_error, wants_return_immediately,
};
