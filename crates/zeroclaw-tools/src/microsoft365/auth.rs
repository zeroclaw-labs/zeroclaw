use anyhow::Context;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use tokio::sync::Mutex;

/// Cached OAuth2 token state persisted to disk between runs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedTokenState {
    pub access_token: String,
    pub refresh_token: Option<String>,
    /// Unix timestamp (seconds) when the access token expires.
    pub expires_at: i64,
}

impl CachedTokenState {
    /// Returns `true` when the token is expired or will expire within 60 seconds.
    pub fn is_expired(&self) -> bool {
        let now = chrono::Utc::now().timestamp();
        self.expires_at <= now + 60
    }
}

/// Thread-safe token cache with disk persistence.
pub struct TokenCache {
    inner: RwLock<Option<CachedTokenState>>,
    /// Serialises the slow acquire/refresh path so only one caller performs the
    /// network round-trip while others wait and then read the updated cache.
    acquire_lock: Mutex<()>,
    config: super::types::Microsoft365ResolvedConfig,
    cache_path: PathBuf,
}

impl TokenCache {
    pub fn new(
        config: super::types::Microsoft365ResolvedConfig,
        zeroclaw_dir: &std::path::Path,
    ) -> anyhow::Result<Self> {
        if config.token_cache_encrypted {
            anyhow::bail!(
                "microsoft365: token_cache_encrypted is enabled but encryption is not yet \
                 implemented; refusing to store tokens in plaintext. Set token_cache_encrypted \
                 to false or wait for encryption support."
            );
        }

        // Scope cache file to (tenant_id, client_id, auth_flow) so config
        // changes never reuse tokens from a different account/flow.
        let mut hasher = DefaultHasher::new();
        config.tenant_id.hash(&mut hasher);
        config.client_id.hash(&mut hasher);
        config.auth_flow.hash(&mut hasher);
        let fingerprint = format!("{:016x}", hasher.finish());

        let cache_path = zeroclaw_dir.join(format!("ms365_token_cache_{fingerprint}.json"));
        let cached = Self::load_from_disk(&cache_path);
        Ok(Self {
            inner: RwLock::new(cached),
            acquire_lock: Mutex::new(()),
            config,
            cache_path,
        })
    }

    /// Get a valid access token, refreshing or re-authenticating as needed.
    pub async fn get_token(&self, client: &reqwest::Client) -> anyhow::Result<String> {
        // Fast path: cached and not expired.
        {
            let guard = self.inner.read();
            if let Some(ref state) = *guard
                && !state.is_expired()
            {
                return Ok(state.access_token.clone());
            }
        }

        // Slow path: serialise through a mutex so only one caller performs the
        // network round-trip while concurrent callers wait and re-check.
        let _lock = self.acquire_lock.lock().await;

        // Re-check after acquiring the lock — another caller may have refreshed
        // while we were waiting.
        {
            let guard = self.inner.read();
            if let Some(ref state) = *guard
                && !state.is_expired()
            {
                return Ok(state.access_token.clone());
            }
        }

        let new_state = self.acquire_token(client).await?;
        let token = new_state.access_token.clone();
        self.persist_to_disk(&new_state);
        *self.inner.write() = Some(new_state);
        Ok(token)
    }

    async fn acquire_token(&self, client: &reqwest::Client) -> anyhow::Result<CachedTokenState> {
        // Try refresh first if we have a refresh token and the flow supports it.
        // Client credentials flow does not issue refresh tokens, so skip the
        // attempt entirely to avoid a wasted round-trip.
        if self.config.auth_flow.as_str() != "client_credentials" {
            // Clone the token out so the RwLock guard is dropped before the await.
            let refresh_token_copy = {
                let guard = self.inner.read();
                guard.as_ref().and_then(|state| state.refresh_token.clone())
            };
            if let Some(refresh_tok) = refresh_token_copy {
                match self.refresh_token(client, &refresh_tok).await {
                    Ok(new_state) => return Ok(new_state),
                    Err(e) => {
                        tracing::debug!("ms365: refresh token failed, re-authenticating: {e}");
                    }
                }
            }
        }

        match self.config.auth_flow.as_str() {
            "client_credentials" => self.client_credentials_flow(client).await,
            "device_code" => self.device_code_flow(client).await,
            other => anyhow::bail!("Unsupported auth flow: {other}"),
        }
    }

    async fn client_credentials_flow(
        &self,
        client: &reqwest::Client,
    ) -> anyhow::Result<CachedTokenState> {
        let client_secret = self
            .config
            .client_secret
            .as_deref()
            .context("client_credentials flow requires client_secret")?;

        let token_url = format!(
            "https://login.microsoftonline.com/{}/oauth2/v2.0/token",
            self.config.tenant_id
        );

        let scope = self.config.scopes.join(" ");

        let resp = client
            .post(&token_url)
            .form(&[
                ("grant_type", "client_credentials"),
                ("client_id", &self.config.client_id),
                ("client_secret", client_secret),
                ("scope", &scope),
            ])
            .send()
            .await
            .context("ms365: failed to request client_credentials token")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            tracing::debug!("ms365: client_credentials raw OAuth error: {body}");
            anyhow::bail!("ms365: client_credentials token request failed ({status})");
        }

        let token_resp: TokenResponse = resp
            .json()
            .await
            .context("ms365: failed to parse token response")?;

        Ok(CachedTokenState {
            access_token: token_resp.access_token,
            refresh_token: token_resp.refresh_token,
            expires_at: chrono::Utc::now().timestamp() + token_resp.expires_in,
        })
    }

    async fn device_code_flow(&self, client: &reqwest::Client) -> anyhow::Result<CachedTokenState> {
        let device_code_url = format!(
            "https://login.microsoftonline.com/{}/oauth2/v2.0/devicecode",
            self.config.tenant_id
        );
        let scope = self.config.scopes.join(" ");

        let resp = client
            .post(&device_code_url)
            .form(&[
                ("client_id", self.config.client_id.as_str()),
                ("scope", &scope),
            ])
            .send()
            .await
            .context("ms365: failed to request device code")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            tracing::debug!("ms365: device_code initiation raw error: {body}");
            anyhow::bail!("ms365: device code request failed ({status})");
        }

        let device_resp: DeviceCodeResponse = resp
            .json()
            .await
            .context("ms365: failed to parse device code response")?;

        // Log only a generic prompt; the full device_resp.message may contain
        // sensitive verification URIs or codes that should not appear in logs.
        tracing::info!(
            "ms365: device code auth required — follow the instructions shown to the user"
        );
        // Print the user-facing message to stderr so the operator can act on it
        // without it being captured in structured log sinks.
        eprintln!("ms365: {}", device_resp.message);

        let token_url = format!(
            "https://login.microsoftonline.com/{}/oauth2/v2.0/token",
            self.config.tenant_id
        );

        let interval = device_resp.interval.max(5);
        let max_polls = u32::try_from(
            (device_resp.expires_in / i64::try_from(interval).unwrap_or(i64::MAX)).max(1),
        )
        .unwrap_or(u32::MAX);

        for _ in 0..max_polls {
            tokio::time::sleep(std::time::Duration::from_secs(interval)).await;

            let poll_resp = client
                .post(&token_url)
                .form(&[
                    ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                    ("client_id", self.config.client_id.as_str()),
                    ("device_code", &device_resp.device_code),
                ])
                .send()
                .await
                .context("ms365: failed to poll device code token")?;

            if poll_resp.status().is_success() {
                let token_resp: TokenResponse = poll_resp
                    .json()
                    .await
                    .context("ms365: failed to parse token response")?;
                return Ok(CachedTokenState {
                    access_token: token_resp.access_token,
                    refresh_token: token_resp.refresh_token,
                    expires_at: chrono::Utc::now().timestamp() + token_resp.expires_in,
                });
            }

            let body = poll_resp.text().await.unwrap_or_default();
            if body.contains("authorization_pending") {
                continue;
            }
            if body.contains("slow_down") {
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                continue;
            }
            tracing::debug!("ms365: device code polling raw error: {body}");
            anyhow::bail!("ms365: device code polling failed");
        }

        anyhow::bail!("ms365: device code flow timed out waiting for user authorization")
    }

    async fn refresh_token(
        &self,
        client: &reqwest::Client,
        refresh_token: &str,
    ) -> anyhow::Result<CachedTokenState> {
        let token_url = format!(
            "https://login.microsoftonline.com/{}/oauth2/v2.0/token",
            self.config.tenant_id
        );

        let mut params = vec![
            ("grant_type", "refresh_token"),
            ("client_id", self.config.client_id.as_str()),
            ("refresh_token", refresh_token),
        ];

        let secret_ref;
        if let Some(ref secret) = self.config.client_secret {
            secret_ref = secret.as_str();
            params.push(("client_secret", secret_ref));
        }

        let resp = client
            .post(&token_url)
            .form(&params)
            .send()
            .await
            .context("ms365: failed to refresh token")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            tracing::debug!("ms365: token refresh raw error: {body}");
            anyhow::bail!("ms365: token refresh failed ({status})");
        }

        let token_resp: TokenResponse = resp
            .json()
            .await
            .context("ms365: failed to parse refresh token response")?;

        Ok(CachedTokenState {
            access_token: token_resp.access_token,
            refresh_token: token_resp
                .refresh_token
                .or_else(|| Some(refresh_token.to_string())),
            expires_at: chrono::Utc::now().timestamp() + token_resp.expires_in,
        })
    }

    fn load_from_disk(path: &std::path::Path) -> Option<CachedTokenState> {
        let data = std::fs::read_to_string(path).ok()?;
        serde_json::from_str(&data).ok()
    }

    fn persist_to_disk(&self, state: &CachedTokenState) {
        if let Ok(json) = serde_json::to_string_pretty(state)
            && let Err(e) = std::fs::write(&self.cache_path, json)
        {
            tracing::warn!("ms365: failed to persist token cache: {e}");
        }
    }
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default = "default_expires_in")]
    expires_in: i64,
}

fn default_expires_in() -> i64 {
    3600
}

#[derive(Deserialize)]
struct DeviceCodeResponse {
    device_code: String,
    message: String,
    #[serde(default = "default_device_interval")]
    interval: u64,
    #[serde(default = "default_device_expires_in")]
    expires_in: i64,
}

fn default_device_interval() -> u64 {
    5
}

fn default_device_expires_in() -> i64 {
    900
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_is_expired_when_past_deadline() {
        let state = CachedTokenState {
            access_token: "test".into(),
            refresh_token: None,
            expires_at: chrono::Utc::now().timestamp() - 10,
        };
        assert!(state.is_expired());
    }

    #[test]
    fn token_is_expired_within_buffer() {
        let state = CachedTokenState {
            access_token: "test".into(),
            refresh_token: None,
            expires_at: chrono::Utc::now().timestamp() + 30,
        };
        assert!(state.is_expired());
    }

    #[test]
    fn token_is_valid_when_far_from_expiry() {
        let state = CachedTokenState {
            access_token: "test".into(),
            refresh_token: None,
            expires_at: chrono::Utc::now().timestamp() + 3600,
        };
        assert!(!state.is_expired());
    }

    #[test]
    fn load_from_disk_returns_none_for_missing_file() {
        let path = std::path::Path::new("/nonexistent/ms365_token_cache.json");
        assert!(TokenCache::load_from_disk(path).is_none());
    }
}
