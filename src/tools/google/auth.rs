use anyhow::Context;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use tokio::sync::Mutex;

const GOOGLE_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";

/// Cached OAuth2 access token state persisted to disk between runs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedTokenState {
    pub access_token: String,
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

/// Thread-safe OAuth2 token cache with disk persistence.
///
/// Uses a refresh token to obtain new access tokens when the cached one
/// expires. The refresh token itself is never rotated by this cache.
pub struct GoogleTokenCache {
    inner: RwLock<Option<CachedTokenState>>,
    /// Serialises the slow acquire/refresh path so only one caller performs
    /// the network round-trip while others wait and then re-read the cache.
    acquire_lock: Mutex<()>,
    client_id: String,
    client_secret: String,
    refresh_token: String,
    cache_path: PathBuf,
}

impl GoogleTokenCache {
    /// Create a new cache.
    ///
    /// `cache_dir` is the directory where the token state JSON will be
    /// persisted (typically `~/.zeroclaw/`). The file name is scoped to
    /// `client_id` so config changes never reuse tokens from a different
    /// OAuth application.
    pub fn new(
        client_id: String,
        client_secret: String,
        refresh_token: String,
        cache_dir: &std::path::Path,
    ) -> Self {
        let mut hasher = DefaultHasher::new();
        client_id.hash(&mut hasher);
        let fingerprint = format!("{:016x}", hasher.finish());
        let cache_path = cache_dir.join(format!("google_token_cache_{fingerprint}.json"));
        let cached = Self::load_from_disk(&cache_path);
        Self {
            inner: RwLock::new(cached),
            acquire_lock: Mutex::new(()),
            client_id,
            client_secret,
            refresh_token,
            cache_path,
        }
    }

    /// Return a valid access token, refreshing via the refresh token if needed.
    pub async fn get_token(&self, http: &reqwest::Client) -> anyhow::Result<String> {
        // Fast path: valid cached token.
        {
            let guard = self.inner.read();
            if let Some(ref state) = *guard {
                if !state.is_expired() {
                    return Ok(state.access_token.clone());
                }
            }
        }

        // Slow path: serialise so only one caller performs the network
        // round-trip while concurrent callers wait then re-check.
        let _lock = self.acquire_lock.lock().await;
        {
            let guard = self.inner.read();
            if let Some(ref state) = *guard {
                if !state.is_expired() {
                    return Ok(state.access_token.clone());
                }
            }
        }

        let new_state = self.refresh_access_token(http).await?;
        let token = new_state.access_token.clone();
        self.persist_to_disk(&new_state);
        *self.inner.write() = Some(new_state);
        Ok(token)
    }

    async fn refresh_access_token(
        &self,
        http: &reqwest::Client,
    ) -> anyhow::Result<CachedTokenState> {
        let resp = http
            .post(GOOGLE_TOKEN_URL)
            .form(&[
                ("grant_type", "refresh_token"),
                ("client_id", self.client_id.as_str()),
                ("client_secret", self.client_secret.as_str()),
                ("refresh_token", self.refresh_token.as_str()),
            ])
            .send()
            .await
            .context("google: failed to send token refresh request")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            tracing::debug!("google: token refresh raw error: {body}");
            anyhow::bail!("google: token refresh failed ({status})");
        }

        let token_resp: TokenResponse = resp
            .json()
            .await
            .context("google: failed to parse token response")?;

        Ok(CachedTokenState {
            access_token: token_resp.access_token,
            expires_at: chrono::Utc::now().timestamp() + token_resp.expires_in,
        })
    }

    fn load_from_disk(path: &std::path::Path) -> Option<CachedTokenState> {
        let data = std::fs::read_to_string(path).ok()?;
        serde_json::from_str(&data).ok()
    }

    fn persist_to_disk(&self, state: &CachedTokenState) {
        if let Ok(json) = serde_json::to_string_pretty(state) {
            if let Err(e) = std::fs::write(&self.cache_path, json) {
                tracing::warn!("google: failed to persist token cache to disk: {e}");
            }
        }
    }
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default = "default_expires_in")]
    expires_in: i64,
}

fn default_expires_in() -> i64 {
    3600
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_is_expired_when_past_deadline() {
        let state = CachedTokenState {
            access_token: "test".into(),
            expires_at: chrono::Utc::now().timestamp() - 10,
        };
        assert!(state.is_expired());
    }

    #[test]
    fn token_is_expired_within_buffer() {
        let state = CachedTokenState {
            access_token: "test".into(),
            expires_at: chrono::Utc::now().timestamp() + 30,
        };
        assert!(state.is_expired());
    }

    #[test]
    fn token_is_valid_when_far_from_expiry() {
        let state = CachedTokenState {
            access_token: "test".into(),
            expires_at: chrono::Utc::now().timestamp() + 3600,
        };
        assert!(!state.is_expired());
    }

    #[test]
    fn load_from_disk_returns_none_for_missing_file() {
        let path = std::path::Path::new("/nonexistent/google_token_cache.json");
        assert!(GoogleTokenCache::load_from_disk(path).is_none());
    }
}
