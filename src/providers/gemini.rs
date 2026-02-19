//! Google Gemini provider with support for:
//! - Direct API key (`GEMINI_API_KEY` env var or config)
//! - Gemini CLI OAuth tokens (reuse existing ~/.gemini/ authentication)
//! - Google Cloud ADC (`GOOGLE_APPLICATION_CREDENTIALS`)

use crate::providers::traits::{ChatMessage, Provider};
use async_trait::async_trait;
use directories::UserDirs;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Gemini provider supporting multiple authentication methods.
pub struct GeminiProvider {
    auth: Option<GeminiAuth>,
    antigravity_project_id: Option<String>,
    client: Client,
}

/// Resolved credential — the variant determines both the HTTP auth method
/// and the diagnostic label returned by `auth_source()`.
#[derive(Debug)]
enum GeminiAuth {
    /// Explicit API key from config: sent as `?key=` query parameter.
    ExplicitKey(String),
    /// API key from `GEMINI_API_KEY` env var: sent as `?key=`.
    EnvGeminiKey(String),
    /// API key from `GOOGLE_API_KEY` env var: sent as `?key=`.
    EnvGoogleKey(String),
    /// OAuth access token from Gemini CLI: sent as `Authorization: Bearer`.
    OAuthToken(String),
    /// Google Antigravity OAuth token: Bearer auth against the
    /// `daily-cloudcode-pa.sandbox.googleapis.com` endpoint with standard
    /// Gemini `generateContent` URL format.
    AntigravityToken(String),
}

impl GeminiAuth {
    /// Whether this credential is an API key (sent as `?key=` query param).
    fn is_api_key(&self) -> bool {
        matches!(
            self,
            GeminiAuth::ExplicitKey(_) | GeminiAuth::EnvGeminiKey(_) | GeminiAuth::EnvGoogleKey(_)
        )
    }

    /// The raw credential string.
    fn credential(&self) -> &str {
        match self {
            GeminiAuth::ExplicitKey(s)
            | GeminiAuth::EnvGeminiKey(s)
            | GeminiAuth::EnvGoogleKey(s)
            | GeminiAuth::OAuthToken(s)
            | GeminiAuth::AntigravityToken(s) => s,
        }
    }
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

/// Inner request body nested under `request` in the internal cloudcode-pa API.
/// Mirrors `VertexGenerateContentRequest` from the Gemini CLI source.
#[derive(Debug, Serialize)]
struct InternalRequestBody {
    contents: Vec<Content>,
    #[serde(rename = "generationConfig")]
    generation_config: GenerationConfig,
    #[serde(rename = "systemInstruction", skip_serializing_if = "Option::is_none")]
    system_instruction: Option<Content>,
}

/// Outer request envelope for the internal cloudcode-pa API (`/v1internal:generateContent`).
///
/// Mirrors `CAGenerateContentRequest` from the Gemini CLI source:
/// ```json
/// { "model": "...", "project": "...", "request": { "contents": [...], "generationConfig": {...} } }
/// ```
/// The `generationConfig` / `contents` / `systemInstruction` fields must be nested under
/// `request`, not at the top level — sending them at the top level results in 400
/// "Unknown name" errors.
#[derive(Debug, Serialize)]
struct InternalGenerateContentRequest {
    model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    project: Option<String>,
    request: InternalRequestBody,
}

#[derive(Debug, Serialize, Clone)]
struct Content {
    #[serde(skip_serializing_if = "Option::is_none")]
    role: Option<String>,
    parts: Vec<Part>,
}

#[derive(Debug, Serialize, Clone)]
struct Part {
    text: String,
}

#[derive(Debug, Serialize, Clone)]
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
    expiry: Option<String>,
}

/// Internal API endpoint used by Gemini CLI for OAuth users.
/// See: https://github.com/google-gemini/gemini-cli/issues/19200
const CLOUDCODE_PA_ENDPOINT: &str = "https://cloudcode-pa.googleapis.com/v1internal";

/// Public API endpoint for API key users.
const PUBLIC_API_ENDPOINT: &str = "https://generativelanguage.googleapis.com/v1beta";

/// Antigravity API endpoint for Google Cloud Code Assist.
/// Uses the same `v1internal:generateContent` format as Gemini CLI OAuth,
/// but routed to the sandbox endpoint.
const ANTIGRAVITY_ENDPOINT: &str = "https://daily-cloudcode-pa.sandbox.googleapis.com/v1internal";
const CODE_ASSIST_USER_AGENT: &str = "google-api-rust-client/0.1";
const CODE_ASSIST_API_CLIENT: &str = "google-cloud-sdk vscode_cloudshelleditor/0.1";
const CODE_ASSIST_CLIENT_METADATA: &str =
    r#"{"ideType":"IDE_UNSPECIFIED","platform":"PLATFORM_UNSPECIFIED","pluginType":"GEMINI"}"#;
const ANTIGRAVITY_PROJECT_ID_FILE: &str = "google_antigravity_project_id";

impl GeminiProvider {
    /// Create a new Gemini provider.
    ///
    /// Authentication priority:
    /// 1. Explicit API key passed in
    /// 2. `GEMINI_API_KEY` environment variable
    /// 3. `GOOGLE_API_KEY` environment variable
    /// 4. Gemini CLI OAuth tokens (`~/.gemini/oauth_creds.json`)
    pub fn new(api_key: Option<&str>) -> Self {
        let resolved_auth = api_key
            .and_then(Self::normalize_non_empty)
            .map(GeminiAuth::ExplicitKey)
            .or_else(|| Self::load_non_empty_env("GEMINI_API_KEY").map(GeminiAuth::EnvGeminiKey))
            .or_else(|| Self::load_non_empty_env("GOOGLE_API_KEY").map(GeminiAuth::EnvGoogleKey))
            .or_else(|| Self::try_load_gemini_cli_token().map(GeminiAuth::OAuthToken));

        Self {
            auth: resolved_auth,
            antigravity_project_id: None,
            client: Client::builder()
                .timeout(std::time::Duration::from_secs(120))
                .connect_timeout(std::time::Duration::from_secs(10))
                .build()
                .unwrap_or_else(|_| Client::new()),
        }
    }

    /// Create a Gemini provider with a Google Antigravity OAuth token.
    ///
    /// Routes requests through `daily-cloudcode-pa.sandbox.googleapis.com`
    /// using the standard Gemini `models/{model}:generateContent` URL format
    /// with Bearer auth. This endpoint serves both Anthropic (Claude) and
    /// Gemini models via the Google Cloud Code Assist API.
    pub fn with_antigravity_token(token: Option<&str>) -> Self {
        let resolved_auth = token
            .and_then(Self::normalize_non_empty)
            .map(GeminiAuth::AntigravityToken);

        Self {
            auth: resolved_auth,
            antigravity_project_id: Self::resolve_antigravity_project_id(),
            client: Client::builder()
                .timeout(std::time::Duration::from_secs(120))
                .connect_timeout(std::time::Duration::from_secs(10))
                .build()
                .unwrap_or_else(|_| Client::new()),
        }
    }

    /// Create a Gemini provider pre-authenticated with an OAuth access token.
    ///
    /// This bypasses the normal API-key resolution and forces the provider to
    /// use the `OAuthToken` auth path (Bearer header + cloudcode-pa endpoint).
    /// Used by the Antigravity / Gemini CLI OAuth flows that have already
    /// obtained an access token.
    pub fn new_with_oauth_token(access_token: &str) -> Self {
        let auth = Self::normalize_non_empty(access_token).map(GeminiAuth::OAuthToken);
        Self {
            auth,
            antigravity_project_id: None,
            client: Client::builder()
                .timeout(std::time::Duration::from_secs(120))
                .connect_timeout(std::time::Duration::from_secs(10))
                .build()
                .unwrap_or_else(|_| Client::new()),
        }
    }

    fn normalize_non_empty(value: &str) -> Option<String> {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    }

    fn load_non_empty_env(name: &str) -> Option<String> {
        std::env::var(name)
            .ok()
            .and_then(|value| Self::normalize_non_empty(&value))
    }

    fn resolve_antigravity_project_id() -> Option<String> {
        Self::load_non_empty_env("GOOGLE_ANTIGRAVITY_PROJECT_ID")
            .or_else(|| Self::load_non_empty_env("GOOGLE_CLOUD_PROJECT"))
            .or_else(|| Self::load_non_empty_env("GCLOUD_PROJECT"))
            .or_else(Self::load_antigravity_project_id_from_file)
    }

    fn load_antigravity_project_id_from_file() -> Option<String> {
        let default_dir = crate::config::schema::default_config_dir().ok()?;
        let config_dir = match crate::config::schema::resolve_active_config_dir(&default_dir) {
            Some(dir) => {
                tracing::debug!(
                    config_dir = %dir.display(),
                    "Using active workspace config directory for Antigravity project ID"
                );
                dir
            }
            None => {
                tracing::debug!(
                    default_dir = %default_dir.display(),
                    "No active workspace marker found, using default config directory"
                );
                default_dir
            }
        };
        let path = config_dir.join(ANTIGRAVITY_PROJECT_ID_FILE);
        match std::fs::read_to_string(&path) {
            Ok(content) => Self::normalize_non_empty(&content),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                tracing::debug!(
                    path = %path.display(),
                    "Antigravity project ID file not found"
                );
                None
            }
            Err(err) => {
                tracing::debug!(
                    path = %path.display(),
                    error = %err,
                    "Failed to read Antigravity project ID file"
                );
                None
            }
        }
    }

    fn apply_internal_headers(request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        request
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .header("User-Agent", CODE_ASSIST_USER_AGENT)
            .header("X-Goog-Api-Client", CODE_ASSIST_API_CLIENT)
            .header("Client-Metadata", CODE_ASSIST_CLIENT_METADATA)
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
                    tracing::warn!("Gemini CLI OAuth token expired — re-run `gemini` to refresh");
                    return None;
                }
            }
        }

        creds
            .access_token
            .and_then(|token| Self::normalize_non_empty(&token))
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
        Self::load_non_empty_env("GEMINI_API_KEY").is_some()
            || Self::load_non_empty_env("GOOGLE_API_KEY").is_some()
            || Self::has_cli_credentials()
    }

    /// Get authentication source description for diagnostics.
    /// Uses the stored enum variant — no env var re-reading at call time.
    pub fn auth_source(&self) -> &'static str {
        match self.auth.as_ref() {
            Some(GeminiAuth::ExplicitKey(_)) => "config",
            Some(GeminiAuth::EnvGeminiKey(_)) => "GEMINI_API_KEY env var",
            Some(GeminiAuth::EnvGoogleKey(_)) => "GOOGLE_API_KEY env var",
            Some(GeminiAuth::OAuthToken(_)) => "Gemini CLI OAuth",
            Some(GeminiAuth::AntigravityToken(_)) => "Google Antigravity OAuth",
            None => "none",
        }
    }

    fn format_model_name(model: &str) -> String {
        if model.starts_with("models/") {
            model.to_string()
        } else {
            format!("models/{model}")
        }
    }

    /// Build the API URL based on auth type.
    ///
    /// - API key users → public `generativelanguage.googleapis.com/v1beta`
    /// - OAuth users → internal `cloudcode-pa.googleapis.com/v1internal`
    ///
    /// The Gemini CLI OAuth tokens are scoped for the internal Code Assist API,
    /// not the public API. Sending them to the public endpoint results in
    /// "400 Bad Request: API key not valid" errors.
    /// See: https://github.com/google-gemini/gemini-cli/issues/19200
    fn build_generate_content_url(model: &str, auth: &GeminiAuth) -> String {
        match auth {
            GeminiAuth::OAuthToken(_) => {
                // OAuth tokens from Gemini CLI are scoped for the internal
                // Code Assist API. The model is passed in the request body,
                // not the URL path.
                format!("{CLOUDCODE_PA_ENDPOINT}:generateContent")
            }
            GeminiAuth::AntigravityToken(_) => {
                // Antigravity uses the same v1internal format as Gemini CLI,
                // with the model in the request body, not the URL path.
                format!("{ANTIGRAVITY_ENDPOINT}:generateContent")
            }
            _ => {
                let model_name = Self::format_model_name(model);
                let base_url = format!("{PUBLIC_API_ENDPOINT}/{model_name}:generateContent");

                if auth.is_api_key() {
                    format!("{base_url}?key={}", auth.credential())
                } else {
                    base_url
                }
            }
        }
    }

    /// Build an `InternalGenerateContentRequest` for the `/v1internal:generateContent` endpoint.
    ///
    /// The correct shape (matching `CAGenerateContentRequest` in Gemini CLI) is:
    /// ```json
    /// {
    ///   "model": "models/...",
    ///   "project": "<optional>",
    ///   "request": {
    ///     "contents": [...],
    ///     "generationConfig": {...},
    ///     "systemInstruction": {...}
    ///   }
    /// }
    /// ```
    /// Sending `contents`/`generationConfig` at the top level causes 400 "Unknown name" errors.
    fn build_internal_request(
        request: &GenerateContentRequest,
        model: &str,
        project_id: Option<&str>,
    ) -> InternalGenerateContentRequest {
        InternalGenerateContentRequest {
            model: Self::format_model_name(model),
            project: project_id.map(ToString::to_string),
            request: InternalRequestBody {
                contents: request.contents.clone(),
                generation_config: request.generation_config.clone(),
                system_instruction: request.system_instruction.clone(),
            },
        }
    }

    fn build_generate_content_request(
        &self,
        auth: &GeminiAuth,
        url: &str,
        request: &GenerateContentRequest,
        model: &str,
    ) -> reqwest::RequestBuilder {
        match auth {
            GeminiAuth::OAuthToken(token) | GeminiAuth::AntigravityToken(token) => {
                // Internal Code Assist API: model + nested `request` envelope.
                // Antigravity may include a project ID; standard OAuth does not.
                let project_id = if matches!(auth, GeminiAuth::AntigravityToken(_)) {
                    self.antigravity_project_id.as_deref()
                } else {
                    None
                };
                let internal = Self::build_internal_request(request, model, project_id);
                Self::apply_internal_headers(self.client.post(url))
                    .json(&internal)
                    .bearer_auth(token)
            }
            _ => self.client.post(url).json(request),
        }
    }

    /// Convert ChatMessage slice to Gemini API format.
    ///
    /// System messages become `system_instruction` (first one wins).
    /// Assistant messages map to `"model"` role (Gemini convention).
    /// Tool results map to `"user"` role (non-native-tool mode).
    fn convert_chat_messages(messages: &[ChatMessage]) -> (Option<Content>, Vec<Content>) {
        let mut system_instruction: Option<Content> = None;
        let mut contents: Vec<Content> = Vec::new();

        for msg in messages {
            match msg.role.as_str() {
                "system" => {
                    if system_instruction.is_none() {
                        system_instruction = Some(Content {
                            role: None,
                            parts: vec![Part {
                                text: msg.content.clone(),
                            }],
                        });
                    }
                }
                "assistant" => {
                    // Gemini API uses "model" role for assistant messages.
                    contents.push(Content {
                        role: Some("model".to_string()),
                        parts: vec![Part {
                            text: msg.content.clone(),
                        }],
                    });
                }
                "tool" => {
                    // Tool results are sent as user messages in non-native-tool mode.
                    contents.push(Content {
                        role: Some("user".to_string()),
                        parts: vec![Part {
                            text: msg.content.clone(),
                        }],
                    });
                }
                _ => {
                    // "user" and any other role → user message
                    contents.push(Content {
                        role: Some("user".to_string()),
                        parts: vec![Part {
                            text: msg.content.clone(),
                        }],
                    });
                }
            }
        }

        (system_instruction, contents)
    }

    /// Process a Gemini API response, handling errors and extracting text.
    ///
    /// Shared by `chat_with_system` and `chat_with_history` to avoid
    /// duplicating error-handling logic (401/403 specializations, sanitization).
    async fn handle_response(
        &self,
        response: reqwest::Response,
        auth: &GeminiAuth,
    ) -> anyhow::Result<String> {
        if !response.status().is_success() {
            let status = response.status();
            let error_text = response.text().await.unwrap_or_default();
            let sanitized = crate::providers::sanitize_api_error(&error_text);
            tracing::debug!(
                %status,
                error_text = %sanitized,
                "Gemini API returned error"
            );

            if status == reqwest::StatusCode::UNAUTHORIZED {
                match auth {
                    GeminiAuth::AntigravityToken(_) => {
                        anyhow::bail!(
                            "Google Antigravity OAuth token invalid or expired (401 Unauthorized). \
                             Re-run `zeroclaw onboard --interactive` to refresh login, \
                             or set a fresh GOOGLE_ANTIGRAVITY_ACCESS_TOKEN."
                        );
                    }
                    GeminiAuth::OAuthToken(_) => {
                        anyhow::bail!(
                            "Gemini CLI OAuth token invalid or expired (401 Unauthorized). \
                             Re-run `gemini` to refresh ~/.gemini/oauth_creds.json."
                        );
                    }
                    _ => {}
                }
            }

            if status == reqwest::StatusCode::FORBIDDEN
                && matches!(auth, GeminiAuth::AntigravityToken(_))
                && (error_text.contains("SUBSCRIPTION_REQUIRED")
                    || error_text.contains("Gemini Code Assist license"))
            {
                let project = self
                    .antigravity_project_id
                    .as_deref()
                    .unwrap_or("auto-detected");
                anyhow::bail!(
                    "Google Antigravity project lacks Gemini Code Assist license \
                     (403 Forbidden SUBSCRIPTION_REQUIRED). Current project: {project}. \
                     Request license: https://cloud.google.com/gemini/docs/codeassist/request-license \
                     . If you have another licensed project, set GOOGLE_ANTIGRAVITY_PROJECT_ID."
                );
            }

            anyhow::bail!("Gemini API error ({status}): {sanitized}");
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

#[async_trait]
impl Provider for GeminiProvider {
    async fn chat_with_system(
        &self,
        system_prompt: Option<&str>,
        message: &str,
        model: &str,
        temperature: f64,
    ) -> anyhow::Result<String> {
        let auth = self.auth.as_ref().ok_or_else(|| {
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

        let url = Self::build_generate_content_url(model, auth);

        let response = self
            .build_generate_content_request(auth, &url, &request, model)
            .send()
            .await?;

        self.handle_response(response, auth).await
    }

    async fn chat_with_history(
        &self,
        messages: &[ChatMessage],
        model: &str,
        temperature: f64,
    ) -> anyhow::Result<String> {
        let auth = self.auth.as_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "Gemini API key not found. Options:\n\
                 1. Set GEMINI_API_KEY env var\n\
                 2. Run `gemini` CLI to authenticate (tokens will be reused)\n\
                 3. Get an API key from https://aistudio.google.com/app/apikey\n\
                 4. Run `zeroclaw onboard` to configure"
            )
        })?;

        let (system_instruction, contents) = Self::convert_chat_messages(messages);

        if contents.is_empty() {
            anyhow::bail!("No user or assistant messages provided");
        }

        let request = GenerateContentRequest {
            contents,
            system_instruction,
            generation_config: GenerationConfig {
                temperature,
                max_output_tokens: 8192,
            },
        };

        let url = Self::build_generate_content_url(model, auth);

        let response = self
            .build_generate_content_request(auth, &url, &request, model)
            .send()
            .await?;

        self.handle_response(response, auth).await
    }

    // TODO: Implement native function calling for Gemini/Antigravity.
    //
    // The Antigravity endpoint (cloudcode-pa.googleapis.com/v1internal) supports
    // Gemini-format function calling (functionDeclarations / functionCall / functionResponse).
    // Verified via gemini-cli issues: https://github.com/google-gemini/gemini-cli/issues/9535
    //
    // To enable native tool calling:
    // 1. Add `tools` field (Vec<FunctionDeclaration>) to GenerateContentRequest / InternalRequestBody
    // 2. Add `functionCall` field to ResponsePart
    // 3. Override `supports_native_tools() -> true`
    // 4. Implement `chat_with_tools()` converting OpenAI-format tool defs to functionDeclarations
    // 5. Parse functionCall responses into ChatResponse::tool_calls
    // 6. Handle functionResponse in convert_chat_messages (role mapping)
    //
    // Until implemented, agent loop uses prompt-based tool calling (functional but less reliable).
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::AUTHORIZATION;

    fn make_test_request() -> GenerateContentRequest {
        GenerateContentRequest {
            contents: vec![Content {
                role: Some("user".into()),
                parts: vec![Part {
                    text: "hello".into(),
                }],
            }],
            system_instruction: None,
            generation_config: GenerationConfig {
                temperature: 0.7,
                max_output_tokens: 8192,
            },
        }
    }

    #[test]
    fn normalize_non_empty_trims_and_filters() {
        assert_eq!(
            GeminiProvider::normalize_non_empty(" value "),
            Some("value".into())
        );
        assert_eq!(GeminiProvider::normalize_non_empty(""), None);
        assert_eq!(GeminiProvider::normalize_non_empty(" \t\n"), None);
    }

    #[test]
    fn provider_creates_without_key() {
        let provider = GeminiProvider::new(None);
        // May pick up env vars; just verify it doesn't panic
        let _ = provider.auth_source();
    }

    #[test]
    fn provider_creates_with_key() {
        let provider = GeminiProvider::new(Some("test-api-key"));
        assert!(matches!(
            provider.auth,
            Some(GeminiAuth::ExplicitKey(ref key)) if key == "test-api-key"
        ));
    }

    #[test]
    fn provider_rejects_empty_key() {
        let provider = GeminiProvider::new(Some(""));
        assert!(!matches!(provider.auth, Some(GeminiAuth::ExplicitKey(_))));
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
    fn auth_source_explicit_key() {
        let provider = GeminiProvider {
            auth: Some(GeminiAuth::ExplicitKey("key".into())),
            antigravity_project_id: None,
            client: Client::new(),
        };
        assert_eq!(provider.auth_source(), "config");
    }

    #[test]
    fn auth_source_none_without_credentials() {
        let provider = GeminiProvider {
            auth: None,
            antigravity_project_id: None,
            client: Client::new(),
        };
        assert_eq!(provider.auth_source(), "none");
    }

    #[test]
    fn auth_source_oauth() {
        let provider = GeminiProvider {
            auth: Some(GeminiAuth::OAuthToken("ya29.mock".into())),
            antigravity_project_id: None,
            client: Client::new(),
        };
        assert_eq!(provider.auth_source(), "Gemini CLI OAuth");
    }

    #[test]
    fn model_name_formatting() {
        assert_eq!(
            GeminiProvider::format_model_name("gemini-2.0-flash"),
            "models/gemini-2.0-flash"
        );
        assert_eq!(
            GeminiProvider::format_model_name("models/gemini-1.5-pro"),
            "models/gemini-1.5-pro"
        );
    }

    #[test]
    fn api_key_url_includes_key_query_param() {
        let auth = GeminiAuth::ExplicitKey("api-key-123".into());
        let url = GeminiProvider::build_generate_content_url("gemini-2.0-flash", &auth);
        assert!(url.contains(":generateContent?key=api-key-123"));
    }

    #[test]
    fn oauth_url_uses_internal_endpoint() {
        let auth = GeminiAuth::OAuthToken("ya29.test-token".into());
        let url = GeminiProvider::build_generate_content_url("gemini-2.0-flash", &auth);
        assert!(url.starts_with("https://cloudcode-pa.googleapis.com/v1internal"));
        assert!(url.ends_with(":generateContent"));
        assert!(!url.contains("generativelanguage.googleapis.com"));
        assert!(!url.contains("?key="));
    }

    #[test]
    fn api_key_url_uses_public_endpoint() {
        let auth = GeminiAuth::ExplicitKey("api-key-123".into());
        let url = GeminiProvider::build_generate_content_url("gemini-2.0-flash", &auth);
        assert!(url.contains("generativelanguage.googleapis.com/v1beta"));
        assert!(url.contains("models/gemini-2.0-flash"));
    }

    #[test]
    fn oauth_request_uses_bearer_auth_header() {
        let provider = GeminiProvider {
            auth: Some(GeminiAuth::OAuthToken("ya29.mock-token".into())),
            antigravity_project_id: None,
            client: Client::new(),
        };
        let auth = GeminiAuth::OAuthToken("ya29.mock-token".into());
        let url = GeminiProvider::build_generate_content_url("gemini-2.0-flash", &auth);
        let body = make_test_request();

        let request = provider
            .build_generate_content_request(&auth, &url, &body, "gemini-2.0-flash")
            .build()
            .unwrap();

        assert_eq!(
            request
                .headers()
                .get(AUTHORIZATION)
                .and_then(|h| h.to_str().ok()),
            Some("Bearer ya29.mock-token")
        );
    }

    #[test]
    fn api_key_request_does_not_set_bearer_header() {
        let provider = GeminiProvider {
            auth: Some(GeminiAuth::ExplicitKey("api-key-123".into())),
            antigravity_project_id: None,
            client: Client::new(),
        };
        let auth = GeminiAuth::ExplicitKey("api-key-123".into());
        let url = GeminiProvider::build_generate_content_url("gemini-2.0-flash", &auth);
        let body = make_test_request();

        let request = provider
            .build_generate_content_request(&auth, &url, &body, "gemini-2.0-flash")
            .build()
            .unwrap();

        assert!(request.headers().get(AUTHORIZATION).is_none());
    }

    #[test]
    fn auth_source_antigravity() {
        let provider = GeminiProvider {
            auth: Some(GeminiAuth::AntigravityToken("ya29.test".into())),
            antigravity_project_id: None,
            client: Client::new(),
        };
        assert_eq!(provider.auth_source(), "Google Antigravity OAuth");
    }

    #[test]
    fn antigravity_url_uses_sandbox_endpoint_v1internal() {
        let auth = GeminiAuth::AntigravityToken("ya29.test-token".into());
        let url = GeminiProvider::build_generate_content_url("claude-opus-4-6-thinking", &auth);
        assert!(url.starts_with("https://daily-cloudcode-pa.sandbox.googleapis.com/v1internal"));
        assert!(url.ends_with(":generateContent"));
        // v1internal format: model is in the body, not the URL
        assert!(!url.contains("models/"));
        assert!(!url.contains("?key="));
    }

    #[test]
    fn antigravity_request_uses_internal_body_with_bearer() {
        let provider = GeminiProvider {
            auth: Some(GeminiAuth::AntigravityToken(
                "ya29.antigravity-token".into(),
            )),
            antigravity_project_id: None,
            client: Client::new(),
        };
        let auth = GeminiAuth::AntigravityToken("ya29.antigravity-token".into());
        let url = GeminiProvider::build_generate_content_url("claude-opus-4-6-thinking", &auth);
        let body = make_test_request();

        let request = provider
            .build_generate_content_request(&auth, &url, &body, "claude-opus-4-6-thinking")
            .build()
            .unwrap();

        // Verify Bearer auth header
        assert_eq!(
            request
                .headers()
                .get(AUTHORIZATION)
                .and_then(|h| h.to_str().ok()),
            Some("Bearer ya29.antigravity-token")
        );

        // Verify InternalGenerateContentRequest body (model in body)
        let req_body = request.body().unwrap().as_bytes().unwrap();
        let body_str = std::str::from_utf8(req_body).unwrap();
        assert!(body_str.contains("\"generationConfig\""));
        assert!(body_str.contains("\"contents\""));
        assert!(body_str.contains("\"model\":\"models/claude-opus-4-6-thinking\""));
    }

    #[test]
    fn antigravity_request_includes_project_when_configured() {
        let provider = GeminiProvider {
            auth: Some(GeminiAuth::AntigravityToken(
                "ya29.antigravity-token".into(),
            )),
            antigravity_project_id: Some("licensed-project-123".into()),
            client: Client::new(),
        };
        let auth = GeminiAuth::AntigravityToken("ya29.antigravity-token".into());
        let url = GeminiProvider::build_generate_content_url("gemini-3-flash", &auth);
        let body = make_test_request();

        let request = provider
            .build_generate_content_request(&auth, &url, &body, "gemini-3-flash")
            .build()
            .unwrap();

        let req_body = request.body().unwrap().as_bytes().unwrap();
        let body_str = std::str::from_utf8(req_body).unwrap();
        assert!(body_str.contains("\"project\":\"licensed-project-123\""));
    }

    #[test]
    fn with_antigravity_token_creates_antigravity_auth() {
        let provider = GeminiProvider::with_antigravity_token(Some("ya29.test"));
        assert!(matches!(
            provider.auth,
            Some(GeminiAuth::AntigravityToken(ref t)) if t == "ya29.test"
        ));
    }

    #[test]
    fn with_antigravity_token_rejects_empty() {
        let provider = GeminiProvider::with_antigravity_token(Some(""));
        assert!(provider.auth.is_none());
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
    fn internal_request_includes_model_nested_under_request() {
        let request = InternalGenerateContentRequest {
            model: "models/gemini-3-pro-preview".to_string(),
            project: None,
            request: InternalRequestBody {
                generation_config: GenerationConfig {
                    temperature: 0.7,
                    max_output_tokens: 8192,
                },
                contents: vec![Content {
                    role: Some("user".to_string()),
                    parts: vec![Part {
                        text: "Hello".to_string(),
                    }],
                }],
                system_instruction: None,
            },
        };

        let json = serde_json::to_string(&request).unwrap();
        // Model is at top level
        assert!(json.contains("\"model\":\"models/gemini-3-pro-preview\""));
        // Contents and generationConfig are nested under "request"
        assert!(json.contains("\"request\":{"));
        assert!(json.contains("\"role\":\"user\""));
        assert!(json.contains("\"temperature\":0.7"));
        // Must NOT have contents/generationConfig at top level
        assert!(
            !json.starts_with("{\"model\":\"models/gemini-3-pro-preview\",\"generationConfig\"")
        );
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

    #[test]
    fn convert_chat_messages_maps_roles_correctly() {
        let messages = vec![
            ChatMessage::system("Be helpful"),
            ChatMessage::user("Hello"),
            ChatMessage::assistant("Hi there"),
            ChatMessage::user("What is 2+2?"),
        ];
        let (system, contents) = GeminiProvider::convert_chat_messages(&messages);

        assert!(system.is_some());
        assert_eq!(system.unwrap().parts[0].text, "Be helpful");
        // 3 non-system messages: user, model (assistant), user
        assert_eq!(contents.len(), 3);
        assert_eq!(contents[0].role.as_deref(), Some("user"));
        assert_eq!(contents[1].role.as_deref(), Some("model"));
        assert_eq!(contents[2].role.as_deref(), Some("user"));
    }

    #[test]
    fn convert_chat_messages_tool_becomes_user() {
        let messages = vec![
            ChatMessage::user("Use a tool"),
            ChatMessage::tool("{\"result\": \"done\"}"),
        ];
        let (system, contents) = GeminiProvider::convert_chat_messages(&messages);

        assert!(system.is_none());
        assert_eq!(contents.len(), 2);
        assert_eq!(contents[0].role.as_deref(), Some("user"));
        assert_eq!(contents[1].role.as_deref(), Some("user"));
        assert_eq!(contents[1].parts[0].text, "{\"result\": \"done\"}");
    }

    #[test]
    fn convert_chat_messages_empty_returns_empty() {
        let (system, contents) = GeminiProvider::convert_chat_messages(&[]);
        assert!(system.is_none());
        assert!(contents.is_empty());
    }

    #[test]
    fn convert_chat_messages_multiple_system_uses_first() {
        let messages = vec![
            ChatMessage::system("First system"),
            ChatMessage::user("Hello"),
            ChatMessage::system("Second system"),
        ];
        let (system, contents) = GeminiProvider::convert_chat_messages(&messages);

        assert!(system.is_some());
        assert_eq!(system.unwrap().parts[0].text, "First system");
        // Second system message is ignored; only user message in contents
        assert_eq!(contents.len(), 1);
    }
}
