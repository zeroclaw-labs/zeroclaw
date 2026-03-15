use anyhow::Result;
use rumqttc::{Event, Packet};
use tracing::{error, info};

use crate::config::BridgeConfig;
use crate::mqtt_client::MqttClient;
use crate::transform;
use crate::ws_client::WsClient;

pub struct Bridge {
    mqtt: MqttClient,
    ws: WsClient,
}

impl Bridge {
    pub fn new(config: &BridgeConfig) -> Result<Self> {
        let mqtt = MqttClient::new("localhost", 1883, "zeroclaw-bridge")?;
        let ws = WsClient::new(&config.websocket_url).with_token(&config.auth_token);
        Ok(Self { mqtt, ws })
    }

    pub async fn run(&mut self) -> Result<()> {
        info!("Bridge starting...");

        self.mqtt.connect().await?;
        self.ws.connect_with_retry().await?;

        self.mqtt.subscribe("zeroclaw/nodes/+/register").await?;
        self.mqtt.subscribe("zeroclaw/nodes/+/result").await?;

        info!("Bridge connected and subscribed");

        loop {
            tokio::select! {
                mqtt_event = self.mqtt.poll() => {
                    match mqtt_event {
                        Ok(Event::Incoming(Packet::Publish(publish))) => {
                            if let Ok(payload) = std::str::from_utf8(&publish.payload) {
                                match transform::mqtt_to_ws(payload) {
                                    Ok(ws_json) => {
                                        if let Err(e) = self.ws.send(ws_json).await {
                                            error!("Failed to forward MQTT→WS: {}", e);
                                        }
                                    }
                                    Err(e) => error!("Transform MQTT→WS failed: {}", e),
                                }
                            }
                        }
                        Ok(_) => {}
                        Err(e) => {
                            error!("MQTT poll error: {}, reconnecting...", e);
                            if let Err(e) = self.mqtt.connect().await {
                                error!("MQTT reconnect failed: {}", e);
                                break;
                            }
                            if let Err(e) = self.mqtt.subscribe("zeroclaw/nodes/+/register").await {
                                error!("MQTT resubscribe failed: {}", e);
                                break;
                            }
                            if let Err(e) = self.mqtt.subscribe("zeroclaw/nodes/+/result").await {
                                error!("MQTT resubscribe failed: {}", e);
                                break;
                            }
                            info!("MQTT reconnected successfully");
                        }
                    }
                }

                ws_msg = self.ws.receive() => {
                    match ws_msg {
                        Ok(Some(text)) => {
                            match transform::ws_to_mqtt(&text) {
                                Ok(mqtt_json) => {
                                    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&mqtt_json) {
                                        if parsed["type"] == "invoke" {
                                            let topic = "zeroclaw/nodes/+/invoke";
                                            if let Err(e) = self.mqtt.publish(topic, mqtt_json.as_bytes()).await {
                                                error!("Failed to forward WS→MQTT: {}", e);
                                            }
                                        }
                                    }
                                }
                                Err(e) => error!("Transform WS→MQTT failed: {}", e),
                            }
                        }
                        Ok(None) => {
                            info!("WebSocket closed, reconnecting...");
                            if let Err(e) = self.ws.connect_with_retry().await {
                                error!("WebSocket reconnect failed: {}", e);
                                break;
                            }
                            info!("WebSocket reconnected successfully");
                        }
                        Err(e) => {
                            error!("WebSocket receive error: {}, reconnecting...", e);
                            if let Err(e) = self.ws.connect_with_retry().await {
                                error!("WebSocket reconnect failed: {}", e);
                                break;
                            }
                            info!("WebSocket reconnected successfully");
                        }
                    }
                }
            }
        }

        error!("Bridge shutting down due to critical error");
        Ok(())
    }
}
