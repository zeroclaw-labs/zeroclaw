use crate::clawhub::types::{ClawHubSkill, SearchResult};
use anyhow::{Context, Result};
use reqwest::Client;
use std::time::Duration;

/// ClawHub API client
pub struct ClawHubClient {
    pub api_url: String,
    http_client: Client,
    github_token: Option<String>,
}

impl ClawHubClient {
    /// Create new client with default API URL
    pub fn new(api_url: impl Into<String>) -> Self {
        Self::with_token(api_url, None)
    }

    /// Create new client with GitHub token
    pub fn with_token(api_url: impl Into<String>, token: Option<String>) -> Self {
        let http_client = Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .expect("Failed to create HTTP client");

        Self {
            api_url: api_url.into(),
            http_client,
            github_token: token,
        }
    }

    /// Search skills on ClawHub
    pub async fn search(&self, query: &str, limit: usize) -> Result<SearchResult> {
        let url = format!("{}/api/search?q={}&limit={}", self.api_url, query, limit);

        let mut request = self.http_client.get(&url);

        if let Some(token) = &self.github_token {
            request = request.header("Authorization", format!("Bearer {}", token));
        }

        let response = request.send().await.context("Failed to search ClawHub")?;
        let result = response
            .json::<SearchResult>()
            .await
            .context("Failed to parse search results")?;

        Ok(result)
    }

    /// Get skill metadata by slug
    pub async fn get_skill(&self, slug: &str) -> Result<ClawHubSkill> {
        let url = format!("{}/api/skills/{}", self.api_url, slug);

        let mut request = self.http_client.get(&url);

        if let Some(token) = &self.github_token {
            request = request.header("Authorization", format!("Bearer {}", token));
        }

        let response = request.send().await.context("Failed to get skill")?;
        let skill = response
            .json::<ClawHubSkill>()
            .await
            .context("Failed to parse skill")?;

        Ok(skill)
    }

    /// Get authenticated user info
    pub async fn get_user(&self) -> Result<ClawHubUser> {
        let url = format!("{}/api/user", self.api_url);

        let mut request = self.http_client.get(&url);

        let token = self
            .github_token
            .as_ref()
            .context("No GitHub token configured")?;
        request = request.header("Authorization", format!("Bearer {}", token));

        let response = request.send().await.context("Failed to get user")?;
        let user = response
            .json::<ClawHubUser>()
            .await
            .context("Failed to parse user")?;

        Ok(user)
    }
}

impl Default for ClawHubClient {
    fn default() -> Self {
        Self::new("https://clawhub.ai")
    }
}

/// ClawHub user info
#[derive(Debug, Clone, serde::Deserialize)]
pub struct ClawHubUser {
    pub login: String,
    pub name: Option<String>,
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_default_api_url() {
        let client = super::ClawHubClient::default();
        assert_eq!(client.api_url, "https://clawhub.ai");
    }

    #[test]
    fn test_custom_api_url() {
        let client = super::ClawHubClient::new("https://custom.clawhub.io");
        assert_eq!(client.api_url, "https://custom.clawhub.io");
    }
}
