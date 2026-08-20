pub mod builtin;
mod runner;
mod traits;

pub use runner::HookRunner;
pub(crate) use runner::tool_call_hook_context;
// These types are part of the crate's public hook API surface.
// They may appear unused internally but are intentionally re-exported for
// external integrations and future plugin authors.
#[allow(unused_imports)]
pub use traits::{HookHandler, HookResult};
pub use zeroclaw_api::hook::ToolCallHookContext;
