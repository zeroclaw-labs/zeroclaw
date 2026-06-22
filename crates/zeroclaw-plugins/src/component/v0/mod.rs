// Component Model (WASIP2 / WIT) v0 plugin adapters.
//
// Exports the three adapter types (`ComponentTool`, `ComponentMemory`,
// `ComponentChannel`) and the shared `ComponentEngine`. The `bindings` module
// is internal — consumers use the adapters instead of the bindgen output.

pub use channel_component::ComponentChannel;
pub use memory_component::ComponentMemory;
pub use tool_component::ComponentTool;

mod bindings;
mod channel_component;
pub(super) mod gateway_host;
pub(super) mod http_helpers_host;
pub(super) mod logging;
mod memory_component;
pub(super) mod plugin_config;
mod plugin_linker;
mod tool_component;
pub(super) mod websocket_host;
