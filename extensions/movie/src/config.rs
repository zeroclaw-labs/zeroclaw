//! Configuration management for movie extension

use serde::{Deserialize, Serialize};
use std::path::Path;

/// Movie extension configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MovieConfig {
    /// Enable or disable the movie tool
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    
    /// China (Douban - FREE) API configuration
    #[serde(default)]
    pub china: ChinaConfig,
    
    /// US (MovieGlu) API configuration
    #[serde(default)]
    pub us: UsConfig,
    
    /// Default search parameters
    #[serde(default)]
    pub defaults: DefaultSearchConfig,
}

/// China API configuration (Douban - Free, no key required)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChinaConfig {
    /// Enable China API
    #[serde(default = "default_true")]
    pub enabled: bool,
    
    /// Douban API URL (optional, uses community proxy by default)
    #[serde(default)]
    pub api_url: Option<String>,
}

/// US API configuration (TMDB - Free with registration)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsConfig {
    /// Enable US API
    #[serde(default = "default_true")]
    pub enabled: bool,
    
    /// TMDB API key (get free at: https://www.themoviedb.org/settings/api)
    #[serde(default)]
    pub api_key: Option<String>,
    
    /// Custom API endpoint (optional)
    #[serde(default)]
    pub api_url: Option<String>,
}

/// Default search parameters
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DefaultSearchConfig {
    /// Default hours ahead to search
    #[serde(default = "default_hours_ahead")]
    pub hours_ahead: u32,
    
    /// Maximum results per cinema
    #[serde(default = "default_max_results")]
    pub max_results_per_cinema: usize,
    
    /// Maximum total results
    #[serde(default = "default_max_total")]
    pub max_total_results: usize,
}

fn default_enabled() -> bool {
    true
}

fn default_true() -> bool {
    true
}

fn default_hours_ahead() -> u32 {
    3
}

fn default_max_results() -> usize {
    10
}

fn default_max_total() -> usize {
    50
}

impl Default for MovieConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            china: ChinaConfig::default(),
            us: UsConfig::default(),
            defaults: DefaultSearchConfig::default(),
        }
    }
}

impl Default for ChinaConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            api_url: None,
        }
    }
}

impl Default for UsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            api_key: None,
            api_url: None,
        }
    }
}

impl Default for DefaultSearchConfig {
    fn default() -> Self {
        Self {
            hours_ahead: default_hours_ahead(),
            max_results_per_cinema: default_max_results(),
            max_total_results: default_max_total(),
        }
    }
}

impl MovieConfig {
    /// Load configuration from a TOML file
    pub fn from_file<P: AsRef<Path>>(path: P) -> Result<Self, Box<dyn std::error::Error>> {
        let content = std::fs::read_to_string(path)?;
        let config: MovieConfig = toml::from_str(&content)?;
        Ok(config)
    }
    
    /// Save configuration to a TOML file
    pub fn save_to_file<P: AsRef<Path>>(&self, path: P) -> Result<(), Box<dyn std::error::Error>> {
        let content = toml::to_string_pretty(self)?;
        std::fs::write(path, content)?;
        Ok(())
    }
    
    /// Create configuration from environment variables
    pub fn from_env() -> Self {
        Self {
            china: ChinaConfig {
                enabled: std::env::var("MOVIE_CHINA_ENABLED")
                    .map(|v| v == "true" || v == "1")
                    .unwrap_or(true),
                api_url: std::env::var("DOUBAN_API_URL").ok(),
            },
            us: UsConfig {
                enabled: std::env::var("MOVIE_US_ENABLED")
                    .map(|v| v == "true" || v == "1")
                    .unwrap_or(true),
                api_key: std::env::var("TMDB_API_KEY").ok(),
                api_url: std::env::var("TMDB_API_URL").ok(),
            },
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_default_config() {
        let config = MovieConfig::default();
        assert!(config.enabled);
        assert!(config.china.enabled);
        assert!(config.us.enabled);
        assert_eq!(config.defaults.hours_ahead, 3);
    }
    
    #[test]
    fn test_serialize_config() {
        let config = MovieConfig::default();
        let toml_str = toml::to_string(&config).unwrap();
        assert!(toml_str.contains("enabled = true"));
    }
}
