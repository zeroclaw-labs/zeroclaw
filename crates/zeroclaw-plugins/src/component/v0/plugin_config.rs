// Host-side WIT `plugin-config` implementation for all three component-model
// plugin worlds (`tool-plugin`, `memory-plugin`, `channel-plugin`).

use super::bindings;
use crate::component::plugin_store::PluginStore;

/// Generate `plugin-config::Host for PluginStore` for one bindgen world.
macro_rules! impl_plugin_config_host {
    ($world:ident) => {
        impl bindings::$world::zeroclaw::plugin::plugin_config::Host for PluginStore {
            async fn get_proxy_url(&mut self) -> Option<String> {
                self.network_config.proxy_url.clone()
            }
        }
    };
}

impl_plugin_config_host!(tool);
impl_plugin_config_host!(memory);
impl_plugin_config_host!(channel);
