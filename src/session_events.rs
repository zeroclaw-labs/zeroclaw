//! Application composition and the narrow bridge to the existing turn engine.
//! Kernel never depends on runtime or constructs concrete capabilities.
use std::path::Path;
use std::sync::Arc;
use zeroclaw_api::session_events::{AppendError, SessionEventStore};
use zeroclaw_infra::session_events::open_event_store;
use zeroclaw_kernel::session::SessionCoordinator;

#[cfg(feature = "agent-runtime")]
mod cli;
#[cfg(feature = "agent-runtime")]
pub use zeroclaw_session_host::NativeSessionHub;
#[cfg(all(test, feature = "agent-runtime"))]
mod tests;
#[cfg(feature = "agent-runtime")]
pub use cli::{new_cli_session, open_cli_session};

pub struct NativeSessionRuntime {
    coordinator: Arc<SessionCoordinator>,
}
impl NativeSessionRuntime {
    /// Application admission resolves the current installation format. A
    /// preexisting handle cannot keep writing after its generation is retired.
    pub fn open_family(data_dir: &Path, family: &str) -> Result<Self, AppendError> {
        let store: Arc<dyn SessionEventStore> = Arc::new(
            zeroclaw_infra::session_events::family::FamilyEventStore::open(data_dir, family)?,
        );
        Ok(Self {
            coordinator: Arc::new(SessionCoordinator::new(store).with_search(Arc::new(
                zeroclaw_infra::session_events::search::FullTextSearch,
            ))),
        })
    }
    pub fn open(data_dir: &Path, backend: &str) -> Result<Self, AppendError> {
        let store: Arc<dyn SessionEventStore> = open_event_store(data_dir, backend)?;
        Ok(Self {
            coordinator: Arc::new(SessionCoordinator::new(store).with_search(Arc::new(
                zeroclaw_infra::session_events::search::FullTextSearch,
            ))),
        })
    }
    #[must_use]
    pub fn coordinator(&self) -> Arc<SessionCoordinator> {
        Arc::clone(&self.coordinator)
    }
}

/// This bridge borrows the already-composed engine. It does not create a
/// second loop, config authority, provider factory or persistent transcript.
#[cfg(feature = "agent-runtime")]
pub struct EmbeddedTurnExecutor<'a> {
    agent: &'a mut zeroclaw_runtime::agent::Agent,
}
#[cfg(feature = "agent-runtime")]
impl<'a> EmbeddedTurnExecutor<'a> {
    pub fn new(agent: &'a mut zeroclaw_runtime::agent::Agent) -> Self {
        Self { agent }
    }
}
#[cfg(feature = "agent-runtime")]
#[async_trait::async_trait]
impl zeroclaw_kernel::session::execution::TurnExecutorPort for EmbeddedTurnExecutor<'_> {
    fn diagnostic(&self, error: &anyhow::Error) -> Option<String> {
        Some(zeroclaw_runtime::agent::Agent::session_execution_diagnostic(error))
    }
    async fn execute(
        &mut self,
        text: &str,
        admission: Arc<dyn zeroclaw_api::session_events::capture::ProviderAdmissionPort>,
    ) -> anyhow::Result<String> {
        zeroclaw_api::session_events::capture::scope_provider_capture(
            admission,
            self.agent.turn(text),
        )
        .await
    }
}
