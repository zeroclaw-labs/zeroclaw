#![allow(clippy::doc_markdown)]
#![allow(clippy::doc_link_with_quotes)]
#![allow(clippy::cast_sign_loss)]

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};
use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::{Mutex, RwLock, mpsc};
use tokio::time::sleep;
use tracing::{debug, error, info, warn};

use super::traits::{Channel, ChannelMessage, SendMessage};

// ── Constants ───────────────────────────────────────────────────────────────

/// Refresh access token when within this many seconds of expiration (1 day).
const TOKEN_REFRESH_BUFFER_SECS: u64 = 86_400;

/// Default token lifetime when the server doesn't specify `expires_in` (30 days).
const DEFAULT_TOKEN_EXPIRES_IN_SECS: u64 = 30 * 24 * 60 * 60;

/// SHA-1 hashcash difficulty for account creation (20 leading zero bits).
const HASHCASH_DIFFICULTY: u32 = 20;

/// Maximum number of emails to fetch per poll cycle.
const EMAIL_POLL_LIMIT: u64 = 20;

// ── Config ──────────────────────────────────────────────────────────────────

/// InboxAPI channel configuration — agent-native email via InboxAPI MCP.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct InboxApiConfig {
    /// MCP endpoint URL.
    #[serde(default = "default_endpoint")]
    pub endpoint: String,
    /// Agent account name (used for hashcash resource and display name).
    pub account_name: String,
    /// Path to credentials JSON file (default: ~/.local/inboxapi/credentials.json).
    #[serde(default = "default_credentials_path")]
    pub credentials_path: String,
    /// Allowed sender addresses/domains (empty = deny all, ["*"] = allow all).
    #[serde(default)]
    pub allowed_senders: Vec<String>,
    /// Inbox polling interval in seconds (default: 30).
    #[serde(default = "default_poll_interval_secs")]
    pub poll_interval_secs: u64,
    /// Content format for retrieved emails: "text" (default), "html", or "all".
    #[serde(default = "default_content_format")]
    pub content_format: String,
}

impl crate::config::traits::ChannelConfig for InboxApiConfig {
    fn name() -> &'static str {
        "InboxAPI"
    }
    fn desc() -> &'static str {
        "Agent-native email via InboxAPI MCP"
    }
}

fn default_endpoint() -> String {
    "https://mcp.inboxapi.ai/mcp".into()
}

fn default_credentials_path() -> String {
    "~/.local/inboxapi/credentials.json".into()
}

fn default_poll_interval_secs() -> u64 {
    30
}

fn default_content_format() -> String {
    "text".into()
}

impl Default for InboxApiConfig {
    fn default() -> Self {
        Self {
            endpoint: default_endpoint(),
            account_name: String::new(),
            credentials_path: default_credentials_path(),
            allowed_senders: Vec::new(),
            poll_interval_secs: default_poll_interval_secs(),
            content_format: default_content_format(),
        }
    }
}

// ── Credentials ─────────────────────────────────────────────────────────────

/// Persisted InboxAPI credentials.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Credentials {
    account_name: String,
    access_token: String,
    refresh_token: String,
    /// Seconds since UNIX epoch when the access token expires.
    expires_at: u64,
    /// The provisioned email address (informational).
    #[serde(default)]
    email_address: String,
}

/// Runtime token state shared across send/listen/health operations.
struct TokenState {
    access_token: String,
    refresh_token: String,
    expires_at: u64,
    email_address: String,
}

// ── Channel ─────────────────────────────────────────────────────────────────

/// InboxAPI channel — agent-native email over HTTP/MCP.
pub struct InboxApiChannel {
    config: InboxApiConfig,
    client: reqwest::Client,
    seen_messages: Arc<Mutex<HashSet<String>>>,
    tokens: Arc<RwLock<Option<TokenState>>>,
    /// RFC3339/ISO-8601 timestamp cursor for incremental polling via `search_emails`.
    last_poll_time: Arc<Mutex<Option<String>>>,
}

impl InboxApiChannel {
    pub fn new(config: InboxApiConfig) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .unwrap_or_else(|e| {
                warn!("Failed to build reqwest client with timeout: {e}; using default client");
                reqwest::Client::new()
            });
        Self {
            config,
            client,
            seen_messages: Arc::new(Mutex::new(HashSet::new())),
            tokens: Arc::new(RwLock::new(None)),
            last_poll_time: Arc::new(Mutex::new(None)),
        }
    }

    /// Check if a sender email is in the allowlist.
    pub fn is_sender_allowed(&self, email: &str) -> bool {
        if self.config.allowed_senders.is_empty() {
            return false; // Empty = deny all (use ["*"] to allow all)
        }
        if self.config.allowed_senders.iter().any(|a| a == "*") {
            return true; // Wildcard = allow all
        }
        let email_lower = email.to_lowercase();
        self.config.allowed_senders.iter().any(|allowed| {
            if allowed.starts_with('@') {
                // Domain match with @ prefix: "@example.com"
                email_lower.ends_with(&allowed.to_lowercase())
            } else if allowed.contains('@') {
                // Full email address match
                allowed.eq_ignore_ascii_case(email)
            } else {
                // Domain match without @ prefix: "example.com"
                email_lower.ends_with(&format!("@{}", allowed.to_lowercase()))
            }
        })
    }

    // ── MCP transport ───────────────────────────────────────────────────

    /// Send a JSON-RPC 2.0 tool call to the MCP endpoint.
    async fn call_mcp_tool(
        &self,
        tool_name: &str,
        args: serde_json::Value,
        auth_token: Option<&str>,
    ) -> Result<serde_json::Value> {
        let payload = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": tool_name,
                "arguments": args
            }
        });

        let mut req = self
            .client
            .post(&self.config.endpoint)
            // Prefer a single JSON response; streaming SSE responses can hang if the server
            // keeps the connection open.
            .header("Accept", "application/json")
            .json(&payload);
        if let Some(token) = auth_token {
            req = req.bearer_auth(token);
        }

        let resp = req.send().await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!("MCP request failed ({}): {}", status, body));
        }

        let body_text = resp.text().await?;
        let body: serde_json::Value = Self::parse_sse_response(&body_text)?;

        // Check for JSON-RPC error
        if let Some(err) = body.get("error") {
            return Err(anyhow!("MCP tool error: {}", err));
        }

        // Extract result — tool responses nest content inside result.content[]
        Ok(body
            .get("result")
            .cloned()
            .unwrap_or(serde_json::Value::Null))
    }

    /// Parse a response body that may be SSE-formatted or plain JSON.
    ///
    /// rmcp's `StreamableHttpService` (stateless mode) responds with
    /// `text/event-stream` containing `data: {json}\n\n` events.
    /// This extracts the JSON-RPC payload from either format.
    fn parse_sse_response(body: &str) -> Result<serde_json::Value> {
        // Try SSE: scan for `data:` lines
        for line in body.lines() {
            if let Some(data) = line.strip_prefix("data:") {
                let data = data.trim();
                if !data.is_empty() {
                    return Ok(serde_json::from_str(data)?);
                }
            }
        }
        // Fallback: treat as plain JSON
        Ok(serde_json::from_str(body)?)
    }

    /// Parse an email timestamp from InboxAPI fields into a canonical UTC time.
    fn parse_email_received_at(email: &serde_json::Value) -> Option<chrono::DateTime<chrono::Utc>> {
        fn parse_any(s: &str) -> Option<chrono::DateTime<chrono::Utc>> {
            chrono::DateTime::parse_from_rfc3339(s)
                .or_else(|_| chrono::DateTime::parse_from_rfc2822(s))
                .ok()
                .map(|dt| dt.with_timezone(&chrono::Utc))
        }

        email
            .get("received_at")
            .and_then(|v| v.as_str())
            .and_then(parse_any)
            .or_else(|| {
                email
                    .get("date")
                    .and_then(|v| v.as_str())
                    .and_then(parse_any)
            })
    }

    /// Format a UTC timestamp for use as an incremental polling cursor.
    fn format_cursor(ts: chrono::DateTime<chrono::Utc>) -> String {
        ts.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
    }

    /// Extract text content from an MCP tool result envelope.
    fn extract_text_content(result: &serde_json::Value) -> Option<String> {
        result
            .get("content")
            .and_then(|c| c.as_array())
            .and_then(|arr| arr.first())
            .and_then(|item| item.get("text"))
            .and_then(|t| t.as_str())
            .map(|s| s.to_string())
    }

    // ── Hashcash ────────────────────────────────────────────────────────

    /// Generate a hashcash stamp with the given resource and difficulty bits.
    fn generate_hashcash(resource: &str, bits: u32) -> String {
        let now = chrono::Utc::now().format("%y%m%d").to_string();
        let rand_bytes: [u8; 8] = rand::random();
        let rand_b64 =
            base64::Engine::encode(&base64::engine::general_purpose::STANDARD, rand_bytes);

        let mut counter: u64 = 0;
        loop {
            let stamp = format!("1:{}:{}:{}::{}:{}", bits, now, resource, rand_b64, counter);
            if Self::check_hashcash_bits(&stamp, bits) {
                return stamp;
            }
            counter += 1;
        }
    }

    /// Check whether a hashcash stamp has the required leading zero bits in its SHA-1 hash.
    fn check_hashcash_bits(stamp: &str, bits: u32) -> bool {
        let hash = Sha1::digest(stamp.as_bytes());
        let hash_bytes = hash.as_slice();

        let full_bytes = (bits / 8) as usize;
        let remaining_bits = bits % 8;

        // Check full zero bytes
        for byte in &hash_bytes[..full_bytes] {
            if *byte != 0 {
                return false;
            }
        }

        // Check remaining bits in the next byte
        if remaining_bits > 0 && full_bytes < hash_bytes.len() {
            let mask = 0xFF_u8 << (8 - remaining_bits);
            if hash_bytes[full_bytes] & mask != 0 {
                return false;
            }
        }

        true
    }

    // ── Credential management ───────────────────────────────────────────

    /// Resolve the credentials file path (expand ~).
    fn credentials_path(&self) -> std::path::PathBuf {
        let expanded = shellexpand::tilde(&self.config.credentials_path).into_owned();
        std::path::PathBuf::from(expanded)
    }

    /// Load credentials from disk.
    fn load_credentials(&self) -> Option<Credentials> {
        let path = self.credentials_path();
        let data = std::fs::read_to_string(&path).ok()?;
        let creds: Credentials = serde_json::from_str(&data).ok()?;
        if creds.account_name == self.config.account_name
            || creds
                .account_name
                .starts_with(&format!("{}-", self.config.account_name))
        {
            Some(creds)
        } else {
            None
        }
    }

    /// Persist credentials to disk with 0600 permissions.
    fn save_credentials(&self, creds: &Credentials) -> Result<()> {
        use std::io::Write;

        let path = self.credentials_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(creds)?;
        let parent = path
            .parent()
            .ok_or_else(|| anyhow!("credentials path has no parent: {}", path.display()))?;
        let file_name = path
            .file_name()
            .ok_or_else(|| anyhow!("credentials path has no file name: {}", path.display()))?;
        let temp_path = parent.join(format!(
            ".{}.tmp.{}.{}",
            file_name.to_string_lossy(),
            std::process::id(),
            SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
        ));

        #[cfg(unix)]
        let mut temp_file = {
            use std::os::unix::fs::OpenOptionsExt;
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&temp_path)?
        };

        #[cfg(not(unix))]
        let mut temp_file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)?;

        temp_file.write_all(json.as_bytes())?;
        temp_file.sync_all()?;
        drop(temp_file);

        #[cfg(windows)]
        {
            let _ = std::fs::remove_file(&path);
        }

        std::fs::rename(&temp_path, &path).inspect_err(|_| {
            let _ = std::fs::remove_file(&temp_path);
        })?;

        debug!("Credentials saved to {}", path.display());
        Ok(())
    }

    /// Populate runtime token state from loaded credentials.
    fn apply_credentials(&self, creds: &Credentials) -> TokenState {
        TokenState {
            access_token: creds.access_token.clone(),
            refresh_token: creds.refresh_token.clone(),
            expires_at: creds.expires_at,
            email_address: creds.email_address.clone(),
        }
    }

    // ── Token lifecycle ─────────────────────────────────────────────────

    /// Get a valid access token, refreshing if needed.
    async fn get_access_token(&self) -> Result<String> {
        // Check if we have a token and it's not about to expire
        {
            let state = self.tokens.read().await;
            if let Some(ref ts) = *state {
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                // Refresh if within 1 day of expiration
                if ts.expires_at > now + TOKEN_REFRESH_BUFFER_SECS {
                    return Ok(ts.access_token.clone());
                }
            }
        }

        // Need to refresh
        self.refresh_tokens().await
    }

    /// Refresh the access token using the refresh token.
    async fn refresh_tokens(&self) -> Result<String> {
        let refresh_token = {
            let state = self.tokens.read().await;
            match *state {
                Some(ref ts) => ts.refresh_token.clone(),
                None => return Err(anyhow!("No refresh token available")),
            }
        };

        info!("Refreshing InboxAPI access token");
        let result = self
            .call_mcp_tool(
                "auth_refresh",
                serde_json::json!({ "refresh_token": refresh_token }),
                None,
            )
            .await?;

        let content_text = Self::extract_text_content(&result)
            .ok_or_else(|| anyhow!("Empty auth_refresh response"))?;
        let parsed: serde_json::Value = serde_json::from_str(&content_text)?;

        let access_token = parsed["access_token"]
            .as_str()
            .ok_or_else(|| anyhow!("Missing access_token in refresh response"))?
            .to_string();
        let new_refresh = parsed["refresh_token"]
            .as_str()
            .unwrap_or(&refresh_token)
            .to_string();
        let expires_at = if let Some(ts) = parsed["access_token_expires_at"].as_u64() {
            ts
        } else {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let expires_in = parsed["expires_in"]
                .as_u64()
                .unwrap_or(DEFAULT_TOKEN_EXPIRES_IN_SECS);
            now + expires_in
        };

        // Update runtime state
        let (email_address, persisted_account_name) = {
            let mut state = self.tokens.write().await;
            let email = state
                .as_ref()
                .map(|s| s.email_address.clone())
                .unwrap_or_default();
            *state = Some(TokenState {
                access_token: access_token.clone(),
                refresh_token: new_refresh.clone(),
                expires_at,
                email_address: email.clone(),
            });
            // Use account name from existing credentials file (may be suffixed)
            let acct = self
                .load_credentials()
                .map(|c| c.account_name)
                .unwrap_or_else(|| self.config.account_name.clone());
            (email, acct)
        };

        // Persist updated credentials
        if let Err(e) = self.save_credentials(&Credentials {
            account_name: persisted_account_name,
            access_token: access_token.clone(),
            refresh_token: new_refresh,
            expires_at,
            email_address,
        }) {
            warn!("Failed to persist refreshed InboxAPI credentials: {e}");
        }

        Ok(access_token)
    }

    // ── Auto-onboarding ─────────────────────────────────────────────────

    /// Ensure we have valid tokens, creating an account if needed.
    async fn ensure_authenticated(&self) -> Result<()> {
        if self.config.account_name.trim().is_empty() {
            return Err(anyhow!(
                "InboxAPI account_name must be set to a non-empty, non-whitespace value"
            ));
        }

        // 1. Try loading existing credentials
        if let Some(creds) = self.load_credentials() {
            let ts = self.apply_credentials(&creds);
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();

            if ts.expires_at > now + TOKEN_REFRESH_BUFFER_SECS {
                // Token still valid — verify with introspect
                let result = self
                    .call_mcp_tool(
                        "auth_introspect",
                        serde_json::json!({ "token": ts.access_token }),
                        None,
                    )
                    .await;

                if result.is_ok() {
                    info!(
                        "InboxAPI authenticated as {} ({})",
                        self.config.account_name, ts.email_address
                    );
                    *self.tokens.write().await = Some(ts);
                    return Ok(());
                }
                debug!("Stored token failed introspect, will try refresh");
            }

            // Try refresh
            *self.tokens.write().await = Some(ts);
            if let Ok(token) = self.refresh_tokens().await {
                info!("InboxAPI token refreshed for {}", self.config.account_name);
                // Verify the refreshed token
                let _ = self
                    .call_mcp_tool(
                        "auth_introspect",
                        serde_json::json!({ "token": token }),
                        None,
                    )
                    .await?;
                return Ok(());
            }
            warn!("Token refresh failed, will create new account");
        }

        // 2. Create new account
        self.create_account().await
    }

    /// Attempt to create an account with a specific name, returning the
    /// account_create result on success.
    async fn try_account_create(&self, name: &str) -> Result<serde_json::Value> {
        let resource = name.to_string();
        let stamp = tokio::task::spawn_blocking(move || {
            Self::generate_hashcash(&resource, HASHCASH_DIFFICULTY)
        })
        .await
        .map_err(|e| anyhow!("Hashcash generation failed: {}", e))?;

        debug!("Hashcash generated for '{}': {}", name, stamp);

        self.call_mcp_tool(
            "account_create",
            serde_json::json!({ "name": name, "hashcash": stamp }),
            None,
        )
        .await
    }

    /// Create a new InboxAPI account via hashcash proof-of-work.
    /// Retries with a random suffix if the configured name is already taken.
    async fn create_account(&self) -> Result<()> {
        let base_name = &self.config.account_name;
        if base_name.trim().is_empty() {
            return Err(anyhow!(
                "InboxAPI account_name must be set to a non-empty, non-whitespace value before creating an account"
            ));
        }
        info!("Creating InboxAPI account for '{}'...", base_name);

        // Try with configured name first; on collision, retry with suffix
        let (result, effective_name) = match self.try_account_create(base_name).await {
            Ok(r) => (r, base_name.clone()),
            Err(e) if e.to_string().contains("already exists") => {
                let mut last_err = e;
                let mut created = None;
                for _ in 0..3 {
                    let suffix: u16 = rand::random();
                    let suffixed = format!("{}-{:04x}", base_name, suffix);
                    warn!("Account '{}' taken, trying '{}'", base_name, suffixed);
                    match self.try_account_create(&suffixed).await {
                        Ok(r) => {
                            created = Some((r, suffixed));
                            break;
                        }
                        Err(e) => last_err = e,
                    }
                }
                created.ok_or(last_err)?
            }
            Err(e) => return Err(e),
        };

        let content_text = Self::extract_text_content(&result)
            .ok_or_else(|| anyhow!("Empty account_create response"))?;
        let parsed: serde_json::Value = serde_json::from_str(&content_text)?;

        let bootstrap_token = parsed["bootstrap_token"]
            .as_str()
            .ok_or_else(|| anyhow!("Missing bootstrap_token in account_create response"))?;

        // Exchange bootstrap for access + refresh tokens
        let exchange_result = self
            .call_mcp_tool(
                "auth_exchange",
                serde_json::json!({ "bootstrap_token": bootstrap_token }),
                None,
            )
            .await?;

        let exchange_text = Self::extract_text_content(&exchange_result)
            .ok_or_else(|| anyhow!("Empty auth_exchange response"))?;
        let exchange_parsed: serde_json::Value = serde_json::from_str(&exchange_text)?;

        let access_token = exchange_parsed["access_token"]
            .as_str()
            .ok_or_else(|| anyhow!("Missing access_token in auth_exchange response"))?
            .to_string();
        let refresh_token = exchange_parsed["refresh_token"]
            .as_str()
            .ok_or_else(|| anyhow!("Missing refresh_token in auth_exchange response"))?
            .to_string();
        let expires_at = if let Some(ts) = exchange_parsed["access_token_expires_at"].as_u64() {
            ts
        } else {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let expires_in = exchange_parsed["expires_in"]
                .as_u64()
                .unwrap_or(DEFAULT_TOKEN_EXPIRES_IN_SECS);
            now + expires_in
        };

        // Try to get the provisioned email address
        let email_address = exchange_parsed["email"]
            .as_str()
            .or_else(|| parsed["email"].as_str())
            .unwrap_or("")
            .to_string();

        let creds = Credentials {
            account_name: effective_name.clone(),
            access_token: access_token.clone(),
            refresh_token: refresh_token.clone(),
            expires_at,
            email_address: email_address.clone(),
        };

        self.save_credentials(&creds)?;

        *self.tokens.write().await = Some(TokenState {
            access_token,
            refresh_token,
            expires_at,
            email_address: email_address.clone(),
        });

        info!(
            "InboxAPI account created: {} (email: {})",
            effective_name,
            if email_address.is_empty() {
                "pending"
            } else {
                &email_address
            }
        );

        Ok(())
    }

    // ── Polling ─────────────────────────────────────────────────────────

    /// Poll for new emails and dispatch to the channel sender.
    async fn poll_emails(&self, tx: &mpsc::Sender<ChannelMessage>) -> Result<()> {
        let token = self.get_access_token().await?;
        debug!("InboxAPI polling for emails...");

        let mut args = serde_json::json!({
            "content_format": self.config.content_format,
            "limit": EMAIL_POLL_LIMIT,
        });

        // Use timestamp cursor for incremental polling
        let since = self.last_poll_time.lock().await.clone();
        if let Some(ref ts) = since {
            args["since"] = serde_json::json!(ts);
        }

        let result = self
            .call_mcp_tool("search_emails", args, Some(&token))
            .await?;

        let content_text = match Self::extract_text_content(&result) {
            Some(text) => text,
            None => return Ok(()), // No content = no emails
        };

        let emails: serde_json::Value = match serde_json::from_str(&content_text) {
            Ok(json) => json,
            Err(e) => {
                error!("Failed to parse emails from InboxAPI: {e}");
                return Ok(());
            }
        };
        let email_array = if let Some(arr) = emails.get("emails").and_then(|e| e.as_array()) {
            arr
        } else if let Some(arr) = emails.as_array() {
            arr
        } else {
            warn!(
                "InboxAPI search_emails returned unrecognized format: {}",
                emails
                    .as_object()
                    .map(|o| o.keys().cloned().collect::<Vec<_>>().join(", "))
                    .unwrap_or_else(|| "non-object".into())
            );
            return Ok(());
        };
        let returned_meta = emails.get("returned").and_then(|v| v.as_u64());
        debug!(
            "InboxAPI poll: {} email(s) returned{}",
            email_array.len(),
            returned_meta
                .map(|r| format!(" (server reported: {})", r))
                .unwrap_or_default()
        );

        let mut latest_received_at: Option<chrono::DateTime<chrono::Utc>> = None;

        for email in email_array {
            let msg_id = email["id"]
                .as_str()
                .or_else(|| email["message_id"].as_str())
                .unwrap_or("")
                .to_string();

            if msg_id.is_empty() {
                continue;
            }

            // Check if already seen
            {
                let mut seen = self.seen_messages.lock().await;
                if !seen.insert(msg_id.clone()) {
                    continue;
                }
            }

            let sender = email["from"]
                .as_str()
                .or_else(|| email["sender"].as_str())
                .unwrap_or("unknown")
                .to_string();

            // Check allowlist
            if !self.is_sender_allowed(&sender) {
                warn!("Blocked email from {}", sender);
                continue;
            }

            let subject = email["subject"].as_str().unwrap_or("(no subject)");
            let body = email["body"]
                .as_str()
                .or_else(|| email["text_body"].as_str())
                .or_else(|| email["content"].as_str())
                .unwrap_or("");
            let content = format!("Subject: {}\n\n{}", subject, body);

            // Always set thread_ts to the email's own ID so replies use send_reply
            let thread_ts = Some(msg_id.clone());

            let received_at = Self::parse_email_received_at(email);
            let timestamp = received_at
                .map(|dt| dt.timestamp() as u64)
                .unwrap_or_else(|| {
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0)
                });

            // Track latest timestamp for the since cursor (compare by actual time, store canonical RFC3339)
            if let Some(dt) = received_at {
                if latest_received_at.map_or(true, |prev| dt > prev) {
                    latest_received_at = Some(dt);
                }
            }

            let msg = ChannelMessage {
                id: msg_id,
                reply_target: sender.clone(),
                sender,
                content,
                channel: "inboxapi".to_string(),
                timestamp,
                thread_ts,
                interruption_scope_id: None,
                attachments: vec![],
            };

            if tx.send(msg).await.is_err() {
                return Ok(()); // Channel closed
            }
        }

        // Advance the since cursor for next poll
        if let Some(ts) = latest_received_at {
            *self.last_poll_time.lock().await = Some(Self::format_cursor(ts));
        }

        Ok(())
    }
}

// ── Channel trait ───────────────────────────────────────────────────────────

#[async_trait]
impl Channel for InboxApiChannel {
    fn name(&self) -> &str {
        "inboxapi"
    }

    async fn send(&self, message: &SendMessage) -> Result<()> {
        // Ensure in-memory tokens are populated from disk if not already set
        {
            let has_tokens = self.tokens.read().await.is_some();
            if !has_tokens {
                if let Some(creds) = self.load_credentials() {
                    let ts = self.apply_credentials(&creds);
                    *self.tokens.write().await = Some(ts);
                }
            }
        }

        let token = self.get_access_token().await?;

        if let Some(ref reply_to) = message.thread_ts {
            // Reply: use send_reply (subject/to auto-resolved by server)
            let args = serde_json::json!({
                "in_reply_to": reply_to,
                "body": message.content,
            });
            self.call_mcp_tool("send_reply", args, Some(&token)).await?;
        } else {
            // New email: use send_email (subject required)
            let args = serde_json::json!({
                "to": message.recipient,
                "subject": message.subject.as_deref().unwrap_or("(no subject)"),
                "body": message.content,
            });
            self.call_mcp_tool("send_email", args, Some(&token)).await?;
        }

        info!("InboxAPI email sent to {}", message.recipient);
        Ok(())
    }

    async fn listen(&self, tx: mpsc::Sender<ChannelMessage>) -> Result<()> {
        // Auto-onboarding: ensure we have valid credentials
        self.ensure_authenticated().await?;

        let poll_interval = Duration::from_secs(self.config.poll_interval_secs);
        info!(
            "InboxAPI channel listening (poll every {}s)",
            self.config.poll_interval_secs
        );

        loop {
            if let Err(e) = self.poll_emails(&tx).await {
                error!("InboxAPI poll error: {}", e);
            }
            sleep(poll_interval).await;
        }
    }

    async fn health_check(&self) -> bool {
        // Ensure in-memory tokens are populated from disk if not already set
        {
            let has_tokens = self.tokens.read().await.is_some();
            if !has_tokens {
                if let Some(creds) = self.load_credentials() {
                    let ts = self.apply_credentials(&creds);
                    *self.tokens.write().await = Some(ts);
                }
            }
        }

        let token = match self.get_access_token().await {
            Ok(t) => t,
            Err(_) => return false,
        };

        self.call_mcp_tool(
            "auth_introspect",
            serde_json::json!({ "token": token }),
            None,
        )
        .await
        .is_ok()
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Config defaults ─────────────────────────────────────────────────

    #[test]
    fn config_defaults() {
        let config = InboxApiConfig::default();
        assert_eq!(config.endpoint, "https://mcp.inboxapi.ai/mcp");
        assert_eq!(config.account_name, "");
        assert_eq!(
            config.credentials_path,
            "~/.local/inboxapi/credentials.json"
        );
        assert_eq!(config.poll_interval_secs, 30);
        assert_eq!(config.content_format, "text");
        assert!(config.allowed_senders.is_empty());
    }

    #[test]
    fn config_custom() {
        let config = InboxApiConfig {
            endpoint: "https://custom.example.com/mcp".into(),
            account_name: "test-agent".into(),
            credentials_path: "/tmp/creds.json".into(),
            allowed_senders: vec!["user@example.com".into()],
            poll_interval_secs: 60,
            content_format: "html".into(),
        };
        assert_eq!(config.endpoint, "https://custom.example.com/mcp");
        assert_eq!(config.account_name, "test-agent");
        assert_eq!(config.poll_interval_secs, 60);
        assert_eq!(config.content_format, "html");
    }

    #[test]
    fn config_clone() {
        let config = InboxApiConfig {
            account_name: "my-agent".into(),
            allowed_senders: vec!["*".into()],
            ..Default::default()
        };
        let cloned = config.clone();
        assert_eq!(cloned.account_name, config.account_name);
        assert_eq!(cloned.allowed_senders, config.allowed_senders);
        assert_eq!(cloned.endpoint, config.endpoint);
    }

    #[test]
    fn config_serde_roundtrip() {
        let config = InboxApiConfig {
            account_name: "test-bot".into(),
            poll_interval_secs: 45,
            ..Default::default()
        };
        let json = serde_json::to_string(&config).unwrap();
        let parsed: InboxApiConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.account_name, "test-bot");
        assert_eq!(parsed.poll_interval_secs, 45);
    }

    #[test]
    fn config_deserialize_with_defaults() {
        let json = r#"{"account_name": "minimal"}"#;
        let config: InboxApiConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.account_name, "minimal");
        assert_eq!(config.endpoint, default_endpoint());
        assert_eq!(config.credentials_path, default_credentials_path());
        assert_eq!(config.poll_interval_secs, 30);
    }

    // ── Channel basics ──────────────────────────────────────────────────

    #[test]
    fn channel_name() {
        let channel = InboxApiChannel::new(InboxApiConfig::default());
        assert_eq!(channel.name(), "inboxapi");
    }

    #[tokio::test]
    async fn seen_messages_starts_empty() {
        let channel = InboxApiChannel::new(InboxApiConfig::default());
        let seen = channel.seen_messages.lock().await;
        assert!(seen.is_empty());
    }

    #[tokio::test]
    async fn seen_messages_tracks_unique_ids() {
        let channel = InboxApiChannel::new(InboxApiConfig::default());
        let mut seen = channel.seen_messages.lock().await;
        assert!(seen.insert("msg-1".into()));
        assert!(!seen.insert("msg-1".into()));
        assert!(seen.insert("msg-2".into()));
        assert_eq!(seen.len(), 2);
    }

    // ── Sender allowlist ────────────────────────────────────────────────

    #[test]
    fn is_sender_allowed_empty_list_denies_all() {
        let channel = InboxApiChannel::new(InboxApiConfig::default());
        assert!(!channel.is_sender_allowed("anyone@example.com"));
        assert!(!channel.is_sender_allowed("other@test.org"));
    }

    #[test]
    fn is_sender_allowed_wildcard_allows_all() {
        let channel = InboxApiChannel::new(InboxApiConfig {
            allowed_senders: vec!["*".into()],
            ..Default::default()
        });
        assert!(channel.is_sender_allowed("anyone@example.com"));
        assert!(channel.is_sender_allowed("other@test.org"));
    }

    #[test]
    fn is_sender_allowed_exact_match() {
        let channel = InboxApiChannel::new(InboxApiConfig {
            allowed_senders: vec!["alice@example.com".into()],
            ..Default::default()
        });
        assert!(channel.is_sender_allowed("alice@example.com"));
        assert!(channel.is_sender_allowed("Alice@Example.COM"));
        assert!(!channel.is_sender_allowed("bob@example.com"));
    }

    #[test]
    fn is_sender_allowed_domain_with_at() {
        let channel = InboxApiChannel::new(InboxApiConfig {
            allowed_senders: vec!["@example.com".into()],
            ..Default::default()
        });
        assert!(channel.is_sender_allowed("alice@example.com"));
        assert!(channel.is_sender_allowed("bob@example.com"));
        assert!(!channel.is_sender_allowed("alice@other.com"));
    }

    #[test]
    fn is_sender_allowed_domain_without_at() {
        let channel = InboxApiChannel::new(InboxApiConfig {
            allowed_senders: vec!["example.com".into()],
            ..Default::default()
        });
        assert!(channel.is_sender_allowed("alice@example.com"));
        assert!(!channel.is_sender_allowed("alice@notexample.com"));
    }

    // ── Hashcash ────────────────────────────────────────────────────────

    #[test]
    fn hashcash_stamp_format() {
        let stamp = InboxApiChannel::generate_hashcash("test-resource", 1);
        let parts: Vec<&str> = stamp.split(':').collect();
        assert_eq!(parts[0], "1", "version must be 1");
        assert_eq!(parts[1], "1", "bits must match requested");
        assert_eq!(parts[3], "test-resource", "resource must match");
    }

    #[test]
    fn hashcash_has_required_leading_zero_bits() {
        // Use low difficulty for fast test
        let stamp = InboxApiChannel::generate_hashcash("test", 8);
        assert!(
            InboxApiChannel::check_hashcash_bits(&stamp, 8),
            "Generated stamp must pass its own verification"
        );
    }

    #[test]
    fn check_hashcash_bits_rejects_insufficient() {
        // A stamp with 1-bit difficulty should not reliably pass 20-bit check
        let stamp = InboxApiChannel::generate_hashcash("test", 1);
        // Very unlikely to accidentally have 20 leading zero bits
        // (probability 2^-19 ≈ 0.0002%), but check the mechanism works
        let hash = Sha1::digest(stamp.as_bytes());
        let has_20_bits = hash[0] == 0 && hash[1] == 0 && (hash[2] & 0xF0) == 0;
        assert_eq!(
            InboxApiChannel::check_hashcash_bits(&stamp, 20),
            has_20_bits
        );
    }

    #[test]
    fn check_hashcash_bits_zero() {
        // 0 bits = everything passes
        assert!(InboxApiChannel::check_hashcash_bits("anything", 0));
    }

    // ── Message mapping ─────────────────────────────────────────────────

    #[test]
    fn send_message_to_mcp_args() {
        let msg = SendMessage {
            content: "Hello world".into(),
            recipient: "user@example.com".into(),
            subject: Some("Test Subject".into()),
            thread_ts: Some("msg-123".into()),
            cancellation_token: None,
            attachments: vec![],
        };

        let mut args = serde_json::json!({
            "to": msg.recipient,
            "body": msg.content,
        });
        if let Some(ref subject) = msg.subject {
            args["subject"] = serde_json::json!(subject);
        }
        if let Some(ref thread) = msg.thread_ts {
            args["in_reply_to"] = serde_json::json!(thread);
        }

        assert_eq!(args["to"], "user@example.com");
        assert_eq!(args["body"], "Hello world");
        assert_eq!(args["subject"], "Test Subject");
        assert_eq!(args["in_reply_to"], "msg-123");
    }

    #[test]
    fn email_json_to_channel_message() {
        let email = serde_json::json!({
            "id": "msg-abc",
            "from": "sender@example.com",
            "subject": "Test",
            "body": "Hello",
            "in_reply_to": "msg-parent"
        });

        let msg_id = email["id"].as_str().unwrap().to_string();
        let sender = email["from"].as_str().unwrap().to_string();
        let subject = email["subject"].as_str().unwrap_or("(no subject)");
        let body = email["body"].as_str().unwrap_or("");
        let content = format!("Subject: {}\n\n{}", subject, body);
        // thread_ts is always the email's own ID so replies use send_reply
        let thread_ts = Some(msg_id.clone());

        let msg = ChannelMessage {
            id: msg_id.clone(),
            reply_target: sender.clone(),
            sender: sender.clone(),
            content: content.clone(),
            channel: "inboxapi".to_string(),
            timestamp: 0,
            thread_ts: thread_ts.clone(),
            interruption_scope_id: None,
            attachments: vec![],
        };

        assert_eq!(msg.id, "msg-abc");
        assert_eq!(msg.sender, "sender@example.com");
        assert_eq!(msg.content, "Subject: Test\n\nHello");
        assert_eq!(msg.channel, "inboxapi");
        assert_eq!(msg.thread_ts.as_deref(), Some("msg-abc"));
    }

    // ── Credential serialization ────────────────────────────────────────

    #[test]
    fn credentials_serde_roundtrip() {
        let creds = Credentials {
            account_name: "test-bot".into(),
            access_token: "acc-tok-123".into(),
            refresh_token: "ref-tok-456".into(),
            expires_at: 1_700_000_000,
            email_address: "test-bot@abc.inboxapi.ai".into(),
        };
        let json = serde_json::to_string(&creds).unwrap();
        let parsed: Credentials = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.account_name, "test-bot");
        assert_eq!(parsed.access_token, "acc-tok-123");
        assert_eq!(parsed.refresh_token, "ref-tok-456");
        assert_eq!(parsed.expires_at, 1_700_000_000);
        assert_eq!(parsed.email_address, "test-bot@abc.inboxapi.ai");
    }

    #[test]
    fn credentials_email_address_defaults_empty() {
        let json = r#"{
            "account_name": "test",
            "access_token": "a",
            "refresh_token": "r",
            "expires_at": 0
        }"#;
        let creds: Credentials = serde_json::from_str(json).unwrap();
        assert_eq!(creds.email_address, "");
    }

    // ── JSON-RPC request format ─────────────────────────────────────────

    #[test]
    fn jsonrpc_request_format() {
        let payload = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "send_email",
                "arguments": {
                    "to": "user@example.com",
                    "body": "test"
                }
            }
        });

        assert_eq!(payload["jsonrpc"], "2.0");
        assert_eq!(payload["method"], "tools/call");
        assert_eq!(payload["params"]["name"], "send_email");
        assert_eq!(payload["params"]["arguments"]["to"], "user@example.com");
    }

    #[test]
    fn extract_text_content_from_mcp_response() {
        let result = serde_json::json!({
            "content": [
                {
                    "type": "text",
                    "text": "{\"access_token\": \"tok-123\"}"
                }
            ]
        });
        let text = InboxApiChannel::extract_text_content(&result);
        assert_eq!(text, Some("{\"access_token\": \"tok-123\"}".to_string()));
    }

    #[test]
    fn extract_text_content_empty_result() {
        let result = serde_json::json!({});
        assert_eq!(InboxApiChannel::extract_text_content(&result), None);
    }

    #[test]
    fn extract_text_content_no_text_field() {
        let result = serde_json::json!({
            "content": [{"type": "image", "data": "..."}]
        });
        assert_eq!(InboxApiChannel::extract_text_content(&result), None);
    }

    // ── SSE response parsing ───────────────────────────────────────────

    #[test]
    fn parse_sse_response_single_data_line() {
        let body = "data: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"content\":[]}}\n\n";
        let parsed = InboxApiChannel::parse_sse_response(body).unwrap();
        assert_eq!(parsed["jsonrpc"], "2.0");
        assert_eq!(parsed["id"], 1);
        assert!(parsed["result"]["content"].is_array());
    }

    #[test]
    fn parse_sse_response_plain_json() {
        let body = r#"{"jsonrpc":"2.0","id":1,"result":{"content":[]}}"#;
        let parsed = InboxApiChannel::parse_sse_response(body).unwrap();
        assert_eq!(parsed["jsonrpc"], "2.0");
        assert_eq!(parsed["id"], 1);
    }

    #[test]
    fn parse_sse_response_with_event_fields() {
        let body =
            "event: message\nid: 42\ndata: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":null}\n\n";
        let parsed = InboxApiChannel::parse_sse_response(body).unwrap();
        assert_eq!(parsed["jsonrpc"], "2.0");
        assert!(parsed["result"].is_null());
    }

    #[test]
    fn parse_sse_response_empty_data_line() {
        // Empty data: line followed by a real one
        let body = "data: \ndata: {\"ok\":true}\n\n";
        let parsed = InboxApiChannel::parse_sse_response(body).unwrap();
        assert_eq!(parsed["ok"], true);
    }

    #[test]
    fn parse_sse_response_invalid_json() {
        let body = "data: not-json\n\n";
        assert!(InboxApiChannel::parse_sse_response(body).is_err());
    }

    // ── Email response parsing ──────────────────────────────────────────

    #[test]
    fn poll_parses_wrapped_email_response() {
        let response = serde_json::json!({
            "emails": [
                {"id": "msg-1", "from": "alice@example.com", "subject": "Hi", "body": "Hello"}
            ],
            "returned": 1,
            "offset": 0,
            "limit": 20
        });
        let arr = response
            .get("emails")
            .and_then(|e| e.as_array())
            .or_else(|| response.as_array());
        assert!(arr.is_some());
        assert_eq!(arr.unwrap().len(), 1);
        assert_eq!(arr.unwrap()[0]["id"], "msg-1");
    }

    #[test]
    fn poll_parses_raw_array_response() {
        let response = serde_json::json!([
            {"id": "msg-1", "from": "alice@example.com", "subject": "Hi", "body": "Hello"}
        ]);
        let arr = response
            .get("emails")
            .and_then(|e| e.as_array())
            .or_else(|| response.as_array());
        assert!(arr.is_some());
        assert_eq!(arr.unwrap().len(), 1);
    }

    #[test]
    fn poll_handles_empty_wrapped_response() {
        let response = serde_json::json!({
            "emails": [],
            "returned": 0,
            "offset": 0,
            "limit": 20
        });
        let arr = response
            .get("emails")
            .and_then(|e| e.as_array())
            .or_else(|| response.as_array());
        assert!(arr.is_some());
        assert_eq!(arr.unwrap().len(), 0);
    }

    // ── Timestamp cursor ───────────────────────────────────────────────

    #[tokio::test]
    async fn last_poll_time_starts_none() {
        let channel = InboxApiChannel::new(InboxApiConfig::default());
        let cursor = channel.last_poll_time.lock().await;
        assert!(cursor.is_none());
    }

    #[tokio::test]
    async fn last_poll_time_advances_after_set() {
        let channel = InboxApiChannel::new(InboxApiConfig::default());
        *channel.last_poll_time.lock().await = Some("2026-03-14T00:00:00Z".to_string());
        let cursor = channel.last_poll_time.lock().await.clone();
        assert_eq!(cursor.as_deref(), Some("2026-03-14T00:00:00Z"));
    }

    // ── thread_ts always set to msg_id ─────────────────────────────────

    #[test]
    fn thread_ts_set_to_email_message_id() {
        // When an email has no in_reply_to, thread_ts should still be Some(msg_id)
        let email = serde_json::json!({
            "id": "msg-new",
            "from": "sender@example.com",
            "subject": "Fresh email",
            "body": "No parent thread"
        });

        let msg_id = email["id"].as_str().unwrap().to_string();
        let thread_ts = Some(msg_id.clone());

        assert_eq!(thread_ts.as_deref(), Some("msg-new"));
    }

    #[test]
    fn thread_ts_ignores_in_reply_to_uses_own_id() {
        // Even when in_reply_to exists, thread_ts is the email's own ID
        let email = serde_json::json!({
            "id": "msg-reply",
            "from": "sender@example.com",
            "subject": "Re: Original",
            "body": "Reply body",
            "in_reply_to": "msg-original"
        });

        let msg_id = email["id"].as_str().unwrap().to_string();
        let thread_ts = Some(msg_id.clone());

        assert_eq!(thread_ts.as_deref(), Some("msg-reply"));
    }

    // ── Timestamp cursor tracking logic ────────────────────────────────

    #[test]
    fn latest_received_at_tracks_max_timestamp() {
        let emails = vec![
            serde_json::json!({"received_at": "2026-03-14T10:00:00Z"}),
            serde_json::json!({"received_at": "2026-03-14T12:00:00Z"}),
            serde_json::json!({"received_at": "2026-03-14T11:00:00Z"}),
        ];

        let mut latest_received_at: Option<chrono::DateTime<chrono::Utc>> = None;
        for email in &emails {
            if let Some(dt) = InboxApiChannel::parse_email_received_at(email) {
                if latest_received_at.map_or(true, |prev| dt > prev) {
                    latest_received_at = Some(dt);
                }
            }
        }

        assert_eq!(
            latest_received_at
                .map(InboxApiChannel::format_cursor)
                .as_deref(),
            Some("2026-03-14T12:00:00Z")
        );
    }

    #[test]
    fn latest_received_at_falls_back_to_received_at() {
        let email = serde_json::json!({"date": "Sat, 14 Mar 2026 09:00:00 +0000"});
        let ts =
            InboxApiChannel::parse_email_received_at(&email).map(InboxApiChannel::format_cursor);
        assert_eq!(ts.as_deref(), Some("2026-03-14T09:00:00Z"));
    }
}
