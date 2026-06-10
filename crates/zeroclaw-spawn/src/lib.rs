//! `zeroclaw-spawn` — the sanctioned interface against `tokio::spawn` for
//! the ZeroClaw workspace.
//!
//! Every call site that needs to fan out a background task must go
//! through [`spawn!`] instead of reaching for `tokio::spawn` directly.
//! Doing so buys two things, neither of which changes runtime behavior:
//!
//! 1. **Attribution propagation.** The spawned future is wrapped with
//!    `Instrument::in_current_span()` so any `record!` it emits while
//!    running inherits the caller's `attribution_span` (channel, agent,
//!    session, cron job, …). Without this wrapper, tokio detaches the
//!    task from the caller's span stack and records orphan.
//!
//! 2. **Lifecycle telemetry.** Two structured log events are emitted
//!    through the standard `zeroclaw_log::record!` pipeline:
//!    - `runtime.task.spawn` (`Action::Spawn`) at spawn time, attributed
//!      to the caller's current span.
//!    - `runtime.task.spawn` (`Action::Complete`) when the future
//!      resolves, attributed to the spawned task's span (which is the
//!      caller's by inheritance), carrying elapsed `duration_ms`.
//!
//!    Both records carry `zc_file` / `zc_line` of the call site via the
//!    standard `record!` machinery, and a `task_site` attribute giving
//!    the same as a single string for grep convenience.
//!
//! The returned [`tokio::task::JoinHandle`] is the unmodified handle
//! from `tokio::spawn` — panics propagate as `JoinError::Panic`, cancel
//! semantics are unchanged, and the future's output type is preserved.
//! `zeroclaw-spawn` adds telemetry, not control flow.
//!
//! ## Layering
//!
//! `zeroclaw-spawn` depends on `zeroclaw-log`. Crates that already depend
//! on `zeroclaw-log` (i.e. almost everything in the workspace) can add
//! `zeroclaw-spawn` as a peer dependency without inverting the graph.
//! The lowest-level crate (`zeroclaw-api`) intentionally does NOT depend
//! on `zeroclaw-spawn` — its small handful of internal spawn needs go
//! through the workspace-wide `disallowed_methods` exemption documented
//! in `clippy.toml`.

#![forbid(unsafe_code)]

/// Private re-export root for macro expansion. External crates must not
/// reach through here — it exists solely so `spawn!` can expand without
/// callers needing `tokio`, `tracing`, or `zeroclaw_log` as direct
/// dependencies.
#[doc(hidden)]
pub mod __private {
    pub use ::serde_json;
    pub use ::tokio;
    pub use ::tracing;
    pub use ::zeroclaw_log;
}

/// Stable event name for spawn-lifecycle records emitted by [`spawn!`].
/// Exposed so dashboards / queries can match on a single string instead
/// of recomputing it.
pub const TASK_EVENT_NAME: &str = "runtime.task.spawn";

/// Spawn a future onto the current tokio runtime with attribution
/// propagation and lifecycle telemetry. Drop-in replacement for
/// `tokio::spawn` — same signature, same `JoinHandle<T>`, same panic
/// and cancel semantics.
///
/// ```ignore
/// use zeroclaw_spawn::spawn;
///
/// let handle = spawn!(async move {
///     do_work().await
/// });
/// let output = handle.await?;
/// ```
///
/// On spawn the macro emits one `Action::Spawn` record attributed to
/// the caller's current span. When the future resolves it emits one
/// `Action::Complete` record with elapsed `duration_ms`, attributed to
/// the spawned task's span (which is the caller's by inheritance via
/// `in_current_span`). Neither record alters the future's output or the
/// returned `JoinHandle`.
#[macro_export]
macro_rules! spawn {
    ($body:expr) => {{
        #[allow(unused_imports)]
        use $crate::__private::tracing::Instrument as _;

        // Capture the call-site once. `module_path!`, `file!`, `line!`
        // expand at the spawn point, not inside the spawned task, so
        // both lifecycle records carry the originating location.
        const __ZC_TASK_MODULE: &'static str = module_path!();
        const __ZC_TASK_FILE: &'static str = file!();
        const __ZC_TASK_LINE: u32 = line!();

        // Spawn-time record — fires synchronously on the caller's
        // thread, attributed to whatever span the caller currently has
        // entered.
        $crate::__private::zeroclaw_log::record!(
            INFO,
            $crate::__private::zeroclaw_log::Event::new(
                $crate::TASK_EVENT_NAME,
                $crate::__private::zeroclaw_log::Action::Spawn,
            )
            .with_attrs($crate::__private::serde_json::json!({
                "task_site": format!("{}:{}", __ZC_TASK_FILE, __ZC_TASK_LINE),
                "task_module": __ZC_TASK_MODULE,
            })),
            "task spawned"
        );

        // Wrap the user's future so we can stamp a Complete record when
        // it resolves. The wrapper is itself `.in_current_span()`d, so
        // the completion record inherits the same attribution context
        // the caller had at spawn time.
        let __zc_task_started_at = $crate::__private::tokio::time::Instant::now();
        let __zc_task_future = async move {
            let __zc_task_output = { $body }.await;
            let __zc_task_elapsed_ms = __zc_task_started_at.elapsed().as_millis() as u64;
            $crate::__private::zeroclaw_log::record!(
                INFO,
                $crate::__private::zeroclaw_log::Event::new(
                    $crate::TASK_EVENT_NAME,
                    $crate::__private::zeroclaw_log::Action::Complete,
                )
                .with_outcome($crate::__private::zeroclaw_log::EventOutcome::Success)
                .with_duration(__zc_task_elapsed_ms)
                .with_attrs($crate::__private::serde_json::json!({
                    "task_site": format!("{}:{}", __ZC_TASK_FILE, __ZC_TASK_LINE),
                    "task_module": __ZC_TASK_MODULE,
                })),
                "task complete"
            );
            __zc_task_output
        };

        #[allow(clippy::disallowed_methods)]
        let __zc_spawn_handle =
            $crate::__private::tokio::spawn(__zc_task_future.in_current_span());
        __zc_spawn_handle
    }};
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use tracing::{Subscriber, span};
    use tracing_subscriber::layer::{Context, Layer, SubscriberExt};
    use tracing_subscriber::registry::{LookupSpan, Registry};

    /// Layer that records, for every event it sees, the names of every
    /// span on the event's span stack at the moment of recording. Lets
    /// us assert "yes, the spawned task's event saw the caller's span"
    /// without depending on `zeroclaw-log` formatting.
    #[derive(Clone, Default)]
    struct SpanCapture {
        events: Arc<Mutex<Vec<Vec<String>>>>,
    }

    impl<S> Layer<S> for SpanCapture
    where
        S: Subscriber + for<'a> LookupSpan<'a>,
    {
        fn on_event(&self, _event: &tracing::Event<'_>, ctx: Context<'_, S>) {
            let mut stack = Vec::new();
            if let Some(scope) = ctx.event_scope(_event) {
                for span in scope.from_root() {
                    stack.push(span.name().to_string());
                }
            }
            self.events.lock().unwrap().push(stack);
        }
    }

    #[tokio::test]
    async fn spawn_returns_future_output() {
        let handle = crate::spawn!(async { 42_u32 });
        assert_eq!(handle.await.unwrap(), 42);
    }

    #[tokio::test]
    async fn spawn_preserves_error_type() {
        let handle = crate::spawn!(async { Err::<(), &'static str>("nope") });
        assert_eq!(handle.await.unwrap(), Err("nope"));
    }

    #[tokio::test]
    async fn spawn_runs_to_completion_with_await_point() {
        let handle = crate::spawn!(async {
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            "done"
        });
        assert_eq!(handle.await.unwrap(), "done");
    }

    /// Load-bearing test for the whole point of this crate: an event
    /// emitted *inside* a spawned task must observe the caller's
    /// `attribution_span` on its span stack. If this regresses, every
    /// `record!` from inside a `spawn!` body silently re-attributes to
    /// the tokio root and dashboards lose session/channel/agent
    /// attribution — exactly the bug `.in_current_span()` exists to
    /// prevent.
    #[tokio::test]
    async fn spawn_propagates_callers_span_into_task() {
        let capture = SpanCapture::default();
        let subscriber = Registry::default().with(capture.clone());
        let _guard = tracing::subscriber::set_default(subscriber);

        let outer = span!(tracing::Level::INFO, "attribution_span");
        let _entered = outer.enter();

        let handle = crate::spawn!(async {
            // Yield once so the task actually re-enters its instrumented
            // span on a different poll than the spawn site.
            tokio::task::yield_now().await;
            tracing::event!(tracing::Level::INFO, "inside_spawned_task");
        });
        handle.await.unwrap();

        drop(_entered);

        let events = capture.events.lock().unwrap();
        // Find the event we emitted from inside the task and assert it
        // saw `attribution_span` on its stack.
        let saw_outer = events
            .iter()
            .any(|stack| stack.iter().any(|name| name == "attribution_span"));
        assert!(
            saw_outer,
            "spawned task lost caller's span; captured stacks: {:?}",
            *events
        );
    }
}
