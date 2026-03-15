//! Lightweight atomic-counter metrics registry for production observability.
//!
//! Provides a `MetricsRegistry` backed by `std::sync::atomic` counters
//! with Prometheus text format export. No heavy dependencies required.

use std::collections::BTreeMap;
use std::fmt::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::Instant;

/// A single atomic counter metric.
struct AtomicMetric {
    value: AtomicU64,
    help: &'static str,
    metric_type: MetricType,
}

#[derive(Clone, Copy)]
enum MetricType {
    Counter,
    Gauge,
}

impl MetricType {
    fn as_str(self) -> &'static str {
        match self {
            Self::Counter => "counter",
            Self::Gauge => "gauge",
        }
    }
}

/// Registry of atomic metrics for lightweight production observability.
///
/// All operations are lock-free using atomic operations.
/// Metrics are registered at init time; runtime only records values.
pub struct MetricsRegistry {
    metrics: BTreeMap<&'static str, AtomicMetric>,
    started_at: Instant,
    prefix: String,
}

impl MetricsRegistry {
    /// Create a new registry with the given prefix for metric names.
    fn new(prefix: &str) -> Self {
        let mut metrics = BTreeMap::new();

        let counters = [
            ("requests_total", "Total HTTP requests received"),
            ("tool_calls_total", "Total tool calls executed"),
            ("tool_errors_total", "Total tool call errors"),
            (
                "provider_requests_total",
                "Total provider (LLM) requests sent",
            ),
        ];
        for (name, help) in counters {
            metrics.insert(
                name,
                AtomicMetric {
                    value: AtomicU64::new(0),
                    help,
                    metric_type: MetricType::Counter,
                },
            );
        }

        let gauges = [
            (
                "request_duration_ms",
                "Last observed request duration in milliseconds",
            ),
            (
                "provider_latency_ms",
                "Last observed provider latency in milliseconds",
            ),
            ("active_conversations", "Number of active conversations"),
        ];
        for (name, help) in gauges {
            metrics.insert(
                name,
                AtomicMetric {
                    value: AtomicU64::new(0),
                    help,
                    metric_type: MetricType::Gauge,
                },
            );
        }

        Self {
            metrics,
            started_at: Instant::now(),
            prefix: prefix.to_string(),
        }
    }

    /// Increment a counter metric by 1.
    pub fn increment(&self, name: &str) {
        if let Some(metric) = self.metrics.get(name) {
            metric.value.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Record (set) a gauge metric to the given value.
    pub fn record(&self, name: &str, value: u64) {
        if let Some(metric) = self.metrics.get(name) {
            metric.value.store(value, Ordering::Relaxed);
        }
    }

    /// Get the current value of a metric.
    pub fn get(&self, name: &str) -> Option<u64> {
        self.metrics
            .get(name)
            .map(|m| m.value.load(Ordering::Relaxed))
    }

    /// Uptime in seconds since registry creation.
    pub fn uptime_secs(&self) -> u64 {
        self.started_at.elapsed().as_secs()
    }

    /// Export all metrics in Prometheus text exposition format.
    pub fn export_prometheus(&self) -> String {
        let mut buf = String::with_capacity(1024);

        // Uptime gauge
        let _ = writeln!(
            buf,
            "# HELP {prefix}_uptime_seconds Process uptime in seconds",
            prefix = self.prefix,
        );
        let _ = writeln!(
            buf,
            "# TYPE {prefix}_uptime_seconds gauge",
            prefix = self.prefix,
        );
        let _ = writeln!(
            buf,
            "{prefix}_uptime_seconds {value}",
            prefix = self.prefix,
            value = self.uptime_secs(),
        );

        for (name, metric) in &self.metrics {
            let full_name = format!("{}_{}", self.prefix, name);
            let value = metric.value.load(Ordering::Relaxed);
            let _ = writeln!(buf, "# HELP {full_name} {help}", help = metric.help);
            let _ = writeln!(
                buf,
                "# TYPE {full_name} {ty}",
                ty = metric.metric_type.as_str()
            );
            let _ = writeln!(buf, "{full_name} {value}");
        }

        buf
    }
}

// ── Global singleton ───────────────────────────────────────────────────

static GLOBAL_REGISTRY: OnceLock<MetricsRegistry> = OnceLock::new();

/// Initialize the global metrics registry with the given prefix.
/// Safe to call multiple times; only the first call takes effect.
pub fn init_global(prefix: &str) {
    GLOBAL_REGISTRY.get_or_init(|| MetricsRegistry::new(prefix));
}

/// Get a reference to the global metrics registry.
/// Initializes with default prefix "zeroclaw" if not yet initialized.
pub fn global() -> &'static MetricsRegistry {
    GLOBAL_REGISTRY.get_or_init(|| MetricsRegistry::new("zeroclaw"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_registry() -> MetricsRegistry {
        MetricsRegistry::new("test")
    }

    #[test]
    fn increment_counter() {
        let reg = test_registry();
        assert_eq!(reg.get("requests_total"), Some(0));
        reg.increment("requests_total");
        reg.increment("requests_total");
        assert_eq!(reg.get("requests_total"), Some(2));
    }

    #[test]
    fn record_gauge() {
        let reg = test_registry();
        reg.record("active_conversations", 5);
        assert_eq!(reg.get("active_conversations"), Some(5));
        reg.record("active_conversations", 3);
        assert_eq!(reg.get("active_conversations"), Some(3));
    }

    #[test]
    fn increment_unknown_metric_is_noop() {
        let reg = test_registry();
        reg.increment("nonexistent");
        assert_eq!(reg.get("nonexistent"), None);
    }

    #[test]
    fn record_unknown_metric_is_noop() {
        let reg = test_registry();
        reg.record("nonexistent", 42);
        assert_eq!(reg.get("nonexistent"), None);
    }

    #[test]
    fn uptime_is_non_negative() {
        let reg = test_registry();
        assert!(reg.uptime_secs() < 2);
    }

    #[test]
    fn export_prometheus_format() {
        let reg = test_registry();
        reg.increment("requests_total");
        reg.increment("tool_calls_total");
        reg.increment("tool_calls_total");
        reg.record("active_conversations", 7);

        let output = reg.export_prometheus();

        // Verify HELP/TYPE/value lines for counters
        assert!(output.contains("# HELP test_requests_total Total HTTP requests received"));
        assert!(output.contains("# TYPE test_requests_total counter"));
        assert!(output.contains("test_requests_total 1"));

        assert!(output.contains("# HELP test_tool_calls_total Total tool calls executed"));
        assert!(output.contains("# TYPE test_tool_calls_total counter"));
        assert!(output.contains("test_tool_calls_total 2"));

        // Verify gauge
        assert!(output.contains("# TYPE test_active_conversations gauge"));
        assert!(output.contains("test_active_conversations 7"));

        // Verify uptime
        assert!(output.contains("# HELP test_uptime_seconds Process uptime in seconds"));
        assert!(output.contains("# TYPE test_uptime_seconds gauge"));
        assert!(output.contains("test_uptime_seconds "));
    }

    #[test]
    fn export_prometheus_zero_counters() {
        let reg = test_registry();
        let output = reg.export_prometheus();

        assert!(output.contains("test_requests_total 0"));
        assert!(output.contains("test_tool_calls_total 0"));
        assert!(output.contains("test_tool_errors_total 0"));
        assert!(output.contains("test_provider_requests_total 0"));
    }

    #[test]
    fn all_registered_metrics_present() {
        let reg = test_registry();
        let expected = [
            "requests_total",
            "tool_calls_total",
            "tool_errors_total",
            "provider_requests_total",
            "request_duration_ms",
            "provider_latency_ms",
            "active_conversations",
        ];
        for name in expected {
            assert!(reg.get(name).is_some(), "metric {name} should exist");
        }
    }

    #[test]
    fn runtime_metric_increments_wired_correctly() {
        // Verify that the metrics used by runtime call sites
        // (gateway requests, tool calls, tool errors, provider requests)
        // are all registered and increment as expected.
        let reg = test_registry();

        // Simulate gateway request increment
        reg.increment("requests_total");
        assert_eq!(reg.get("requests_total"), Some(1));

        // Simulate tool call + error increments
        reg.increment("tool_calls_total");
        reg.increment("tool_calls_total");
        reg.increment("tool_errors_total");
        assert_eq!(reg.get("tool_calls_total"), Some(2));
        assert_eq!(reg.get("tool_errors_total"), Some(1));

        // Simulate provider request increment
        reg.increment("provider_requests_total");
        reg.increment("provider_requests_total");
        reg.increment("provider_requests_total");
        assert_eq!(reg.get("provider_requests_total"), Some(3));

        // Verify all appear in Prometheus export
        let output = reg.export_prometheus();
        assert!(output.contains("test_requests_total 1"));
        assert!(output.contains("test_tool_calls_total 2"));
        assert!(output.contains("test_tool_errors_total 1"));
        assert!(output.contains("test_provider_requests_total 3"));
    }
}
