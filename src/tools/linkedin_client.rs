use crate::config::LinkedInImageConfig;
use anyhow::Context;
use reqwest::header::{HeaderMap, HeaderValue};
use reqwest::Method;
use serde_json::json;
use std::path::{Path, PathBuf};

const LINKEDIN_API_BASE: &str = "https://api.linkedin.com";
const LINKEDIN_OAUTH_TOKEN_URL: &str = "https://www.linkedin.com/oauth/v2/accessToken";
const LINKEDIN_REQUEST_TIMEOUT_SECS: u64 = 30;
const LINKEDIN_CONNECT_TIMEOUT_SECS: u64 = 10;

pub struct LinkedInClient {
    workspace_dir: PathBuf,
    api_version: String,
}

#[derive(Debug)]
pub struct LinkedInCredentials {
    pub client_id: String,
    pub client_secret: String,
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub person_id: String,
}

#[derive(Debug, serde::Serialize)]
pub struct PostSummary {
    pub id: String,
    pub text: String,
    pub created_at: String,
    pub visibility: String,
}

#[derive(Debug, serde::Serialize)]
pub struct ProfileInfo {
    pub id: String,
    pub name: String,
    pub headline: String,
}

#[derive(Debug, serde::Serialize)]
pub struct EngagementSummary {
    pub likes: u64,
    pub comments: u64,
    pub shares: u64,
}

impl LinkedInClient {
    pub fn new(workspace_dir: PathBuf, api_version: String) -> Self {
        Self {
            workspace_dir,
            api_version,
        }
    }

    fn parse_env_value(raw: &str) -> String {
        let raw = raw.trim();

        let unquoted = if raw.len() >= 2
            && ((raw.starts_with('"') && raw.ends_with('"'))
                || (raw.starts_with('\'') && raw.ends_with('\'')))
        {
            &raw[1..raw.len() - 1]
        } else {
            raw
        };

        // Strip inline comments in unquoted values: KEY=value # comment
        unquoted.split_once(" #").map_or_else(
            || unquoted.trim().to_string(),
            |(value, _)| value.trim().to_string(),
        )
    }

    pub async fn get_credentials(&self) -> anyhow::Result<LinkedInCredentials> {
        let env_path = self.workspace_dir.join(".env");
        let content = tokio::fs::read_to_string(&env_path)
            .await
            .with_context(|| format!("Failed to read {}", env_path.display()))?;

        let mut client_id = None;
        let mut client_secret = None;
        let mut access_token = None;
        let mut refresh_token = None;
        let mut person_id = None;

        for line in content.lines() {
            let line = line.trim();
            if line.starts_with('#') || line.is_empty() {
                continue;
            }
            let line = line.strip_prefix("export ").map(str::trim).unwrap_or(line);
            if let Some((key, value)) = line.split_once('=') {
                let key = key.trim();
                let value = Self::parse_env_value(value);

                match key {
                    "LINKEDIN_CLIENT_ID" => client_id = Some(value),
                    "LINKEDIN_CLIENT_SECRET" => client_secret = Some(value),
                    "LINKEDIN_ACCESS_TOKEN" => access_token = Some(value),
                    "LINKEDIN_REFRESH_TOKEN" => {
                        if !value.is_empty() {
                            refresh_token = Some(value);
                        }
                    }
                    "LINKEDIN_PERSON_ID" => person_id = Some(value),
                    _ => {}
                }
            }
        }

        let client_id =
            client_id.ok_or_else(|| anyhow::anyhow!("LINKEDIN_CLIENT_ID not found in .env"))?;
        let client_secret = client_secret
            .ok_or_else(|| anyhow::anyhow!("LINKEDIN_CLIENT_SECRET not found in .env"))?;
        let access_token = access_token
            .ok_or_else(|| anyhow::anyhow!("LINKEDIN_ACCESS_TOKEN not found in .env"))?;
        let person_id =
            person_id.ok_or_else(|| anyhow::anyhow!("LINKEDIN_PERSON_ID not found in .env"))?;

        Ok(LinkedInCredentials {
            client_id,
            client_secret,
            access_token,
            refresh_token,
            person_id,
        })
    }

    fn client() -> reqwest::Client {
        crate::config::build_runtime_proxy_client_with_timeouts(
            "tool.linkedin",
            LINKEDIN_REQUEST_TIMEOUT_SECS,
            LINKEDIN_CONNECT_TIMEOUT_SECS,
        )
    }

    fn api_headers(&self, token: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        let bearer = format!("Bearer {}", token);
        headers.insert(
            reqwest::header::AUTHORIZATION,
            HeaderValue::from_str(&bearer).expect("valid bearer token header"),
        );
        headers.insert(
            "LinkedIn-Version",
            HeaderValue::from_str(&self.api_version).expect("valid api version header"),
        );
        headers.insert(
            "X-Restli-Protocol-Version",
            HeaderValue::from_static("2.0.0"),
        );
        headers
    }

    async fn api_request(
        &self,
        method: Method,
        url: &str,
        token: &str,
        body: Option<serde_json::Value>,
    ) -> anyhow::Result<reqwest::Response> {
        let client = Self::client();
        let headers = self.api_headers(token);

        let mut req = client.request(method.clone(), url).headers(headers);
        if let Some(ref json_body) = body {
            req = req.json(json_body);
        }

        let response = req.send().await.context("LinkedIn API request failed")?;

        if response.status() == reqwest::StatusCode::UNAUTHORIZED {
            // Attempt token refresh and retry once
            let creds = self.get_credentials().await?;
            let new_token = self.refresh_token(&creds).await?;
            self.update_env_token(&new_token).await?;

            let retry_headers = self.api_headers(&new_token);
            let mut retry_req = Self::client().request(method, url).headers(retry_headers);
            if let Some(json_body) = body {
                retry_req = retry_req.json(&json_body);
            }

            let retry_response = retry_req
                .send()
                .await
                .context("LinkedIn API retry request failed")?;

            return Ok(retry_response);
        }

        Ok(response)
    }

    pub async fn create_post(
        &self,
        text: &str,
        visibility: &str,
        article_url: Option<&str>,
        article_title: Option<&str>,
    ) -> anyhow::Result<String> {
        let creds = self.get_credentials().await?;
        let author_urn = format!("urn:li:person:{}", creds.person_id);

        let mut body = json!({
            "author": author_urn,
            "lifecycleState": "PUBLISHED",
            "visibility": visibility,
            "commentary": text,
            "distribution": {
                "feedDistribution": "MAIN_FEED",
                "targetEntities": [],
                "thirdPartyDistributionChannels": []
            }
        });

        if let Some(url) = article_url {
            let mut article = json!({
                "source": url,
                "title": article_title.unwrap_or(""),
            });
            if article_title.is_none() || article_title.map_or(false, |t| t.is_empty()) {
                article.as_object_mut().unwrap().remove("title");
            }
            body.as_object_mut().unwrap().insert(
                "content".to_string(),
                json!({
                    "article": {
                        "source": url,
                        "title": article_title.unwrap_or("")
                    }
                }),
            );
        }

        let url = format!("{}/rest/posts", LINKEDIN_API_BASE);
        let response = self
            .api_request(Method::POST, &url, &creds.access_token, Some(body))
            .await?;

        let status = response.status();
        if !status.is_success() {
            let body_text = response.text().await.unwrap_or_default();
            anyhow::bail!("LinkedIn create_post failed ({}): {}", status, body_text);
        }

        // The post URN is returned in the x-restli-id header
        let post_urn = response
            .headers()
            .get("x-restli-id")
            .and_then(|v| v.to_str().ok())
            .map(String::from)
            .unwrap_or_default();

        Ok(post_urn)
    }

    pub async fn list_posts(&self, count: usize) -> anyhow::Result<Vec<PostSummary>> {
        let creds = self.get_credentials().await?;
        let author_urn = format!("urn:li:person:{}", creds.person_id);
        let url = format!(
            "{}/rest/posts?author={}&q=author&count={}",
            LINKEDIN_API_BASE, author_urn, count
        );

        let response = self
            .api_request(Method::GET, &url, &creds.access_token, None)
            .await?;

        let status = response.status();
        if !status.is_success() {
            let body_text = response.text().await.unwrap_or_default();
            anyhow::bail!("LinkedIn list_posts failed ({}): {}", status, body_text);
        }

        let json: serde_json::Value = response
            .json()
            .await
            .context("Failed to parse list_posts response")?;

        let elements = json
            .get("elements")
            .and_then(|e| e.as_array())
            .cloned()
            .unwrap_or_default();

        let posts = elements
            .iter()
            .map(|el| PostSummary {
                id: el
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
                text: el
                    .get("commentary")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
                created_at: el
                    .get("createdAt")
                    .and_then(|v| v.as_u64())
                    .map(|ts| ts.to_string())
                    .unwrap_or_default(),
                visibility: el
                    .get("visibility")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
            })
            .collect();

        Ok(posts)
    }

    pub async fn add_comment(&self, post_id: &str, text: &str) -> anyhow::Result<String> {
        let creds = self.get_credentials().await?;
        let actor_urn = format!("urn:li:person:{}", creds.person_id);
        let url = format!(
            "{}/rest/socialActions/{}/comments",
            LINKEDIN_API_BASE, post_id
        );

        let body = json!({
            "actor": actor_urn,
            "message": {
                "text": text
            }
        });

        let response = self
            .api_request(Method::POST, &url, &creds.access_token, Some(body))
            .await?;

        let status = response.status();
        if !status.is_success() {
            let body_text = response.text().await.unwrap_or_default();
            anyhow::bail!("LinkedIn add_comment failed ({}): {}", status, body_text);
        }

        let json: serde_json::Value = response
            .json()
            .await
            .context("Failed to parse add_comment response")?;

        let comment_id = json
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();

        Ok(comment_id)
    }

    pub async fn add_reaction(&self, post_id: &str, reaction_type: &str) -> anyhow::Result<()> {
        let creds = self.get_credentials().await?;
        let actor_urn = format!("urn:li:person:{}", creds.person_id);
        let url = format!("{}/rest/reactions?actor={}", LINKEDIN_API_BASE, actor_urn);

        let body = json!({
            "reactionType": reaction_type,
            "object": post_id
        });

        let response = self
            .api_request(Method::POST, &url, &creds.access_token, Some(body))
            .await?;

        let status = response.status();
        if !status.is_success() {
            let body_text = response.text().await.unwrap_or_default();
            anyhow::bail!("LinkedIn add_reaction failed ({}): {}", status, body_text);
        }

        Ok(())
    }

    pub async fn delete_post(&self, post_id: &str) -> anyhow::Result<()> {
        let creds = self.get_credentials().await?;
        let url = format!("{}/rest/posts/{}", LINKEDIN_API_BASE, post_id);

        let response = self
            .api_request(Method::DELETE, &url, &creds.access_token, None)
            .await?;

        let status = response.status();
        if !status.is_success() {
            let body_text = response.text().await.unwrap_or_default();
            anyhow::bail!("LinkedIn delete_post failed ({}): {}", status, body_text);
        }

        Ok(())
    }

    pub async fn get_engagement(&self, post_id: &str) -> anyhow::Result<EngagementSummary> {
        let creds = self.get_credentials().await?;
        let url = format!("{}/rest/socialActions/{}", LINKEDIN_API_BASE, post_id);

        let response = self
            .api_request(Method::GET, &url, &creds.access_token, None)
            .await?;

        let status = response.status();
        if !status.is_success() {
            let body_text = response.text().await.unwrap_or_default();
            anyhow::bail!("LinkedIn get_engagement failed ({}): {}", status, body_text);
        }

        let json: serde_json::Value = response
            .json()
            .await
            .context("Failed to parse get_engagement response")?;

        let likes = json
            .get("likesSummary")
            .and_then(|v| v.get("totalLikes"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0);

        let comments = json
            .get("commentsSummary")
            .and_then(|v| v.get("totalFirstLevelComments"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0);

        let shares = json
            .get("sharesSummary")
            .and_then(|v| v.get("totalShares"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0);

        Ok(EngagementSummary {
            likes,
            comments,
            shares,
        })
    }

    pub async fn get_profile(&self) -> anyhow::Result<ProfileInfo> {
        let creds = self.get_credentials().await?;
        let url = format!("{}/rest/me", LINKEDIN_API_BASE);

        let response = self
            .api_request(Method::GET, &url, &creds.access_token, None)
            .await?;

        let status = response.status();
        if !status.is_success() {
            let body_text = response.text().await.unwrap_or_default();
            anyhow::bail!("LinkedIn get_profile failed ({}): {}", status, body_text);
        }

        let json: serde_json::Value = response
            .json()
            .await
            .context("Failed to parse get_profile response")?;

        let id = json
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();

        let first_name = json
            .get("localizedFirstName")
            .and_then(|v| v.as_str())
            .unwrap_or_default();

        let last_name = json
            .get("localizedLastName")
            .and_then(|v| v.as_str())
            .unwrap_or_default();

        let name = format!("{} {}", first_name, last_name).trim().to_string();

        let headline = json
            .get("localizedHeadline")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();

        Ok(ProfileInfo { id, name, headline })
    }

    async fn refresh_token(&self, creds: &LinkedInCredentials) -> anyhow::Result<String> {
        let refresh = creds
            .refresh_token
            .as_deref()
            .filter(|t| !t.is_empty())
            .ok_or_else(|| anyhow::anyhow!("No refresh token available"))?;

        let client = Self::client();
        let response = client
            .post(LINKEDIN_OAUTH_TOKEN_URL)
            .form(&[
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh),
                ("client_id", &creds.client_id),
                ("client_secret", &creds.client_secret),
            ])
            .send()
            .await
            .context("LinkedIn token refresh request failed")?;

        let status = response.status();
        if !status.is_success() {
            let body_text = response.text().await.unwrap_or_default();
            anyhow::bail!("LinkedIn token refresh failed ({}): {}", status, body_text);
        }

        let json: serde_json::Value = response
            .json()
            .await
            .context("Failed to parse token refresh response")?;

        let new_token = json
            .get("access_token")
            .and_then(|v| v.as_str())
            .map(String::from)
            .ok_or_else(|| anyhow::anyhow!("Token refresh response missing access_token field"))?;

        Ok(new_token)
    }

    /// Register an image asset with LinkedIn, upload binary data, and return the asset URN.
    ///
    /// LinkedIn's image post flow is three steps:
    /// 1. Register the upload → get an upload URL + asset URN
    /// 2. PUT the binary image to the upload URL
    /// 3. Reference the asset URN when creating the post
    pub async fn upload_image(
        &self,
        image_bytes: &[u8],
        token: &str,
        person_id: &str,
    ) -> anyhow::Result<String> {
        let owner_urn = format!("urn:li:person:{person_id}");

        // Step 1: Register upload
        let register_body = json!({
            "initializeUploadRequest": {
                "owner": owner_urn
            }
        });
        let register_url = format!("{LINKEDIN_API_BASE}/rest/images?action=initializeUpload");
        let register_resp = self
            .api_request(Method::POST, &register_url, token, Some(register_body))
            .await?;

        let status = register_resp.status();
        if !status.is_success() {
            let body_text = register_resp.text().await.unwrap_or_default();
            anyhow::bail!("LinkedIn image register failed ({status}): {body_text}");
        }

        let register_json: serde_json::Value = register_resp
            .json()
            .await
            .context("Failed to parse image register response")?;

        let upload_url = register_json
            .pointer("/value/uploadUrl")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing uploadUrl in register response"))?
            .to_string();

        let image_urn = register_json
            .pointer("/value/image")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing image URN in register response"))?
            .to_string();

        // Step 2: Upload binary
        let client = Self::client();
        let mut upload_headers = HeaderMap::new();
        upload_headers.insert(
            reqwest::header::AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {token}")).expect("valid bearer token header"),
        );

        let upload_resp = client
            .put(&upload_url)
            .headers(upload_headers)
            .header("Content-Type", "image/png")
            .body(image_bytes.to_vec())
            .send()
            .await
            .context("LinkedIn image upload failed")?;

        let upload_status = upload_resp.status();
        if !upload_status.is_success() {
            let body_text = upload_resp.text().await.unwrap_or_default();
            anyhow::bail!("LinkedIn image upload failed ({upload_status}): {body_text}");
        }

        Ok(image_urn)
    }

    /// Create a post with an attached image.
    pub async fn create_post_with_image(
        &self,
        text: &str,
        visibility: &str,
        image_urn: &str,
    ) -> anyhow::Result<String> {
        let creds = self.get_credentials().await?;
        let author_urn = format!("urn:li:person:{}", creds.person_id);

        let body = json!({
            "author": author_urn,
            "lifecycleState": "PUBLISHED",
            "visibility": visibility,
            "commentary": text,
            "distribution": {
                "feedDistribution": "MAIN_FEED",
                "targetEntities": [],
                "thirdPartyDistributionChannels": []
            },
            "content": {
                "media": {
                    "id": image_urn
                }
            }
        });

        let url = format!("{LINKEDIN_API_BASE}/rest/posts");
        let response = self
            .api_request(Method::POST, &url, &creds.access_token, Some(body))
            .await?;

        let status = response.status();
        if !status.is_success() {
            let body_text = response.text().await.unwrap_or_default();
            anyhow::bail!("LinkedIn create_post_with_image failed ({status}): {body_text}");
        }

        let post_urn = response
            .headers()
            .get("x-restli-id")
            .and_then(|v| v.to_str().ok())
            .map(String::from)
            .unwrap_or_default();

        Ok(post_urn)
    }

    async fn update_env_token(&self, new_token: &str) -> anyhow::Result<()> {
        let env_path = self.workspace_dir.join(".env");
        let content = tokio::fs::read_to_string(&env_path)
            .await
            .with_context(|| format!("Failed to read {}", env_path.display()))?;

        let mut updated_lines: Vec<String> = Vec::new();
        let mut found = false;

        for line in content.lines() {
            let trimmed = line.trim();

            // Detect the LINKEDIN_ACCESS_TOKEN line (with or without export prefix)
            let is_token_line = if trimmed.starts_with('#') || trimmed.is_empty() {
                false
            } else {
                let check = trimmed
                    .strip_prefix("export ")
                    .map(str::trim)
                    .unwrap_or(trimmed);
                check
                    .split_once('=')
                    .map_or(false, |(key, _)| key.trim() == "LINKEDIN_ACCESS_TOKEN")
            };

            if is_token_line {
                // Preserve the export prefix and quoting style
                let has_export = trimmed.starts_with("export ");
                let after_key = trimmed.strip_prefix("export ").unwrap_or(trimmed).trim();
                let (_key, old_val) = after_key
                    .split_once('=')
                    .unwrap_or(("LINKEDIN_ACCESS_TOKEN", ""));
                let old_val = old_val.trim();

                let new_val = if old_val.starts_with('"') {
                    format!("\"{}\"", new_token)
                } else if old_val.starts_with('\'') {
                    format!("'{}'", new_token)
                } else {
                    new_token.to_string()
                };

                let new_line = if has_export {
                    format!("export LINKEDIN_ACCESS_TOKEN={}", new_val)
                } else {
                    format!("LINKEDIN_ACCESS_TOKEN={}", new_val)
                };

                updated_lines.push(new_line);
                found = true;
            } else {
                updated_lines.push(line.to_string());
            }
        }

        if !found {
            anyhow::bail!("LINKEDIN_ACCESS_TOKEN not found in .env for update");
        }

        // Preserve trailing newline if original had one
        let mut output = updated_lines.join("\n");
        if content.ends_with('\n') {
            output.push('\n');
        }

        tokio::fs::write(&env_path, &output)
            .await
            .with_context(|| format!("Failed to write {}", env_path.display()))?;

        Ok(())
    }
}

// ── Image Generation ─────────────────────────────────────────────

/// Multi-provider image generator with SVG fallback card.
///
/// Tries AI providers in configured priority order. If all fail (missing keys,
/// API errors, exhausted credits), falls back to generating a branded SVG card.
pub struct ImageGenerator {
    config: LinkedInImageConfig,
    workspace_dir: PathBuf,
}

impl ImageGenerator {
    pub fn new(config: LinkedInImageConfig, workspace_dir: PathBuf) -> Self {
        Self {
            config,
            workspace_dir,
        }
    }

    /// Generate an image for the given prompt text. Returns the path to the saved PNG/SVG file.
    pub async fn generate(&self, prompt: &str) -> anyhow::Result<PathBuf> {
        let image_dir = self.workspace_dir.join(&self.config.temp_dir);
        tokio::fs::create_dir_all(&image_dir).await?;

        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let base_name = format!("post_{timestamp}");

        // Try each configured provider in order
        for provider_name in &self.config.providers {
            let result = match provider_name.as_str() {
                "stability" => self.try_stability(prompt, &image_dir, &base_name).await,
                "imagen" => self.try_imagen(prompt, &image_dir, &base_name).await,
                "dalle" => self.try_dalle(prompt, &image_dir, &base_name).await,
                "flux" => self.try_flux(prompt, &image_dir, &base_name).await,
                other => {
                    tracing::warn!("Unknown image provider '{other}', skipping");
                    continue;
                }
            };

            match result {
                Ok(path) => {
                    tracing::info!("Image generated via {provider_name}: {}", path.display());
                    return Ok(path);
                }
                Err(e) => {
                    tracing::warn!("Image provider '{provider_name}' failed: {e}");
                }
            }
        }

        // All AI providers failed — try SVG fallback
        if self.config.fallback_card {
            let svg_path = image_dir.join(format!("{base_name}.svg"));
            let svg_content = Self::generate_fallback_card(prompt, &self.config.card_accent_color);
            tokio::fs::write(&svg_path, &svg_content).await?;
            tracing::info!("Fallback SVG card generated: {}", svg_path.display());
            return Ok(svg_path);
        }

        anyhow::bail!("All image generation providers failed and fallback_card is disabled")
    }

    /// Read an env var value from the workspace .env file (same format as LinkedInClient).
    async fn read_env_var(workspace_dir: &Path, var_name: &str) -> anyhow::Result<String> {
        let env_path = workspace_dir.join(".env");
        let content = tokio::fs::read_to_string(&env_path)
            .await
            .with_context(|| format!("Failed to read {}", env_path.display()))?;

        for line in content.lines() {
            let line = line.trim();
            if line.starts_with('#') || line.is_empty() {
                continue;
            }
            let line = line.strip_prefix("export ").map(str::trim).unwrap_or(line);
            if let Some((key, value)) = line.split_once('=') {
                if key.trim() == var_name {
                    let val = LinkedInClient::parse_env_value(value);
                    if !val.is_empty() {
                        return Ok(val);
                    }
                }
            }
        }

        anyhow::bail!("{var_name} not found or empty in .env")
    }

    fn http_client() -> reqwest::Client {
        crate::config::build_runtime_proxy_client_with_timeouts(
            "tool.linkedin.image",
            60, // image gen can be slow
            10,
        )
    }

    // ── Stability AI ────────────────────────────────────────────

    async fn try_stability(
        &self,
        prompt: &str,
        output_dir: &Path,
        base_name: &str,
    ) -> anyhow::Result<PathBuf> {
        let api_key =
            Self::read_env_var(&self.workspace_dir, &self.config.stability.api_key_env).await?;

        let client = Self::http_client();
        let url = format!(
            "https://api.stability.ai/v1/generation/{}/text-to-image",
            self.config.stability.model
        );

        let body = json!({
            "text_prompts": [{"text": prompt, "weight": 1.0}],
            "cfg_scale": 7,
            "height": 1024,
            "width": 1024,
            "samples": 1,
            "steps": 30
        });

        let resp = client
            .post(&url)
            .header("Authorization", format!("Bearer {api_key}"))
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .json(&body)
            .send()
            .await
            .context("Stability AI request failed")?;

        let status = resp.status();
        if !status.is_success() {
            let body_text = resp.text().await.unwrap_or_default();
            anyhow::bail!("Stability AI failed ({status}): {body_text}");
        }

        let json: serde_json::Value = resp.json().await?;
        let b64 = json
            .pointer("/artifacts/0/base64")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("No image data in Stability response"))?;

        let bytes = base64_decode(b64)?;
        let path = output_dir.join(format!("{base_name}_stability.png"));
        tokio::fs::write(&path, &bytes).await?;
        Ok(path)
    }

    // ── Google Imagen (Vertex AI) ───────────────────────────────

    async fn try_imagen(
        &self,
        prompt: &str,
        output_dir: &Path,
        base_name: &str,
    ) -> anyhow::Result<PathBuf> {
        let api_key =
            Self::read_env_var(&self.workspace_dir, &self.config.imagen.api_key_env).await?;
        let project_id =
            Self::read_env_var(&self.workspace_dir, &self.config.imagen.project_id_env).await?;

        let client = Self::http_client();
        let url = format!(
            "https://{}-aiplatform.googleapis.com/v1/projects/{}/locations/{}/publishers/google/models/imagen-3.0-generate-001:predict",
            self.config.imagen.region, project_id, self.config.imagen.region
        );

        let body = json!({
            "instances": [{"prompt": prompt}],
            "parameters": {
                "sampleCount": 1,
                "aspectRatio": "1:1"
            }
        });

        let resp = client
            .post(&url)
            .header("Authorization", format!("Bearer {api_key}"))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .context("Imagen request failed")?;

        let status = resp.status();
        if !status.is_success() {
            let body_text = resp.text().await.unwrap_or_default();
            anyhow::bail!("Imagen failed ({status}): {body_text}");
        }

        let json: serde_json::Value = resp.json().await?;
        let b64 = json
            .pointer("/predictions/0/bytesBase64Encoded")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("No image data in Imagen response"))?;

        let bytes = base64_decode(b64)?;
        let path = output_dir.join(format!("{base_name}_imagen.png"));
        tokio::fs::write(&path, &bytes).await?;
        Ok(path)
    }

    // ── OpenAI DALL-E ───────────────────────────────────────────

    async fn try_dalle(
        &self,
        prompt: &str,
        output_dir: &Path,
        base_name: &str,
    ) -> anyhow::Result<PathBuf> {
        let api_key =
            Self::read_env_var(&self.workspace_dir, &self.config.dalle.api_key_env).await?;

        let client = Self::http_client();
        let url = "https://api.openai.com/v1/images/generations";

        let body = json!({
            "model": self.config.dalle.model,
            "prompt": prompt,
            "n": 1,
            "size": self.config.dalle.size,
            "response_format": "b64_json"
        });

        let resp = client
            .post(url)
            .header("Authorization", format!("Bearer {api_key}"))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .context("DALL-E request failed")?;

        let status = resp.status();
        if !status.is_success() {
            let body_text = resp.text().await.unwrap_or_default();
            anyhow::bail!("DALL-E failed ({status}): {body_text}");
        }

        let json: serde_json::Value = resp.json().await?;
        let b64 = json
            .pointer("/data/0/b64_json")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("No image data in DALL-E response"))?;

        let bytes = base64_decode(b64)?;
        let path = output_dir.join(format!("{base_name}_dalle.png"));
        tokio::fs::write(&path, &bytes).await?;
        Ok(path)
    }

    // ── Flux (fal.ai) ──────────────────────────────────────────

    async fn try_flux(
        &self,
        prompt: &str,
        output_dir: &Path,
        base_name: &str,
    ) -> anyhow::Result<PathBuf> {
        let api_key =
            Self::read_env_var(&self.workspace_dir, &self.config.flux.api_key_env).await?;

        let client = Self::http_client();
        let url = format!("https://fal.run/{}", self.config.flux.model);

        let body = json!({
            "prompt": prompt,
            "image_size": "square_hd",
            "num_images": 1
        });

        let resp = client
            .post(&url)
            .header("Authorization", format!("Key {api_key}"))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .context("Flux request failed")?;

        let status = resp.status();
        if !status.is_success() {
            let body_text = resp.text().await.unwrap_or_default();
            anyhow::bail!("Flux failed ({status}): {body_text}");
        }

        let json: serde_json::Value = resp.json().await?;
        let image_url = json
            .pointer("/images/0/url")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("No image URL in Flux response"))?;

        // Download the image from the returned URL
        let img_resp = client.get(image_url).send().await?;
        if !img_resp.status().is_success() {
            anyhow::bail!("Failed to download Flux image from {image_url}");
        }
        let bytes = img_resp.bytes().await?;
        let path = output_dir.join(format!("{base_name}_flux.png"));
        tokio::fs::write(&path, &bytes).await?;
        Ok(path)
    }

    // ── SVG Fallback Card ───────────────────────────────────────

    /// Generate a branded SVG text card with the post title on a gradient background.
    pub fn generate_fallback_card(title: &str, accent_color: &str) -> String {
        // Truncate title to ~80 chars for clean display
        let display_title = if title.len() > 80 {
            format!("{}...", &title[..77])
        } else {
            title.to_string()
        };

        // Word-wrap at ~35 chars per line, max 3 lines
        let lines = word_wrap(&display_title, 35, 3);
        let line_height: i32 = 48;
        // lines.len() is capped at max_lines=3, so this cast is safe
        #[allow(clippy::cast_possible_truncation)]
        let line_count: i32 = lines.len() as i32;
        let total_text_height = line_count * line_height;
        let start_y = (1024 - total_text_height) / 2 + 24;

        let font = "system-ui, sans-serif";
        let text_elements: String = lines
            .iter()
            .enumerate()
            .map(|(i, line)| {
                #[allow(clippy::cast_possible_truncation)]
                let y = start_y + (i as i32 * line_height); // i is max 2, safe
                format!(
                    "    <text x=\"512\" y=\"{y}\" text-anchor=\"middle\" fill=\"white\" \
                     font-family=\"{font}\" font-size=\"36\" font-weight=\"600\">{}</text>",
                    xml_escape(line)
                )
            })
            .collect::<Vec<_>>()
            .join("\n");

        format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"1024\" height=\"1024\" \
             viewBox=\"0 0 1024 1024\">\n\
             \x20 <defs>\n\
             \x20   <linearGradient id=\"bg\" x1=\"0\" y1=\"0\" x2=\"1\" y2=\"1\">\n\
             \x20     <stop offset=\"0%\" stop-color=\"{accent_color}\"/>\n\
             \x20     <stop offset=\"100%\" stop-color=\"#1a1a2e\"/>\n\
             \x20   </linearGradient>\n\
             \x20 </defs>\n\
             \x20 <rect width=\"1024\" height=\"1024\" fill=\"url(#bg)\" rx=\"0\"/>\n\
             \x20 <rect x=\"60\" y=\"60\" width=\"904\" height=\"904\" rx=\"24\" \
             fill=\"none\" stroke=\"rgba(255,255,255,0.15)\" stroke-width=\"2\"/>\n\
             {text_elements}\n\
             \x20 <text x=\"512\" y=\"920\" text-anchor=\"middle\" \
             fill=\"rgba(255,255,255,0.5)\" font-family=\"{font}\" \
             font-size=\"18\">ZeroClaw</text>\n\
             </svg>"
        )
    }

    /// Clean up a generated image file after successful upload.
    pub async fn cleanup(path: &Path) -> anyhow::Result<()> {
        if path.exists() {
            tokio::fs::remove_file(path).await?;
        }
        Ok(())
    }
}

/// Decode a base64-encoded string to bytes.
fn base64_decode(input: &str) -> anyhow::Result<Vec<u8>> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(input)
        .context("Failed to decode base64 image data")
}

/// Simple word-wrap: break text into lines of at most `max_width` chars, capped at `max_lines`.
fn word_wrap(text: &str, max_width: usize, max_lines: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current_line = String::new();

    for word in text.split_whitespace() {
        if current_line.is_empty() {
            current_line = word.to_string();
        } else if current_line.len() + 1 + word.len() <= max_width {
            current_line.push(' ');
            current_line.push_str(word);
        } else {
            lines.push(current_line);
            current_line = word.to_string();
            if lines.len() >= max_lines {
                break;
            }
        }
    }

    if !current_line.is_empty() && lines.len() < max_lines {
        lines.push(current_line);
    }

    lines
}

/// Escape XML special characters for SVG text content.
fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[tokio::test]
    async fn credentials_parsed_plain_values() {
        let tmp = TempDir::new().unwrap();
        let env_path = tmp.path().join(".env");
        fs::write(
            &env_path,
            "LINKEDIN_CLIENT_ID=cid123\n\
             LINKEDIN_CLIENT_SECRET=csecret456\n\
             LINKEDIN_ACCESS_TOKEN=tok789\n\
             LINKEDIN_PERSON_ID=person001\n",
        )
        .unwrap();

        let client = LinkedInClient::new(tmp.path().to_path_buf(), "202602".to_string());
        let creds = client.get_credentials().await.unwrap();

        assert_eq!(creds.client_id, "cid123");
        assert_eq!(creds.client_secret, "csecret456");
        assert_eq!(creds.access_token, "tok789");
        assert_eq!(creds.person_id, "person001");
        assert!(creds.refresh_token.is_none());
    }

    #[tokio::test]
    async fn credentials_parsed_with_double_quotes() {
        let tmp = TempDir::new().unwrap();
        let env_path = tmp.path().join(".env");
        fs::write(
            &env_path,
            "LINKEDIN_CLIENT_ID=\"cid_quoted\"\n\
             LINKEDIN_CLIENT_SECRET=\"csecret_quoted\"\n\
             LINKEDIN_ACCESS_TOKEN=\"tok_quoted\"\n\
             LINKEDIN_PERSON_ID=\"person_quoted\"\n",
        )
        .unwrap();

        let client = LinkedInClient::new(tmp.path().to_path_buf(), "202602".to_string());
        let creds = client.get_credentials().await.unwrap();

        assert_eq!(creds.client_id, "cid_quoted");
        assert_eq!(creds.client_secret, "csecret_quoted");
        assert_eq!(creds.access_token, "tok_quoted");
        assert_eq!(creds.person_id, "person_quoted");
    }

    #[tokio::test]
    async fn credentials_parsed_with_single_quotes() {
        let tmp = TempDir::new().unwrap();
        let env_path = tmp.path().join(".env");
        fs::write(
            &env_path,
            "LINKEDIN_CLIENT_ID='cid_sq'\n\
             LINKEDIN_CLIENT_SECRET='csecret_sq'\n\
             LINKEDIN_ACCESS_TOKEN='tok_sq'\n\
             LINKEDIN_PERSON_ID='person_sq'\n",
        )
        .unwrap();

        let client = LinkedInClient::new(tmp.path().to_path_buf(), "202602".to_string());
        let creds = client.get_credentials().await.unwrap();

        assert_eq!(creds.client_id, "cid_sq");
        assert_eq!(creds.access_token, "tok_sq");
    }

    #[tokio::test]
    async fn credentials_parsed_with_export_prefix() {
        let tmp = TempDir::new().unwrap();
        let env_path = tmp.path().join(".env");
        fs::write(
            &env_path,
            "export LINKEDIN_CLIENT_ID=cid_exp\n\
             export LINKEDIN_CLIENT_SECRET=\"csecret_exp\"\n\
             export LINKEDIN_ACCESS_TOKEN='tok_exp'\n\
             export LINKEDIN_PERSON_ID=person_exp\n",
        )
        .unwrap();

        let client = LinkedInClient::new(tmp.path().to_path_buf(), "202602".to_string());
        let creds = client.get_credentials().await.unwrap();

        assert_eq!(creds.client_id, "cid_exp");
        assert_eq!(creds.client_secret, "csecret_exp");
        assert_eq!(creds.access_token, "tok_exp");
        assert_eq!(creds.person_id, "person_exp");
    }

    #[tokio::test]
    async fn credentials_ignore_comments_and_blanks() {
        let tmp = TempDir::new().unwrap();
        let env_path = tmp.path().join(".env");
        fs::write(
            &env_path,
            "# LinkedIn credentials\n\
             \n\
             LINKEDIN_CLIENT_ID=cid_c\n\
             # secret below\n\
             LINKEDIN_CLIENT_SECRET=csecret_c\n\
             LINKEDIN_ACCESS_TOKEN=tok_c # inline comment\n\
             LINKEDIN_PERSON_ID=person_c\n",
        )
        .unwrap();

        let client = LinkedInClient::new(tmp.path().to_path_buf(), "202602".to_string());
        let creds = client.get_credentials().await.unwrap();

        assert_eq!(creds.client_id, "cid_c");
        assert_eq!(creds.client_secret, "csecret_c");
        assert_eq!(creds.access_token, "tok_c");
        assert_eq!(creds.person_id, "person_c");
    }

    #[tokio::test]
    async fn credentials_with_refresh_token() {
        let tmp = TempDir::new().unwrap();
        let env_path = tmp.path().join(".env");
        fs::write(
            &env_path,
            "LINKEDIN_CLIENT_ID=cid\n\
             LINKEDIN_CLIENT_SECRET=csecret\n\
             LINKEDIN_ACCESS_TOKEN=tok\n\
             LINKEDIN_REFRESH_TOKEN=refresh123\n\
             LINKEDIN_PERSON_ID=person\n",
        )
        .unwrap();

        let client = LinkedInClient::new(tmp.path().to_path_buf(), "202602".to_string());
        let creds = client.get_credentials().await.unwrap();

        assert_eq!(creds.refresh_token.as_deref(), Some("refresh123"));
    }

    #[tokio::test]
    async fn credentials_empty_refresh_token_becomes_none() {
        let tmp = TempDir::new().unwrap();
        let env_path = tmp.path().join(".env");
        fs::write(
            &env_path,
            "LINKEDIN_CLIENT_ID=cid\n\
             LINKEDIN_CLIENT_SECRET=csecret\n\
             LINKEDIN_ACCESS_TOKEN=tok\n\
             LINKEDIN_REFRESH_TOKEN=\n\
             LINKEDIN_PERSON_ID=person\n",
        )
        .unwrap();

        let client = LinkedInClient::new(tmp.path().to_path_buf(), "202602".to_string());
        let creds = client.get_credentials().await.unwrap();

        assert!(creds.refresh_token.is_none());
    }

    #[tokio::test]
    async fn credentials_fail_missing_client_id() {
        let tmp = TempDir::new().unwrap();
        let env_path = tmp.path().join(".env");
        fs::write(
            &env_path,
            "LINKEDIN_CLIENT_SECRET=csecret\n\
             LINKEDIN_ACCESS_TOKEN=tok\n\
             LINKEDIN_PERSON_ID=person\n",
        )
        .unwrap();

        let client = LinkedInClient::new(tmp.path().to_path_buf(), "202602".to_string());
        let err = client.get_credentials().await.unwrap_err();
        assert!(err.to_string().contains("LINKEDIN_CLIENT_ID"));
    }

    #[tokio::test]
    async fn credentials_fail_missing_access_token() {
        let tmp = TempDir::new().unwrap();
        let env_path = tmp.path().join(".env");
        fs::write(
            &env_path,
            "LINKEDIN_CLIENT_ID=cid\n\
             LINKEDIN_CLIENT_SECRET=csecret\n\
             LINKEDIN_PERSON_ID=person\n",
        )
        .unwrap();

        let client = LinkedInClient::new(tmp.path().to_path_buf(), "202602".to_string());
        let err = client.get_credentials().await.unwrap_err();
        assert!(err.to_string().contains("LINKEDIN_ACCESS_TOKEN"));
    }

    #[tokio::test]
    async fn credentials_fail_missing_person_id() {
        let tmp = TempDir::new().unwrap();
        let env_path = tmp.path().join(".env");
        fs::write(
            &env_path,
            "LINKEDIN_CLIENT_ID=cid\n\
             LINKEDIN_CLIENT_SECRET=csecret\n\
             LINKEDIN_ACCESS_TOKEN=tok\n",
        )
        .unwrap();

        let client = LinkedInClient::new(tmp.path().to_path_buf(), "202602".to_string());
        let err = client.get_credentials().await.unwrap_err();
        assert!(err.to_string().contains("LINKEDIN_PERSON_ID"));
    }

    #[tokio::test]
    async fn credentials_fail_no_env_file() {
        let tmp = TempDir::new().unwrap();
        let client = LinkedInClient::new(tmp.path().to_path_buf(), "202602".to_string());
        let err = client.get_credentials().await.unwrap_err();
        assert!(err.to_string().contains("Failed to read"));
    }

    #[tokio::test]
    async fn update_env_token_preserves_other_keys() {
        let tmp = TempDir::new().unwrap();
        let env_path = tmp.path().join(".env");
        fs::write(
            &env_path,
            "# Config\n\
             LINKEDIN_CLIENT_ID=cid\n\
             LINKEDIN_CLIENT_SECRET=csecret\n\
             LINKEDIN_ACCESS_TOKEN=old_token\n\
             LINKEDIN_PERSON_ID=person\n\
             OTHER_KEY=keepme\n",
        )
        .unwrap();

        let client = LinkedInClient::new(tmp.path().to_path_buf(), "202602".to_string());
        client.update_env_token("new_token_value").await.unwrap();

        let updated = fs::read_to_string(&env_path).unwrap();
        assert!(updated.contains("LINKEDIN_ACCESS_TOKEN=new_token_value"));
        assert!(updated.contains("LINKEDIN_CLIENT_ID=cid"));
        assert!(updated.contains("LINKEDIN_CLIENT_SECRET=csecret"));
        assert!(updated.contains("LINKEDIN_PERSON_ID=person"));
        assert!(updated.contains("OTHER_KEY=keepme"));
        assert!(updated.contains("# Config"));
        assert!(!updated.contains("old_token"));
    }

    #[tokio::test]
    async fn update_env_token_preserves_export_prefix() {
        let tmp = TempDir::new().unwrap();
        let env_path = tmp.path().join(".env");
        fs::write(
            &env_path,
            "export LINKEDIN_CLIENT_ID=cid\n\
             export LINKEDIN_CLIENT_SECRET=csecret\n\
             export LINKEDIN_ACCESS_TOKEN=\"old_tok\"\n\
             export LINKEDIN_PERSON_ID=person\n",
        )
        .unwrap();

        let client = LinkedInClient::new(tmp.path().to_path_buf(), "202602".to_string());
        client.update_env_token("refreshed_tok").await.unwrap();

        let updated = fs::read_to_string(&env_path).unwrap();
        assert!(updated.contains("export LINKEDIN_ACCESS_TOKEN=\"refreshed_tok\""));
        assert!(updated.contains("export LINKEDIN_CLIENT_ID=cid"));
    }

    #[tokio::test]
    async fn update_env_token_preserves_single_quote_style() {
        let tmp = TempDir::new().unwrap();
        let env_path = tmp.path().join(".env");
        fs::write(
            &env_path,
            "LINKEDIN_CLIENT_ID=cid\n\
             LINKEDIN_CLIENT_SECRET=csecret\n\
             LINKEDIN_ACCESS_TOKEN='old'\n\
             LINKEDIN_PERSON_ID=person\n",
        )
        .unwrap();

        let client = LinkedInClient::new(tmp.path().to_path_buf(), "202602".to_string());
        client.update_env_token("new_sq").await.unwrap();

        let updated = fs::read_to_string(&env_path).unwrap();
        assert!(updated.contains("LINKEDIN_ACCESS_TOKEN='new_sq'"));
    }

    #[tokio::test]
    async fn update_env_token_fails_if_key_missing() {
        let tmp = TempDir::new().unwrap();
        let env_path = tmp.path().join(".env");
        fs::write(
            &env_path,
            "LINKEDIN_CLIENT_ID=cid\n\
             LINKEDIN_PERSON_ID=person\n",
        )
        .unwrap();

        let client = LinkedInClient::new(tmp.path().to_path_buf(), "202602".to_string());
        let err = client.update_env_token("tok").await.unwrap_err();
        assert!(err.to_string().contains("LINKEDIN_ACCESS_TOKEN not found"));
    }

    #[test]
    fn parse_env_value_strips_double_quotes() {
        assert_eq!(LinkedInClient::parse_env_value("\"hello\""), "hello");
    }

    #[test]
    fn parse_env_value_strips_single_quotes() {
        assert_eq!(LinkedInClient::parse_env_value("'hello'"), "hello");
    }

    #[test]
    fn parse_env_value_strips_inline_comment() {
        assert_eq!(LinkedInClient::parse_env_value("value # comment"), "value");
    }

    #[test]
    fn parse_env_value_trims_whitespace() {
        assert_eq!(LinkedInClient::parse_env_value("  spaced  "), "spaced");
    }

    #[test]
    fn parse_env_value_plain() {
        assert_eq!(LinkedInClient::parse_env_value("plain"), "plain");
    }

    #[test]
    fn api_headers_contains_required_headers() {
        let tmp = TempDir::new().unwrap();
        let client = LinkedInClient::new(tmp.path().to_path_buf(), "202602".to_string());
        let headers = client.api_headers("test_token");
        assert_eq!(
            headers.get("Authorization").unwrap().to_str().unwrap(),
            "Bearer test_token"
        );
        assert_eq!(
            headers.get("LinkedIn-Version").unwrap().to_str().unwrap(),
            "202602"
        );
        assert_eq!(
            headers
                .get("X-Restli-Protocol-Version")
                .unwrap()
                .to_str()
                .unwrap(),
            "2.0.0"
        );
    }

    // ── Image Generation Tests ──────────────────────────────────

    #[test]
    fn fallback_card_contains_svg_structure() {
        let svg = ImageGenerator::generate_fallback_card("Test Title", "#0A66C2");
        assert!(svg.starts_with("<svg"));
        assert!(svg.contains("1024"));
        assert!(svg.contains("#0A66C2"));
        assert!(svg.contains("Test Title"));
        assert!(svg.contains("ZeroClaw"));
    }

    #[test]
    fn fallback_card_escapes_xml_characters() {
        let svg =
            ImageGenerator::generate_fallback_card("AI & ML <Trends> for \"2026\"", "#0A66C2");
        assert!(svg.contains("&amp;"));
        assert!(svg.contains("&lt;"));
        assert!(svg.contains("&gt;"));
        assert!(svg.contains("&quot;"));
        assert!(!svg.contains("& "));
    }

    #[test]
    fn fallback_card_truncates_long_titles() {
        let long_title = "A".repeat(100);
        let svg = ImageGenerator::generate_fallback_card(&long_title, "#0A66C2");
        assert!(svg.contains("..."));
        // Should not contain the full 100-char string
        assert!(!svg.contains(&long_title));
    }

    #[test]
    fn fallback_card_uses_custom_accent_color() {
        let svg = ImageGenerator::generate_fallback_card("Title", "#FF5733");
        assert!(svg.contains("#FF5733"));
        assert!(!svg.contains("#0A66C2"));
    }

    #[test]
    fn word_wrap_basic() {
        let lines = word_wrap("Hello world this is a test", 15, 3);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], "Hello world");
        assert_eq!(lines[1], "this is a test");
    }

    #[test]
    fn word_wrap_respects_max_lines() {
        let lines = word_wrap("one two three four five six seven eight", 10, 2);
        assert!(lines.len() <= 2);
    }

    #[test]
    fn word_wrap_single_word() {
        let lines = word_wrap("Hello", 35, 3);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0], "Hello");
    }

    #[test]
    fn word_wrap_empty() {
        let lines = word_wrap("", 35, 3);
        assert!(lines.is_empty());
    }

    #[test]
    fn xml_escape_handles_all_special_chars() {
        assert_eq!(xml_escape("a&b"), "a&amp;b");
        assert_eq!(xml_escape("a<b>c"), "a&lt;b&gt;c");
        assert_eq!(xml_escape("a\"b'c"), "a&quot;b&apos;c");
    }

    #[test]
    fn xml_escape_preserves_normal_text() {
        assert_eq!(xml_escape("hello world 123"), "hello world 123");
    }

    #[tokio::test]
    async fn image_generator_fallback_creates_svg_file() {
        let tmp = TempDir::new().unwrap();
        let config = LinkedInImageConfig {
            enabled: true,
            providers: vec![], // no AI providers — force fallback
            fallback_card: true,
            card_accent_color: "#0A66C2".into(),
            temp_dir: "images".into(),
            ..Default::default()
        };

        let generator = ImageGenerator::new(config, tmp.path().to_path_buf());
        let path = generator.generate("Test post about Rust").await.unwrap();

        assert!(path.exists());
        assert_eq!(path.extension().unwrap(), "svg");

        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("Test post about Rust"));
    }

    #[tokio::test]
    async fn image_generator_fails_when_no_providers_and_no_fallback() {
        let tmp = TempDir::new().unwrap();
        let config = LinkedInImageConfig {
            enabled: true,
            providers: vec![],
            fallback_card: false, // no fallback either
            ..Default::default()
        };

        let generator = ImageGenerator::new(config, tmp.path().to_path_buf());
        let result = generator.generate("Test").await;
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("All image generation providers failed"));
    }

    #[tokio::test]
    async fn image_generator_skips_provider_without_key() {
        let tmp = TempDir::new().unwrap();
        // Create .env without any image API keys
        fs::write(tmp.path().join(".env"), "SOME_OTHER_KEY=value\n").unwrap();

        let config = LinkedInImageConfig {
            enabled: true,
            providers: vec!["stability".into(), "dalle".into()],
            fallback_card: true,
            temp_dir: "images".into(),
            ..Default::default()
        };

        let generator = ImageGenerator::new(config, tmp.path().to_path_buf());
        let path = generator.generate("Test").await.unwrap();

        // Should fall through to SVG fallback since no API keys
        assert_eq!(path.extension().unwrap(), "svg");
    }

    #[tokio::test]
    async fn image_generator_cleanup_removes_file() {
        let tmp = TempDir::new().unwrap();
        let file_path = tmp.path().join("test.png");
        fs::write(&file_path, b"fake image data").unwrap();
        assert!(file_path.exists());

        ImageGenerator::cleanup(&file_path).await.unwrap();
        assert!(!file_path.exists());
    }

    #[tokio::test]
    async fn image_generator_cleanup_noop_for_missing_file() {
        let tmp = TempDir::new().unwrap();
        let file_path = tmp.path().join("nonexistent.png");
        // Should not error
        ImageGenerator::cleanup(&file_path).await.unwrap();
    }

    #[tokio::test]
    async fn read_env_var_reads_value() {
        let tmp = TempDir::new().unwrap();
        fs::write(
            tmp.path().join(".env"),
            "STABILITY_API_KEY=sk-test-123\nOTHER=val\n",
        )
        .unwrap();

        let val = ImageGenerator::read_env_var(tmp.path(), "STABILITY_API_KEY")
            .await
            .unwrap();
        assert_eq!(val, "sk-test-123");
    }

    #[tokio::test]
    async fn read_env_var_fails_for_missing_key() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join(".env"), "OTHER=val\n").unwrap();

        let result = ImageGenerator::read_env_var(tmp.path(), "STABILITY_API_KEY").await;
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("STABILITY_API_KEY"));
    }

    #[test]
    fn image_config_default_has_all_providers() {
        let config = LinkedInImageConfig::default();
        assert_eq!(config.providers.len(), 4);
        assert_eq!(config.providers[0], "stability");
        assert_eq!(config.providers[1], "imagen");
        assert_eq!(config.providers[2], "dalle");
        assert_eq!(config.providers[3], "flux");
        assert!(config.fallback_card);
        assert!(!config.enabled);
    }
}
