//! Capability identifiers shared by configuration and provider consumers.

use serde::{Deserialize, Serialize};

/// A provider protocol surface that can be bound to a logical model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum Capability {
    Chat,
    Embedding,
    Tts,
    Asr,
    Decision,
}

impl Capability {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Chat => "chat",
            Self::Embedding => "embedding",
            Self::Tts => "tts",
            Self::Asr => "asr",
            Self::Decision => "decision",
        }
    }
}

pub type CapabilitySet = std::collections::BTreeSet<Capability>;
