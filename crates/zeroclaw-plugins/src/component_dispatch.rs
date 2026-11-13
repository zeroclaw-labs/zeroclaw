//! Host implementation for the tool world's `dispatch` import.
//!
//! `bindgen!` always generates the interface, so the world's import list needs
//! a `Host` impl to type-check. Whether a store can *reach* it is a separate
//! decision owned by the linker: `plugins_wit_v0_dispatch` is derived from the
//! `agent_dispatch` grant in [`crate::component::OptionalImports`], and
//! [`crate::component::ensure_imports_coherent`] refuses to instantiate a
//! store/linker pair that disagrees, so a component importing `dispatch` fails
//! to instantiate unless its scope was granted the permission.
//!
//! The adapter itself is thin: every admission decision downstream of the guest
//! boundary lives in [`PluginState::wake_agent`], which re-checks the call
//! frame, the frame budget, and the grant before it validates the request and
//! hands it to the runtime's wake backend.

use crate::component::PluginState;
use crate::component::bindings;
use crate::services::AgentWakeError;

/// Convert the host's closed wake failure into the generated guest enum.
///
/// The match is exhaustive, so adding a variant to either side stops this
/// compiling until the two are reconciled.
fn map_wake_error(
    error: AgentWakeError,
) -> bindings::tool::zeroclaw::plugin::dispatch::DispatchError {
    use bindings::tool::zeroclaw::plugin::dispatch::DispatchError;
    match error {
        AgentWakeError::AccessDenied => DispatchError::AccessDenied,
        AgentWakeError::UnknownAgent => DispatchError::UnknownAgent,
        AgentWakeError::RateLimited => DispatchError::RateLimited,
        AgentWakeError::InvalidRequest => DispatchError::InvalidRequest,
        AgentWakeError::Unavailable => DispatchError::Unavailable,
    }
}

impl bindings::tool::zeroclaw::plugin::dispatch::Host for PluginState {
    async fn wake(
        &mut self,
        target_agent: String,
        prompt: String,
    ) -> Result<(), bindings::tool::zeroclaw::plugin::dispatch::DispatchError> {
        self.wake_agent(target_agent, prompt)
            .await
            .map_err(map_wake_error)
    }
}
