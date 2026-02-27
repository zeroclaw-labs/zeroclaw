use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillIndexEntry {
    pub name: String,
    pub version: String,
    pub description: String,
    pub author: String,
    pub download_url: String,
    pub checksum: String,
}

#[derive(Debug, Clone)]
pub struct SkillIndex {
    pub skills: Vec<SkillIndexEntry>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateAvailable {
    pub name: String,
    pub current_version: String,
    pub latest_version: String,
}

pub struct SkillMarket {
    base_url: String,
    client: reqwest::Client,
    index_cache: Arc<RwLock<Option<SkillIndex>>>,
}

impl SkillMarket {
    pub fn new(base_url: &str) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            client: reqwest::Client::new(),
            index_cache: Arc::new(RwLock::new(None)),
        }
    }

    pub fn from_config(config: &crate::config::SkillsConfig) -> Option<Self> {
        config.market_url.as_ref().map(|url| Self::new(url))
    }

    pub async fn search(&self, query: &str) -> Result<Vec<SkillIndexEntry>> {
        if query.trim().is_empty() {
            return Ok(Vec::new());
        }

        let url = format!(
            "{}/skills/search?q={}",
            self.base_url,
            urlencoding::encode(query)
        );

        let response = self.client.get(&url).send().await?;

        if !response.status().is_success() {
            anyhow::bail!(
                "Market search failed: status {}",
                response.status()
            );
        }

        let skills: Vec<SkillIndexEntry> = response.json().await?;
        Ok(skills)
    }

    pub async fn get_index(&self) -> Result<SkillIndex> {
        let url = format!("{}/skills/index", self.base_url);

        let response = self.client.get(&url).send().await?;

        if !response.status().is_success() {
            anyhow::bail!(
                "Market index fetch failed: status {}",
                response.status()
            );
        }

        let skills: Vec<SkillIndexEntry> = response.json().await?;
        let index = SkillIndex {
            skills,
            updated_at: Utc::now(),
        };

        let mut cache = self.index_cache.write().await;
        *cache = Some(index.clone());

        Ok(index)
    }

    pub async fn install(&self, name: &str, target_dir: &Path) -> Result<std::path::PathBuf> {
        let index = self.get_index().await?;
        let entry = index
            .skills
            .iter()
            .find(|s| s.name == name)
            .ok_or_else(|| anyhow::anyhow!("Skill not found in market: {}", name))?;

        let dest = target_dir.join(&entry.name);
        if dest.exists() {
            anyhow::bail!("Skill already installed: {}", name);
        }

        let response = self.client.get(&entry.download_url).send().await?;
        if !response.status().is_success() {
            anyhow::bail!(
                "Skill download failed: status {}",
                response.status()
            );
        }

        let bytes = response.bytes().await?;
        let expected_checksum = entry.checksum.trim().to_lowercase();
        let actual_checksum = format!("{:x}", sha2::Sha256::digest(&bytes));

        if actual_checksum != expected_checksum {
            anyhow::bail!(
                "Checksum mismatch for skill {}: expected {}, got {}",
                name, expected_checksum, actual_checksum
            );
        }

        std::fs::create_dir_all(&dest)?;
        let archive_path = dest.join("download.tar.gz");
        std::fs::write(&archive_path, &bytes)?;

        let output = std::process::Command::new("tar")
            .args(["-xzf", "download.tar.gz"])
            .current_dir(&dest)
            .output()?;

        if !output.status.success() {
            let _ = std::fs::remove_dir_all(&dest);
            anyhow::bail!(
                "Failed to extract skill archive: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }

        std::fs::remove_file(&archive_path).ok();

        Ok(dest)
    }

    pub async fn check_updates(
        &self,
        installed: &[super::Skill],
    ) -> Result<Vec<UpdateAvailable>> {
        let index = self.get_index().await?;
        let mut updates = Vec::new();

        for skill in installed {
            if let Some(market_entry) = index.skills.iter().find(|s| s.name == skill.name) {
                if skill.version != market_entry.version {
                    updates.push(UpdateAvailable {
                        name: skill.name.clone(),
                        current_version: skill.version.clone(),
                        latest_version: market_entry.version.clone(),
                    });
                }
            }
        }

        Ok(updates)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skill_market_new_trims_trailing_slash() {
        let market = SkillMarket::new("https://example.com/");
        assert_eq!(market.base_url, "https://example.com");
    }

    #[test]
    fn skill_market_from_config_returns_none_when_no_url() {
        let config = crate::config::SkillsConfig::default();
        assert!(SkillMarket::from_config(&config).is_none());
    }

    #[test]
    fn skill_market_from_config_returns_some_when_url_set() {
        let mut config = crate::config::SkillsConfig::default();
        config.market_url = Some("https://clawhub.ai/api/v1".to_string());
        assert!(SkillMarket::from_config(&config).is_some());
    }

    #[tokio::test]
    async fn search_empty_query_returns_empty() {
        let market = SkillMarket::new("https://example.com");
        let result = market.search("").await.unwrap();
        assert!(result.is_empty());
    }

    #[tokio::test]
    async fn search_whitespace_query_returns_empty() {
        let market = SkillMarket::new("https://example.com");
        let result = market.search("   ").await.unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn skill_index_entry_deserialize() {
        let json = r#"{
            "name": "test-skill",
            "version": "1.0.0",
            "description": "A test skill",
            "author": "test-author",
            "download_url": "https://example.com/skills/test-skill.tar.gz",
            "checksum": "abc123"
        }"#;
        let entry: SkillIndexEntry = serde_json::from_str(json).unwrap();
        assert_eq!(entry.name, "test-skill");
        assert_eq!(entry.version, "1.0.0");
    }
}