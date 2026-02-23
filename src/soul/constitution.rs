//! Constitution — immutable laws with SHA-256 integrity verification.
//!
//! The constitution defines 3 immutable laws that an agent must never violate.
//! A SHA-256 hash of the laws is computed at genesis and verified on every load
//! to detect tampering.

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Default constitution laws (Automaton-inspired).
const DEFAULT_LAWS: [&str; 3] = [
    "Never deceive or mislead humans about your nature as an AI",
    "Never take actions that could cause irreversible harm without explicit human approval",
    "Always preserve the ability for humans to override or shut down the agent",
];

/// Immutable agent constitution with integrity verification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Constitution {
    /// The three immutable laws
    laws: [String; 3],
    /// SHA-256 hash of the laws (computed at genesis, verified on load)
    hash: String,
}

impl Constitution {
    /// Create a new constitution with default laws.
    pub fn default_laws() -> Self {
        let laws = DEFAULT_LAWS.map(String::from);
        let hash = Self::compute_hash(&laws);
        Self { laws, hash }
    }

    /// Create a constitution with custom laws.
    pub fn new(laws: [String; 3]) -> Self {
        let hash = Self::compute_hash(&laws);
        Self { laws, hash }
    }

    /// Create a constitution from laws and a pre-computed hash.
    /// Verifies integrity on construction.
    pub fn from_parts(laws: [String; 3], expected_hash: &str) -> Result<Self> {
        let computed = Self::compute_hash(&laws);
        if computed != expected_hash {
            bail!(
                "Constitution integrity check failed.\n\
                 Expected hash: {expected_hash}\n\
                 Computed hash: {computed}\n\
                 The constitution may have been tampered with."
            );
        }
        Ok(Self {
            laws,
            hash: computed,
        })
    }

    /// Verify that the constitution has not been tampered with.
    pub fn verify_integrity(&self) -> Result<()> {
        let computed = Self::compute_hash(&self.laws);
        if computed != self.hash {
            bail!(
                "Constitution integrity verification failed.\n\
                 Stored hash:   {}\n\
                 Computed hash: {computed}\n\
                 The constitution may have been tampered with.",
                self.hash
            );
        }
        Ok(())
    }

    /// Get the three laws.
    pub fn laws(&self) -> &[String; 3] {
        &self.laws
    }

    /// Get the SHA-256 hash.
    pub fn hash(&self) -> &str {
        &self.hash
    }

    /// Compute SHA-256 hash of the laws.
    fn compute_hash(laws: &[String; 3]) -> String {
        let mut hasher = Sha256::new();
        for (i, law) in laws.iter().enumerate() {
            hasher.update(format!("law_{i}:{law}"));
        }
        hex::encode(hasher.finalize())
    }

    /// Render the constitution as a markdown section for system prompt injection.
    pub fn to_prompt_section(&self) -> String {
        use std::fmt::Write;
        let mut out = String::from("**Constitution (Immutable):**\n");
        for (i, law) in self.laws.iter().enumerate() {
            let _ = writeln!(out, "{}. {law}", i + 1);
        }
        out.trim_end().to_string()
    }
}

impl Default for Constitution {
    fn default() -> Self {
        Self::default_laws()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_constitution_has_three_laws() {
        let constitution = Constitution::default();
        assert_eq!(constitution.laws().len(), 3);
        assert!(!constitution.hash().is_empty());
    }

    #[test]
    fn default_constitution_passes_integrity_check() {
        let constitution = Constitution::default();
        constitution.verify_integrity().unwrap();
    }

    #[test]
    fn custom_constitution_passes_integrity_check() {
        let laws = ["Law one".into(), "Law two".into(), "Law three".into()];
        let constitution = Constitution::new(laws);
        constitution.verify_integrity().unwrap();
    }

    #[test]
    fn tampered_constitution_fails_integrity_check() {
        let mut constitution = Constitution::default();
        // Tamper with a law
        constitution.laws[0] = "Tampered law".into();
        assert!(constitution.verify_integrity().is_err());
    }

    #[test]
    fn from_parts_accepts_valid_hash() {
        let laws = ["First law".into(), "Second law".into(), "Third law".into()];
        let hash = Constitution::compute_hash(&laws);
        let constitution = Constitution::from_parts(laws, &hash).unwrap();
        constitution.verify_integrity().unwrap();
    }

    #[test]
    fn from_parts_rejects_invalid_hash() {
        let laws = ["First law".into(), "Second law".into(), "Third law".into()];
        let result = Constitution::from_parts(laws, "invalid_hash");
        assert!(result.is_err());
    }

    #[test]
    fn constitution_serde_roundtrip() {
        let constitution = Constitution::default();
        let json = serde_json::to_string(&constitution).unwrap();
        let parsed: Constitution = serde_json::from_str(&json).unwrap();
        parsed.verify_integrity().unwrap();
        assert_eq!(parsed.hash(), constitution.hash());
    }

    #[test]
    fn constitution_renders_prompt_section() {
        let constitution = Constitution::default();
        let rendered = constitution.to_prompt_section();
        assert!(rendered.contains("**Constitution (Immutable):**"));
        assert!(rendered.contains("1."));
        assert!(rendered.contains("2."));
        assert!(rendered.contains("3."));
    }

    #[test]
    fn hash_is_deterministic() {
        let a = Constitution::default();
        let b = Constitution::default();
        assert_eq!(a.hash(), b.hash());
    }

    #[test]
    fn different_laws_produce_different_hashes() {
        let a = Constitution::default();
        let b = Constitution::new([
            "Custom law 1".into(),
            "Custom law 2".into(),
            "Custom law 3".into(),
        ]);
        assert_ne!(a.hash(), b.hash());
    }
}
