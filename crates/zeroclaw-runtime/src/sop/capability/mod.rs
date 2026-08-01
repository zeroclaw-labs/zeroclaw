mod bridge;
mod builtins;
mod forge_comment;
mod llm_generate;
mod registry;
mod types;

pub(crate) use forge_comment::resolve_forge_comment_target;
pub use forge_comment::{ChannelForgeAdapter, ForgeCommentAdapter, ForgeCommentCapability};
pub use llm_generate::{LlmGenerateAdapter, LlmGenerateCapability, ProviderLlmAdapter};
pub use registry::SopCapabilityRegistry;
pub use types::{CapabilityContext, CapabilityInfo, CapabilityResult, SopCapability};
