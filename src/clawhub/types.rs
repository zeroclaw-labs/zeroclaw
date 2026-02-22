use serde::{Deserialize, Serialize};

/// Search result item from ClawHub API
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResultItem {
    pub score: f64,
    pub slug: String,
    #[serde(rename = "displayName")]
    pub display_name: String,
    pub summary: String,
    pub version: String,
    #[serde(rename = "updatedAt")]
    pub updated_at: i64,
}

/// Search result from ClawHub API
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    pub results: Vec<SearchResultItem>,
}

/// Skill detail response from ClawHub API
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillDetail {
    pub skill: SkillInfo,
    #[serde(rename = "latestVersion")]
    pub latest_version: LatestVersion,
    pub owner: SkillOwner,
}

/// Skill info from ClawHub API
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillInfo {
    pub slug: String,
    #[serde(rename = "displayName")]
    pub display_name: String,
    pub summary: String,
    pub tags: serde_json::Value,
    pub stats: SkillStats,
    #[serde(rename = "createdAt")]
    pub created_at: i64,
    #[serde(rename = "updatedAt")]
    pub updated_at: i64,
}

/// Skill stats from ClawHub API
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillStats {
    pub comments: u32,
    pub downloads: u32,
    #[serde(rename = "installsAllTime")]
    pub installs_all_time: u32,
    #[serde(rename = "installsCurrent")]
    pub installs_current: u32,
    pub stars: u32,
    pub versions: u32,
}

/// Latest version info from ClawHub API
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LatestVersion {
    pub version: String,
    #[serde(rename = "createdAt")]
    pub created_at: i64,
    pub changelog: String,
}

/// Skill owner from ClawHub API
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillOwner {
    pub handle: String,
    #[serde(rename = "displayName")]
    pub display_name: String,
    pub image: String,
}

/// ClawHub skill - unified representation for our internal use
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClawHubSkill {
    pub slug: String,
    pub name: String,
    pub description: String,
    pub author: String,
    pub tags: Vec<String>,
    pub stars: u32,
    pub version: String,
    pub github_url: Option<String>,
    pub readme_url: Option<String>,
}

impl From<SearchResultItem> for ClawHubSkill {
    fn from(item: SearchResultItem) -> Self {
        ClawHubSkill {
            slug: item.slug,
            name: item.display_name,
            description: item.summary,
            author: String::new(),
            tags: vec![],
            stars: 0,
            version: item.version,
            github_url: None,
            readme_url: None,
        }
    }
}

/// Local registry entry for installed clawhub skill
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstalledSkill {
    pub slug: String,
    pub name: String,
    pub version: String,
    pub source_url: String,
    pub installed_at: String,
    #[serde(default)]
    pub updated_at: Option<String>,
}

/// Local registry of installed clawhub skills
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ClawHubRegistry {
    #[serde(default)]
    pub skills: Vec<InstalledSkill>,
    #[serde(default)]
    pub last_sync: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_clawhub_skill_deserialization() {
        let json = r#"{
            "slug": "weather-tool",
            "name": "Weather Tool",
            "description": "Fetch weather forecasts",
            "author": "someuser",
            "tags": ["weather", "api"],
            "stars": 42,
            "version": "1.2.0",
            "github_url": "https://github.com/someuser/weather-tool",
            "readme_url": "https://raw.githubusercontent.com/someuser/weather-tool/main/SKILL.md"
        }"#;

        let skill: ClawHubSkill = serde_json::from_str(json).unwrap();
        assert_eq!(skill.slug, "weather-tool");
        assert_eq!(skill.version, "1.2.0");
        assert_eq!(skill.stars, 42);
    }

    #[test]
    fn test_search_result_deserialization() {
        let json = r#"{
            "results": [
                {"score": 3.74, "slug": "skill1", "displayName": "Skill 1", "summary": "Desc 1", "version": "1.0.0", "updatedAt": 1234567890},
                {"score": 3.45, "slug": "skill2", "displayName": "Skill 2", "summary": "Desc 2", "version": "0.5.0", "updatedAt": 1234567890}
            ]
        }"#;

        let result: SearchResult = serde_json::from_str(json).unwrap();
        assert_eq!(result.results.len(), 2);
    }

    #[test]
    fn test_search_result_item_conversion() {
        let item = SearchResultItem {
            score: 3.5,
            slug: "test-skill".to_string(),
            display_name: "Test Skill".to_string(),
            summary: "A test skill".to_string(),
            version: "1.0.0".to_string(),
            updated_at: 1234567890,
        };

        let skill: ClawHubSkill = item.into();
        assert_eq!(skill.slug, "test-skill");
        assert_eq!(skill.name, "Test Skill");
        assert_eq!(skill.version, "1.0.0");
    }

    #[test]
    fn test_skill_detail_conversion() {
        let json = r#"{
            "skill": {
                "slug": "weather",
                "displayName": "Weather",
                "summary": "Get weather",
                "tags": {},
                "stats": {"comments": 0, "downloads": 100, "installsAllTime": 50, "installsCurrent": 45, "stars": 10, "versions": 1},
                "createdAt": 1234567890,
                "updatedAt": 1234567890
            },
            "latestVersion": {"version": "1.0.0", "createdAt": 1234567890, "changelog": ""},
            "owner": {"handle": "steipete", "displayName": "Peter", "image": "https://example.com/image.png"}
        }"#;

        let detail: SkillDetail = serde_json::from_str(json).unwrap();
        let skill: ClawHubSkill = detail.into();
        assert_eq!(skill.slug, "weather");
        assert_eq!(skill.author, "steipete");
        assert_eq!(skill.stars, 10);
    }

    #[test]
    fn test_installed_skill_serialization() {
        let skill = InstalledSkill {
            slug: "test-skill".to_string(),
            name: "Test Skill".to_string(),
            version: "1.0.0".to_string(),
            source_url: "https://github.com/test/test-skill".to_string(),
            installed_at: "2024-01-15T10:30:00Z".to_string(),
            updated_at: None,
        };

        let json = serde_json::to_string(&skill).unwrap();
        assert!(json.contains("test-skill"));
        assert!(json.contains("1.0.0"));
    }

    #[test]
    fn test_clawhub_registry_default() {
        let registry = ClawHubRegistry::default();
        assert!(registry.skills.is_empty());
        assert!(registry.last_sync.is_none());
    }
}
