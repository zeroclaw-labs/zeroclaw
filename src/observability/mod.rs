pub mod log;
pub mod multi;
pub mod noop;
#[cfg(feature = "prometheus")]
pub mod prometheus;
pub mod traits;

pub use self::log::LogObserver;
pub use noop::NoopObserver;
pub use traits::{Observer, ObserverEvent};

use crate::config::ObservabilityConfig;

/// Factory: create the right observer from config
pub fn create_observer(config: &ObservabilityConfig) -> Box<dyn Observer> {
    match config.backend.as_str() {
        "log" => Box::new(LogObserver::new()),
        "none" | "noop" => Box::new(NoopObserver),
        #[cfg(feature = "prometheus")]
        "prometheus" => Box::new(prometheus::PrometheusObserver::new()),
        #[cfg(not(feature = "prometheus"))]
        "prometheus" => {
            tracing::warn!(
                "Prometheus backend requested but the 'prometheus' feature is not enabled. Rebuild with --features prometheus. Falling back to noop."
            );
            Box::new(NoopObserver)
        }
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
        };
        assert_eq!(create_observer(&cfg).name(), "noop");
    }

    #[test]
    fn factory_noop_returns_noop() {
        let cfg = ObservabilityConfig {
            backend: "noop".into(),
        };
        assert_eq!(create_observer(&cfg).name(), "noop");
    }

    #[test]
    fn factory_log_returns_log() {
        let cfg = ObservabilityConfig {
            backend: "log".into(),
        };
        assert_eq!(create_observer(&cfg).name(), "log");
    }

    #[test]
    fn factory_unknown_falls_back_to_noop() {
        let cfg = ObservabilityConfig {
            backend: "otlp".into(),
        };
        assert_eq!(create_observer(&cfg).name(), "noop");
    }

    #[cfg(feature = "prometheus")]
    #[test]
    fn factory_prometheus_returns_prometheus() {
        let cfg = ObservabilityConfig {
            backend: "prometheus".into(),
        };
        assert_eq!(create_observer(&cfg).name(), "prometheus");
    }

    #[test]
    fn factory_empty_string_falls_back_to_noop() {
        let cfg = ObservabilityConfig {
            backend: String::new(),
        };
        assert_eq!(create_observer(&cfg).name(), "noop");
    }

    #[test]
    fn factory_garbage_falls_back_to_noop() {
        let cfg = ObservabilityConfig {
            backend: "xyzzy_garbage_123".into(),
        };
        assert_eq!(create_observer(&cfg).name(), "noop");
    }
}
