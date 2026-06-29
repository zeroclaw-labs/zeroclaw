#![doc = "Chat onboarding flow for ZeroClaw."]

pub mod agent_responder;
pub mod cli_transport;
pub mod llm_transport;
pub mod spec_builder;

pub use agent_responder::{AgentResponder, AgentTurn, InProcessAgentTurn};
pub use cli_transport::CliTransport;
pub use llm_transport::{LlmResponder, LlmTransport, SecretReader};
pub use spec_builder::{build_spec, required_fields, response_type_for, section_fields};
pub use zeroclaw_runtime::flow::{
    ConfiguredItem, FlowTransport, Node, NodeId, Outcome, Prompt, Spec, Step,
};
