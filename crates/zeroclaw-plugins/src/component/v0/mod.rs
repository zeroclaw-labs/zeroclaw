// Component Model (WASIP2 / WIT) v0 plugin adapters.
//
// Exports the three adapter types (`ComponentTool`, `ComponentMemory`,
// `ComponentChannel`) and the shared `ComponentEngine`. The `bindings` module
// is internal — consumers use the adapters instead of the bindgen output.

pub use channel_component::ComponentChannel;
pub use memory_component::ComponentMemory;
pub use tool_component::ComponentTool;

mod bindings;
mod call_plugin;
mod channel_component;
pub(super) mod logging;
mod memory_component;
mod plugin_store;
mod tool_component;
mod wrap_plugin;
