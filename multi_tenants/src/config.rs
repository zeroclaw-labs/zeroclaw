use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Deserialize, Clone)]
pub struct PlatformConfig {
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default = "default_db_path")]
    pub database_path: PathBuf,
    #[serde(default = "default_key_path")]
    pub master_key_path: PathBuf,
    #[serde(default = "default_data_dir")]
    pub data_dir: PathBuf,
    #[serde(default = "default_docker_image")]
    pub docker_image: String,
    #[serde(default = "default_network")]
    pub docker_network: String,
    #[serde(default = "default_port_range")]
    pub port_range: [u16; 2],
    #[serde(default = "default_uid_range")]
    pub uid_range: [u32; 2],
    pub domain: Option<String>,
    #[serde(default = "default_caddy_api")]
    pub caddy_api_url: String,
    pub jwt_secret: Option<String>,
    #[serde(default)]
    pub smtp: Option<SmtpConfig>,
    #[serde(default)]
    pub plans: PlansConfig,
}

#[derive(Debug, Deserialize, Clone)]
pub struct SmtpConfig {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: String,
    pub from_email: String,
    #[serde(default = "default_from_name")]
    pub from_name: String,
}

fn default_from_name() -> String {
    "ZeroClaw".into()
}

#[derive(Debug, Deserialize, Clone)]
pub struct PlansConfig {
    #[serde(default = "default_free_plan")]
    pub free: PlanLimits,
    #[serde(default = "default_pro_plan")]
    pub pro: PlanLimits,
}

impl Default for PlansConfig {
    fn default() -> Self {
        Self {
            free: default_free_plan(),
            pro: default_pro_plan(),
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
pub struct PlanLimits {
    pub max_messages_per_day: u32,
    pub max_channels: u32,
    pub max_members: u32,
    pub memory_mb: u32,
    pub cpu_limit: f64,
}

// Default functions
fn default_host() -> String {
    "127.0.0.1".into()
}
fn default_port() -> u16 {
    8080
}
fn default_db_path() -> PathBuf {
    PathBuf::from("data/platform.db")
}
fn default_key_path() -> PathBuf {
    PathBuf::from("data/master.key")
}
fn default_data_dir() -> PathBuf {
    PathBuf::from("data/tenants")
}
fn default_docker_image() -> String {
    "zeroclaw:latest".into()
}
fn default_network() -> String {
    "zcplatform-internal".into()
}
fn default_port_range() -> [u16; 2] {
    [10001, 10999]
}
fn default_uid_range() -> [u32; 2] {
    [10001, 10999]
}
fn default_caddy_api() -> String {
    "http://localhost:2019".into()
}

fn default_free_plan() -> PlanLimits {
    PlanLimits {
        max_messages_per_day: 100,
        max_channels: 2,
        max_members: 3,
        memory_mb: 128,
        cpu_limit: 0.25,
    }
}

fn default_pro_plan() -> PlanLimits {
    PlanLimits {
        max_messages_per_day: 1000,
        max_channels: 10,
        max_members: 20,
        memory_mb: 256,
        cpu_limit: 0.5,
    }
}

/// Load config from TOML file with env var overrides.
pub fn load(path: &str) -> anyhow::Result<PlatformConfig> {
    let content = if std::path::Path::new(path).exists() {
        std::fs::read_to_string(path)?
    } else {
        tracing::warn!("Config file not found at {}, using defaults", path);
        String::new()
    };

    let mut config: PlatformConfig = toml::from_str(&content)?;

    // Env var overrides
    if let Ok(v) = std::env::var("ZCPLATFORM_HOST") {
        config.host = v;
    }
    if let Ok(v) = std::env::var("ZCPLATFORM_PORT") {
        config.port = v.parse()?;
    }
    if let Ok(v) = std::env::var("ZCPLATFORM_DB_PATH") {
        config.database_path = PathBuf::from(v);
    }
    if let Ok(v) = std::env::var("ZCPLATFORM_DOMAIN") {
        config.domain = Some(v);
    }
    if let Ok(v) = std::env::var("ZCPLATFORM_JWT_SECRET") {
        config.jwt_secret = Some(v);
    }

    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_applied_on_empty_toml() {
        let cfg: PlatformConfig = toml::from_str("").expect("empty toml should parse");
        assert_eq!(cfg.host, "127.0.0.1");
        assert_eq!(cfg.port, 8080);
        assert_eq!(cfg.port_range, [10001, 10999]);
        assert_eq!(cfg.uid_range, [10001, 10999]);
        assert_eq!(cfg.docker_image, "zeroclaw:latest");
        assert_eq!(cfg.plans.free.max_messages_per_day, 100);
        assert_eq!(cfg.plans.pro.max_messages_per_day, 1000);
        assert!(cfg.domain.is_none());
        assert!(cfg.jwt_secret.is_none());
    }

    #[test]
    fn partial_toml_overrides_only_set_fields() {
        let toml_str = r#"
host = "0.0.0.0"
port = 9090
domain = "example.com"
"#;
        let cfg: PlatformConfig = toml::from_str(toml_str).expect("valid toml");
        assert_eq!(cfg.host, "0.0.0.0");
        assert_eq!(cfg.port, 9090);
        assert_eq!(cfg.domain.as_deref(), Some("example.com"));
        // defaults preserved for unset fields
        assert_eq!(cfg.docker_image, "zeroclaw:latest");
        assert_eq!(cfg.plans.free.memory_mb, 128);
    }
}
