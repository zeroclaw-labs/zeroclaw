//! Collects the host's egress refusal records so a test can assert on them.
//!
//! The collector is an exporter on the canonical log stream, installed under
//! the test line sink, which carries the capture layer but not the `log`
//! bridge: the `log` crate's traces (Cranelift compiles fixtures here) never
//! reach it. Records are shared by every test in the binary, so a test filters
//! them by something only its own attempt carries, such as its binding.

use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use zeroclaw_log::{LogEvent, LogRecordExporter};

#[derive(Default)]
struct Collected(Mutex<Vec<serde_json::Value>>);

impl LogRecordExporter for Collected {
    fn emit(&self, event: &LogEvent) {
        let error_key = event
            .attributes
            .get("error_key")
            .and_then(serde_json::Value::as_str);
        if matches!(
            error_key,
            Some("plugin_egress_denied" | "plugin_egress_connection_limit")
        ) {
            self.0
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(event.attributes.clone());
        }
    }
}

fn collected() -> &'static Arc<Collected> {
    static COLLECTED: OnceLock<Arc<Collected>> = OnceLock::new();
    COLLECTED.get_or_init(|| {
        let collected = Arc::new(Collected::default());
        zeroclaw_log::try_install_line_sink_for_tests(|_| {});
        zeroclaw_log::set_log_exporter(collected.clone());
        collected
    })
}

/// Start collecting. Call before the attempt whose record a test expects.
pub fn start() {
    collected();
}

/// Every collected refusal record for which `predicate` holds.
pub fn matching(predicate: impl Fn(&serde_json::Value) -> bool) -> Vec<serde_json::Value> {
    collected()
        .0
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .filter(|record| predicate(record))
        .cloned()
        .collect()
}
