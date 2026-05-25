pub mod background_llm;
pub mod command_logger;
pub mod dialectic;
pub mod skill_autogen;
pub mod skill_patcher;
pub mod webhook_audit;

pub use background_llm::{BackgroundLlmConfig, background_llm_call};
pub use command_logger::CommandLoggerHook;
pub use dialectic::DialecticHook;
pub use skill_autogen::SkillAutogenHook;
pub use skill_patcher::SkillPatcherHook;
pub use webhook_audit::WebhookAuditHook;
