#![doc = "Chat onboarding flow for ZeroClaw."]

pub mod agent_responder;
pub mod cli_transport;
pub mod driver;
pub mod i18n;
pub mod llm_transport;
pub mod outcome_message;
pub mod phrasing;
pub mod spec_builder;

pub use agent_responder::{
    AgentResponder, AgentTurn, InProcessAgentTurn, OperatorIo, TtyOperatorIo,
};
pub use cli_transport::{CliSecretSource, CliTransport, NoSecretSource, TtyPasswordSource};
pub use driver::{DriverError, FlowRequest, run_flow};
pub use llm_transport::{LlmResponder, LlmTransport, SecretReader, TtySecretReader};
pub use phrasing::{
    AgentPhraser, DescriptionPhraser, FieldPhrasingContext, PromptPhraser, phrase_spec,
};
pub use spec_builder::{
    FieldScope, append_peer_group_branch, append_personality_branch, build_spec, build_spec_scoped,
    required_fields, response_type_for, section_fields,
};
pub use zeroclaw_runtime::flow::{
    ConfiguredItem, FlowTransport, Node, NodeId, Outcome, Prompt, Spec, Step,
};
