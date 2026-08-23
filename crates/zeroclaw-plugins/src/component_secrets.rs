//! Host implementation for the tool world's `secrets` import.

use crate::component::PluginState;
use crate::component::bindings;
use crate::services::SecretLookupError;

impl bindings::tool::zeroclaw::plugin::secrets::Host for PluginState {
    async fn get(
        &mut self,
        name: String,
    ) -> Result<String, bindings::tool::zeroclaw::plugin::secrets::SecretError> {
        use bindings::tool::zeroclaw::plugin::secrets::SecretError;

        self.secret(&name).map_err(|error| match error {
            SecretLookupError::AccessDenied => SecretError::AccessDenied,
            SecretLookupError::NotFound => SecretError::NotFound,
            SecretLookupError::Unavailable => SecretError::Unavailable,
        })
    }
}
