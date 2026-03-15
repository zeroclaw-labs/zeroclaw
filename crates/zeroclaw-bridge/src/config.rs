use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BridgeConfig {
    pub mqtt_broker_url: String,
    pub websocket_url: String,
    pub auth_token: String,
}

impl BridgeConfig {
    pub fn load<P: AsRef<Path>>(path: P) -> Result<Self> {
        let content =
            std::fs::read_to_string(path.as_ref()).context("Failed to read config file")?;
        let expanded = shellexpand::tilde(&content).to_string();
        toml::from_str(&expanded).context("Failed to parse config")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn test_config_loading() {
        let mut temp = tempfile::NamedTempFile::new().unwrap();
        writeln!(
            temp,
            r#"
mqtt_broker_url = "mqtt://localhost:1883"
websocket_url = "ws://localhost:42617"
auth_token = "test-token"
"#
        )
        .unwrap();

        let config = BridgeConfig::load(temp.path()).unwrap();
        assert_eq!(config.mqtt_broker_url, "mqtt://localhost:1883");
        assert_eq!(config.websocket_url, "ws://localhost:42617");
        assert_eq!(config.auth_token, "test-token");
    }
}
