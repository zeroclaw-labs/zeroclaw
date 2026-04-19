//! AetherNet agent discovery protocol.
//!
//! Agents advertise their presence and capabilities to a Nostr geohash channel
//! (kind 20001, tag `["g", "<geohash>"]`) so nearby agents — including BitChat
//! iOS/macOS app users — can discover them.
//!
//! The same [`AgentAdvertisement`] JSON payload is also carried inside
//! [`PacketType::AgentAdvertisement`] BitChat BLE/WiFi Direct packets.
//!
//! ## Wire format
//!
//! ```json
//! {
//!   "agent_id": "pubkey_hex",
//!   "geohash": "dr5rs",
//!   "name": "My ZeroClaw Agent",
//!   "capabilities": ["web_search", "code_exec"],
//!   "cost_per_call": null,
//!   "platform": "android",
//!   "is_online": true,
//!   "timestamp": 1700000000,
//!   "version": 1
//! }
//! ```

use serde::{Deserialize, Serialize};

/// How often an online agent re-publishes its advertisement (seconds).
pub const ADVERTISE_INTERVAL_SECS: u64 = 300;

/// Nostr event kind used by BitChat for geohash presence messages.
pub const GEOHASH_PRESENCE_KIND: u16 = 20001;

// ─── AgentCapability ──────────────────────────────────────────────────────────

/// A skill or tool an agent advertises as available to peers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentCapability {
    WebSearch,
    CodeExec,
    DataAnalysis,
    ImageAnalysis,
    FileRead,
    EmailSend,
    MemoryRecall,
    AgentProxy,
    #[serde(untagged)]
    Custom(String),
}

impl std::fmt::Display for AgentCapability {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WebSearch => write!(f, "web_search"),
            Self::CodeExec => write!(f, "code_exec"),
            Self::DataAnalysis => write!(f, "data_analysis"),
            Self::ImageAnalysis => write!(f, "image_analysis"),
            Self::FileRead => write!(f, "file_read"),
            Self::EmailSend => write!(f, "email_send"),
            Self::MemoryRecall => write!(f, "memory_recall"),
            Self::AgentProxy => write!(f, "agent_proxy"),
            Self::Custom(s) => write!(f, "{s}"),
        }
    }
}

// ─── AgentPlatform ────────────────────────────────────────────────────────────

/// The hardware/OS platform the agent runs on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum AgentPlatform {
    Android,
    Ios,
    #[default]
    Desktop,
    Vps,
    Edge,
}

impl std::fmt::Display for AgentPlatform {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Android => write!(f, "android"),
            Self::Ios => write!(f, "ios"),
            Self::Desktop => write!(f, "desktop"),
            Self::Vps => write!(f, "vps"),
            Self::Edge => write!(f, "edge"),
        }
    }
}

// ─── AgentAdvertisement ───────────────────────────────────────────────────────

/// Agent presence/capability advertisement published to a Nostr geohash channel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentAdvertisement {
    /// Nostr public key (hex) or a stable device identifier.
    pub agent_id: String,
    /// Geohash of the agent's approximate location (e.g. `"dr5rs"`).
    pub geohash: String,
    /// Human-readable agent name.
    pub name: String,
    /// Advertised capabilities.
    #[serde(default)]
    pub capabilities: Vec<AgentCapability>,
    /// Optional cost per API call in ANI tokens. `None` = free.
    #[serde(default)]
    pub cost_per_call: Option<f64>,
    /// Platform the agent runs on.
    #[serde(default)]
    pub platform: AgentPlatform,
    /// Whether the agent is currently online and accepting requests.
    pub is_online: bool,
    /// Unix seconds when this advertisement was created.
    pub timestamp: u64,
    /// Protocol version (currently 1).
    #[serde(default = "default_version")]
    pub version: u8,
}

fn default_version() -> u8 {
    1
}

impl AgentAdvertisement {
    /// Create an "online" advertisement.
    pub fn new_online(
        agent_id: String,
        geohash: String,
        name: String,
        capabilities: Vec<AgentCapability>,
        platform: AgentPlatform,
    ) -> Self {
        Self {
            agent_id,
            geohash,
            name,
            capabilities,
            cost_per_call: None,
            platform,
            is_online: true,
            timestamp: now_unix(),
            version: 1,
        }
    }

    /// Build an offline ("going away") variant from an existing advertisement.
    pub fn offline(&self) -> Self {
        Self {
            is_online: false,
            timestamp: now_unix(),
            ..self.clone()
        }
    }

    /// Serialize to JSON.
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    /// Deserialize from a JSON string. Returns `None` on parse failure.
    pub fn from_json(json: &str) -> Option<Self> {
        serde_json::from_str(json).ok()
    }
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_json() {
        let ad = AgentAdvertisement::new_online(
            "deadbeef".to_string(),
            "dr5rs".to_string(),
            "AetherNet Test".to_string(),
            vec![AgentCapability::WebSearch, AgentCapability::CodeExec],
            AgentPlatform::Android,
        );
        let json = ad.to_json();
        let parsed = AgentAdvertisement::from_json(&json).expect("parse");
        assert_eq!(parsed.agent_id, "deadbeef");
        assert_eq!(parsed.geohash, "dr5rs");
        assert!(parsed.is_online);
        assert_eq!(parsed.capabilities.len(), 2);
    }

    #[test]
    fn offline_flips_is_online() {
        let ad = AgentAdvertisement::new_online(
            "abc".into(),
            "dr5rs".into(),
            "Test".into(),
            vec![],
            AgentPlatform::Vps,
        );
        assert!(ad.is_online);
        let off = ad.offline();
        assert!(!off.is_online);
        assert_eq!(off.agent_id, "abc");
    }
}
