//! `zeroclaw-spawn` — the sanctioned interface against `tokio::spawn` for
//! the ZeroClaw workspace.

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
