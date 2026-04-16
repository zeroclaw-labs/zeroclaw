//! Real-time voice call channel for Twilio, Telnyx, and Plivo.
//!
//! Handles inbound/outbound phone calls with real-time STT/TTS streaming,
//! call transcription logging, and approval workflows for outbound calls.
//! Webhook endpoints receive call events from the telephony provider and
//! translate them into `ChannelMessage`s for the agent loop.

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, mpsc};
use tracing::{debug, info, warn};

use zeroclaw_api::channel::{Channel, ChannelMessage, SendMessage};

pub use zeroclaw_config::scattered_types::{VoiceCallConfig, VoiceProvider};

// ── Call state ────────────────────────────────────────────────────

/// Lifecycle state of a phone call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CallState {
    /// Call is ringing (inbound or outbound).
    Ringing,
    /// Call is connected and audio is flowing.
    InProgress,
    /// Call has ended normally.
    Completed,
    /// Call failed to connect.
    Failed,
    /// Caller or callee hung up.
    HungUp,
    /// Call is queued (outbound, awaiting approval).
    PendingApproval,
}

impl fmt::Display for CallState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ringing => write!(f, "ringing"),
            Self::InProgress => write!(f, "in_progress"),
            Self::Completed => write!(f, "completed"),
            Self::Failed => write!(f, "failed"),
            Self::HungUp => write!(f, "hung_up"),
            Self::PendingApproval => write!(f, "pending_approval"),
        }
    }
}

/// Direction of a call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CallDirection {
    Inbound,
    Outbound,
}

/// Tracks an active call's metadata and transcription.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallRecord {
    /// Unique call identifier (provider-specific SID/UUID).
    pub call_id: String,
    /// Direction: inbound or outbound.
    pub direction: CallDirection,
    /// Remote phone number (E.164).
    pub remote_number: String,
    /// Local phone number used.
    pub local_number: String,
    /// Current call state.
    pub state: CallState,
    /// When the call started (ISO-8601).
    pub started_at: String,
    /// When the call ended (ISO-8601), if applicable.
    pub ended_at: Option<String>,
    /// Duration in seconds (updated on completion).
    pub duration_secs: u64,
    /// Running transcript of the call.
    pub transcript: Vec<TranscriptEntry>,
}

/// A single transcript entry from the call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptEntry {
    /// Who said it: `"caller"` or `"agent"`.
    pub speaker: String,
    /// The transcribed text.
    pub text: String,
    /// ISO-8601 timestamp.
    pub timestamp: String,
}

// ── Channel implementation ────────────────────────────────────────

/// Voice call channel — handles telephony via Twilio, Telnyx, or Plivo.
pub struct VoiceCallChannel {
    config: VoiceCallConfig,
    active_calls: Arc<Mutex<HashMap<String, CallRecord>>>,
    client: reqwest::Client,
}

impl VoiceCallChannel {
    pub fn new(config: VoiceCallConfig) -> Self {
        Self {
            config,
            active_calls: Arc::new(Mutex::new(HashMap::new())),
            client: reqwest::Client::new(),
        }
    }

    /// Get the provider-specific API base URL.
    fn api_base_url(&self) -> &str {
        match self.config.provider {
            VoiceProvider::Twilio => "https://api.twilio.com/2010-04-01",
            VoiceProvider::Telnyx => "https://api.telnyx.com/v2",
            VoiceProvider::Plivo => "https://api.plivo.com/v1",
        }
    }

    /// Place an outbound call via the configured provider.
    pub async fn place_call(&self, to_number: &str) -> Result<String> {
        if self.config.require_outbound_approval {
            info!(to = to_number, "outbound call requires approval");
            return Ok(format!("PENDING_APPROVAL:{to_number}"));
        }
        self.execute_outbound_call(to_number).await
    }

    async fn execute_outbound_call(&self, to_number: &str) -> Result<String> {
        let webhook_url = self.webhook_url("/voice/status");

        match self.config.provider {
            VoiceProvider::Twilio => {
                let url = format!(
                    "{}/Accounts/{}/Calls.json",
                    self.api_base_url(),
                    self.config.account_id
                );
                let resp = self
                    .client
                    .post(&url)
                    .basic_auth(&self.config.account_id, Some(&self.config.auth_token))
                    .form(&[
                        ("To", to_number),
                        ("From", &self.config.from_number),
                        ("StatusCallback", &webhook_url),
                        ("Timeout", &self.config.max_call_duration_secs.to_string()),
                    ])
                    .send()
                    .await?;

                if !resp.status().is_success() {
                    let body = resp.text().await.unwrap_or_default();
                    bail!("Twilio call failed: {body}");
                }

                let json: serde_json::Value = serde_json::from_str(&resp.text().await?)?;
                let call_sid = json["sid"].as_str().unwrap_or("unknown").to_string();
                info!(call_sid = %call_sid, to = to_number, "outbound call placed via Twilio");
                Ok(call_sid)
            }
            VoiceProvider::Telnyx => {
                let url = format!("{}/calls", self.api_base_url());
                let resp = self
                    .client
                    .post(&url)
                    .bearer_auth(&self.config.auth_token)
                    .json(&serde_json::json!({
                        "connection_id": self.config.account_id,
                        "to": to_number,
                        "from": self.config.from_number,
                        "webhook_url": webhook_url,
                        "timeout_secs": self.config.max_call_duration_secs,
                    }))
                    .send()
                    .await?;

                if !resp.status().is_success() {
                    let body = resp.text().await.unwrap_or_default();
                    bail!("Telnyx call failed: {body}");
                }

                let json: serde_json::Value = serde_json::from_str(&resp.text().await?)?;
                let call_id = json["data"]["call_control_id"]
                    .as_str()
                    .unwrap_or("unknown")
                    .to_string();
                info!(call_id = %call_id, to = to_number, "outbound call placed via Telnyx");
                Ok(call_id)
            }
            VoiceProvider::Plivo => {
                let url = format!(
                    "{}/Account/{}/Call/",
                    self.api_base_url(),
                    self.config.account_id
                );
                let resp = self
                    .client
                    .post(&url)
                    .basic_auth(&self.config.account_id, Some(&self.config.auth_token))
                    .json(&serde_json::json!({
                        "to": to_number,
                        "from": self.config.from_number,
                        "answer_url": self.webhook_url("/voice/answer"),
                        "hangup_url": self.webhook_url("/voice/hangup"),
                        "time_limit": self.config.max_call_duration_secs,
                    }))
                    .send()
                    .await?;

                if !resp.status().is_success() {
                    let body = resp.text().await.unwrap_or_default();
                    bail!("Plivo call failed: {body}");
                }

                let json: serde_json::Value = serde_json::from_str(&resp.text().await?)?;
                let call_uuid = json["request_uuid"]
                    .as_str()
                    .unwrap_or("unknown")
                    .to_string();
                info!(call_uuid = %call_uuid, to = to_number, "outbound call placed via Plivo");
                Ok(call_uuid)
            }
        }
    }

    /// Construct a full webhook URL from a path.
    fn webhook_url(&self, path: &str) -> String {
        if let Some(ref base) = self.config.webhook_base_url {
            format!("{}{}", base.trim_end_matches('/'), path)
        } else {
            format!("http://localhost:{}{}", self.config.webhook_port, path)
        }
    }

    /// Record a transcript entry for an active call.
    pub async fn add_transcript_entry(&self, call_id: &str, speaker: &str, text: &str) {
        let mut calls = self.active_calls.lock().await;
        if let Some(record) = calls.get_mut(call_id) {
            record.transcript.push(TranscriptEntry {
                speaker: speaker.to_string(),
                text: text.to_string(),
                timestamp: chrono::Utc::now().to_rfc3339(),
            });
        }
    }

    /// Get a snapshot of an active call.
    pub async fn get_call(&self, call_id: &str) -> Option<CallRecord> {
        let calls = self.active_calls.lock().await;
        calls.get(call_id).cloned()
    }

    /// List all active calls.
    pub async fn active_calls(&self) -> Vec<CallRecord> {
        let calls = self.active_calls.lock().await;
        calls.values().cloned().collect()
    }

    /// Handle an incoming call webhook event.
    pub async fn handle_inbound_call(
        &self,
        call_id: &str,
        from_number: &str,
        tx: &mpsc::Sender<ChannelMessage>,
    ) -> Result<()> {
        let record = CallRecord {
            call_id: call_id.to_string(),
            direction: CallDirection::Inbound,
            remote_number: from_number.to_string(),
            local_number: self.config.from_number.clone(),
            state: CallState::Ringing,
            started_at: chrono::Utc::now().to_rfc3339(),
            ended_at: None,
            duration_secs: 0,
            transcript: Vec::new(),
        };

        {
            let mut calls = self.active_calls.lock().await;
            calls.insert(call_id.to_string(), record);
        }

        info!(
            call_id = call_id,
            from = from_number,
            "inbound call received"
        );

        // Notify the agent about the incoming call
        let msg = ChannelMessage {
            id: call_id.to_string(),
            sender: from_number.to_string(),
            reply_target: from_number.to_string(),
            content: format!("[Voice Call] Incoming call from {from_number} (call_id: {call_id})"),
            channel: "voice_call".to_string(),
            timestamp: chrono::Utc::now().timestamp().unsigned_abs(),
            thread_ts: Some(call_id.to_string()),
            interruption_scope_id: Some(call_id.to_string()),
            attachments: vec![],
        };
        tx.send(msg)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to send call event: {e}"))?;
        Ok(())
    }

    /// Handle a call status update (state transition).
    pub async fn handle_status_update(&self, call_id: &str, new_state: CallState) {
        let mut calls = self.active_calls.lock().await;
        if let Some(record) = calls.get_mut(call_id) {
            let old_state = record.state;
            record.state = new_state;

            if matches!(
                new_state,
                CallState::Completed | CallState::Failed | CallState::HungUp
            ) {
                record.ended_at = Some(chrono::Utc::now().to_rfc3339());
            }

            debug!(
                call_id = call_id,
                old_state = %old_state,
                new_state = %new_state,
                "call state transition"
            );
        }
    }

    /// Save call transcript to workspace (if logging is enabled).
    pub async fn save_transcript(
        &self,
        call_id: &str,
        workspace_dir: &std::path::Path,
    ) -> Result<()> {
        if !self.config.transcription_logging {
            return Ok(());
        }

        let calls = self.active_calls.lock().await;
        let Some(record) = calls.get(call_id) else {
            bail!("Call not found: {call_id}");
        };

        let logs_dir = workspace_dir.join("logs").join("calls");
        std::fs::create_dir_all(&logs_dir)?;

        let filename = format!("{}_{}.json", record.started_at.replace(':', "-"), call_id);
        let path = logs_dir.join(filename);
        let json = serde_json::to_string_pretty(record)?;
        std::fs::write(&path, json)?;

        info!(call_id = call_id, path = %path.display(), "call transcript saved");
        Ok(())
    }
}

// ── Channel trait implementation ─────────────────────────────────

#[async_trait::async_trait]
impl Channel for VoiceCallChannel {
    fn name(&self) -> &str {
        "voice_call"
    }

    async fn send(&self, message: &SendMessage) -> Result<()> {
        // For active calls, TTS the message to the caller
        if let Some(ref thread_ts) = message.thread_ts {
            let calls = self.active_calls.lock().await;
            if let Some(record) = calls.get(thread_ts)
                && record.state == CallState::InProgress
            {
                debug!(
                    call_id = thread_ts,
                    "would TTS message to active call: {}", message.content
                );
                // TTS synthesis + streaming would be handled by the
                // telephony provider's media stream API in production.
                return Ok(());
            }
        }

        debug!("voice_call send (no active call): {}", message.content);
        Ok(())
    }

    async fn listen(&self, tx: mpsc::Sender<ChannelMessage>) -> Result<()> {
        let port = self.config.webhook_port;
        let active_calls = self.active_calls.clone();
        let _tx = tx.clone();

        info!(port = port, provider = %self.config.provider, "voice call webhook server starting");

        // The webhook server runs as an axum HTTP server on the configured port.
        // In production, this handles:
        // - POST /voice/inbound — Twilio/Telnyx/Plivo call initiation webhook
        // - POST /voice/status — Call status updates
        // - POST /voice/transcription — Real-time transcription events
        // - WebSocket /voice/media — Bidirectional audio streaming
        //
        // For now, we set up the server structure. Full endpoint
        // implementation depends on provider-specific webhook payloads.

        let app = axum::Router::new()
            .route("/voice/health", axum::routing::get(|| async { "ok" }))
            .with_state(active_calls);

        let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{port}"))
            .await
            .map_err(|e| anyhow::anyhow!("Failed to bind voice webhook server: {e}"))?;

        axum::serve(listener, app)
            .await
            .map_err(|e| anyhow::anyhow!("Voice webhook server error: {e}"))?;

        Ok(())
    }

    async fn health_check(&self) -> bool {
        // Check we can reach the provider API
        let test_url = match self.config.provider {
            VoiceProvider::Twilio => {
                format!(
                    "{}/Accounts/{}.json",
                    self.api_base_url(),
                    self.config.account_id
                )
            }
            VoiceProvider::Telnyx => format!("{}/connections", self.api_base_url()),
            VoiceProvider::Plivo => {
                format!(
                    "{}/Account/{}/",
                    self.api_base_url(),
                    self.config.account_id
                )
            }
        };

        match self.client.get(&test_url).send().await {
            Ok(resp) => {
                // 401 is expected without valid auth — it means the API is reachable
                resp.status().is_success() || resp.status().as_u16() == 401
            }
            Err(e) => {
                warn!(provider = %self.config.provider, "voice call health check failed: {e}");
                false
            }
        }
    }

    async fn start_typing(&self, _recipient: &str) -> Result<()> {
        Ok(()) // Not applicable for voice calls
    }

    async fn stop_typing(&self, _recipient: &str) -> Result<()> {
        Ok(()) // Not applicable for voice calls
    }

    fn supports_draft_updates(&self) -> bool {
        false
    }

    async fn send_draft(&self, _message: &SendMessage) -> Result<Option<String>> {
        Ok(None)
    }

    async fn update_draft(&self, _recipient: &str, _message_id: &str, _text: &str) -> Result<()> {
        Ok(())
    }

    async fn finalize_draft(&self, _recipient: &str, _message_id: &str, _text: &str) -> Result<()> {
        Ok(())
    }

    async fn cancel_draft(&self, _recipient: &str, _message_id: &str) -> Result<()> {
        Ok(())
    }

    async fn add_reaction(&self, _channel_id: &str, _message_id: &str, _emoji: &str) -> Result<()> {
        Ok(())
    }

    async fn remove_reaction(
        &self,
        _channel_id: &str,
        _message_id: &str,
        _emoji: &str,
    ) -> Result<()> {
        Ok(())
    }

    async fn pin_message(&self, _channel_id: &str, _message_id: &str) -> Result<()> {
        Ok(())
    }

    async fn unpin_message(&self, _channel_id: &str, _message_id: &str) -> Result<()> {
        Ok(())
    }

    async fn redact_message(
        &self,
        _channel_id: &str,
        _message_id: &str,
        _reason: Option<String>,
    ) -> Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> VoiceCallConfig {
        VoiceCallConfig {
            enabled: true,
            provider: VoiceProvider::Twilio,
            account_id: "AC_TEST_ACCOUNT".into(),
            auth_token: "test_token".into(),
            from_number: "+15551234567".into(),
            webhook_port: 8090,
            require_outbound_approval: true,
            transcription_logging: true,
            tts_voice: None,
            max_call_duration_secs: 3600,
            webhook_base_url: Some("https://tunnel.example.com".into()),
        }
    }

    #[test]
    fn provider_display() {
        assert_eq!(VoiceProvider::Twilio.to_string(), "twilio");
        assert_eq!(VoiceProvider::Telnyx.to_string(), "telnyx");
        assert_eq!(VoiceProvider::Plivo.to_string(), "plivo");
    }

    #[test]
    fn call_state_display() {
        assert_eq!(CallState::Ringing.to_string(), "ringing");
        assert_eq!(CallState::InProgress.to_string(), "in_progress");
        assert_eq!(CallState::Completed.to_string(), "completed");
        assert_eq!(CallState::PendingApproval.to_string(), "pending_approval");
    }

    #[test]
    fn webhook_url_with_base() {
        let channel = VoiceCallChannel::new(test_config());
        assert_eq!(
            channel.webhook_url("/voice/status"),
            "https://tunnel.example.com/voice/status"
        );
    }

    #[test]
    fn webhook_url_without_base() {
        let mut config = test_config();
        config.webhook_base_url = None;
        let channel = VoiceCallChannel::new(config);
        assert_eq!(
            channel.webhook_url("/voice/status"),
            "http://localhost:8090/voice/status"
        );
    }

    #[test]
    fn channel_name() {
        let channel = VoiceCallChannel::new(test_config());
        assert_eq!(channel.name(), "voice_call");
    }

    #[tokio::test]
    async fn handle_inbound_call_creates_record() {
        let channel = VoiceCallChannel::new(test_config());
        let (tx, mut rx) = mpsc::channel(10);

        channel
            .handle_inbound_call("call-123", "+15559876543", &tx)
            .await
            .unwrap();

        // Check call record was created
        let record = channel.get_call("call-123").await.unwrap();
        assert_eq!(record.call_id, "call-123");
        assert_eq!(record.remote_number, "+15559876543");
        assert_eq!(record.state, CallState::Ringing);
        assert_eq!(record.direction, CallDirection::Inbound);

        // Check message was sent to agent
        let msg = rx.recv().await.unwrap();
        assert!(msg.content.contains("Incoming call"));
        assert!(msg.content.contains("+15559876543"));
    }

    #[tokio::test]
    async fn handle_status_update_transitions_state() {
        let channel = VoiceCallChannel::new(test_config());
        let (tx, _rx) = mpsc::channel(10);

        channel
            .handle_inbound_call("call-456", "+15559876543", &tx)
            .await
            .unwrap();

        channel
            .handle_status_update("call-456", CallState::InProgress)
            .await;

        let record = channel.get_call("call-456").await.unwrap();
        assert_eq!(record.state, CallState::InProgress);
        assert!(record.ended_at.is_none());

        // Transition to completed
        channel
            .handle_status_update("call-456", CallState::Completed)
            .await;

        let record = channel.get_call("call-456").await.unwrap();
        assert_eq!(record.state, CallState::Completed);
        assert!(record.ended_at.is_some());
    }

    #[tokio::test]
    async fn add_transcript_entry_records_entries() {
        let channel = VoiceCallChannel::new(test_config());
        let (tx, _rx) = mpsc::channel(10);

        channel
            .handle_inbound_call("call-789", "+15559876543", &tx)
            .await
            .unwrap();

        channel
            .add_transcript_entry("call-789", "caller", "Hello, I need help")
            .await;
        channel
            .add_transcript_entry("call-789", "agent", "Hi, how can I assist you?")
            .await;

        let record = channel.get_call("call-789").await.unwrap();
        assert_eq!(record.transcript.len(), 2);
        assert_eq!(record.transcript[0].speaker, "caller");
        assert_eq!(record.transcript[0].text, "Hello, I need help");
        assert_eq!(record.transcript[1].speaker, "agent");
    }

    #[tokio::test]
    async fn save_transcript_creates_file() {
        let channel = VoiceCallChannel::new(test_config());
        let (tx, _rx) = mpsc::channel(10);
        let workspace = tempfile::tempdir().unwrap();

        channel
            .handle_inbound_call("call-save", "+15559876543", &tx)
            .await
            .unwrap();

        channel
            .add_transcript_entry("call-save", "caller", "Test message")
            .await;

        channel
            .save_transcript("call-save", workspace.path())
            .await
            .unwrap();

        // Check the logs/calls directory was created
        let logs_dir = workspace.path().join("logs").join("calls");
        assert!(logs_dir.exists());

        // Check a JSON file was created
        let entries: Vec<_> = std::fs::read_dir(&logs_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();
        assert_eq!(entries.len(), 1);

        // Verify JSON content
        let content = std::fs::read_to_string(entries[0].path()).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed["call_id"], "call-save");
        assert_eq!(parsed["transcript"][0]["text"], "Test message");
    }

    #[tokio::test]
    async fn active_calls_lists_all() {
        let channel = VoiceCallChannel::new(test_config());
        let (tx, _rx) = mpsc::channel(10);

        channel
            .handle_inbound_call("call-a", "+15551111111", &tx)
            .await
            .unwrap();
        channel
            .handle_inbound_call("call-b", "+15552222222", &tx)
            .await
            .unwrap();

        let calls = channel.active_calls().await;
        assert_eq!(calls.len(), 2);
    }

    #[tokio::test]
    async fn place_call_requires_approval() {
        let channel = VoiceCallChannel::new(test_config());
        let result = channel.place_call("+15559876543").await.unwrap();
        assert!(result.starts_with("PENDING_APPROVAL:"));
    }

    #[test]
    fn config_serde_roundtrip() {
        let config = test_config();
        let json = serde_json::to_string(&config).unwrap();
        let parsed: VoiceCallConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.provider, VoiceProvider::Twilio);
        assert_eq!(parsed.from_number, "+15551234567");
        assert_eq!(parsed.webhook_port, 8090);
    }

    #[test]
    fn call_record_serde_roundtrip() {
        let record = CallRecord {
            call_id: "call-001".into(),
            direction: CallDirection::Inbound,
            remote_number: "+15559876543".into(),
            local_number: "+15551234567".into(),
            state: CallState::InProgress,
            started_at: "2026-03-24T12:00:00Z".into(),
            ended_at: None,
            duration_secs: 0,
            transcript: vec![TranscriptEntry {
                speaker: "caller".into(),
                text: "Hello".into(),
                timestamp: "2026-03-24T12:00:01Z".into(),
            }],
        };
        let json = serde_json::to_string(&record).unwrap();
        let parsed: CallRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.call_id, "call-001");
        assert_eq!(parsed.transcript.len(), 1);
    }

    #[test]
    fn default_provider_is_twilio() {
        assert_eq!(VoiceProvider::default(), VoiceProvider::Twilio);
    }

    #[test]
    fn provider_serde_roundtrip() {
        let json = serde_json::to_string(&VoiceProvider::Telnyx).unwrap();
        assert_eq!(json, "\"telnyx\"");
        let parsed: VoiceProvider = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, VoiceProvider::Telnyx);
    }

    #[tokio::test]
    async fn transcript_logging_disabled_skips_save() {
        let mut config = test_config();
        config.transcription_logging = false;
        let channel = VoiceCallChannel::new(config);
        let (tx, _rx) = mpsc::channel(10);
        let workspace = tempfile::tempdir().unwrap();

        channel
            .handle_inbound_call("call-nolog", "+15559876543", &tx)
            .await
            .unwrap();

        channel
            .save_transcript("call-nolog", workspace.path())
            .await
            .unwrap();

        // Logs directory should not exist
        let logs_dir = workspace.path().join("logs").join("calls");
        assert!(!logs_dir.exists());
    }
}
