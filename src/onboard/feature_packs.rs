//! Onboarding feature pack and preset catalog.
//!
//! This module keeps pack/preset metadata centralized so onboarding UI/CLI,
//! docs, and future installer flows can share one canonical source.

/// A compile/runtime capability bundle that can be selected during onboarding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FeaturePack {
    pub id: &'static str,
    pub description: &'static str,
    pub cargo_features: &'static [&'static str],
    pub requires_confirmation: bool,
}

/// A curated composition of multiple feature packs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Preset {
    pub id: &'static str,
    pub description: &'static str,
    pub packs: &'static [&'static str],
}

/// Canonical pack registry.
pub const FEATURE_PACKS: &[FeaturePack] = &[
    FeaturePack {
        id: "core-agent",
        description: "Agent loop + core local tools",
        cargo_features: &[],
        requires_confirmation: false,
    },
    FeaturePack {
        id: "hardware-core",
        description: "USB hardware discovery/peripheral serial support",
        cargo_features: &["hardware"],
        requires_confirmation: false,
    },
    FeaturePack {
        id: "probe-rs",
        description: "Live MCU memory/register probing (probe-rs)",
        cargo_features: &["probe"],
        requires_confirmation: false,
    },
    FeaturePack {
        id: "browser-native",
        description: "Rust-native browser automation backend",
        cargo_features: &["browser-native"],
        requires_confirmation: false,
    },
    FeaturePack {
        id: "tools-update",
        description: "Agent-callable self-update tool",
        cargo_features: &["tool-update"],
        requires_confirmation: true,
    },
    FeaturePack {
        id: "rag-pdf",
        description: "PDF ingestion for datasheet RAG",
        cargo_features: &["rag-pdf"],
        requires_confirmation: false,
    },
    FeaturePack {
        id: "sandbox-landlock",
        description: "Linux Landlock sandbox policy",
        cargo_features: &["sandbox-landlock"],
        requires_confirmation: true,
    },
    FeaturePack {
        id: "peripheral-rpi",
        description: "Native Raspberry Pi GPIO peripheral backend",
        cargo_features: &["peripheral-rpi"],
        requires_confirmation: false,
    },
];

/// Built-in onboarding presets.
pub const PRESETS: &[Preset] = &[
    Preset {
        id: "minimal",
        description: "Smallest install for local core agent workflows",
        packs: &["core-agent"],
    },
    Preset {
        id: "default",
        description: "Balanced setup for most users",
        packs: &["core-agent", "hardware-core", "tools-update"],
    },
    Preset {
        id: "automation",
        description: "Automation-heavy setup with browser + scheduling foundations",
        packs: &[
            "core-agent",
            "hardware-core",
            "browser-native",
            "tools-update",
        ],
    },
    Preset {
        id: "hardware-lab",
        description: "Embedded/hardware lab workflow",
        packs: &[
            "core-agent",
            "hardware-core",
            "probe-rs",
            "rag-pdf",
            "tools-update",
        ],
    },
    Preset {
        id: "hardened-linux",
        description: "Linux-focused setup with sandbox hardening",
        packs: &[
            "core-agent",
            "hardware-core",
            "sandbox-landlock",
            "tools-update",
        ],
    },
];

pub fn feature_pack_by_id(id: &str) -> Option<&'static FeaturePack> {
    FEATURE_PACKS.iter().find(|pack| pack.id == id)
}

pub fn preset_by_id(id: &str) -> Option<&'static Preset> {
    PRESETS.iter().find(|preset| preset.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn feature_pack_ids_are_unique() {
        let mut ids = HashSet::new();
        for pack in FEATURE_PACKS {
            assert!(ids.insert(pack.id), "duplicate pack id: {}", pack.id);
        }
    }

    #[test]
    fn preset_ids_are_unique() {
        let mut ids = HashSet::new();
        for preset in PRESETS {
            assert!(ids.insert(preset.id), "duplicate preset id: {}", preset.id);
        }
    }

    #[test]
    fn presets_reference_existing_packs() {
        for preset in PRESETS {
            for pack_id in preset.packs {
                assert!(
                    feature_pack_by_id(pack_id).is_some(),
                    "preset '{}' references unknown pack '{}'",
                    preset.id,
                    pack_id
                );
            }
        }
    }

    #[test]
    fn update_pack_exists_and_is_marked_confirmed() {
        let pack = feature_pack_by_id("tools-update").expect("missing tools-update pack");
        assert!(pack.requires_confirmation);
        assert_eq!(pack.cargo_features, &["tool-update"]);
    }
}
