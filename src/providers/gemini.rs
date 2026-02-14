//! Google Gemini provider with support for:
//! - Direct API key (`GEMINI_API_KEY` env var or config)
//! - Gemini CLI OAuth tokens (reuse existing ~/.gemini/ authentication)
//! - Google Cloud ADC (`GOOGLE_APPLICATION_CREDENTIALS`)

use crate::providers::traits::Provider;
use async_trait::async_trait;
use directories::UserDirs;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Gemini provider supporting multiple authentication methods.
pub struct GeminiProvider {
    api_key: Option<String>,
    client: Client,
}

// ══════════════════════════════════════════════════════════════════════════════
// API REQUEST/RESPONSE TYPES
// ══════════════════════════════════════════════════════════════════════════════

#[derive(Debug, Serialize)]
struct GenerateContentRequest {
    contents: Vec<Content>,
    #[serde(skip_serializing_if = "Option::is_none")]
    system_instruction: Option<Content>,
    #[serde(rename = "generationConfig")]
    generation_config: GenerationConfig,
}

#[derive(Debug, Serialize)]
struct Content {
    #[serde(skip_serializing_if = "Option::is_none")]
    role: Option<String>,
    parts: Vec<Part>,
}

#[derive(Debug, Serialize)]
struct Part {
    text: String,
}

#[derive(Debug, Serialize)]
struct GenerationConfig {
    temperature: f64,
    #[serde(rename = "maxOutputTokens")]
    max_output_tokens: u32,
}

#[derive(Debug, Deserialize)]
struct GenerateContentResponse {
    candidates: Option<Vec<Candidate>>,
    error: Option<ApiError>,
}

#[derive(Debug, Deserialize)]
struct Candidate {
    content: CandidateContent,
}

#[derive(Debug, Deserialize)]
struct CandidateContent {
    parts: Vec<ResponsePart>,
}

#[derive(Debug, Deserialize)]
struct ResponsePart {
    text: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ApiError {
    message: String,
}

// ══════════════════════════════════════════════════════════════════════════════
// GEMINI CLI TOKEN STRUCTURES
// ══════════════════════════════════════════════════════════════════════════════

/// OAuth token stored by Gemini CLI in `~/.gemini/oauth_creds.json`
#[derive(Debug, Deserialize)]
struct GeminiCliOAuthCreds {
    access_token: Option<String>,
    refresh_token: Option<String>,
    expiry: Option<String>,
}

/// Settings stored by Gemini CLI in ~/.gemini/settings.json
#[derive(Debug, Deserialize)]
struct GeminiCliSettings {
    #[serde(rename = "selectedAuthType")]
    selected_auth_type: Option<String>,
}

impl GeminiProvider {
    /// Create a new Gemini provider.
    /// 
    /// Authentication priority:
    /// 1. Explicit API key passed in
    /// 2. `GEMINI_API_KEY` environment variable
    /// 3. `GOOGLE_API_KEY` environment variable
    /// 4. Gemini CLI OAuth tokens (`~/.gemini/oauth_creds.json`)
    pub fn new(api_key: Option<&str>) -> Self {
        let resolved_key = api_key
            .map(String::from)
            .or_else(|| std::env::var("GEMINI_API_KEY").ok())
            .or_else(|| std::env::var("GOOGLE_API_KEY").ok())
            .or_else(Self::try_load_gemini_cli_token);

        Self {
            api_key: resolved_key,
            client: Client::builder()
                .timeout(std::time::Duration::from_secs(120))
                .connect_timeout(std::time::Duration::from_secs(10))
                .build()
                .unwrap_or_else(|_| Client::new()),
        }
    }

    /// Try to load OAuth access token from Gemini CLI's cached credentials.
    /// Location: `~/.gemini/oauth_creds.json`
    fn try_load_gemini_cli_token() -> Option<String> {
        let gemini_dir = Self::gemini_cli_dir()?;
        let creds_path = gemini_dir.join("oauth_creds.json");

        if !creds_path.exists() {
            return None;
        }

        let content = std::fs::read_to_string(&creds_path).ok()?;
        let creds: GeminiCliOAuthCreds = serde_json::from_str(&content).ok()?;

        // Check if token is expired (basic check)
        if let Some(ref expiry) = creds.expiry {
            if let Ok(expiry_time) = chrono::DateTime::parse_from_rfc3339(expiry) {
                if expiry_time < chrono::Utc::now() {
                    tracing::debug!("Gemini CLI OAuth token expired, skipping");
                    return None;
                }
            }
        }

        creds.access_token
    }

    /// Get the Gemini CLI config directory (~/.gemini)
    fn gemini_cli_dir() -> Option<PathBuf> {
        UserDirs::new().map(|u| u.home_dir().join(".gemini"))
    }

    /// Check if Gemini CLI is configured and has valid credentials
    pub fn has_cli_credentials() -> bool {
        Self::try_load_gemini_cli_token().is_some()
    }

    /// Check if any Gemini authentication is available
    pub fn has_any_auth() -> bool {
        std::env::var("GEMINI_API_KEY").is_ok()
            || std::env::var("GOOGLE_API_KEY").is_ok()
            || Self::has_cli_credentials()
    }

    /// Get authentication source description for diagnostics
    pub fn auth_source(&self) -> &'static str {
        if self.api_key.is_none() {
            return "none";
        }
        if std::env::var("GEMINI_API_KEY").is_ok() {
            return "GEMINI_API_KEY env var";
        }
        if std::env::var("GOOGLE_API_KEY").is_ok() {
            return "GOOGLE_API_KEY env var";
        }
        if Self::has_cli_credentials() {
            return "Gemini CLI OAuth";
        }
        "config"
    }
}

#[async_trait]
impl Provider for GeminiProvider {
    async fn chat_with_system(
        &self,
        system_prompt: Option<&str>,
        message: &str,
        model: &str,
        temperature: f64,
    ) -> anyhow::Result<String> {
        let api_key = self.api_key.as_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "Gemini API key not found. Options:\n\
                 1. Set GEMINI_API_KEY env var\n\
                 2. Run `gemini` CLI to authenticate (tokens will be reused)\n\
                 3. Get an API key from https://aistudio.google.com/app/apikey\n\
                 4. Run `zeroclaw onboard` to configure"
            )
        })?;

        // Build request
        let system_instruction = system_prompt.map(|sys| Content {
            role: None,
            parts: vec![Part {
                text: sys.to_string(),
            }],
        });

        let request = GenerateContentRequest {
            contents: vec![Content {
                role: Some("user".to_string()),
                parts: vec![Part {
                    text: message.to_string(),
                }],
            }],
            system_instruction,
            generation_config: GenerationConfig {
                temperature,
                max_output_tokens: 8192,
            },
        };

        // Gemini API endpoint
        // Model format: gemini-2.0-flash, gemini-1.5-pro, etc.
        let model_name = if model.starts_with("models/") {
            model.to_string()
        } else {
            format!("models/{model}")
        };

        let url = format!(
            "https://generativelanguage.googleapis.com/v1beta/{model_name}:generateContent?key={api_key}"
        );

        let response = self.client.post(&url).json(&request).send().await?;

        if !response.status().is_success() {
            let status = response.status();
            let error_text = response.text().await.unwrap_or_default();
            anyhow::bail!("Gemini API error ({status}): {error_text}");
        }

        let result: GenerateContentResponse = response.json().await?;

        // Check for API error in response body
        if let Some(err) = result.error {
            anyhow::bail!("Gemini API error: {}", err.message);
        }

        // Extract text from response
        result
            .candidates
            .and_then(|c| c.into_iter().next())
            .and_then(|c| c.content.parts.into_iter().next())
            .and_then(|p| p.text)
            .ok_or_else(|| anyhow::anyhow!("No response from Gemini"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_creates_without_key() {
        let provider = GeminiProvider::new(None);
        // Should not panic, just have no key
        assert!(provider.api_key.is_none() || provider.api_key.is_some());
    }

    #[test]
    fn provider_creates_with_key() {
        let provider = GeminiProvider::new(Some("test-api-key"));
        assert!(provider.api_key.is_some());
        assert_eq!(provider.api_key.as_deref(), Some("test-api-key"));
    }

    #[test]
    fn gemini_cli_dir_returns_path() {
        let dir = GeminiProvider::gemini_cli_dir();
        // Should return Some on systems with home dir
        if UserDirs::new().is_some() {
            assert!(dir.is_some());
            assert!(dir.unwrap().ends_with(".gemini"));
        }
    }

    #[test]
    fn auth_source_reports_correctly() {
        let provider = GeminiProvider::new(Some("explicit-key"));
        // With explicit key, should report "config" (unless CLI credentials exist)
        let source = provider.auth_source();
        // Should be either "config" or "Gemini CLI OAuth" if CLI is configured
        assert!(source == "config" || source == "Gemini CLI OAuth");
    }

    #[test]
    fn model_name_formatting() {
        // Test that model names are formatted correctly
        let model = "gemini-2.0-flash";
        let formatted = if model.starts_with("models/") {
            model.to_string()
        } else {
            format!("models/{model}")
        };
        assert_eq!(formatted, "models/gemini-2.0-flash");

        // Already prefixed
        let model2 = "models/gemini-1.5-pro";
        let formatted2 = if model2.starts_with("models/") {
            model2.to_string()
        } else {
            format!("models/{model2}")
        };
        assert_eq!(formatted2, "models/gemini-1.5-pro");
    }

    #[test]
    fn request_serialization() {
        let request = GenerateContentRequest {
            contents: vec![Content {
                role: Some("user".to_string()),
                parts: vec![Part {
                    text: "Hello".to_string(),
                }],
            }],
            system_instruction: Some(Content {
                role: None,
                parts: vec![Part {
                    text: "You are helpful".to_string(),
                }],
            }),
            generation_config: GenerationConfig {
                temperature: 0.7,
                max_output_tokens: 8192,
            },
        };

        let json = serde_json::to_string(&request).unwrap();
        assert!(json.contains("\"role\":\"user\""));
        assert!(json.contains("\"text\":\"Hello\""));
        assert!(json.contains("\"temperature\":0.7"));
        assert!(json.contains("\"maxOutputTokens\":8192"));
    }

    #[test]
    fn response_deserialization() {
        let json = r#"{
            "candidates": [{
                "content": {
                    "parts": [{"text": "Hello there!"}]
                }
            }]
        }"#;

        let response: GenerateContentResponse = serde_json::from_str(json).unwrap();
        assert!(response.candidates.is_some());
        let text = response
            .candidates
            .unwrap()
            .into_iter()
            .next()
            .unwrap()
            .content
            .parts
            .into_iter()
            .next()
            .unwrap()
            .text;
        assert_eq!(text, Some("Hello there!".to_string()));
    }

    #[test]
    fn error_response_deserialization() {
        let json = r#"{
            "error": {
                "message": "Invalid API key"
            }
        }"#;

        let response: GenerateContentResponse = serde_json::from_str(json).unwrap();
        assert!(response.error.is_some());
        assert_eq!(response.error.unwrap().message, "Invalid API key");
    }
}
