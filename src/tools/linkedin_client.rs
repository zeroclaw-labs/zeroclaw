use anyhow::Context;
use reqwest::header::{HeaderMap, HeaderValue};
use reqwest::Method;
use serde_json::json;
use std::path::PathBuf;

const LINKEDIN_API_BASE: &str = "https://api.linkedin.com";
const LINKEDIN_OAUTH_TOKEN_URL: &str = "https://www.linkedin.com/oauth/v2/accessToken";
const LINKEDIN_REQUEST_TIMEOUT_SECS: u64 = 30;
const LINKEDIN_CONNECT_TIMEOUT_SECS: u64 = 10;

pub struct LinkedInClient {
    workspace_dir: PathBuf,
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
    pub fn new(workspace_dir: PathBuf) -> Self {
        Self { workspace_dir }
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

    fn api_headers(token: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        let bearer = format!("Bearer {}", token);
        headers.insert(
            reqwest::header::AUTHORIZATION,
            HeaderValue::from_str(&bearer).expect("valid bearer token header"),
        );
        headers.insert("LinkedIn-Version", HeaderValue::from_static("202402"));
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
        let headers = Self::api_headers(token);

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

            let retry_headers = Self::api_headers(&new_token);
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

        let client = LinkedInClient::new(tmp.path().to_path_buf());
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

        let client = LinkedInClient::new(tmp.path().to_path_buf());
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

        let client = LinkedInClient::new(tmp.path().to_path_buf());
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

        let client = LinkedInClient::new(tmp.path().to_path_buf());
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

        let client = LinkedInClient::new(tmp.path().to_path_buf());
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

        let client = LinkedInClient::new(tmp.path().to_path_buf());
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

        let client = LinkedInClient::new(tmp.path().to_path_buf());
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

        let client = LinkedInClient::new(tmp.path().to_path_buf());
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

        let client = LinkedInClient::new(tmp.path().to_path_buf());
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

        let client = LinkedInClient::new(tmp.path().to_path_buf());
        let err = client.get_credentials().await.unwrap_err();
        assert!(err.to_string().contains("LINKEDIN_PERSON_ID"));
    }

    #[tokio::test]
    async fn credentials_fail_no_env_file() {
        let tmp = TempDir::new().unwrap();
        let client = LinkedInClient::new(tmp.path().to_path_buf());
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

        let client = LinkedInClient::new(tmp.path().to_path_buf());
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

        let client = LinkedInClient::new(tmp.path().to_path_buf());
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

        let client = LinkedInClient::new(tmp.path().to_path_buf());
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

        let client = LinkedInClient::new(tmp.path().to_path_buf());
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
        let headers = LinkedInClient::api_headers("test_token");
        assert_eq!(
            headers.get("Authorization").unwrap().to_str().unwrap(),
            "Bearer test_token"
        );
        assert_eq!(
            headers.get("LinkedIn-Version").unwrap().to_str().unwrap(),
            "202402"
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
}
