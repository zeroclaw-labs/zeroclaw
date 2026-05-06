//! Nevis IAM authentication model_provider for ZeroClaw.
//!
//! Integrates with Nevis Security Suite (Adnovum) for OAuth2/OIDC token
//! validation, FIDO2/passkey verification, and session management. Maps Nevis
//! roles to ZeroClaw tool permissions via [`super::iam_policy::IamPolicy`].

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Identity resolved from a validated Nevis token or session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NevisIdentity {
    /// Unique user identifier from Nevis.
    pub user_id: String,
    /// Nevis roles assigned to this user.
    pub roles: Vec<String>,
    /// OAuth2 scopes granted to this session.
    pub scopes: Vec<String>,
    /// Whether the user completed MFA (FIDO2/passkey/OTP) in this session.
    pub mfa_verified: bool,
    /// When this session expires (seconds since UNIX epoch).
    pub session_expiry: u64,
}

/// Token validation strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenValidationMode {
    /// Validate JWT locally using cached JWKS keys.
    Local,
    /// Validate token by calling the Nevis introspection endpoint.
    Remote,
}

impl TokenValidationMode {
    pub fn from_str_config(s: &str) -> Result<Self> {
        match s.to_ascii_lowercase().as_str() {
            "local" => Ok(Self::Local),
            "remote" => Ok(Self::Remote),
            other => bail!("invalid token_validation mode '{other}': expected 'local' or 'remote'"),
        }
    }
}

/// Authentication model_provider backed by a Nevis instance.
///
/// Validates tokens, manages sessions, and resolves identities. The model_provider
/// is designed to be shared across concurrent requests (`Send + Sync`).
pub struct NevisAuthProvider {
    /// Base URL of the Nevis instance (e.g. `https://nevis.example.com`).
    instance_url: String,
    /// Nevis realm to authenticate against.
    realm: String,
    /// OAuth2 client ID registered in Nevis.
    client_id: String,
    /// OAuth2 client secret (decrypted at startup).
    client_secret: Option<String>,
    /// Token validation strategy.
    validation_mode: TokenValidationMode,
    /// JWKS endpoint for local token validation.
    jwks_url: Option<String>,
    /// Whether MFA is required for all authentications.
    require_mfa: bool,
    /// Session timeout duration.
    session_timeout: Duration,
    /// HTTP client for Nevis API calls.
    http_client: reqwest::Client,
}

impl std::fmt::Debug for NevisAuthProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NevisAuthProvider")
            .field("instance_url", &self.instance_url)
            .field("realm", &self.realm)
            .field("client_id", &self.client_id)
            .field(
                "client_secret",
                &self.client_secret.as_ref().map(|_| "[REDACTED]"),
            )
            .field("validation_mode", &self.validation_mode)
            .field("jwks_url", &self.jwks_url)
            .field("require_mfa", &self.require_mfa)
            .field("session_timeout", &self.session_timeout)
            .finish_non_exhaustive()
    }
}

// Safety: All fields are Send + Sync. The doc comment promises concurrent use,
// so enforce it at compile time to prevent regressions.
#[allow(clippy::used_underscore_items)]
const _: () = {
    fn _assert_send_sync<T: Send + Sync>() {}
    fn _assert() {
        _assert_send_sync::<NevisAuthProvider>();
    }
};

impl NevisAuthProvider {
    /// Create a new Nevis auth model_provider from config values.
    ///
    /// `client_secret` should already be decrypted by the config loader.
    pub fn new(
        instance_url: String,
        realm: String,
        client_id: String,
        client_secret: Option<String>,
        token_validation: &str,
        jwks_url: Option<String>,
        require_mfa: bool,
        session_timeout_secs: u64,
    ) -> Result<Self> {
        let validation_mode = TokenValidationMode::from_str_config(token_validation)?;

        if validation_mode == TokenValidationMode::Local && jwks_url.is_none() {
            bail!(
                "Nevis token_validation is 'local' but no jwks_url is configured. \
                 Either set jwks_url or use token_validation = 'remote'."
            );
        }

        let http_client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .context("Failed to create HTTP client for Nevis")?;

        Ok(Self {
            instance_url,
            realm,
            client_id,
            client_secret,
            validation_mode,
            jwks_url,
            require_mfa,
            session_timeout: Duration::from_secs(session_timeout_secs),
            http_client,
        })
    }

    /// Validate a bearer token and resolve the caller's identity.
    ///
    /// Returns `NevisIdentity` on success, or an error if the token is invalid,
    /// expired, or MFA requirements are not met.
    pub async fn validate_token(&self, token: &str) -> Result<NevisIdentity> {
        if token.is_empty() {
            bail!("empty bearer token");
        }

        let identity = match self.validation_mode {
            TokenValidationMode::Local => self.validate_token_local(token).await?,
            TokenValidationMode::Remote => self.validate_token_remote(token).await?,
        };

        if self.require_mfa && !identity.mfa_verified {
            bail!(
                "MFA is required but user '{}' has not completed MFA verification",
                crate::security::redact(&identity.user_id)
            );
        }

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        if identity.session_expiry > 0 && identity.session_expiry < now {
            bail!("Nevis session expired");
        }

        Ok(identity)
    }

    /// Validate token by calling the Nevis introspection endpoint.
    async fn validate_token_remote(&self, token: &str) -> Result<NevisIdentity> {
        let introspect_url = format!(
            "{}/auth/realms/{}/protocol/openid-connect/token/introspect",
            self.instance_url.trim_end_matches('/'),
            self.realm,
        );

        let mut form = vec![("token", token), ("client_id", &self.client_id)];
        // client_secret is optional (public clients don't need it)
        let secret_ref;
        if let Some(ref secret) = self.client_secret {
            secret_ref = secret.as_str();
            form.push(("client_secret", secret_ref));
        }

        let resp = self
            .http_client
            .post(&introspect_url)
            .form(&form)
            .send()
            .await
            .context("Failed to reach Nevis introspection endpoint")?;

        if !resp.status().is_success() {
            bail!(
                "Nevis introspection returned HTTP {}",
                resp.status().as_u16()
            );
        }

        let body: IntrospectionResponse = resp
            .json()
            .await
            .context("Failed to parse Nevis introspection response")?;

        if !body.active {
            bail!("Token is not active (revoked or expired)");
        }

        let user_id = body
            .sub
            .filter(|s| !s.trim().is_empty())
            .context("Token has missing or empty `sub` claim")?;

        let mut roles = body.realm_access.map(|ra| ra.roles).unwrap_or_default();
        roles.sort();
        roles.dedup();

        Ok(NevisIdentity {
            user_id,
            roles,
            scopes: body
                .scope
                .unwrap_or_default()
                .split_whitespace()
                .map(String::from)
                .collect(),
            mfa_verified: body.acr.as_deref() == Some("mfa")
                || body
                    .amr
                    .iter()
                    .flatten()
                    .any(|m| m == "fido2" || m == "passkey" || m == "otp" || m == "webauthn"),
            session_expiry: body.exp.unwrap_or(0),
        })
    }

    /// Validate token locally using JWKS.
    ///
    /// Local JWT/JWKS validation is not yet implemented. Rather than silently
    /// falling back to the remote introspection endpoint (which would hide a
    /// misconfiguration), this returns an explicit error directing the operator
    /// to use `token_validation = "remote"` until local JWKS support is added.
    #[allow(clippy::unused_async)] // Will use async when JWKS validation is implemented
    async fn validate_token_local(&self, token: &str) -> Result<NevisIdentity> {
        // JWT structure check: header.payload.signature
        let parts: Vec<&str> = token.split('.').collect();
        if parts.len() != 3 {
            bail!("Invalid JWT structure: expected 3 dot-separated parts");
        }

        bail!(
            "Local JWKS token validation is not yet implemented. \
             Set token_validation = \"remote\" to use the Nevis introspection endpoint."
        );
    }

    /// Validate a Nevis session token (cookie-based sessions).
    pub async fn validate_session(&self, session_token: &str) -> Result<NevisIdentity> {
        if session_token.is_empty() {
            bail!("empty session token");
        }

        let session_url = format!(
            "{}/auth/realms/{}/protocol/openid-connect/userinfo",
            self.instance_url.trim_end_matches('/'),
            self.realm,
        );

        let resp = self
            .http_client
            .get(&session_url)
            .bearer_auth(session_token)
            .send()
            .await
            .context("Failed to reach Nevis userinfo endpoint")?;

        if !resp.status().is_success() {
            bail!(
                "Nevis session validation returned HTTP {}",
                resp.status().as_u16()
            );
        }

        let body: UserInfoResponse = resp
            .json()
            .await
            .context("Failed to parse Nevis userinfo response")?;

        if body.sub.trim().is_empty() {
            bail!("Userinfo response has missing or empty `sub` claim");
        }

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let mut roles = body.realm_access.map(|ra| ra.roles).unwrap_or_default();
        roles.sort();
        roles.dedup();

        let identity = NevisIdentity {
            user_id: body.sub,
            roles,
            scopes: body
                .scope
                .unwrap_or_default()
                .split_whitespace()
                .map(String::from)
                .collect(),
            mfa_verified: body.acr.as_deref() == Some("mfa")
                || body
                    .amr
                    .iter()
                    .flatten()
                    .any(|m| m == "fido2" || m == "passkey" || m == "otp" || m == "webauthn"),
            session_expiry: now + self.session_timeout.as_secs(),
        };

        if self.require_mfa && !identity.mfa_verified {
            bail!(
                "MFA is required but user '{}' has not completed MFA verification",
                crate::security::redact(&identity.user_id)
            );
        }

        Ok(identity)
    }

    /// Health check against the Nevis instance.
    pub async fn health_check(&self) -> Result<()> {
        let health_url = format!(
            "{}/auth/realms/{}",
            self.instance_url.trim_end_matches('/'),
            self.realm,
        );

        let resp = self
            .http_client
            .get(&health_url)
            .send()
            .await
            .context("Nevis health check failed: cannot reach instance")?;

        if !resp.status().is_success() {
            bail!("Nevis health check failed: HTTP {}", resp.status().as_u16());
        }

        Ok(())
    }

    /// Getter for instance URL (for diagnostics).
    pub fn instance_url(&self) -> &str {
        &self.instance_url
    }

    /// Getter for realm.
    pub fn realm(&self) -> &str {
        &self.realm
    }
}

// ── Wire types for Nevis API responses ─────────────────────────────

#[derive(Debug, Deserialize)]
struct IntrospectionResponse {
    active: bool,
    sub: Option<String>,
    scope: Option<String>,
    exp: Option<u64>,
    #[serde(rename = "realm_access")]
    realm_access: Option<RealmAccess>,
    /// Authentication Context Class Reference
    acr: Option<String>,
    /// Authentication Methods References
    amr: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct RealmAccess {
    #[serde(default)]
    roles: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct UserInfoResponse {
    sub: String,
    #[serde(rename = "realm_access")]
    realm_access: Option<RealmAccess>,
    scope: Option<String>,
    acr: Option<String>,
    /// Authentication Methods References
    amr: Option<Vec<String>>,
}

// ── Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_validation_mode_from_str() {
        assert_eq!(
            TokenValidationMode::from_str_config("local").unwrap(),
            TokenValidationMode::Local
        );
        assert_eq!(
            TokenValidationMode::from_str_config("REMOTE").unwrap(),
            TokenValidationMode::Remote
        );
        assert!(TokenValidationMode::from_str_config("invalid").is_err());
    }

    #[test]
    fn local_mode_requires_jwks_url() {
        let result = NevisAuthProvider::new(
            "https://nevis.example.com".into(),
            "master".into(),
            "zeroclaw-client".into(),
            None,
            "local",
            None, // no JWKS URL
            false,
            3600,
        );
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("jwks_url"));
    }

    #[test]
    fn remote_mode_works_without_jwks_url() {
        let model_provider = NevisAuthProvider::new(
            "https://nevis.example.com".into(),
            "master".into(),
            "zeroclaw-client".into(),
            None,
            "remote",
            None,
            false,
            3600,
        );
        assert!(model_provider.is_ok());
    }

    #[test]
    fn provider_stores_config_correctly() {
        let model_provider = NevisAuthProvider::new(
            "https://nevis.example.com".into(),
            "test-realm".into(),
            "zeroclaw-client".into(),
            Some("test-secret".into()),
            "remote",
            None,
            true,
            7200,
        )
        .unwrap();

        assert_eq!(model_provider.instance_url(), "https://nevis.example.com");
        assert_eq!(model_provider.realm(), "test-realm");
        assert!(model_provider.require_mfa);
        assert_eq!(model_provider.session_timeout, Duration::from_secs(7200));
    }

    #[test]
    fn debug_redacts_client_secret() {
        let model_provider = NevisAuthProvider::new(
            "https://nevis.example.com".into(),
            "test-realm".into(),
            "zeroclaw-client".into(),
            Some("super-secret-value".into()),
            "remote",
            None,
            false,
            3600,
        )
        .unwrap();

        let debug_output = format!("{:?}", model_provider);
        assert!(
            !debug_output.contains("super-secret-value"),
            "Debug output must not contain the raw client_secret"
        );
        assert!(
            debug_output.contains("[REDACTED]"),
            "Debug output must show [REDACTED] for client_secret"
        );
    }

    #[tokio::test]
    async fn validate_token_rejects_empty() {
        let model_provider = NevisAuthProvider::new(
            "https://nevis.example.com".into(),
            "master".into(),
            "zeroclaw-client".into(),
            None,
            "remote",
            None,
            false,
            3600,
        )
        .unwrap();

        let err = model_provider.validate_token("").await.unwrap_err();
        assert!(err.to_string().contains("empty bearer token"));
    }

    #[tokio::test]
    async fn validate_session_rejects_empty() {
        let model_provider = NevisAuthProvider::new(
            "https://nevis.example.com".into(),
            "master".into(),
            "zeroclaw-client".into(),
            None,
            "remote",
            None,
            false,
            3600,
        )
        .unwrap();

        let err = model_provider.validate_session("").await.unwrap_err();
        assert!(err.to_string().contains("empty session token"));
    }

    #[test]
    fn nevis_identity_serde_roundtrip() {
        let identity = NevisIdentity {
            user_id: "zeroclaw_user".into(),
            roles: vec!["admin".into(), "operator".into()],
            scopes: vec!["openid".into(), "profile".into()],
            mfa_verified: true,
            session_expiry: 1_700_000_000,
        };

        let json = serde_json::to_string(&identity).unwrap();
        let parsed: NevisIdentity = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.user_id, "zeroclaw_user");
        assert_eq!(parsed.roles.len(), 2);
        assert!(parsed.mfa_verified);
    }

    #[tokio::test]
    async fn local_validation_rejects_malformed_jwt() {
        let model_provider = NevisAuthProvider::new(
            "https://nevis.example.com".into(),
            "master".into(),
            "zeroclaw-client".into(),
            None,
            "local",
            Some("https://nevis.example.com/.well-known/jwks.json".into()),
            false,
            3600,
        )
        .unwrap();

        let err = model_provider
            .validate_token("not-a-jwt")
            .await
            .unwrap_err();
        assert!(err.to_string().contains("Invalid JWT structure"));
    }

    #[tokio::test]
    async fn local_validation_errors_instead_of_silent_fallback() {
        let model_provider = NevisAuthProvider::new(
            "https://nevis.example.com".into(),
            "master".into(),
            "zeroclaw-client".into(),
            None,
            "local",
            Some("https://nevis.example.com/.well-known/jwks.json".into()),
            false,
            3600,
        )
        .unwrap();

        // A well-formed JWT structure should hit the "not yet implemented" error
        // instead of silently falling back to remote introspection.
        let err = model_provider
            .validate_token("header.payload.signature")
            .await
            .unwrap_err();
        assert!(err.to_string().contains("not yet implemented"));
    }
}
