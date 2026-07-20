use crate::auth::profiles::TokenSet;
use anyhow::{Context, Result};
use chrono::Utc;
use reqwest::Client;
use serde::Deserialize;
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct DeviceCodeStart {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub verification_uri_complete: Option<String>,
    pub expires_in: u64,
    pub interval: u64,
    pub message: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<i64>,
    #[serde(default)]
    token_type: Option<String>,
    #[serde(default)]
    scope: Option<String>,
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
    #[serde(default)]
    message: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OAuthErrorResponse {
    error: String,
    #[serde(default)]
    error_description: Option<String>,
}

/// Refresh an existing access token using the standard `refresh_token` grant.
pub async fn refresh_access_token(
    client: &Client,
    token_url: &str,
    client_id: &str,
    refresh_token: &str,
    scopes: &[String],
) -> Result<TokenSet> {
    let scope_str = scopes.join(" ");
    let form: &[(&str, &str)] = &[
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token),
        ("client_id", client_id),
        ("scope", &scope_str),
    ];

    let response = client
        .post(token_url)
        .form(form)
        .send()
        .await
        .context("Failed to refresh email OAuth2 token")?;

    parse_token_response(response).await
}

/// Start RFC 8628 device-code flow.
pub async fn start_device_code_flow(
    client: &Client,
    device_code_url: &str,
    client_id: &str,
    scopes: &[String],
) -> Result<DeviceCodeStart> {
    let scope_str = scopes.join(" ");
    let form: &[(&str, &str)] = &[("client_id", client_id), ("scope", &scope_str)];

    let response = client
        .post(device_code_url)
        .form(form)
        .send()
        .await
        .context("Failed to start email OAuth2 device-code flow")?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        anyhow::bail!("Email OAuth2 device-code start failed ({status}): {body}");
    }

    let parsed: DeviceCodeResponse = response
        .json()
        .await
        .context("Failed to parse email OAuth2 device-code response")?;

    Ok(DeviceCodeStart {
        device_code: parsed.device_code,
        user_code: parsed.user_code,
        verification_uri: parsed.verification_uri,
        verification_uri_complete: parsed.verification_uri_complete,
        expires_in: parsed.expires_in,
        interval: parsed.interval.unwrap_or(5).max(1),
        message: parsed.message,
    })
}

/// Poll the token endpoint until the user completes authorization or the flow expires.
pub async fn poll_device_code_tokens(
    client: &Client,
    token_url: &str,
    client_id: &str,
    device: &DeviceCodeStart,
) -> Result<TokenSet> {
    let started = Instant::now();
    let mut interval_secs = device.interval.max(1);

    loop {
        if started.elapsed() > Duration::from_secs(device.expires_in) {
            anyhow::bail!("Email OAuth2 device-code flow timed out before authorization");
        }

        tokio::time::sleep(Duration::from_secs(interval_secs)).await;

        let form: &[(&str, &str)] = &[
            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ("device_code", device.device_code.as_str()),
            ("client_id", client_id),
        ];

        let response = client
            .post(token_url)
            .form(form)
            .send()
            .await
            .context("Failed polling email OAuth2 token endpoint")?;

        if response.status().is_success() {
            return parse_token_response(response).await;
        }

        let status = response.status();
        let text = response.text().await.unwrap_or_default();

        if let Ok(err) = serde_json::from_str::<OAuthErrorResponse>(&text) {
            match err.error.as_str() {
                "authorization_pending" => continue,
                "slow_down" => {
                    interval_secs = interval_secs.saturating_add(5);
                    continue;
                }
                "access_denied" => anyhow::bail!("Email OAuth2 authorization was denied"),
                "expired_token" => anyhow::bail!("Email OAuth2 device-code expired"),
                _ => anyhow::bail!(
                    "Email OAuth2 device-code polling failed ({status}): {}",
                    err.error_description.unwrap_or(err.error)
                ),
            }
        }

        anyhow::bail!("Email OAuth2 device-code polling failed ({status}): {text}");
    }
}

async fn parse_token_response(response: reqwest::Response) -> Result<TokenSet> {
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        anyhow::bail!("Email OAuth2 token request failed ({status}): {body}");
    }

    let token: TokenResponse = response
        .json()
        .await
        .context("Failed to parse email OAuth2 token response")?;

    let expires_at = token.expires_in.and_then(|seconds| {
        if seconds <= 0 {
            None
        } else {
            Some(Utc::now() + chrono::Duration::seconds(seconds))
        }
    });

    Ok(TokenSet {
        access_token: token.access_token,
        refresh_token: token.refresh_token,
        id_token: None,
        expires_at,
        token_type: token.token_type,
        scope: token.scope,
    })
}
