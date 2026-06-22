// Bindgen-specific glue wiring `PluginStore` (defined at the `component`
// level, shared across all binding versions) into the `v0` bindgen worlds.
//
// Kept in `v0` rather than `component` because it's tied directly to the
// `v0` bindgen output (`bindings::tool::ToolPlugin`, etc.) and to the
// `plugins-wit-v0` feature flag — a future `v1` bindgen world would need its
// own version of this file, not a shared one.

use wasmtime::component::HasSelf;

use crate::component::plugin_store::PluginStore;
use crate::error::PluginError;

use super::bindings;

// ── types::Host (empty marker trait) ─────────────────────────────────────────

impl bindings::tool::zeroclaw::plugin::types::Host for PluginStore {}
impl bindings::memory::zeroclaw::plugin::types::Host for PluginStore {}
impl bindings::channel::zeroclaw::plugin::types::Host for PluginStore {}

// ── Linker wiring helpers ─────────────────────────────────────────────────────

/// Wire all host interfaces for the `tool-plugin` world into `linker`.
pub fn add_to_linker_tool(
    linker: &mut wasmtime::component::Linker<PluginStore>,
) -> Result<(), PluginError> {
    // Use feature flags to allow developers to link in wit bindings that aren't stabilized yet.
    let mut options = crate::component::v0::bindings::tool::LinkOptions::default();
    #[cfg(feature = "plugins-wit-v0")]
    {
        options.plugins_wit_v0(true);
    }
    bindings::tool::ToolPlugin::add_to_linker::<PluginStore, HasSelf<PluginStore>>(
        linker,
        &options,
        |x| x,
    )
    .map_err(PluginError::from)?;
    Ok(())
}

/// Wire all host interfaces for the `memory-plugin` world into `linker`.
pub fn add_to_linker_memory(
    linker: &mut wasmtime::component::Linker<PluginStore>,
) -> Result<(), PluginError> {
    // Use feature flags to allow developers to link in wit bindings that aren't stabilized yet.
    let mut options = crate::component::v0::bindings::memory::LinkOptions::default();
    #[cfg(feature = "plugins-wit-v0")]
    {
        options.plugins_wit_v0(true);
    }
    bindings::memory::MemoryPlugin::add_to_linker::<PluginStore, HasSelf<PluginStore>>(
        linker,
        &options,
        |x| x,
    )
    .map_err(PluginError::from)?;
    Ok(())
}

/// Wire all host interfaces for the `channel-plugin` world into `linker`.
pub fn add_to_linker_channel(
    linker: &mut wasmtime::component::Linker<PluginStore>,
) -> Result<(), PluginError> {
    // Use feature flags to allow developers to link in wit bindings that aren't stabilized yet.
    let mut options = crate::component::v0::bindings::channel::LinkOptions::default();
    #[cfg(feature = "plugins-wit-v0")]
    {
        options.plugins_wit_v0(true);
    }
    bindings::channel::ChannelPlugin::add_to_linker::<PluginStore, HasSelf<PluginStore>>(
        linker,
        &options,
        |x| x,
    )
    .map_err(PluginError::from)?;
    Ok(())
}
