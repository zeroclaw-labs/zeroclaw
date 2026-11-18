//! Optional export bridge for the canonical [`crate::LogEvent`] stream.
//!
//! The local JSONL writer and live broadcast remain owned by this crate. A
//! runtime feature (currently the OTLP backend) can install one nonblocking
//! sink without adding exporter dependencies to `zeroclaw-log` or changing the
//! persisted schema consumed by the gateway, CLI, and TUI.

use std::sync::{Arc, OnceLock};

use parking_lot::RwLock;

use crate::LogEvent;

/// Nonblocking exporter invoked for every canonical event before persistence.
/// Implementations own their queueing and must keep [`Self::emit`] off network
/// and file I/O. [`Self::force_flush`] is called when the binding is replaced
/// or cleared so buffered telemetry is not abandoned on configuration reload.
pub trait LogRecordExporter: Send + Sync + 'static {
    fn emit(&self, event: &LogEvent);

    fn force_flush(&self) -> Result<(), String> {
        Ok(())
    }
}

static EXPORTER: OnceLock<RwLock<Option<Arc<dyn LogRecordExporter>>>> = OnceLock::new();

fn slot() -> &'static RwLock<Option<Arc<dyn LogRecordExporter>>> {
    EXPORTER.get_or_init(|| RwLock::new(None))
}

/// Install or replace the process-wide structured-log exporter.
pub fn set_log_exporter(exporter: Arc<dyn LogRecordExporter>) {
    let previous = slot().write().replace(exporter);
    if let Some(previous) = previous {
        report_flush(previous.force_flush(), "replacement");
    }
}

/// Remove and flush the structured-log exporter.
pub fn clear_log_exporter() {
    let previous = slot().write().take();
    if let Some(previous) = previous {
        report_flush(previous.force_flush(), "removal");
    }
}

/// Flush the active exporter without changing the binding.
pub fn flush_log_exporter() {
    let exporter = slot().read().clone();
    if let Some(exporter) = exporter {
        report_flush(exporter.force_flush(), "explicit flush");
    }
}

fn report_flush(result: Result<(), String>, operation: &'static str) {
    if let Err(error) = result {
        tracing::warn!(
            target: "zeroclaw_log_internal",
            error = %error,
            operation,
            "log: structured exporter flush failed"
        );
    }
}

/// Forward without holding the bridge lock while an exporter queues the event.
pub(crate) fn forward(event: &LogEvent) {
    let exporter = slot().read().clone();
    if let Some(exporter) = exporter {
        exporter.emit(event);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EventCategory, LogEvent, Severity};
    use parking_lot::Mutex;

    struct CapturingExporter {
        ids: Arc<Mutex<Vec<String>>>,
        flushes: Arc<Mutex<usize>>,
    }

    impl LogRecordExporter for CapturingExporter {
        fn emit(&self, event: &LogEvent) {
            self.ids.lock().push(event.id.clone());
        }

        fn force_flush(&self) -> Result<(), String> {
            *self.flushes.lock() += 1;
            Ok(())
        }
    }

    #[test]
    fn installed_exporter_receives_event_and_flushes_when_cleared() {
        let _writer_guard = crate::writer::WRITER_TEST_LOCK.lock();
        let captured = Arc::new(Mutex::new(Vec::<String>::new()));
        let flushes = Arc::new(Mutex::new(0));
        set_log_exporter(Arc::new(CapturingExporter {
            ids: Arc::clone(&captured),
            flushes: Arc::clone(&flushes),
        }));

        let event = LogEvent::new(Severity::Info, "complete", EventCategory::System);
        crate::writer::record_event(event.clone());
        assert_eq!(captured.lock().as_slice(), [event.id.as_str()]);

        clear_log_exporter();
        crate::writer::record_event(event.clone());
        assert_eq!(captured.lock().len(), 1);
        assert_eq!(*flushes.lock(), 1);
    }
}
