pub mod log;
pub mod multi;
pub mod noop;
pub mod prometheus;
pub mod traits;

pub use self::log::LogObserver;
pub use noop::NoopObserver;
pub use prometheus::PrometheusObserver;
pub use traits::{Observer, ObserverEvent};

use crate::config::ObservabilityConfig;

/// Factory: create the right observer from config
pub fn create_observer(config: &ObservabilityConfig) -> Box<dyn Observer> {
    match config.backend.as_str() {
        "log" => Box::new(LogObserver::new()),
        "prometheus" => Box::new(PrometheusObserver::new()),
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
            ..Default::default()
        };
        assert_eq!(create_observer(&cfg).name(), "noop");
    }

    #[test]
    fn factory_noop_returns_noop() {
        let cfg = ObservabilityConfig {
            backend: "noop".into(),
            ..Default::default()
        };
        assert_eq!(create_observer(&cfg).name(), "noop");
    }

    #[test]
    fn factory_log_returns_log() {
        let cfg = ObservabilityConfig {
            backend: "log".into(),
            ..Default::default()
        };
        assert_eq!(create_observer(&cfg).name(), "log");
    }

    #[test]
    fn factory_prometheus_returns_prometheus() {
        let cfg = ObservabilityConfig {
            backend: "prometheus".into(),
            ..Default::default()
        };
        assert_eq!(create_observer(&cfg).name(), "prometheus");
    }

    #[test]
    fn factory_unknown_falls_back_to_noop() {
        let cfg = ObservabilityConfig {
            backend: "xyzzy_unknown".into(),
            ..Default::default()
        };
        assert_eq!(create_observer(&cfg).name(), "noop");
    }

    #[test]
    fn factory_empty_string_falls_back_to_noop() {
        let cfg = ObservabilityConfig {
            backend: String::new(),
            ..Default::default()
        };
        assert_eq!(create_observer(&cfg).name(), "noop");
    }

    #[test]
    fn factory_garbage_falls_back_to_noop() {
        let cfg = ObservabilityConfig {
            backend: "xyzzy_garbage_123".into(),
            ..Default::default()
        };
        assert_eq!(create_observer(&cfg).name(), "noop");
    }
}
