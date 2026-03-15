use anyhow::{Context, Result};
use rumqttc::{AsyncClient, Event, EventLoop, MqttOptions, Packet, QoS};
use std::time::Duration;
use tokio::time::sleep;

pub struct MqttClient {
    client: AsyncClient,
    event_loop: EventLoop,
}

impl MqttClient {
    pub fn new(broker: &str, port: u16, client_id: &str) -> Result<Self> {
        let mut mqtt_options = MqttOptions::new(client_id, broker, port);
        mqtt_options.set_keep_alive(Duration::from_secs(60));
        mqtt_options.set_clean_session(true);

        let (client, event_loop) = AsyncClient::new(mqtt_options, 10);

        Ok(Self { client, event_loop })
    }

    pub async fn connect(&mut self) -> Result<()> {
        let mut backoff = Duration::from_secs(1);
        let max_backoff = Duration::from_secs(60);

        loop {
            match self.event_loop.poll().await {
                Ok(Event::Incoming(Packet::ConnAck(_))) => {
                    return Ok(());
                }
                Ok(_) => continue,
                Err(e) => {
                    tracing::warn!("Connection failed: {}, retrying in {:?}", e, backoff);
                    sleep(backoff).await;
                    backoff = (backoff * 2).min(max_backoff);
                }
            }
        }
    }

    pub async fn subscribe(&self, topic: &str) -> Result<()> {
        self.client
            .subscribe(topic, QoS::AtLeastOnce)
            .await
            .context("Failed to subscribe")
    }

    pub async fn publish(&self, topic: &str, payload: &[u8]) -> Result<()> {
        self.client
            .publish(topic, QoS::AtLeastOnce, false, payload)
            .await
            .context("Failed to publish")
    }

    pub async fn poll(&mut self) -> Result<Event> {
        self.event_loop
            .poll()
            .await
            .context("Event loop poll failed")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_client_creation() {
        let client = MqttClient::new("localhost", 1883, "test-client");
        assert!(client.is_ok());
    }

    #[tokio::test]
    async fn test_subscribe() {
        let client = MqttClient::new("localhost", 1883, "test-sub").unwrap();
        let result = client.subscribe("test/topic").await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_publish() {
        let client = MqttClient::new("localhost", 1883, "test-pub").unwrap();
        let result = client.publish("test/topic", b"hello").await;
        assert!(result.is_ok());
    }
}
