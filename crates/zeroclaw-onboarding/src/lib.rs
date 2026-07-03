#![doc = "Chat onboarding flow for ZeroClaw."]

pub mod agent_responder;
pub mod cli_transport;
pub mod driver;
pub mod freeform;
pub mod i18n;
pub mod llm_transport;
pub mod outcome_message;
pub mod phrasing;
pub mod spec_builder;

pub use agent_responder::{
    AgentResponder, AgentTurn, InProcessAgentTurn, OperatorIo, TtyOperatorIo,
};
pub use cli_transport::{CliSecretSource, CliTransport, TtyPasswordSource};
pub use driver::{DriverError, FlowRequest, build_flow_spec, run_flow};
pub use freeform::{FreeformError, render_preview, run_freeform, spec_brief};
pub use llm_transport::{LlmResponder, LlmTransport, SecretReader, TtySecretReader};
pub use phrasing::{AgentPhraser, phrase_spec};
pub use spec_builder::{
    FieldScope, append_peer_group_branch, append_personality_branch, build_spec, section_fields,
};
