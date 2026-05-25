pub mod dora;
pub mod jsonl;
pub mod log;
pub mod multi;
pub mod noop;
#[cfg(feature = "observability-otel")]
pub mod otel;
#[cfg(feature = "observability-prometheus")]
pub mod prometheus;
pub mod runtime_trace;
pub mod traits;
pub mod verbose;

#[allow(unused_imports)]
pub use self::log::LogObserver;
pub use self::jsonl::JsonlObserver;
#[allow(unused_imports)]
pub use self::multi::MultiObserver;
pub use noop::NoopObserver;
#[cfg(feature = "observability-otel")]
pub use otel::OtelObserver;
#[cfg(feature = "observability-prometheus")]
pub use prometheus::PrometheusObserver;
pub use traits::{Observer, ObserverEvent};
#[allow(unused_imports)]
pub use verbose::VerboseObserver;

use daemonclaw_config::schema::ObservabilityConfig;
use std::path::Path;

/// Factory: create the right observer from config.
///
/// When `jsonl_log_path` is set, the JSONL observer is composed alongside
/// the primary backend via MultiObserver — all events flow to both.
pub fn create_observer(config: &ObservabilityConfig) -> Box<dyn Observer> {
    let primary: Box<dyn Observer> = create_primary_observer(config);

    let jsonl_path = config.jsonl_log_path.as_deref().unwrap_or_default().trim();
    if jsonl_path.is_empty() {
        return primary;
    }

    let workspace_dir = Path::new(".");
    let resolved = jsonl::resolve_jsonl_path(jsonl_path, workspace_dir);
    let mode = match config.jsonl_log_mode.as_deref().unwrap_or("rolling") {
        "full" => jsonl::JsonlStorageMode::Full,
        _ => jsonl::JsonlStorageMode::Rolling,
    };
    let max = config.jsonl_log_max_entries.unwrap_or(2000).max(1);

    let jsonl_obs = JsonlObserver::new(resolved, mode, max);
    tracing::info!(path = %jsonl_path, mode = ?mode, max_entries = max, "JSONL observer initialized");

    Box::new(MultiObserver::new(vec![primary, Box::new(jsonl_obs)]))
}

/// Factory: create the right observer from config (workspace-aware variant).
///
/// Use this when workspace_dir is known at the call site — resolves relative
/// JSONL paths correctly.
pub fn create_observer_with_workspace(config: &ObservabilityConfig, workspace_dir: &Path) -> Box<dyn Observer> {
    let primary: Box<dyn Observer> = create_primary_observer(config);

    let jsonl_path = config.jsonl_log_path.as_deref().unwrap_or_default().trim();
    if jsonl_path.is_empty() {
        return primary;
    }

    let resolved = jsonl::resolve_jsonl_path(jsonl_path, workspace_dir);
    let mode = match config.jsonl_log_mode.as_deref().unwrap_or("rolling") {
        "full" => jsonl::JsonlStorageMode::Full,
        _ => jsonl::JsonlStorageMode::Rolling,
    };
    let max = config.jsonl_log_max_entries.unwrap_or(2000).max(1);

    let jsonl_obs = JsonlObserver::new(resolved, mode, max);
    tracing::info!(path = %jsonl_path, "JSONL observer initialized");

    Box::new(MultiObserver::new(vec![primary, Box::new(jsonl_obs)]))
}

fn create_primary_observer(config: &ObservabilityConfig) -> Box<dyn Observer> {
    match config.backend.as_str() {
        "log" => Box::new(LogObserver::new()),
        "verbose" => Box::new(VerboseObserver::new()),
        "prometheus" => {
            #[cfg(feature = "observability-prometheus")]
            {
                Box::new(PrometheusObserver::new())
            }
            #[cfg(not(feature = "observability-prometheus"))]
            {
                tracing::warn!(
                    "Prometheus backend requested but this build was compiled without `observability-prometheus`; falling back to noop."
                );
                Box::new(NoopObserver)
            }
        }
        "otel" | "opentelemetry" | "otlp" => {
            #[cfg(feature = "observability-otel")]
            match OtelObserver::new(
                config.otel_endpoint.as_deref(),
                config.otel_service_name.as_deref(),
                config.otel_headers.clone(),
            ) {
                Ok(obs) => {
                    tracing::info!(
                        endpoint = config
                            .otel_endpoint
                            .as_deref()
                            .unwrap_or("http://localhost:4318"),
                        "OpenTelemetry observer initialized"
                    );
                    Box::new(obs)
                }
                Err(e) => {
                    tracing::error!("Failed to create OTel observer: {e}. Falling back to noop.");
                    Box::new(NoopObserver)
                }
            }
            #[cfg(not(feature = "observability-otel"))]
            {
                tracing::warn!(
                    "OpenTelemetry backend requested but this build was compiled without `observability-otel`; falling back to noop."
                );
                Box::new(NoopObserver)
            }
        }
        "none" | "noop" => Box::new(NoopObserver),
        _ => {
            tracing::warn!(
                "Unknown observability backend '{}', falling back to noop",
                config.backend
            );
            Box::new(NoopObserver)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn factory_none_returns_noop() {
        let cfg = ObservabilityConfig {
            backend: "none".into(),
            ..ObservabilityConfig::default()
        };
        assert_eq!(create_observer(&cfg).name(), "noop");
    }

    #[test]
    fn factory_noop_returns_noop() {
        let cfg = ObservabilityConfig {
            backend: "noop".into(),
            ..ObservabilityConfig::default()
        };
        assert_eq!(create_observer(&cfg).name(), "noop");
    }

    #[test]
    fn factory_log_returns_log() {
        let cfg = ObservabilityConfig {
            backend: "log".into(),
            ..ObservabilityConfig::default()
        };
        assert_eq!(create_observer(&cfg).name(), "log");
    }

    #[test]
    fn factory_verbose_returns_verbose() {
        let cfg = ObservabilityConfig {
            backend: "verbose".into(),
            ..ObservabilityConfig::default()
        };
        assert_eq!(create_observer(&cfg).name(), "verbose");
    }

    #[test]
    fn factory_prometheus_returns_prometheus() {
        let cfg = ObservabilityConfig {
            backend: "prometheus".into(),
            ..ObservabilityConfig::default()
        };
        let expected = if cfg!(feature = "observability-prometheus") {
            "prometheus"
        } else {
            "noop"
        };
        assert_eq!(create_observer(&cfg).name(), expected);
    }

    #[test]
    fn factory_otel_returns_otel() {
        let cfg = ObservabilityConfig {
            backend: "otel".into(),
            otel_endpoint: Some("http://127.0.0.1:19999".into()),
            otel_service_name: Some("test".into()),
            ..ObservabilityConfig::default()
        };
        let expected = if cfg!(feature = "observability-otel") {
            "otel"
        } else {
            "noop"
        };
        assert_eq!(create_observer(&cfg).name(), expected);
    }

    #[test]
    fn factory_opentelemetry_alias() {
        let cfg = ObservabilityConfig {
            backend: "opentelemetry".into(),
            otel_endpoint: Some("http://127.0.0.1:19999".into()),
            otel_service_name: Some("test".into()),
            ..ObservabilityConfig::default()
        };
        let expected = if cfg!(feature = "observability-otel") {
            "otel"
        } else {
            "noop"
        };
        assert_eq!(create_observer(&cfg).name(), expected);
    }

    #[test]
    fn factory_otlp_alias() {
        let cfg = ObservabilityConfig {
            backend: "otlp".into(),
            otel_endpoint: Some("http://127.0.0.1:19999".into()),
            otel_service_name: Some("test".into()),
            ..ObservabilityConfig::default()
        };
        let expected = if cfg!(feature = "observability-otel") {
            "otel"
        } else {
            "noop"
        };
        assert_eq!(create_observer(&cfg).name(), expected);
    }

    #[test]
    fn factory_unknown_falls_back_to_noop() {
        let cfg = ObservabilityConfig {
            backend: "xyzzy_unknown".into(),
            ..ObservabilityConfig::default()
        };
        assert_eq!(create_observer(&cfg).name(), "noop");
    }

    #[test]
    fn factory_empty_string_falls_back_to_noop() {
        let cfg = ObservabilityConfig {
            backend: String::new(),
            ..ObservabilityConfig::default()
        };
        assert_eq!(create_observer(&cfg).name(), "noop");
    }

    #[test]
    fn factory_garbage_falls_back_to_noop() {
        let cfg = ObservabilityConfig {
            backend: "xyzzy_garbage_123".into(),
            ..ObservabilityConfig::default()
        };
        assert_eq!(create_observer(&cfg).name(), "noop");
    }
}
