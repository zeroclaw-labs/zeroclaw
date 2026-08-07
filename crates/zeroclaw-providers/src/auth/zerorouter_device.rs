//! ZeroRouter device-flow login (RFC 8628).
//!
//! Structurally the sibling of `xai_oauth`'s device arm, with the vendor
//! pinning inverted: nothing here names a fixed host. The issuer is the
//! operator's own router — derived from the configured
//! `providers.models.zerorouter` URI (or the family default) by stripping
//! the `/v1` API suffix — discovery comes from the issuer's
//! `/.well-known/openid-configuration`, and every discovered endpoint must
//! stay on the issuer's own origin (scheme + host + port). HTTPS is
//! required except for loopback issuers, so the self-hosted default
//! (`http://localhost:8080`) works while a spoofed discovery document
//! cannot redirect the poll off-box over plain HTTP.
//!
//! The "access token" this flow yields is not an OAuth token: ZeroRouter
//! mints a fresh `zcr_` API key at claim time (the plaintext never rests in
//! the router's database) with no expiry and no refresh arm. It is stored
//! as a Token-kind auth profile, not a `TokenSet`.

use anyhow::{Context, Result};
use reqwest::Client;
use serde::Deserialize;
use std::time::{Duration, Instant};

/// Client id ZeroRouter allowlists by default (`ZEROROUTER_DEVICE_CLIENT_IDS`).
pub const ZEROROUTER_DEVICE_CLIENT_ID: &str = "zeroclaw";
const DEVICE_CODE_GRANT_TYPE: &str = "urn:ietf:params:oauth:grant-type:device_code";
/// RFC 8628 default poll interval when the router does not name one.
const DEFAULT_POLL_INTERVAL_SECS: u64 = 5;

#[derive(Debug, Clone)]
pub struct ZerorouterDeviceStart {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub verification_uri_complete: Option<String>,
    pub expires_in: u64,
    pub interval: u64,
}

#[derive(Debug, Clone)]
pub struct ZerorouterDeviceDiscovery {
    pub device_authorization_endpoint: String,
    pub token_endpoint: String,
}

#[derive(Debug, Deserialize)]
struct DiscoveryResponse {
    device_authorization_endpoint: Option<String>,
    token_endpoint: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DeviceCodeResponse {
    device_code: String,
    user_code: String,
    verification_uri: String,
    #[serde(default)]
    verification_uri_complete: Option<String>,
    expires_in: u64,
    #[serde(default)]
    interval: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
}

#[derive(Debug, Deserialize)]
struct OAuthErrorResponse {
    error: String,
    #[serde(default)]
    error_description: Option<String>,
}

/// The issuer for a configured ZeroRouter provider URI: the `/v1` API base
/// minus the `/v1`. Tolerates a trailing slash and a bare host.
pub fn issuer_from_provider_uri(uri: &str) -> String {
    let trimmed = uri.trim().trim_end_matches('/');
    trimmed
        .strip_suffix("/v1")
        .unwrap_or(trimmed)
        .trim_end_matches('/')
        .to_string()
}

fn is_loopback_host(host: &str) -> bool {
    host == "localhost"
        || host == "127.0.0.1"
        || host == "[::1]"
        || host == "::1"
        || host.starts_with("127.")
}

/// Every endpoint this flow talks to must live on the issuer's own origin:
/// same scheme, host, and port. HTTPS is required unless the issuer itself
/// is loopback (the self-hosted default). This is the parameterized
/// equivalent of the xAI flow's `require_trusted_endpoint` — trust follows
/// the operator's configured issuer instead of a vendor domain.
pub fn require_issuer_origin(issuer: &str, endpoint: &str, label: &str) -> Result<String> {
    let issuer_url = reqwest::Url::parse(issuer)
        .with_context(|| format!("Invalid ZeroRouter issuer: {issuer}"))?;
    let url = reqwest::Url::parse(endpoint)
        .with_context(|| format!("Invalid ZeroRouter {label}: {endpoint}"))?;
    let issuer_host = issuer_url.host_str().unwrap_or_default().to_string();
    if issuer_url.scheme() != "https" && !is_loopback_host(&issuer_host) {
        anyhow::bail!("ZeroRouter issuer must be HTTPS (or loopback for self-hosted): {issuer}");
    }
    if url.scheme() != issuer_url.scheme()
        || url.host_str().unwrap_or_default() != issuer_host
        || url.port_or_known_default() != issuer_url.port_or_known_default()
    {
        anyhow::bail!(
            "ZeroRouter discovery returned {label} off the issuer's origin: {endpoint} (issuer {issuer})"
        );
    }
    Ok(endpoint.to_string())
}

pub async fn fetch_device_discovery(
    client: &Client,
    issuer: &str,
) -> Result<ZerorouterDeviceDiscovery> {
    // The issuer itself passes through the origin check first so a plain-HTTP
    // non-loopback issuer is rejected before any request leaves the machine.
    let discovery_url = require_issuer_origin(
        issuer,
        &format!(
            "{}/.well-known/openid-configuration",
            issuer.trim_end_matches('/')
        ),
        "discovery document",
    )?;
    let response = client
        .get(&discovery_url)
        .header("Accept", "application/json")
        .send()
        .await
        .context("Failed to fetch ZeroRouter OAuth discovery")?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        anyhow::bail!("ZeroRouter OAuth discovery failed ({status}): {body}");
    }
    let parsed: DiscoveryResponse = response
        .json()
        .await
        .context("Failed to parse ZeroRouter OAuth discovery")?;
    let device_authorization_endpoint = require_issuer_origin(
        issuer,
        parsed
            .device_authorization_endpoint
            .as_deref()
            .ok_or_else(|| {
                anyhow::Error::msg("ZeroRouter discovery missing device_authorization_endpoint")
            })?,
        "device authorization endpoint",
    )?;
    let token_endpoint = require_issuer_origin(
        issuer,
        parsed
            .token_endpoint
            .as_deref()
            .ok_or_else(|| anyhow::Error::msg("ZeroRouter discovery missing token_endpoint"))?,
        "token endpoint",
    )?;
    Ok(ZerorouterDeviceDiscovery {
        device_authorization_endpoint,
        token_endpoint,
    })
}

/// Start the flow. `key_name` is ZeroRouter's RFC 8628 extension: the label
/// the minted key will carry in the portal (this machine's hostname).
pub async fn start_device_flow(
    client: &Client,
    issuer: &str,
    device_authorization_endpoint: &str,
    key_name: &str,
) -> Result<ZerorouterDeviceStart> {
    let form = [
        ("client_id", ZEROROUTER_DEVICE_CLIENT_ID),
        ("key_name", key_name),
    ];
    let response = client
        .post(device_authorization_endpoint)
        .form(&form)
        .send()
        .await
        .context("Failed to start ZeroRouter device-code flow")?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        anyhow::bail!("ZeroRouter device-code start failed ({status}): {body}");
    }
    let parsed: DeviceCodeResponse = response
        .json()
        .await
        .context("Failed to parse ZeroRouter device-code response")?;
    Ok(ZerorouterDeviceStart {
        device_code: parsed.device_code,
        user_code: parsed.user_code,
        // The user opens this in a browser; it must not point off-router.
        verification_uri: require_issuer_origin(
            issuer,
            &parsed.verification_uri,
            "verification URI",
        )?,
        verification_uri_complete: parsed
            .verification_uri_complete
            .as_deref()
            .map(|uri| require_issuer_origin(issuer, uri, "complete verification URI"))
            .transpose()?,
        expires_in: parsed.expires_in,
        interval: parsed.interval.unwrap_or(DEFAULT_POLL_INTERVAL_SECS),
    })
}

/// Poll until approval. The returned string IS the minted `zcr_` API key.
pub async fn poll_device_key(
    client: &Client,
    token_endpoint: &str,
    device: &ZerorouterDeviceStart,
) -> Result<String> {
    let deadline = Instant::now() + Duration::from_secs(device.expires_in.max(1));
    let mut interval = Duration::from_secs(device.interval.max(1));
    loop {
        if Instant::now() >= deadline {
            anyhow::bail!("ZeroRouter device authorization expired before approval");
        }
        tokio::time::sleep(interval).await;

        let form = [
            ("grant_type", DEVICE_CODE_GRANT_TYPE),
            ("device_code", device.device_code.as_str()),
            ("client_id", ZEROROUTER_DEVICE_CLIENT_ID),
        ];
        let response = client
            .post(token_endpoint)
            .form(&form)
            .send()
            .await
            .context("Failed to poll ZeroRouter device token endpoint")?;

        if response.status().is_success() {
            let parsed: TokenResponse = response
                .json()
                .await
                .context("Failed to parse ZeroRouter device token response")?;
            return Ok(parsed.access_token);
        }

        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        let Ok(error) = serde_json::from_str::<OAuthErrorResponse>(&body) else {
            anyhow::bail!("ZeroRouter device token poll failed ({status}): {body}");
        };
        match error.error.as_str() {
            "authorization_pending" => {}
            // RFC 8628 §3.5: back off by 5 seconds on slow_down.
            "slow_down" => interval += Duration::from_secs(5),
            "access_denied" => anyhow::bail!("ZeroRouter device authorization was denied"),
            "expired_token" => {
                anyhow::bail!("ZeroRouter device authorization expired before approval")
            }
            other => anyhow::bail!(
                "ZeroRouter device token poll failed ({status}): {other}{}",
                error
                    .error_description
                    .map(|detail| format!(" — {detail}"))
                    .unwrap_or_default()
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issuer_derivation_strips_the_api_suffix() {
        assert_eq!(
            issuer_from_provider_uri("https://router.example.com/v1"),
            "https://router.example.com"
        );
        assert_eq!(
            issuer_from_provider_uri("http://localhost:8080/v1/"),
            "http://localhost:8080"
        );
        // A bare host stays as-is.
        assert_eq!(
            issuer_from_provider_uri("https://router.example.com"),
            "https://router.example.com"
        );
    }

    #[test]
    fn origin_check_pins_endpoints_to_the_issuer() {
        // Same origin: fine.
        require_issuer_origin(
            "https://r.example.com",
            "https://r.example.com/auth/device/token",
            "token endpoint",
        )
        .expect("same origin passes");
        // Different host: rejected — a spoofed discovery document must not
        // be able to send the poll (and the key) elsewhere.
        require_issuer_origin(
            "https://r.example.com",
            "https://evil.example.net/token",
            "token endpoint",
        )
        .expect_err("cross-origin endpoint is rejected");
        // Different port on the same host: rejected.
        require_issuer_origin(
            "http://localhost:8080",
            "http://localhost:9999/token",
            "token endpoint",
        )
        .expect_err("cross-port endpoint is rejected");
    }

    #[test]
    fn plain_http_is_loopback_only() {
        require_issuer_origin(
            "http://localhost:8080",
            "http://localhost:8080/auth/device/code",
            "device authorization endpoint",
        )
        .expect("loopback HTTP is the self-hosted default");
        require_issuer_origin(
            "http://router.example.com",
            "http://router.example.com/auth/device/code",
            "device authorization endpoint",
        )
        .expect_err("non-loopback HTTP issuer is rejected");
    }
}
