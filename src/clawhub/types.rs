use serde::{Deserialize, Serialize};

/// Skill metadata from ClawHub API
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClawHubSkill {
    pub slug: String,
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub author: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub stars: i32,
    pub version: String,
    #[serde(default)]
    pub github_url: Option<String>,
    #[serde(default)]
    pub readme_url: Option<String>,
}

/// Search result from ClawHub API
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    pub skills: Vec<ClawHubSkill>,
    pub total: i32,
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
            "skills": [
                {"slug": "skill1", "name": "Skill 1", "description": "Desc 1", "stars": 10, "version": "1.0.0"},
                {"slug": "skill2", "name": "Skill 2", "description": "Desc 2", "stars": 5, "version": "0.5.0"}
            ],
            "total": 2
        }"#;

        let result: SearchResult = serde_json::from_str(json).unwrap();
        assert_eq!(result.skills.len(), 2);
        assert_eq!(result.total, 2);
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
