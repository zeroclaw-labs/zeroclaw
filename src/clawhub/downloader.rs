// src/clawhub/downloader.rs
//! Skill downloader for fetching skill content from GitHub

use anyhow::{Context, Result};
use reqwest::Client;
use std::path::Path;
use std::time::Duration;

/// Skill downloader - fetches skill content from GitHub
pub struct SkillDownloader {
    http_client: Client,
}

impl SkillDownloader {
    /// Create a new SkillDownloader with default configuration
    pub fn new() -> Self {
        let http_client = Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .expect("Failed to create HTTP client");

        Self { http_client }
    }

    /// Download skill content from a raw URL (e.g., GitHub raw)
    pub async fn download_file(&self, url: &str) -> Result<String> {
        let response = self
            .http_client
            .get(url)
            .send()
            .await
            .context("Failed to download file")?;

        if !response.status().is_success() {
            let status = response.status();
            let url = url.to_string();
            anyhow::bail!("Failed to download {}: HTTP {}", url, status);
        }

        let content = response
            .text()
            .await
            .context("Failed to read response body")?;

        Ok(content)
    }

    /// Download SKILL.md and supporting files to target directory
    pub async fn download_skill(&self, readme_url: &str, target_dir: &Path) -> Result<()> {
        let content = self.download_file(readme_url).await?;
        std::fs::create_dir_all(target_dir)?;
        let skill_md = target_dir.join("SKILL.md");
        std::fs::write(&skill_md, &content)?;
        Ok(())
    }

    /// Download SKILL.md trying both main and master branch URLs
    pub async fn download_skill_with_fallback(
        &self,
        readme_url: Option<&str>,
        readme_url_master: Option<&str>,
        target_dir: &Path,
    ) -> Result<()> {
        // Try main branch first
        if let Some(url) = readme_url {
            match self.download_file(url).await {
                Ok(content) => {
                    std::fs::create_dir_all(target_dir)?;
                    let skill_md = target_dir.join("SKILL.md");
                    std::fs::write(&skill_md, &content)?;
                    return Ok(());
                }
                Err(e) => {
                    tracing::debug!("Failed to download from main branch {}: {}", url, e);
                }
            }
        }

        // Try master branch as fallback
        if let Some(url) = readme_url_master {
            let content = self.download_file(url).await?;
            std::fs::create_dir_all(target_dir)?;
            let skill_md = target_dir.join("SKILL.md");
            std::fs::write(&skill_md, &content)?;
            return Ok(());
        }

        anyhow::bail!("No readme_url provided and master branch fallback also failed");
    }
}

impl Default for SkillDownloader {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_skill_downloader_default() {
        let downloader = SkillDownloader::default();
        // Just verify it can be created without panic
        assert!(std::mem::size_of_val(&downloader) > 0);
    }

    #[test]
    fn test_skill_downloader_new() {
        let downloader = SkillDownloader::new();
        assert!(std::mem::size_of_val(&downloader) > 0);
    }
}
