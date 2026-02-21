//! ClawdTalk voice channel - real-time voice calling via Telnyx SIP infrastructure.
//!
//! ClawdTalk (https://clawdtalk.com) provides AI-powered voice conversations
//! using Telnyx's global SIP network for low-latency, high-quality calls.

use crate::config::traits::ChannelConfig;

use super::traits::{Channel, ChannelMessage, SendMessage};
use async_trait::async_trait;
use reqwest::Client;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

/// ClawdTalk channel configuration
pub struct ClawdTalkChannel {
    /// Telnyx API key for authentication
    api_key: String,
    /// Telnyx connection ID (SIP connection)
    connection_id: String,
    /// Phone number or SIP URI to call from
    from_number: String,
    /// Allowed destination numbers/patterns
    allowed_destinations: Vec<String>,
    /// HTTP client for Telnyx API
    client: Client,
    /// Webhook secret for verifying incoming calls
    webhook_secret: Option<String>,
}

/// Configuration for ClawdTalk channel from config.toml
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ClawdTalkConfig {
    /// Telnyx API key
    pub api_key: String,
    /// Telnyx connection ID for SIP
    pub connection_id: String,
    /// Phone number to call from (E.164 format)
    pub from_number: String,
    /// Allowed destination numbers or patterns
    #[serde(default)]
    pub allowed_destinations: Vec<String>,
    /// Webhook secret for signature verification
    #[serde(default)]
    pub webhook_secret: Option<String>,
}

impl ChannelConfig for ClawdTalkConfig {
    fn name() -> &'static str {
        "ClawdTalk"
    }
    fn desc() -> &'static str {
        "ClawdTalk Channel"
    }
}

impl ClawdTalkChannel {
    /// Create a new ClawdTalk channel
    pub fn new(config: ClawdTalkConfig) -> Self {
        Self {
            api_key: config.api_key,
            connection_id: config.connection_id,
            from_number: config.from_number,
            allowed_destinations: config.allowed_destinations,
            client: Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .unwrap_or_else(|_| Client::new()),
            webhook_secret: config.webhook_secret,
        }
    }

    /// Telnyx API base URL
    const TELNYX_API_URL: &'static str = "https://api.telnyx.com/v2";

    /// Check if a destination is allowed
    fn is_destination_allowed(&self, destination: &str) -> bool {
        if self.allowed_destinations.is_empty() {
            return true;
        }
        self.allowed_destinations.iter().any(|pattern| {
            pattern == "*" || destination.starts_with(pattern) || pattern == destination
        })
    }

    /// Initiate an outbound call via Telnyx
    pub async fn initiate_call(
        &self,
        to: &str,
        _prompt: Option<&str>,
    ) -> anyhow::Result<CallSession> {
        if !self.is_destination_allowed(to) {
            anyhow::bail!("Destination {} is not in allowed list", to);
        }

        let request = CallRequest {
            connection_id: self.connection_id.clone(),
            to: to.to_string(),
            from: self.from_number.clone(),
            answering_machine_detection: Some(AnsweringMachineDetection {
                mode: "premium".to_string(),
            }),
            webhook_url: None,
            // AI voice settings via Telnyx Call Control
            command_id: None,
        };

        let response = self
            .client
            .post(format!("{}/calls", Self::TELNYX_API_URL))
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&request)
            .send()
            .await?;

        if !response.status().is_success() {
            let error = response.text().await?;
            anyhow::bail!("Failed to initiate call: {}", error);
        }

        let call_response: CallResponse = response.json().await?;

        Ok(CallSession {
            call_control_id: call_response.call_control_id,
            call_leg_id: call_response.call_leg_id,
            call_session_id: call_response.call_session_id,
        })
    }

    /// Send audio or TTS to an active call
    pub async fn speak(&self, call_control_id: &str, text: &str) -> anyhow::Result<()> {
        let request = SpeakRequest {
            payload: text.to_string(),
            payload_type: "text".to_string(),
            service_level: "premium".to_string(),
            voice: "female".to_string(),
            language: "en-US".to_string(),
        };

        let response = self
            .client
            .post(format!(
                "{}/calls/{}/actions/speak",
                Self::TELNYX_API_URL,
                call_control_id
            ))
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&request)
            .send()
            .await?;

        if !response.status().is_success() {
            let error = response.text().await?;
            anyhow::bail!("Failed to speak: {}", error);
        }

        Ok(())
    }

    /// Hang up an active call
    pub async fn hangup(&self, call_control_id: &str) -> anyhow::Result<()> {
        let response = self
            .client
            .post(format!(
                "{}/calls/{}/actions/hangup",
                Self::TELNYX_API_URL,
                call_control_id
            ))
            .header("Authorization", format!("Bearer {}", self.api_key))
            .send()
            .await?;

        if !response.status().is_success() {
            let error = response.text().await?;
            tracing::warn!("Failed to hangup call: {}", error);
        }

        Ok(())
    }

    /// Start AI-powered conversation using Telnyx AI inference
    pub async fn start_ai_conversation(
        &self,
        call_control_id: &str,
        system_prompt: &str,
        model: &str,
    ) -> anyhow::Result<()> {
        let request = AiConversationRequest {
            system_prompt: system_prompt.to_string(),
            model: model.to_string(),
            voice_settings: VoiceSettings {
                voice: "alloy".to_string(),
                speed: 1.0,
            },
        };

        let response = self
            .client
            .post(format!(
                "{}/calls/{}/actions/ai_conversation",
                Self::TELNYX_API_URL,
                call_control_id
            ))
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&request)
            .send()
            .await?;

        if !response.status().is_success() {
            let error = response.text().await?;
            anyhow::bail!("Failed to start AI conversation: {}", error);
        }

        Ok(())
    }
}

/// Active call session
#[derive(Debug, Clone)]
pub struct CallSession {
    pub call_control_id: String,
    pub call_leg_id: String,
    pub call_session_id: String,
}

/// Telnyx call initiation request
#[derive(Debug, Serialize)]
struct CallRequest {
    connection_id: String,
    to: String,
    from: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    answering_machine_detection: Option<AnsweringMachineDetection>,
    #[serde(skip_serializing_if = "Option::is_none")]
    webhook_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    command_id: Option<String>,
}

#[derive(Debug, Serialize)]
struct AnsweringMachineDetection {
    mode: String,
}

/// Telnyx call response
#[derive(Debug, Deserialize)]
struct CallResponse {
    call_control_id: String,
    call_leg_id: String,
    call_session_id: String,
}

/// TTS speak request
#[derive(Debug, Serialize)]
struct SpeakRequest {
    payload: String,
    payload_type: String,
    service_level: String,
    voice: String,
    language: String,
}

/// AI conversation request
#[derive(Debug, Serialize)]
struct AiConversationRequest {
    system_prompt: String,
    model: String,
    voice_settings: VoiceSettings,
}

#[derive(Debug, Serialize)]
struct VoiceSettings {
    voice: String,
    speed: f32,
}

#[async_trait]
impl Channel for ClawdTalkChannel {
    fn name(&self) -> &str {
        "ClawdTalk"
    }

    async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
        // For ClawdTalk, "send" initiates a call with the message as TTS
        let session = self.initiate_call(&message.recipient, None).await?;

        // Wait for call to be answered, then speak
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;

        self.speak(&session.call_control_id, &message.content)
            .await?;

        // Give time for TTS to complete before hanging up
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;

        self.hangup(&session.call_control_id).await?;

        Ok(())
    }

    async fn listen(&self, tx: mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        // ClawdTalk listens for incoming calls via webhooks
        // This would typically be handled by the gateway module
        // For now, we signal that this channel is ready and wait indefinitely
        tracing::info!("ClawdTalk channel listening for incoming calls");

        // Keep the listener alive
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;

            // Check if channel is still open
            if tx.is_closed() {
                break;
            }
        }

        Ok(())
    }

    async fn health_check(&self) -> bool {
        // Verify API key by checking Telnyx number configuration
        let response = self
            .client
            .get(format!("{}/phone_numbers", Self::TELNYX_API_URL))
            .header("Authorization", format!("Bearer {}", self.api_key))
            .send()
            .await;

        match response {
            Ok(resp) => resp.status().is_success(),
            Err(e) => {
                tracing::warn!("ClawdTalk health check failed: {}", e);
                false
            }
        }
    }
}

/// Webhook event from Telnyx for incoming calls
#[derive(Debug, Deserialize)]
pub struct TelnyxWebhookEvent {
    pub data: TelnyxWebhookData,
}

#[derive(Debug, Deserialize)]
pub struct TelnyxWebhookData {
    pub event_type: String,
    pub payload: TelnyxCallPayload,
}

#[derive(Debug, Deserialize)]
pub struct TelnyxCallPayload {
    pub call_control_id: Option<String>,
    pub call_leg_id: Option<String>,
    pub call_session_id: Option<String>,
    pub direction: Option<String>,
    pub from: Option<String>,
    pub to: Option<String>,
    pub state: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> ClawdTalkConfig {
        ClawdTalkConfig {
            api_key: "test-key".to_string(),
            connection_id: "test-connection".to_string(),
            from_number: "+15551234567".to_string(),
            allowed_destinations: vec!["+1555".to_string()],
            webhook_secret: None,
        }
    }

    #[test]
    fn creates_channel() {
        let channel = ClawdTalkChannel::new(test_config());
        assert_eq!(channel.name(), "ClawdTalk");
    }

    #[test]
    fn destination_allowed_exact_match() {
        let channel = ClawdTalkChannel::new(test_config());
        assert!(channel.is_destination_allowed("+15559876543"));
        assert!(!channel.is_destination_allowed("+14449876543"));
    }

    #[test]
    fn destination_allowed_wildcard() {
        let mut config = test_config();
        config.allowed_destinations = vec!["*".to_string()];
        let channel = ClawdTalkChannel::new(config);
        assert!(channel.is_destination_allowed("+15559876543"));
        assert!(channel.is_destination_allowed("+14449876543"));
    }

    #[test]
    fn destination_allowed_empty_means_all() {
        let mut config = test_config();
        config.allowed_destinations = vec![];
        let channel = ClawdTalkChannel::new(config);
        assert!(channel.is_destination_allowed("+15559876543"));
        assert!(channel.is_destination_allowed("+14449876543"));
    }

    #[test]
    fn webhook_event_deserializes() {
        let json = r#"{
            "data": {
                "event_type": "call.initiated",
                "payload": {
                    "call_control_id": "call-123",
                    "call_leg_id": "leg-123",
                    "call_session_id": "session-123",
                    "direction": "incoming",
                    "from": "+15551112222",
                    "to": "+15553334444",
                    "state": "ringing"
                }
            }
        }"#;

        let event: TelnyxWebhookEvent = serde_json::from_str(json).unwrap();
        assert_eq!(event.data.event_type, "call.initiated");
        assert_eq!(
            event.data.payload.call_control_id,
            Some("call-123".to_string())
        );
        assert_eq!(event.data.payload.from, Some("+15551112222".to_string()));
    }
}
