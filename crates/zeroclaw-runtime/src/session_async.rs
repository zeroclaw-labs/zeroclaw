//! Shared `spawn_blocking` wrappers around `SessionBackend` operations
//! used by the runtime RPC dispatcher hot paths. This is the runtime
//! counterpart to `zeroclaw_gateway::session_async` — same contract,
//! same rationale (a slow remote database backend cannot stall the
//! async workers), kept as a separate module so each consumer crate
//! does not have to depend on the gateway.

use std::io;
use std::sync::Arc;

use zeroclaw_infra::session_backend::SessionBackend;

/// Run a synchronous `SessionBackend` operation through
/// `tokio::task::spawn_blocking`. Join errors from the blocking
/// pool are converted into `io::Error::Other`.
pub async fn spawn_blocking_session_op<F, T>(op: F) -> io::Result<T>
where
    F: FnOnce() -> io::Result<T> + Send + 'static,
    T: Send + 'static,
{
    match tokio::task::spawn_blocking(op).await {
        Ok(result) => result,
        Err(join_err) => Err(io::Error::other(format!(
            "session backend worker panicked: {join_err}"
        ))),
    }
}

/// Convenience: count_agent_attribution off the blocking pool.
pub async fn count_agent_attribution(
    backend: Arc<dyn SessionBackend>,
    agent_alias: String,
) -> io::Result<usize> {
    spawn_blocking_session_op(move || backend.count_agent_attribution(&agent_alias)).await
}

/// Convenience: set_session_agent_alias off the blocking pool.
pub async fn set_session_agent_alias(
    backend: Arc<dyn SessionBackend>,
    session_key: String,
    alias: String,
) -> io::Result<()> {
    spawn_blocking_session_op(move || backend.set_session_agent_alias(&session_key, &alias)).await
}

/// Convenience: load off the blocking pool.
pub async fn load(
    backend: Arc<dyn SessionBackend>,
    session_key: String,
) -> io::Result<Vec<zeroclaw_api::model_provider::ChatMessage>> {
    spawn_blocking_session_op(move || Ok(backend.load(&session_key))).await
}

/// Convenience: append off the blocking pool.
pub async fn append(
    backend: Arc<dyn SessionBackend>,
    session_key: String,
    message: zeroclaw_api::model_provider::ChatMessage,
) -> io::Result<()> {
    spawn_blocking_session_op(move || backend.append(&session_key, &message)).await
}

/// Convenience: list_sessions_with_metadata off the blocking pool.
pub async fn list_sessions_with_metadata(
    backend: Arc<dyn SessionBackend>,
) -> io::Result<Vec<zeroclaw_infra::session_backend::SessionMetadata>> {
    spawn_blocking_session_op(move || Ok(backend.list_sessions_with_metadata())).await
}

/// Convenience: delete_session off the blocking pool.
pub async fn delete_session(
    backend: Arc<dyn SessionBackend>,
    session_key: String,
) -> io::Result<bool> {
    spawn_blocking_session_op(move || backend.delete_session(&session_key)).await
}

/// Convenience: rename_agent_attribution off the blocking pool.
pub async fn rename_agent_attribution(
    backend: Arc<dyn SessionBackend>,
    from: String,
    to: String,
) -> io::Result<usize> {
    spawn_blocking_session_op(move || backend.rename_agent_attribution(&from, &to)).await
}

/// Convenience: search off the blocking pool.
pub async fn search(
    backend: Arc<dyn SessionBackend>,
    query: zeroclaw_infra::session_backend::SessionQuery,
) -> io::Result<Vec<zeroclaw_infra::session_backend::SessionMetadata>> {
    spawn_blocking_session_op(move || Ok(backend.search(&query))).await
}

/// Convenience: get_session_state off the blocking pool.
pub async fn get_session_state(
    backend: Arc<dyn SessionBackend>,
    session_key: String,
) -> io::Result<Option<zeroclaw_infra::session_backend::SessionState>> {
    spawn_blocking_session_op(move || backend.get_session_state(&session_key)).await
}
