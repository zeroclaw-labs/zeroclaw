//! MQTT → SOP event fan-in listener.
//!
//! This is NOT a `Channel` trait implementor — it routes MQTT messages
//! to the SOP engine via `dispatch_sop_event`, not to the chat loop.

use std::sync::{Arc, Mutex};

use anyhow::Result;
use rumqttc::{AsyncClient, Event, MqttOptions, Packet, QoS, Transport};
use tracing::{info, warn};

use crate::config::MqttConfig;
use crate::sop::audit::SopAuditLogger;
use crate::sop::dispatch::{dispatch_sop_event, process_headless_results};
use crate::sop::engine::{now_iso8601, SopEngine};
use crate::sop::types::{SopEvent, SopTriggerSource};

/// Run the MQTT SOP listener loop.
///
/// Subscribes to configured topics and dispatches incoming publishes
/// to the SOP engine. Blocks until disconnected or cancelled.
pub async fn run_mqtt_sop_listener(
    config: &MqttConfig,
    engine: Arc<Mutex<SopEngine>>,
    audit: Arc<SopAuditLogger>,
) -> Result<()> {
    config.validate()?;

    let mut mqtt_options = MqttOptions::new(
        &config.client_id,
        broker_host(&config.broker_url),
        broker_port(&config.broker_url),
    );
    mqtt_options.set_keep_alive(std::time::Duration::from_secs(config.keep_alive_secs));

    if let (Some(ref user), Some(ref pass)) = (&config.username, &config.password) {
        mqtt_options.set_credentials(user, pass);
    }

    // Configure TLS transport when mqtts:// scheme is used
    if config.use_tls {
        mqtt_options.set_transport(Transport::tls_with_default_config());
        info!("MQTT SOP listener: TLS transport enabled");
    }

    let (client, mut eventloop) = AsyncClient::new(mqtt_options, 64);

    let qos = match config.qos {
        0 => QoS::AtMostOnce,
        1 => QoS::AtLeastOnce,
        _ => QoS::ExactlyOnce,
    };

    // Subscribe to all configured topics
    for topic in &config.topics {
        client.subscribe(topic, qos).await?;
        info!("MQTT SOP listener: subscribed to '{topic}'");
    }

    crate::health::mark_component_ok("mqtt");

    loop {
        match eventloop.poll().await {
            Ok(Event::Incoming(Packet::Publish(msg))) => {
                let topic = msg.topic.clone();
                let payload = String::from_utf8_lossy(&msg.payload).to_string();

                let event = SopEvent {
                    source: SopTriggerSource::Mqtt,
                    topic: Some(topic),
                    payload: Some(payload),
                    timestamp: now_iso8601(),
                };

                let results = dispatch_sop_event(&engine, &audit, event).await;
                process_headless_results(&results).await;
            }
            Ok(Event::Incoming(Packet::ConnAck(_))) => {
                crate::health::mark_component_ok("mqtt");
                info!("MQTT SOP listener: connected to broker");
            }
            Ok(_) => {
                // Other events (PingResp, SubAck, etc.) — ignore
            }
            Err(e) => {
                crate::health::mark_component_error("mqtt", e.to_string());
                warn!("MQTT SOP listener: connection error: {e}");
                // rumqttc handles auto-reconnect; loop continues
            }
        }
    }
}

/// Extract host from broker URL like "mqtt://host:port"
fn broker_host(url: &str) -> String {
    let without_scheme = url
        .strip_prefix("mqtt://")
        .or_else(|| url.strip_prefix("mqtts://"))
        .unwrap_or(url);
    without_scheme
        .split(':')
        .next()
        .unwrap_or("localhost")
        .to_string()
}

/// Extract port from broker URL, defaulting to 1883 for mqtt:// and 8883 for mqtts://.
fn broker_port(url: &str) -> u16 {
    let is_tls = url.starts_with("mqtts://");
    let without_scheme = url
        .strip_prefix("mqtt://")
        .or_else(|| url.strip_prefix("mqtts://"))
        .unwrap_or(url);
    let default_port: u16 = if is_tls { 8883 } else { 1883 };
    without_scheme
        .rsplit(':')
        .next()
        .and_then(|p| p.parse().ok())
        .unwrap_or(default_port)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mqtt_config_validation_rejects_bad_qos() {
        let config = MqttConfig {
            broker_url: "mqtt://localhost:1883".into(),
            client_id: "zeroclaw".into(),
            topics: vec!["test".into()],
            qos: 3,
            username: None,
            password: None,
            use_tls: false,
            keep_alive_secs: 30,
        };
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("qos must be 0, 1, or 2"));
    }

    #[test]
    fn mqtt_config_validation_rejects_bad_url() {
        let config = MqttConfig {
            broker_url: "http://localhost:1883".into(),
            client_id: "zeroclaw".into(),
            topics: vec!["test".into()],
            qos: 1,
            username: None,
            password: None,
            use_tls: false,
            keep_alive_secs: 30,
        };
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("mqtt://"));
    }

    #[test]
    fn mqtt_config_validation_rejects_empty_topics() {
        let config = MqttConfig {
            broker_url: "mqtt://localhost:1883".into(),
            client_id: "zeroclaw".into(),
            topics: vec![],
            qos: 1,
            username: None,
            password: None,
            use_tls: false,
            keep_alive_secs: 30,
        };
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("at least one topic"));
    }

    #[test]
    fn mqtt_config_validation_rejects_empty_client_id() {
        let config = MqttConfig {
            broker_url: "mqtt://localhost:1883".into(),
            client_id: String::new(),
            topics: vec!["test".into()],
            qos: 1,
            username: None,
            password: None,
            use_tls: false,
            keep_alive_secs: 30,
        };
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("client_id must not be empty"));
    }

    #[test]
    fn mqtt_config_validation_accepts_valid() {
        let config = MqttConfig {
            broker_url: "mqtt://localhost:1883".into(),
            client_id: "zeroclaw".into(),
            topics: vec!["sensors/#".into()],
            qos: 1,
            username: None,
            password: None,
            use_tls: false,
            keep_alive_secs: 30,
        };
        assert!(config.validate().is_ok());
    }

    #[test]
    fn mqtt_tls_flag_rejects_mqtt_scheme_with_use_tls() {
        let config = MqttConfig {
            broker_url: "mqtt://localhost:1883".into(),
            client_id: "zeroclaw".into(),
            topics: vec!["test".into()],
            qos: 1,
            username: None,
            password: None,
            use_tls: true,
            keep_alive_secs: 30,
        };
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("use_tls is true"));
    }

    #[test]
    fn mqtt_tls_flag_rejects_mqtts_scheme_without_use_tls() {
        let config = MqttConfig {
            broker_url: "mqtts://localhost:8883".into(),
            client_id: "zeroclaw".into(),
            topics: vec!["test".into()],
            qos: 1,
            username: None,
            password: None,
            use_tls: false,
            keep_alive_secs: 30,
        };
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("mqtts://"));
    }

    #[test]
    fn mqtt_tls_flag_accepts_mqtts_with_use_tls() {
        let config = MqttConfig {
            broker_url: "mqtts://localhost:8883".into(),
            client_id: "zeroclaw".into(),
            topics: vec!["test".into()],
            qos: 1,
            username: None,
            password: None,
            use_tls: true,
            keep_alive_secs: 30,
        };
        assert!(config.validate().is_ok());
    }

    #[test]
    fn broker_host_extracts_host() {
        assert_eq!(broker_host("mqtt://myhost:1883"), "myhost");
        assert_eq!(
            broker_host("mqtts://secure.example.com:8883"),
            "secure.example.com"
        );
    }

    #[test]
    fn broker_port_extracts_port() {
        assert_eq!(broker_port("mqtt://localhost:1883"), 1883);
        assert_eq!(broker_port("mqtts://host:8883"), 8883);
    }

    #[test]
    fn broker_port_defaults_1883_for_mqtt() {
        assert_eq!(broker_port("mqtt://localhost"), 1883);
    }

    #[test]
    fn broker_port_defaults_8883_for_mqtts() {
        assert_eq!(broker_port("mqtts://secure.example.com"), 8883);
    }
}
