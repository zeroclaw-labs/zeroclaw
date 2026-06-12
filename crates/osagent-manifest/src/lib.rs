//! osAgent MANIFEST.toml emission, parsing, and the `--diff` CLI handler.
//!
//! Two responsibilities:
//!
//! 1. **Build-time emission** — each binary's `build.rs` produces a
//!    `MANIFEST.toml` listing every compiled-in channel, provider, and tool in
//!    a `[declared]` section (from `CARGO_FEATURE_*` env vars + Cargo metadata)
//!    AND a `[detected]` section (from post-link symbol analysis). CI fails
//!    the build when `[declared] != [detected]` to catch orphaned features
//!    and silently-linked code.
//!
//! 2. **Runtime `--diff`** — `osagent manifest --diff <config.toml>` parses
//!    the operator's config and rejects it if the config references a
//!    channel/provider/tool that this binary's MANIFEST does not list.
//!    The wizard binary refusing to start when its config has an `[mcp]`
//!    section (because wizard's MANIFEST does not list `mcp`) is the
//!    operational consequence of the structural exclusion enforced by
//!    `bins/wizard/Cargo.toml` + the 4-layer CI gate (Phase 1.3).

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Manifest {
    pub schema_version: u32,
    pub binary_name: String,
    pub binary_version: String,
    pub fork_provenance: String,
    pub declared: Section,
    pub detected: Section,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct Section {
    #[serde(default)]
    pub channels: Vec<String>,
    #[serde(default)]
    pub providers: Vec<String>,
    #[serde(default)]
    pub tools: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum ManifestDiffError {
    #[error("config references channel {0:?} which is not in the binary's MANIFEST")]
    MissingChannel(String),
    #[error("config references provider {0:?} which is not in the binary's MANIFEST")]
    MissingProvider(String),
    #[error("config references tool {0:?} which is not in the binary's MANIFEST")]
    MissingTool(String),
    #[error("MANIFEST [declared] != [detected] for {kind}: declared_only={declared_only:?}, detected_only={detected_only:?}")]
    DeclaredDetectedMismatch {
        kind: &'static str,
        declared_only: Vec<String>,
        detected_only: Vec<String>,
    },
    #[error("config parse error: {0}")]
    ConfigParse(String),
}

impl Manifest {
    /// Compares the [declared] and [detected] sections. Any divergence means
    /// either: (a) a feature was declared but the code was orphaned, or
    /// (b) code linked into the binary that wasn't declared. Both are CI
    /// failures.
    pub fn self_consistency_check(&self) -> Result<(), ManifestDiffError> {
        fn diff_kind(
            kind: &'static str,
            declared: &[String],
            detected: &[String],
        ) -> Result<(), ManifestDiffError> {
            let d: BTreeSet<_> = declared.iter().cloned().collect();
            let det: BTreeSet<_> = detected.iter().cloned().collect();
            let declared_only: Vec<String> = d.difference(&det).cloned().collect();
            let detected_only: Vec<String> = det.difference(&d).cloned().collect();
            if declared_only.is_empty() && detected_only.is_empty() {
                Ok(())
            } else {
                Err(ManifestDiffError::DeclaredDetectedMismatch {
                    kind,
                    declared_only,
                    detected_only,
                })
            }
        }
        diff_kind("channels", &self.declared.channels, &self.detected.channels)?;
        diff_kind("providers", &self.declared.providers, &self.detected.providers)?;
        diff_kind("tools", &self.declared.tools, &self.detected.tools)?;
        Ok(())
    }
}

/// Walks the config TOML and rejects any reference to a channel / provider /
/// tool not present in the manifest's `declared` section.
///
/// The config schema mirrors zeroclaw's:
///   - `[channels.<name>]` table => channel reference
///   - `default_provider = "<name>"` top-level key => provider reference
///   - `[mcp]` table or `[[tools]] name = "<name>"` => tool reference
pub fn manifest_diff(manifest: &Manifest, config_toml: &str) -> Result<(), ManifestDiffError> {
    let parsed: toml::Value = toml::from_str(config_toml)
        .map_err(|e| ManifestDiffError::ConfigParse(e.to_string()))?;

    let declared = &manifest.declared;

    // Channels: top-level `[channels.<name>]` tables (also tolerates the upstream
    // `channels_config.<name>` alias).
    for key in ["channels", "channels_config"] {
        if let Some(toml::Value::Table(channels)) = parsed.get(key) {
            for ch_name in channels.keys() {
                if !declared.channels.iter().any(|c| c == ch_name) {
                    return Err(ManifestDiffError::MissingChannel(ch_name.clone()));
                }
            }
        }
    }

    // Providers: top-level `default_provider = "<name>"`.
    if let Some(toml::Value::String(p)) = parsed.get("default_provider") {
        if !declared.providers.iter().any(|x| x == p) {
            return Err(ManifestDiffError::MissingProvider(p.clone()));
        }
    }

    // Tools — two patterns:
    //  - `[mcp]` table OR `[[mcp.servers]]` array => requires tool "mcp"
    //  - `[[tools]] name = "<name>"` array => explicit tool reference
    if parsed.get("mcp").is_some() && !declared.tools.iter().any(|t| t == "mcp") {
        return Err(ManifestDiffError::MissingTool("mcp".to_string()));
    }
    if let Some(toml::Value::Array(tools)) = parsed.get("tools") {
        for tool in tools {
            if let Some(toml::Value::String(name)) = tool.get("name") {
                if !declared.tools.iter().any(|t| t == name) {
                    return Err(ManifestDiffError::MissingTool(name.clone()));
                }
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    #[test]
    fn section_defaults_to_empty() {
        let s = Section::default();
        assert!(s.channels.is_empty());
        assert!(s.providers.is_empty());
        assert!(s.tools.is_empty());
    }

    #[test]
    fn diff_error_messages_include_resource_name() {
        let err = ManifestDiffError::MissingChannel("discord".to_string());
        assert!(err.to_string().contains("discord"));
    }
}
