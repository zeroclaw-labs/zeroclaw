//! MQTT subscriber that publishes broker messages into the dispatch router.
//!
//! Subscribes to a list of topics on a broker and converts every incoming
//! `PUBLISH` packet into a `DispatchEvent { source: Mqtt, topic, payload }`,
//! then routes it through `EventRouter::dispatch()` and records it via
//! `DispatchAuditLogger`.
//!
//! ## Why this is the right shape
//!
//! - **No SOP coupling.** Unlike the previous (deleted) `src/channels/mqtt.rs`
//!   which hard-wired into the SOP engine, this subscriber depends only on
//!   the generic dispatch primitives. Adding a new MQTT-driven behaviour is
//!   a matter of registering a new `EventHandler` — no code changes here.
//! - **Auto-reconnect.** `rumqttc::EventLoop::poll()` handles reconnect
//!   internally; we just keep looping.
//! - **Cancellable.** Returns when the supplied `cancel` future resolves so
//!   the daemon can shut the subscriber down cleanly.
//!
//! Compiled only when the `mqtt` Cargo feature is enabled.

#![cfg(feature = "mqtt")]

use std::sync::Arc;

use anyhow::{anyhow, Result};
use rumqttc::{AsyncClient, Event, MqttOptions, Packet, QoS, Transport};
use tracing::{info, warn};

use crate::config::schema::MqttConfig;
use crate::dispatch::{
    DispatchAuditLogger, DispatchEvent, EventRouter, EventSource,
};

/// Run the MQTT subscriber loop until `cancel` resolves or the broker connection
/// is permanently closed.
///
/// `router` and `audit` are shared with every other subsystem that uses the
/// dispatch substrate. Handler registration is the application's job and must
/// happen before this loop is started — handlers registered later will still
/// receive events, but messages that arrive before registration will be lost.
pub async fn run_mqtt_subscriber(
    config: MqttConfig,
    router: Arc<EventRouter>,
    audit: Arc<DispatchAuditLogger>,
    cancel: impl std::future::Future<Output = ()> + Send + 'static,
) -> Result<()> {
    if !config.enabled {
        info!("MQTT subscriber: disabled in config, returning");
        return Ok(());
    }
    if config.broker_url.is_empty() {
        return Err(anyhow!("MQTT subscriber: broker_url is empty"));
    }
    if config.topics.is_empty() {
        return Err(anyhow!(
            "MQTT subscriber: topics list is empty (nothing to subscribe to)"
        ));
    }

    let host = broker_host(&config.broker_url);
    let port = broker_port(&config.broker_url);
    let tls = config.use_tls || config.broker_url.starts_with("mqtts://");

    let mut mqtt_options = MqttOptions::new(&config.client_id, host, port);
    mqtt_options.set_keep_alive(std::time::Duration::from_secs(config.keep_alive_secs));

    if let (Some(ref user), Some(ref pass)) = (&config.username, &config.password) {
        mqtt_options.set_credentials(user, pass);
    }
    if tls {
        mqtt_options.set_transport(Transport::tls_with_default_config());
        info!("MQTT subscriber: TLS transport enabled");
    }

    let (client, mut eventloop) = AsyncClient::new(mqtt_options, 64);

    let qos = match config.qos {
        0 => QoS::AtMostOnce,
        1 => QoS::AtLeastOnce,
        _ => QoS::ExactlyOnce,
    };

    for topic in &config.topics {
        client
            .subscribe(topic, qos)
            .await
            .map_err(|e| anyhow!("MQTT subscribe '{topic}' failed: {e}"))?;
        info!("MQTT subscriber: subscribed to '{topic}'");
    }

    tokio::pin!(cancel);

    loop {
        tokio::select! {
            _ = &mut cancel => {
                info!("MQTT subscriber: cancelled");
                let _ = client.disconnect().await;
                return Ok(());
            }
            poll_result = eventloop.poll() => {
                match poll_result {
                    Ok(Event::Incoming(Packet::Publish(msg))) => {
                        let topic = msg.topic.clone();
                        let payload = String::from_utf8_lossy(&msg.payload).to_string();
                        let event = DispatchEvent::new(
                            EventSource::Mqtt,
                            Some(topic),
                            Some(payload),
                        );
                        if let Err(e) = audit.log_event(&event).await {
                            warn!("MQTT subscriber: audit log_event failed: {e}");
                        }
                        let result = router.dispatch(event).await;
                        if let Err(e) = audit.log_result(&result).await {
                            warn!("MQTT subscriber: audit log_result failed: {e}");
                        }
                    }
                    Ok(Event::Incoming(Packet::ConnAck(_))) => {
                        info!("MQTT subscriber: connected to broker");
                    }
                    Ok(_) => {
                        // PingResp, SubAck, etc. — nothing to do.
                    }
                    Err(e) => {
                        warn!("MQTT subscriber: connection error: {e}");
                        // rumqttc handles reconnect; loop continues.
                    }
                }
            }
        }
    }
}

/// Extract host from `mqtt://host:port` or `mqtts://host:port`.
fn broker_host(url: &str) -> String {
    let without_scheme = url
        .strip_prefix("mqtts://")
        .or_else(|| url.strip_prefix("mqtt://"))
        .unwrap_or(url);
    without_scheme
        .split(':')
        .next()
        .unwrap_or("localhost")
        .to_string()
}

/// Extract port from `mqtt://host:port` or `mqtts://host:port`.
/// Defaults to 8883 for TLS scheme, 1883 otherwise.
fn broker_port(url: &str) -> u16 {
    let is_tls = url.starts_with("mqtts://");
    let default = if is_tls { 8883 } else { 1883 };
    let without_scheme = url
        .strip_prefix("mqtts://")
        .or_else(|| url.strip_prefix("mqtt://"))
        .unwrap_or(url);
    without_scheme
        .split(':')
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_plaintext_broker_url() {
        assert_eq!(broker_host("mqtt://broker.example.com:1883"), "broker.example.com");
        assert_eq!(broker_port("mqtt://broker.example.com:1883"), 1883);
    }

    #[test]
    fn parse_tls_broker_url_default_port() {
        assert_eq!(broker_host("mqtts://broker.example.com"), "broker.example.com");
        assert_eq!(broker_port("mqtts://broker.example.com"), 8883);
    }

    #[test]
    fn parse_plaintext_broker_url_default_port() {
        assert_eq!(broker_port("mqtt://broker.example.com"), 1883);
    }

    #[test]
    fn parse_url_without_scheme_uses_string_as_host() {
        assert_eq!(broker_host("plain.example.com:1234"), "plain.example.com");
        assert_eq!(broker_port("plain.example.com:1234"), 1234);
    }

    #[tokio::test]
    async fn run_mqtt_subscriber_returns_when_disabled() {
        let config = MqttConfig {
            enabled: false,
            broker_url: "mqtt://localhost:1883".into(),
            client_id: "test".into(),
            topics: vec!["test/topic".into()],
            qos: 1,
            keep_alive_secs: 60,
            username: None,
            password: None,
            use_tls: false,
        };
        let router = Arc::new(EventRouter::new());
        let mem_cfg = crate::config::MemoryConfig {
            backend: "sqlite".into(),
            ..crate::config::MemoryConfig::default()
        };
        let tmp = tempfile::tempdir().unwrap();
        let memory: Arc<dyn crate::memory::traits::Memory> =
            Arc::from(crate::memory::create_memory(&mem_cfg, tmp.path(), None).unwrap());
        let audit = Arc::new(DispatchAuditLogger::new(memory));

        let (_cancel_tx, cancel_rx) = tokio::sync::oneshot::channel::<()>();
        let cancel_fut = async move {
            let _ = cancel_rx.await;
        };

        // Should return Ok immediately because enabled = false.
        let result = run_mqtt_subscriber(config, router, audit, cancel_fut).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn run_mqtt_subscriber_rejects_empty_broker_url() {
        let config = MqttConfig {
            enabled: true,
            broker_url: "".into(),
            client_id: "test".into(),
            topics: vec!["test/topic".into()],
            qos: 1,
            keep_alive_secs: 60,
            username: None,
            password: None,
            use_tls: false,
        };
        let router = Arc::new(EventRouter::new());
        let mem_cfg = crate::config::MemoryConfig {
            backend: "sqlite".into(),
            ..crate::config::MemoryConfig::default()
        };
        let tmp = tempfile::tempdir().unwrap();
        let memory: Arc<dyn crate::memory::traits::Memory> =
            Arc::from(crate::memory::create_memory(&mem_cfg, tmp.path(), None).unwrap());
        let audit = Arc::new(DispatchAuditLogger::new(memory));
        let (_cancel_tx, cancel_rx) = tokio::sync::oneshot::channel::<()>();
        let cancel_fut = async move {
            let _ = cancel_rx.await;
        };

        let result = run_mqtt_subscriber(config, router, audit, cancel_fut).await;
        assert!(result.is_err());
        assert!(
            result.unwrap_err().to_string().contains("broker_url"),
            "error should mention broker_url"
        );
    }

    #[tokio::test]
    async fn run_mqtt_subscriber_rejects_empty_topics() {
        let config = MqttConfig {
            enabled: true,
            broker_url: "mqtt://localhost:1883".into(),
            client_id: "test".into(),
            topics: vec![],
            qos: 1,
            keep_alive_secs: 60,
            username: None,
            password: None,
            use_tls: false,
        };
        let router = Arc::new(EventRouter::new());
        let mem_cfg = crate::config::MemoryConfig {
            backend: "sqlite".into(),
            ..crate::config::MemoryConfig::default()
        };
        let tmp = tempfile::tempdir().unwrap();
        let memory: Arc<dyn crate::memory::traits::Memory> =
            Arc::from(crate::memory::create_memory(&mem_cfg, tmp.path(), None).unwrap());
        let audit = Arc::new(DispatchAuditLogger::new(memory));
        let (_cancel_tx, cancel_rx) = tokio::sync::oneshot::channel::<()>();
        let cancel_fut = async move {
            let _ = cancel_rx.await;
        };

        let result = run_mqtt_subscriber(config, router, audit, cancel_fut).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("topics"));
    }
}
