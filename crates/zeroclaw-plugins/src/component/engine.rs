// Shared wasmtime engine configured for the Component Model.
// `ComponentEngine` is cheaply shareable across threads via `Arc`.

use crate::error::PluginError;

/// A `wasmtime::Engine` configured for the Component Model.
///
/// Wrap in `Arc` to share across multiple plugin instances; compilation
/// (`compile`) is expensive and is cached per-component by the caller.
pub struct ComponentEngine(wasmtime::Engine);

impl ComponentEngine {
    /// Create a new engine with Component Model support enabled.
    pub fn new() -> Result<Self, PluginError> {
        let mut config = wasmtime::Config::new();
        config.wasm_component_model(true);
        Ok(Self(
            wasmtime::Engine::new(&config).map_err(PluginError::from)?,
        ))
    }

    /// Compile a raw WASM component binary into a cached `Component`.
    ///
    /// Callers should store the returned `Arc<Component>` and reuse it
    /// across instantiations — compilation is the expensive step.
    pub fn compile(&self, bytes: &[u8]) -> Result<wasmtime::component::Component, PluginError> {
        let component =
            wasmtime::component::Component::new(&self.0, bytes).map_err(PluginError::from)?;
        Ok(component)
    }

    /// Access the underlying `wasmtime::Engine` (e.g. to build a `Linker`).
    pub fn engine(&self) -> &wasmtime::Engine {
        &self.0
    }
}
