//! Shared `spawn_blocking` wrappers around `SessionBackend` operations
//! for the gateway hot paths (WebSocket handler + HTTP `/api/sessions`
//! endpoints). The lifetime is narrow: any sync `Box<dyn
//! SessionBackend>` call that lives inside an `async fn` in this crate
//! must route through these helpers, not call the backend inline.
//!
//! Why this exists: PR 1 of the multi-database session-backend series.
//! A remote backend (Postgres/MySQL/MariaDB/Oracle/Db2 — each coming
//! in its own follow-up PR) can make a query hang on a network round
//! trip. If that query runs inline in an async task, the worker is
//! blocked until the round trip completes — which limits the gateway
//! to one slow concurrent request per worker, and on the small
//! `current_thread` test executors can stall the runtime entirely.
//! Routing through `tokio::task::spawn_blocking` keeps the async
//! pool running while the sync backend call executes on a
//! dedicated blocking thread.
//!
//! Today's local drivers (sqlite / jsonl) complete in microseconds
//! and pay only the spawn/join overhead, so this wrapper is invisible
//! for them — but the foundation is in place for every remote
//! backend that follows.

use std::io;
use std::sync::Arc;

use zeroclaw_infra::session_backend::SessionBackend;

/// Run a synchronous `SessionBackend` operation through
/// `tokio::task::spawn_blocking`. Use this from any `async fn` that
/// previously called a backend method directly. Join errors from the
/// blocking pool are converted into `io::Error::Other` so the call
/// site can keep using `std::io::Result<T>` matching the
/// `SessionBackend` trait signature.
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

/// Convenience: load session messages off the blocking pool.
pub async fn load(
    backend: Arc<dyn SessionBackend>,
    session_key: String,
) -> io::Result<Vec<zeroclaw_api::model_provider::ChatMessage>> {
    spawn_blocking_session_op(move || backend.load(&session_key)).await
}

/// Convenience: load session messages with their persisted timestamps
/// off the blocking pool.
pub async fn load_with_timestamps(
    backend: Arc<dyn SessionBackend>,
    session_key: String,
) -> io::Result<Vec<zeroclaw_infra::session_backend::TimestampedMessage>> {
    spawn_blocking_session_op(move || backend.load_with_timestamps(&session_key)).await
}

/// Convenience: append a single message off the blocking pool.
pub async fn append(
    backend: Arc<dyn SessionBackend>,
    session_key: String,
    message: zeroclaw_api::model_provider::ChatMessage,
) -> io::Result<()> {
    spawn_blocking_session_op(move || backend.append(&session_key, &message)).await
}

/// Convenience: delete-session off the blocking pool.
pub async fn delete_session(
    backend: Arc<dyn SessionBackend>,
    session_key: String,
) -> io::Result<bool> {
    spawn_blocking_session_op(move || backend.delete_session(&session_key)).await
}

/// Convenience: list-sessions-with-metadata off the blocking pool.
pub async fn list_sessions_with_metadata(
    backend: Arc<dyn SessionBackend>,
) -> io::Result<Vec<zeroclaw_infra::session_backend::SessionMetadata>> {
    spawn_blocking_session_op(move || backend.list_sessions_with_metadata()).await
}

/// Convenience: set session state off the blocking pool.
pub async fn set_session_state(
    backend: Arc<dyn SessionBackend>,
    session_key: String,
    state: String,
    turn_id: Option<String>,
) -> io::Result<()> {
    spawn_blocking_session_op(move || {
        backend.set_session_state(&session_key, &state, turn_id.as_deref())
    })
    .await
}

/// Convenience: get session name off the blocking pool.
pub async fn get_session_name(
    backend: Arc<dyn SessionBackend>,
    session_key: String,
) -> io::Result<Option<String>> {
    spawn_blocking_session_op(move || backend.get_session_name(&session_key)).await
}

/// Convenience: set session name off the blocking pool.
pub async fn set_session_name(
    backend: Arc<dyn SessionBackend>,
    session_key: String,
    name: String,
) -> io::Result<()> {
    spawn_blocking_session_op(move || backend.set_session_name(&session_key, &name)).await
}

/// Convenience: set session agent alias off the blocking pool.
pub async fn set_session_agent_alias(
    backend: Arc<dyn SessionBackend>,
    session_key: String,
    alias: String,
) -> io::Result<()> {
    spawn_blocking_session_op(move || backend.set_session_agent_alias(&session_key, &alias)).await
}

/// Convenience: check whether a session exists off the blocking pool.
pub async fn session_exists(
    backend: Arc<dyn SessionBackend>,
    session_key: String,
) -> io::Result<bool> {
    spawn_blocking_session_op(move || backend.session_exists(&session_key)).await
}

/// Async equivalent of `ws::persist_conversation_messages`. Runs the
/// session-existence guard, role filter, and per-message append loop
/// on the blocking pool so the WS handler does not stall on a slow
/// network round trip when the operator wires up a remote session
/// backend in a follow-up PR. The semantics match the original
/// helper exactly — see that function for the existence-guard and
/// role-filter rules.
pub async fn persist_conversation_messages(
    backend: Arc<dyn SessionBackend>,
    session_key: String,
    messages: Vec<zeroclaw_providers::ConversationMessage>,
) {
    let join_result = tokio::task::spawn_blocking(move || {
        // Propagate session_exists errors instead of swallowing them as false.
        // A backend error here means we cannot safely persist, so we bail out
        // and log the failure.
        let exists = match backend.session_exists(&session_key) {
            Ok(exists) => exists,
            Err(e) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"session_key": &session_key, "error": format!("{e}")})),
                    "session_exists check failed; skipping persist"
                );
                return;
            }
        };
        if !exists {
            return;
        }
        for message in messages {
            let zeroclaw_providers::ConversationMessage::Chat(message) = message else {
                continue;
            };
            if message.role == "system" {
                continue;
            }
            let _ = backend.append(&session_key, &message);
        }
    })
    .await;
    if join_result.is_err() {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
            "session persist worker panicked"
        );
    }
}
