use super::traits::{Observer, ObserverEvent, ObserverMetric};
use crate::config::schema::AlertingConfig;
use parking_lot::Mutex;
use std::any::Any;
use std::collections::HashMap;
use std::time::Instant;
use reqwest::Url;

const MAX_COOLDOWN_ENTRIES: usize = 64;
const MAX_WEBHOOK_MSG_LEN: usize = 256;

pub struct WebhookAlertingObserver {
    config: AlertingConfig,
    last_alert: Mutex<HashMap<String, Instant>>,
    client: reqwest::Client,
}

impl WebhookAlertingObserver {
    pub fn new(config: &AlertingConfig) -> Option<Self> {
        let url_str = config.webhook_url.as_deref().unwrap_or("");
        if url_str.is_empty() || config.alert_on.is_empty() {
            return None;
        }
        // Validate URL: HTTPS required (HTTP only for localhost dev)
        match Url::parse(url_str) {
            Ok(parsed) => {
                let scheme = parsed.scheme();
                let host = parsed.host_str().unwrap_or("");
                let is_local = host == "localhost" || host == "127.0.0.1";
                if scheme != "https" && !(scheme == "http" && is_local) {
                    tracing::warn!(url = url_str, "Alerting URL rejected: HTTPS required");
                    return None;
                }
                if Self::is_private_host(host) {
                    tracing::warn!(url = url_str, "Alerting URL rejected: private host");
                    return None;
                }
            }
            Err(_) => {
                tracing::warn!(url = url_str, "Alerting URL rejected: invalid URL");
                return None;
            }
        }
        Some(Self {
            config: config.clone(),
            last_alert: Mutex::new(HashMap::new()),
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .build()
                .unwrap_or_default(),
        })
    }

    fn is_private_host(host: &str) -> bool {
        host == "169.254.169.254"
            || host.starts_with("10.")
            || host.starts_with("192.168.")
            || host.starts_with("172.16.")
            || host.starts_with("172.17.")
            || host.starts_with("172.18.")
            || host.starts_with("172.19.")
            || host.starts_with("172.2")
            || host.starts_with("172.30.")
            || host.starts_with("172.31.")
            || host == "[::1]"
            || host == "0.0.0.0"
    }

    fn should_alert(&self, event_type: &str) -> bool {
        if !self.config.alert_on.iter().any(|e| e == event_type) {
            return false;
        }
        let mut map = self.last_alert.lock();
        let now = Instant::now();
        if let Some(last) = map.get(event_type) {
            if now.duration_since(*last).as_secs() < self.config.cooldown_secs {
                return false;
            }
        }
        // Evict oldest entry if map is at capacity
        if map.len() >= MAX_COOLDOWN_ENTRIES && !map.contains_key(event_type) {
            if let Some(oldest) = map
                .iter()
                .min_by_key(|(_, t)| *t)
                .map(|(k, _)| k.clone())
            {
                map.remove(&oldest);
            }
        }
        map.insert(event_type.to_string(), now);
        true
    }

    fn fire_alert(&self, payload: serde_json::Value) {
        if let Some(url) = &self.config.webhook_url {
            let url = url.clone();
            let client = self.client.clone();
            tokio::spawn(async move {
                match client.post(&url).json(&payload).send().await {
                    Ok(resp) if !resp.status().is_success() => {
                        tracing::warn!(
                            status = %resp.status(),
                            "Webhook alert delivery failed"
                        );
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "Webhook alert delivery error");
                    }
                    _ => {}
                }
            });
        }
    }

    /// Truncate a message to MAX_WEBHOOK_MSG_LEN, UTF-8 safe.
    fn sanitize_message(msg: &str) -> String {
        if msg.len() <= MAX_WEBHOOK_MSG_LEN {
            return msg.to_string();
        }
        let boundary = msg
            .char_indices()
            .take_while(|(i, _)| *i < MAX_WEBHOOK_MSG_LEN)
            .last()
            .map(|(i, c)| i + c.len_utf8())
            .unwrap_or(0);
        format!("{}...", &msg[..boundary])
    }
}

impl Observer for WebhookAlertingObserver {
    fn record_event(&self, event: &ObserverEvent) {
        match event {
            ObserverEvent::LoopDetected {
                tool, strategy, category, consecutive_failures, warning,
            } => {
                let et = if *warning { "loop_warning" } else { "loop_hard_stop" };
                if self.should_alert(et) {
                    self.fire_alert(serde_json::json!({
                        "event": et, "tool": tool, "strategy": strategy,
                        "category": category, "consecutive_failures": consecutive_failures,
                        "source": "zeroclaw",
                    }));
                }
            }
            ObserverEvent::Error { component, message } => {
                if component == "provider" && self.should_alert("provider_error") {
                    self.fire_alert(serde_json::json!({
                        "event": "provider_error", "component": component,
                        "message": Self::sanitize_message(message),
                        "source": "zeroclaw",
                    }));
                }
            }
            _ => {}
        }
    }

    fn record_metric(&self, _metric: &ObserverMetric) {}
    fn name(&self) -> &str { "webhook_alerting" }
    fn as_any(&self) -> &dyn Any { self }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_returns_none_when_no_url() {
        assert!(WebhookAlertingObserver::new(&AlertingConfig::default()).is_none());
    }

    #[test]
    fn new_returns_some_when_configured() {
        let c = AlertingConfig {
            webhook_url: Some("https://example.com/hook".into()),
            alert_on: vec!["loop_hard_stop".into()],
            cooldown_secs: 300,
        };
        assert_eq!(WebhookAlertingObserver::new(&c).unwrap().name(), "webhook_alerting");
    }

    #[test]
    fn cooldown_prevents_duplicates() {
        let c = AlertingConfig {
            webhook_url: Some("https://example.com/hook".into()),
            alert_on: vec!["loop_hard_stop".into()],
            cooldown_secs: 300,
        };
        let obs = WebhookAlertingObserver::new(&c).unwrap();
        assert!(obs.should_alert("loop_hard_stop"));
        assert!(!obs.should_alert("loop_hard_stop"));
    }

    #[test]
    fn unsubscribed_event_not_alerted() {
        let c = AlertingConfig {
            webhook_url: Some("https://example.com/hook".into()),
            alert_on: vec!["loop_hard_stop".into()],
            cooldown_secs: 300,
        };
        let obs = WebhookAlertingObserver::new(&c).unwrap();
        assert!(!obs.should_alert("loop_warning"));
    }

    #[test]
    fn rejects_http_non_localhost() {
        let c = AlertingConfig {
            webhook_url: Some("http://example.com/hook".into()),
            alert_on: vec!["loop_hard_stop".into()],
            cooldown_secs: 300,
        };
        assert!(WebhookAlertingObserver::new(&c).is_none());
    }

    #[test]
    fn allows_http_localhost() {
        let c = AlertingConfig {
            webhook_url: Some("http://localhost:8080/hook".into()),
            alert_on: vec!["loop_hard_stop".into()],
            cooldown_secs: 300,
        };
        assert!(WebhookAlertingObserver::new(&c).is_some());
    }

    #[test]
    fn rejects_private_ip() {
        for url in &[
            "https://10.0.0.1/hook",
            "https://192.168.1.1/hook",
            "https://169.254.169.254/latest/meta-data",
        ] {
            let c = AlertingConfig {
                webhook_url: Some(url.to_string()),
                alert_on: vec!["loop_hard_stop".into()],
                cooldown_secs: 300,
            };
            assert!(WebhookAlertingObserver::new(&c).is_none(), "should reject {url}");
        }
    }

    #[test]
    fn rejects_invalid_url() {
        let c = AlertingConfig {
            webhook_url: Some("not-a-url".into()),
            alert_on: vec!["loop_hard_stop".into()],
            cooldown_secs: 300,
        };
        assert!(WebhookAlertingObserver::new(&c).is_none());
    }

    #[test]
    fn sanitize_message_truncates_long_text() {
        let long = "a".repeat(500);
        let result = WebhookAlertingObserver::sanitize_message(&long);
        assert!(result.len() <= MAX_WEBHOOK_MSG_LEN + 3); // +3 for "..."
        assert!(result.ends_with("..."));
    }

    #[test]
    fn sanitize_message_preserves_short_text() {
        let short = "short error";
        assert_eq!(WebhookAlertingObserver::sanitize_message(short), short);
    }
}
