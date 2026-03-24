//! Microsoft Teams channel integration.
//!
//! Supports two modes:
//! - **Webhook**: Send-only via incoming webhook URL
//! - **API**: Full bidirectional via Bot Framework token + team/channel IDs
//!
//! Config:
//! ```toml
//! [channels_config.teams]
//! bot_id = "your-bot-id"
//! bot_secret = "your-bot-secret"
//! tenant_id = "your-tenant-id"
//! webhook_url = "https://outlook.office.com/webhook/..."
//! service_url = "https://smba.trafficmanager.net/..."
//! allowed_users = []
//! ```

use super::traits::{Channel, ChannelMessage, SendMessage};
use async_trait::async_trait;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::Mutex;

/// Bot Framework OAuth token endpoint.
const TOKEN_URL: &str = "https://login.microsoftonline.com/botframework.com/oauth2/v2.0/token";

/// Default poll interval for activity feed.
const POLL_INTERVAL: Duration = Duration::from_secs(3);

/// Style instruction for Teams messages.
const TEAMS_STYLE_PREFIX: &str = "\
[context: you are responding over Microsoft Teams. \
Use markdown formatting. Be concise. \
Avoid excessively long messages.]\n";

pub struct TeamsConfig {
    /// Bot (app) ID from Azure AD registration.
    pub bot_id: Option<String>,
    /// Bot secret (client secret) for OAuth.
    pub bot_secret: Option<String>,
    /// Azure AD tenant ID (or "botframework.com" for multi-tenant).
    pub tenant_id: Option<String>,
    /// Incoming webhook URL (send-only mode).
    pub webhook_url: Option<String>,
    /// Bot Framework service URL for replies.
    pub service_url: Option<String>,
    /// Optional conversation/channel ID to listen on.
    pub conversation_id: Option<String>,
    /// Users allowed to interact with the bot.
    pub allowed_users: Vec<String>,
}

pub struct TeamsChannel {
    bot_id: Option<String>,
    bot_secret: Option<String>,
    tenant_id: Option<String>,
    webhook_url: Option<String>,
    service_url: Option<String>,
    conversation_id: Option<String>,
    allowed_users: Vec<String>,
    client: reqwest::Client,
    seen: Arc<Mutex<HashSet<String>>>,
    /// Cached OAuth token + expiry.
    token_cache: Arc<Mutex<Option<CachedToken>>>,
}

struct CachedToken {
    access_token: String,
    expires_at: Instant,
}

impl TeamsChannel {
    pub fn new(cfg: TeamsConfig) -> Self {
        Self {
            bot_id: cfg.bot_id,
            bot_secret: cfg.bot_secret,
            tenant_id: cfg.tenant_id,
            webhook_url: cfg.webhook_url,
            service_url: cfg.service_url,
            conversation_id: cfg.conversation_id,
            allowed_users: cfg.allowed_users,
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .unwrap_or_default(),
            seen: Arc::new(Mutex::new(HashSet::new())),
            token_cache: Arc::new(Mutex::new(None)),
        }
    }

    fn has_api_mode(&self) -> bool {
        self.bot_id.is_some() && self.bot_secret.is_some()
    }

    /// Get a valid OAuth token, refreshing if expired.
    async fn get_token(&self) -> anyhow::Result<String> {
        {
            let cache = self.token_cache.lock().await;
            if let Some(ref cached) = *cache {
                if Instant::now() < cached.expires_at {
                    return Ok(cached.access_token.clone());
                }
            }
        }

        let bot_id = self
            .bot_id
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("Teams: bot_id required for API mode"))?;
        let bot_secret = self
            .bot_secret
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("Teams: bot_secret required for API mode"))?;

        let resp = self
            .client
            .post(TOKEN_URL)
            .form(&[
                ("grant_type", "client_credentials"),
                ("client_id", bot_id),
                ("client_secret", bot_secret),
                ("scope", "https://api.botframework.com/.default"),
            ])
            .send()
            .await?
            .error_for_status()?
            .json::<serde_json::Value>()
            .await?;

        let access_token = resp["access_token"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Teams: missing access_token in OAuth response"))?
            .to_string();

        let expires_in = resp["expires_in"].as_u64().unwrap_or(3600);
        // Refresh 60s before actual expiry to avoid edge-case failures.
        let expires_at = Instant::now() + Duration::from_secs(expires_in.saturating_sub(60));

        let mut cache = self.token_cache.lock().await;
        *cache = Some(CachedToken {
            access_token: access_token.clone(),
            expires_at,
        });

        Ok(access_token)
    }

    /// Send a reply to a conversation via Bot Framework REST API.
    async fn send_activity(
        &self,
        service_url: &str,
        conversation_id: &str,
        text: &str,
    ) -> anyhow::Result<()> {
        let token = self.get_token().await?;
        let url = format!(
            "{}/v3/conversations/{}/activities",
            service_url.trim_end_matches('/'),
            conversation_id
        );

        let body = serde_json::json!({
            "type": "message",
            "text": text,
        });

        self.client
            .post(&url)
            .bearer_auth(&token)
            .json(&body)
            .send()
            .await?
            .error_for_status()?;

        Ok(())
    }

    /// Send via incoming webhook (Adaptive Card with text body).
    async fn send_webhook(&self, webhook_url: &str, text: &str) -> anyhow::Result<()> {
        // Teams webhooks accept Adaptive Card or simple MessageCard payloads.
        let body = serde_json::json!({
            "@type": "MessageCard",
            "@context": "http://schema.org/extensions",
            "text": text,
        });

        self.client
            .post(webhook_url)
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await?
            .error_for_status()?;

        Ok(())
    }
}

#[async_trait]
impl Channel for TeamsChannel {
    fn name(&self) -> &str {
        "teams"
    }

    async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
        // Prefer webhook for send-only mode.
        if let Some(ref webhook_url) = self.webhook_url {
            return self.send_webhook(webhook_url, &message.content).await;
        }

        // API mode: reply via Bot Framework.
        let service_url = self
            .service_url
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("Teams: service_url required for API mode send"))?;

        let conversation_id = if message.recipient.is_empty() {
            self.conversation_id.as_deref().ok_or_else(|| {
                anyhow::anyhow!("Teams: no conversation_id or recipient for API mode send")
            })?
        } else {
            &message.recipient
        };

        self.send_activity(service_url, conversation_id, &message.content)
            .await
    }

    async fn listen(&self, tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        if !self.has_api_mode() {
            tracing::info!("Teams: webhook-only mode, no polling — messages arrive via gateway webhook endpoint");
            loop {
                tokio::time::sleep(Duration::from_secs(60)).await;
            }
        }

        let service_url = self
            .service_url
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("Teams: service_url required for listening"))?;
        let conversation_id = self
            .conversation_id
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("Teams: conversation_id required for listening"))?;

        // Verify token works before entering the poll loop.
        let _ = self.get_token().await?;
        let activities_url = format!(
            "{}/v3/conversations/{}/activities",
            service_url.trim_end_matches('/'),
            conversation_id,
        );

        tracing::info!(
            conversation_id,
            "Teams: polling activities via Bot Framework"
        );

        loop {
            // Refresh token if needed.
            let token = match self.get_token().await {
                Ok(t) => t,
                Err(e) => {
                    tracing::warn!("Teams: token refresh failed: {e}");
                    tokio::time::sleep(POLL_INTERVAL * 3).await;
                    continue;
                }
            };

            match self
                .client
                .get(&activities_url)
                .bearer_auth(&token)
                .send()
                .await
            {
                Ok(resp) => {
                    if let Ok(json) = resp.json::<serde_json::Value>().await {
                        if let Some(activities) = json["activities"].as_array() {
                            for activity in activities {
                                let id = activity["id"].as_str().unwrap_or_default();
                                let activity_type = activity["type"].as_str().unwrap_or_default();

                                if activity_type != "message" {
                                    continue;
                                }

                                let mut seen = self.seen.lock().await;
                                if seen.contains(id) {
                                    continue;
                                }
                                seen.insert(id.to_string());

                                // Cap dedup set.
                                if seen.len() > 10_000 {
                                    seen.clear();
                                }

                                let sender = activity["from"]["name"]
                                    .as_str()
                                    .unwrap_or("unknown")
                                    .to_string();

                                if !self.allowed_users.is_empty()
                                    && !self.allowed_users.iter().any(|u| u == &sender)
                                {
                                    continue;
                                }

                                let text =
                                    activity["text"].as_str().unwrap_or_default().to_string();

                                if text.is_empty() {
                                    continue;
                                }

                                let conv_id = activity["conversation"]["id"]
                                    .as_str()
                                    .unwrap_or_default()
                                    .to_string();

                                let msg = ChannelMessage {
                                    id: format!("teams_{id}"),
                                    sender: sender.clone(),
                                    reply_target: if conv_id.is_empty() {
                                        sender
                                    } else {
                                        conv_id.clone()
                                    },
                                    content: format!("{TEAMS_STYLE_PREFIX}{text}"),
                                    channel: "teams".to_string(),
                                    timestamp: std::time::SystemTime::now()
                                        .duration_since(std::time::UNIX_EPOCH)
                                        .unwrap_or_default()
                                        .as_secs(),
                                    thread_ts: Some(conv_id),
                                    interruption_scope_id: None,
                                    attachments: Vec::new(),
                                };

                                if tx.send(msg).await.is_err() {
                                    tracing::warn!("Teams: channel receiver dropped");
                                    return Ok(());
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!("Teams: poll error: {e}");
                }
            }

            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    async fn health_check(&self) -> bool {
        if let Some(ref webhook_url) = self.webhook_url {
            // Webhook mode — just check the URL is reachable.
            return self
                .client
                .head(webhook_url)
                .send()
                .await
                .map(|r| r.status().is_success() || r.status().is_redirection())
                .unwrap_or(false);
        }
        if self.has_api_mode() {
            return self.get_token().await.is_ok();
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_channel() -> TeamsChannel {
        TeamsChannel::new(TeamsConfig {
            bot_id: Some("test-bot-id".into()),
            bot_secret: Some("test-secret".into()),
            tenant_id: Some("test-tenant".into()),
            webhook_url: Some("https://outlook.office.com/webhook/test".into()),
            service_url: Some("https://smba.trafficmanager.net/test".into()),
            conversation_id: Some("conv-123".into()),
            allowed_users: vec!["alice".into()],
        })
    }

    #[test]
    fn name_is_teams() {
        let ch = test_channel();
        assert_eq!(ch.name(), "teams");
    }

    #[test]
    fn has_api_mode_requires_both() {
        let ch = test_channel();
        assert!(ch.has_api_mode());

        let ch_no_secret = TeamsChannel::new(TeamsConfig {
            bot_id: Some("id".into()),
            bot_secret: None,
            tenant_id: None,
            webhook_url: None,
            service_url: None,
            conversation_id: None,
            allowed_users: vec![],
        });
        assert!(!ch_no_secret.has_api_mode());
    }

    #[test]
    fn webhook_only_no_api() {
        let ch = TeamsChannel::new(TeamsConfig {
            bot_id: None,
            bot_secret: None,
            tenant_id: None,
            webhook_url: Some("https://hook.example.com".into()),
            service_url: None,
            conversation_id: None,
            allowed_users: vec![],
        });
        assert!(!ch.has_api_mode());
        assert!(ch.webhook_url.is_some());
    }

    #[test]
    fn test_style_prefix() {
        assert!(TEAMS_STYLE_PREFIX.contains("Microsoft Teams"));
    }
}
