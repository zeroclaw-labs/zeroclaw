//! SimpleX transport configuration. Contact authorization remains owned by
//! peer_groups; this block creates only transport and call activation policy.
use serde::{Deserialize, Serialize};
use zeroclaw_macros::Configurable;

#[derive(Debug, Clone, Default, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "channels.simplex"]
#[serde(default)]
pub struct SimplexConfig {
    /// Activate this adapter. Connections are denied unless peer_groups explicitly allow them.
    pub enabled: bool,
    /// Local official SimpleX CLI WebSocket endpoint, e.g. ws://127.0.0.1:5225.
    pub websocket_url: String,
    /// External bridge executable followed by its arguments. No shell is invoked.
    pub bridge_command: Vec<String>,
    /// Accept encrypted audio calls from explicitly authorized contacts. Default: false.
    pub calls_enabled: bool,
    /// Supersede an earlier turn when this contact sends a new message.
    pub interrupt_on_new_message: bool,
}

impl SimplexConfig {
    pub fn validate(&self) -> anyhow::Result<()> {
        if !self.enabled {
            return Ok(());
        }
        let url = url::Url::parse(&self.websocket_url)?;
        anyhow::ensure!(
            url.scheme() == "ws"
                && matches!(url.host_str(), Some("127.0.0.1" | "[::1]" | "localhost"))
                && url.username().is_empty()
                && url.password().is_none(),
            "simplex_requires_loopback_cli"
        );
        anyhow::ensure!(
            self.bridge_command
                .first()
                .is_some_and(|s| !s.trim().is_empty()),
            "simplex_requires_bridge_command"
        );
        Ok(())
    }
}

impl crate::traits::ChannelConfig for SimplexConfig {
    fn name() -> &'static str {
        "SimpleX"
    }
    fn desc() -> &'static str {
        "Private messaging and encrypted audio calls"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn transport_is_closed_by_default_and_refuses_remote_unauthenticated_apis() {
        let mut config = SimplexConfig::default();
        assert!(!config.enabled);
        assert!(!config.calls_enabled);
        config.enabled = true;
        config.bridge_command = vec!["bridge".into()];
        for url in [
            "ws://example.org:5225",
            "wss://127.0.0.1:5225",
            "ws://user@localhost:5225",
        ] {
            config.websocket_url = url.into();
            assert!(config.validate().is_err());
        }
        for url in ["ws://127.0.0.1:5225", "ws://[::1]:5225"] {
            config.websocket_url = url.into();
            assert!(config.validate().is_ok());
        }
    }
}
