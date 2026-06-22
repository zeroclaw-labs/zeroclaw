// Plugin call wrappers for tracing and timing.

use std::time::Instant;

use serde_json::json;
use zeroclaw_log::{Action, Event, info_span, record};

/// Async call wrapper
///
/// Enters a tracing span, emits start/complete trace records with timing,
/// then returns the result of `f.await`.
pub async fn wrap_plugin_call<F, T>(
    plugin_name: &str,
    plugin_version: &str,
    op_name: &str,
    f: F,
) -> T
where
    F: std::future::Future<Output = T>,
{
    // Enter a span for the entire plugin call if the log level is Info, Debug or Trace. This
    // will attach the plugin_name and plugin_version fields to all logs emitted by the plugin
    // during this call.
    let span = info_span!(
        "plugin_call",
        plugin_name = %plugin_name,
        plugin_version = %plugin_version,
    );
    let _guard = span.enter();

    // When tracing, also record the start and end of the call along with its duration.
    record!(
        TRACE,
        Event::new(module_path!(), Action::Invoke)
            .with_attrs(json!({ "plugin": plugin_name, "op": op_name })),
        "plugin call start",
    );
    let start = Instant::now();
    let result = f.await;
    let duration_ms = start.elapsed().as_millis() as u64;
    record!(
        TRACE,
        Event::new(module_path!(), Action::Complete)
            .with_duration(duration_ms)
            .with_attrs(json!({ "plugin": plugin_name, "op": op_name })),
        "plugin call complete",
    );
    result
}
