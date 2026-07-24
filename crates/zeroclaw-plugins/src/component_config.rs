//! Host implementation for the channel world's point-of-use `config` import.

use crate::component::PluginState;
use crate::component::bindings::channel::zeroclaw::plugin::config::{ConfigError, Host};
use crate::services::ConfigLookupError;

impl Host for PluginState {
    async fn get(&mut self) -> Result<String, ConfigError> {
        let config = self.guest_public_config().map_err(|error| match error {
            ConfigLookupError::AccessDenied => ConfigError::AccessDenied,
            ConfigLookupError::Unavailable => ConfigError::Unavailable,
        })?;
        serde_json::to_string(&config).map_err(|_| ConfigError::Unavailable)
    }
}
