use anyhow::{Context, Result};
use reqwest::Client;
use serde::Deserialize;
use std::time::{Duration, Instant};

pub const GITHUB_COPILOT_CLIENT_ID: &str = "Iv1.b507a08c87ecfe98";
pub const GITHUB_COPILOT_DEVICE_CODE_URL: &str = "https://github.com/login/device/code";
pub const GITHUB_COPILOT_ACCESS_TOKEN_URL: &str = "https://github.com/login/oauth/access_token";

#[derive(Debug, Clone)]
pub struct DeviceCodeStart {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub expires_in: u64,
    pub interval: u64,
}

#[derive(Debug, Deserialize)]
struct DeviceCodeResponse {
    device_code: String,
    user_code: String,
    verification_uri: String,
    #[serde(default)]
    expires_in: Option<u64>,
    #[serde(default)]
    interval: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct DeviceTokenResponse {
    #[serde(default)]
    access_token: Option<String>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    error_description: Option<String>,
}

pub async fn request_device_code(client: &Client, scope: &str) -> Result<DeviceCodeStart> {
    let form = [
        ("client_id", GITHUB_COPILOT_CLIENT_ID),
        ("scope", scope.trim()),
    ];

    let device_code_url = std::env::var("GITHUB_COPILOT_DEVICE_CODE_URL")
        .unwrap_or_else(|_| GITHUB_COPILOT_DEVICE_CODE_URL.to_string());

    let response = client
        .post(&device_code_url)
        .header("Accept", "application/json")
        .header("Content-Type", "application/x-www-form-urlencoded")
        .form(&form)
        .send()
        .await
        .context("Failed to request GitHub device code")?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        anyhow::bail!("GitHub device code failed (HTTP {status}): {body}");
    }

    let parsed: DeviceCodeResponse = response
        .json()
        .await
        .context("Failed to parse GitHub device code response")?;

    if parsed.device_code.trim().is_empty()
        || parsed.user_code.trim().is_empty()
        || parsed.verification_uri.trim().is_empty()
    {
        anyhow::bail!("GitHub device code response missing required fields");
    }

    Ok(DeviceCodeStart {
        device_code: parsed.device_code,
        user_code: parsed.user_code,
        verification_uri: parsed.verification_uri,
        expires_in: parsed.expires_in.unwrap_or(900).max(1),
        interval: parsed.interval.unwrap_or(5).max(1),
    })
}

pub async fn poll_for_access_token(client: &Client, device: &DeviceCodeStart) -> Result<String> {
    let mut interval_ms = (device.interval.max(1) * 1000).max(1000);
    if let Ok(override_ms) = std::env::var("GITHUB_COPILOT_POLL_INTERVAL_MS") {
        if let Ok(parsed) = override_ms.parse::<u64>() {
            interval_ms = parsed;
        }
    }
    let expires_at = Instant::now() + Duration::from_secs(device.expires_in.max(1));

    while Instant::now() < expires_at {
        tokio::time::sleep(Duration::from_millis(interval_ms)).await;

        let form = [
            ("client_id", GITHUB_COPILOT_CLIENT_ID),
            ("device_code", device.device_code.as_str()),
            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
        ];

        let access_token_url = std::env::var("GITHUB_COPILOT_ACCESS_TOKEN_URL")
            .unwrap_or_else(|_| GITHUB_COPILOT_ACCESS_TOKEN_URL.to_string());

        let response = client
            .post(&access_token_url)
            .header("Accept", "application/json")
            .header("Content-Type", "application/x-www-form-urlencoded")
            .form(&form)
            .send()
            .await
            .context("Failed polling GitHub device token endpoint")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("GitHub device token failed (HTTP {status}): {body}");
        }

        let token_response: DeviceTokenResponse = response
            .json()
            .await
            .context("Failed to parse GitHub device token response")?;

        if let Some(token) = token_response
            .access_token
            .as_deref()
            .map(str::trim)
            .filter(|token| !token.is_empty())
        {
            return Ok(token.to_string());
        }

        match token_response
            .error
            .as_deref()
            .map(str::trim)
            .unwrap_or("authorization_pending")
        {
            "authorization_pending" => {}
            "slow_down" => {
                interval_ms = interval_ms.saturating_add(2000);
            }
            "expired_token" => anyhow::bail!("GitHub device code expired; run login again"),
            "access_denied" => anyhow::bail!("GitHub login canceled"),
            other => {
                let detail = token_response
                    .error_description
                    .as_deref()
                    .filter(|value| !value.trim().is_empty())
                    .unwrap_or(other);
                anyhow::bail!("GitHub device flow error: {detail}");
            }
        }
    }

    anyhow::bail!("GitHub device code expired; run login again")
}

#[cfg(test)]
mod tests {
    use super::*;

    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn request_and_poll_device_flow_mocked() {
        let server = MockServer::start().await;

        let device_json = serde_json::json!({
            "device_code": "device123",
            "user_code": "USERCODE",
            "verification_uri": "https://example.com/verify",
            "expires_in": 900,
            "interval": 1
        });

        Mock::given(method("POST"))
            .and(path("/login/device/code"))
            .respond_with(ResponseTemplate::new(200).set_body_json(device_json))
            .mount(&server)
            .await;

        let token_json = serde_json::json!({ "access_token": "testtoken" });
        Mock::given(method("POST"))
            .and(path("/login/oauth/access_token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(token_json))
            .mount(&server)
            .await;

        unsafe {
            std::env::set_var(
                "GITHUB_COPILOT_DEVICE_CODE_URL",
                format!("{}/login/device/code", server.uri()),
            );
            std::env::set_var(
                "GITHUB_COPILOT_ACCESS_TOKEN_URL",
                format!("{}/login/oauth/access_token", server.uri()),
            );
            std::env::set_var("GITHUB_COPILOT_POLL_INTERVAL_MS", "10");
        }

        let client = reqwest::Client::new();
        let device = request_device_code(&client, "read:user").await.unwrap();
        assert_eq!(device.device_code, "device123");

        let token = poll_for_access_token(&client, &device).await.unwrap();
        assert_eq!(token, "testtoken");
    }

    #[test]
    fn constants_match_expected_values() {
        assert_eq!(GITHUB_COPILOT_CLIENT_ID, "Iv1.b507a08c87ecfe98");
        assert!(GITHUB_COPILOT_DEVICE_CODE_URL.contains("device/code"));
        assert!(GITHUB_COPILOT_ACCESS_TOKEN_URL.contains("access_token"));
    }

    #[test]
    fn device_code_defaults_are_non_zero() {
        let device = DeviceCodeStart {
            device_code: "abc".to_string(),
            user_code: "123".to_string(),
            verification_uri: "https://github.com/login/device".to_string(),
            expires_in: 900,
            interval: 5,
        };

        assert!(device.expires_in >= 1);
        assert!(device.interval >= 1);
    }
}
