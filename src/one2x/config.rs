use crate::config::traits::ChannelConfig;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct WebChannelConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
}

impl ChannelConfig for WebChannelConfig {
    fn name() -> &'static str {
        "Web"
    }

    fn desc() -> &'static str {
        "WebSocket real-time channel"
    }
}
