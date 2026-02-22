use crate::clawhub::types::{ClawHubSkill, SearchResult, SearchResultItem, SkillDetail};
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

    /// Search and convert to internal ClawHubSkill format
    pub async fn search_skills(&self, query: &str, limit: usize) -> Result<Vec<ClawHubSkill>> {
        let result = self.search(query, limit).await?;

        let skills: Vec<ClawHubSkill> = result
            .results
            .into_iter()
            .map(|item| SearchResultItem::into_skill(item))
            .collect();

        Ok(skills)
    }

    /// Get skill metadata by slug
    pub async fn get_skill(&self, slug: &str) -> Result<ClawHubSkill> {
        let url = format!("{}/api/skill?slug={}", self.api_url, slug);

        let mut request = self.http_client.get(&url);

        if let Some(token) = &self.github_token {
            request = request.header("Authorization", format!("Bearer {}", token));
        }

        let response = request.send().await.context("Failed to get skill")?;
        let detail = response
            .json::<SkillDetail>()
            .await
            .context("Failed to parse skill")?;

        Ok(detail.into())
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

impl SearchResultItem {
    /// Convert API response to internal ClawHubSkill format
    pub fn into_skill(self) -> ClawHubSkill {
        ClawHubSkill {
            slug: self.slug,
            name: self.display_name,
            description: self.summary,
            author: String::new(), // Not available in search results
            tags: vec![],         // Not available in search results
            stars: 0,             // Not available in search results
            version: self.version,
            github_url: None,     // Not available in search results
            readme_url: None,     // Not available in search results
        }
    }
}

impl From<SkillDetail> for ClawHubSkill {
    fn from(detail: SkillDetail) -> Self {
        let slug = detail.skill.slug.clone();
        let handle = detail.owner.handle.clone();
        let version = detail.latest_version.version.clone();
        ClawHubSkill {
            slug: slug.clone(),
            name: detail.skill.display_name,
            description: detail.skill.summary,
            author: handle.clone(),
            tags: vec![], // Tags is a complex object, simplified for now
            stars: detail.skill.stats.stars,
            version,
            github_url: Some(format!("https://github.com/{}/{}", handle, slug)),
            readme_url: Some(format!(
                "https://raw.githubusercontent.com/{}/{}/main/SKILL.md",
                handle, slug
            )),
        }
    }
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
