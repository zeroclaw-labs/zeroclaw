//! Host implementation for the tool world's `dispatch` import.
//!
//! `bindgen!` always generates the interface, so the world's import list needs
//! a `Host` impl to type-check. Whether a store can *reach* it is a separate
//! decision owned by the linker: `plugins_wit_v0_dispatch` stays off, so a
//! component that imports `dispatch` fails to instantiate until the wake
//! service is wired and the flag is derived from the `agent_dispatch` grant.
//!
//! Until then this impl fails closed for both audiences rather than reporting
//! a success that enqueued nothing: an instance without the grant is refused
//! outright, and a granted instance is told the service is unavailable.

use crate::PluginPermission;
use crate::component::PluginState;
use crate::component::bindings;

impl bindings::tool::zeroclaw::plugin::dispatch::Host for PluginState {
    async fn wake(
        &mut self,
        _target_agent: String,
        _prompt: String,
    ) -> Result<(), bindings::tool::zeroclaw::plugin::dispatch::DispatchError> {
        use bindings::tool::zeroclaw::plugin::dispatch::DispatchError;
        if !self.permission_enabled(PluginPermission::AgentDispatch) {
            return Err(DispatchError::AccessDenied);
        }
        Err(DispatchError::Unavailable)
    }
}
