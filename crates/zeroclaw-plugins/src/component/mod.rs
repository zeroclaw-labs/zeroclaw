// Component Model (WASIP2 / WIT) plugin adapters.
//
// Exports the three adapter types (`ComponentTool`, `ComponentMemory`,
// `ComponentChannel`) and the shared `ComponentEngine`. The `bindings` module
// is internal — consumers use the adapters instead of the bindgen output.

mod call_plugin;
mod engine;
pub(crate) mod plugin_store;
pub mod v0;
mod wrap_plugin;

pub use engine::ComponentEngine;
