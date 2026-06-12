// Component Model (WASIP2 / WIT) plugin adapters.
//
// Exports the three adapter types (`ComponentTool`, `ComponentMemory`,
// `ComponentChannel`) and the shared `ComponentEngine`. The `bindings` module
// is internal — consumers use the adapters instead of the bindgen output.

mod engine;
pub mod v0;
