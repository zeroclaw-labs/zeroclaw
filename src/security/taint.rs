//! Information flow taint tracking for data origin labeling.
//!
//! Labels data with its origin (user input, external API, tool output, etc.)
//! and provides utilities to check whether tainted data flows into sensitive
//! operations. This is an informational MVP — taint labels are tracked and
//! warnings are emitted, but tainted data is not blocked from flowing.
//!
//! # Usage
//!
//! ```rust
//! use zeroclaw::security::taint::{TaintLabel, TaintSource};
//!
//! let label = TaintLabel::untrusted(TaintSource::UserInput);
//! assert!(label.is_tainted());
//!
//! let trusted = TaintLabel::trusted();
//! assert!(!trusted.is_tainted());
//!
//! let merged = label.merge(&trusted);
//! assert!(merged.is_tainted());
//! ```

use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// Origin of a piece of data flowing through the agent runtime.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaintSource {
    /// Content received from a channel message (human or bot).
    UserInput,
    /// Content fetched from an external API (`web_fetch`, `http_request`).
    ExternalApi,
    /// Output produced by a tool execution (`shell`, `browser`, etc.).
    ToolOutput,
    /// Content read from the filesystem (`file_read`).
    FileSystem,
    /// Content recalled from memory (`memory_recall`).
    Memory,
    /// System-generated content considered inherently trusted.
    Trusted,
}

/// A set of taint sources attached to a piece of data.
///
/// An empty source set means the data is trusted (system-generated).
/// Any non-empty set indicates potentially untrusted data that should
/// be treated with care when flowing into sensitive operations.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TaintLabel {
    #[serde(default)]
    pub sources: HashSet<TaintSource>,
}

impl TaintLabel {
    /// Create a label marking data as originating from a single untrusted source.
    pub fn untrusted(source: TaintSource) -> Self {
        let mut sources = HashSet::new();
        sources.insert(source);
        Self { sources }
    }

    /// Create a label marking data as trusted (no taint sources).
    pub fn trusted() -> Self {
        Self {
            sources: HashSet::new(),
        }
    }

    /// Merge two taint labels, producing a label with the union of all sources.
    ///
    /// This models data composition: when two data values are combined, the
    /// result inherits taint from both.
    pub fn merge(&self, other: &TaintLabel) -> Self {
        Self {
            sources: self.sources.union(&other.sources).cloned().collect(),
        }
    }

    /// Returns `true` if this label carries any non-`Trusted` taint source.
    pub fn is_tainted(&self) -> bool {
        self.sources
            .iter()
            .any(|s| !matches!(s, TaintSource::Trusted))
    }

    /// Returns `true` if this label has no taint sources (i.e. fully trusted).
    pub fn is_empty(&self) -> bool {
        self.sources.is_empty()
    }

    /// Returns `true` if this label contains the specified taint source.
    pub fn contains(&self, source: &TaintSource) -> bool {
        self.sources.contains(source)
    }

    /// Determine the appropriate taint source for a tool by name.
    ///
    /// Maps well-known tool names to their corresponding taint source:
    /// - `shell`, `browser`, `browser_open` → `ToolOutput`
    /// - `web_fetch`, `http_request` → `ExternalApi`
    /// - `file_read` → `FileSystem`
    /// - `memory_recall` → `Memory`
    /// - All others → `ToolOutput` (conservative default)
    pub fn source_for_tool(tool_name: &str) -> TaintSource {
        match tool_name {
            "web_fetch" | "http_request" => TaintSource::ExternalApi,
            "file_read" => TaintSource::FileSystem,
            "memory_recall" => TaintSource::Memory,
            _ => TaintSource::ToolOutput,
        }
    }
}

/// Check whether tainted data is flowing into a sensitive operation.
///
/// Returns a warning message describing the taint if the label is tainted,
/// or `None` if the data is clean. This is informational only in the current
/// MVP — callers may log the warning but should not block execution.
pub fn check_taint(label: &TaintLabel, operation: &str) -> Option<String> {
    if !label.is_tainted() {
        return None;
    }

    let source_names: Vec<&str> = label
        .sources
        .iter()
        .filter(|s| !matches!(s, TaintSource::Trusted))
        .map(|s| match s {
            TaintSource::UserInput => "user_input",
            TaintSource::ExternalApi => "external_api",
            TaintSource::ToolOutput => "tool_output",
            TaintSource::FileSystem => "file_system",
            TaintSource::Memory => "memory",
            TaintSource::Trusted => unreachable!(),
        })
        .collect();

    Some(format!(
        "taint warning: data with sources [{}] flowing into {operation}",
        source_names.join(", ")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trusted_label_is_not_tainted() {
        let label = TaintLabel::trusted();
        assert!(!label.is_tainted());
        assert!(label.is_empty());
    }

    #[test]
    fn untrusted_label_is_tainted() {
        let label = TaintLabel::untrusted(TaintSource::UserInput);
        assert!(label.is_tainted());
        assert!(!label.is_empty());
    }

    #[test]
    fn trusted_source_alone_is_not_tainted() {
        let label = TaintLabel::untrusted(TaintSource::Trusted);
        assert!(!label.is_tainted());
        assert!(!label.is_empty());
    }

    #[test]
    fn contains_checks_specific_source() {
        let label = TaintLabel::untrusted(TaintSource::ExternalApi);
        assert!(label.contains(&TaintSource::ExternalApi));
        assert!(!label.contains(&TaintSource::UserInput));
    }

    #[test]
    fn merge_unions_sources() {
        let a = TaintLabel::untrusted(TaintSource::UserInput);
        let b = TaintLabel::untrusted(TaintSource::ExternalApi);
        let merged = a.merge(&b);

        assert!(merged.contains(&TaintSource::UserInput));
        assert!(merged.contains(&TaintSource::ExternalApi));
        assert!(!merged.contains(&TaintSource::FileSystem));
        assert!(merged.is_tainted());
    }

    #[test]
    fn merge_with_trusted_preserves_taint() {
        let tainted = TaintLabel::untrusted(TaintSource::ToolOutput);
        let trusted = TaintLabel::trusted();
        let merged = tainted.merge(&trusted);

        assert!(merged.is_tainted());
        assert!(merged.contains(&TaintSource::ToolOutput));
    }

    #[test]
    fn merge_two_trusted_remains_untainted() {
        let a = TaintLabel::trusted();
        let b = TaintLabel::trusted();
        let merged = a.merge(&b);

        assert!(!merged.is_tainted());
        assert!(merged.is_empty());
    }

    #[test]
    fn check_taint_returns_none_for_trusted() {
        let label = TaintLabel::trusted();
        assert!(check_taint(&label, "secret_store_write").is_none());
    }

    #[test]
    fn check_taint_returns_warning_for_tainted() {
        let label = TaintLabel::untrusted(TaintSource::UserInput);
        let warning = check_taint(&label, "config_update");
        assert!(warning.is_some());
        let msg = warning.unwrap();
        assert!(msg.contains("user_input"));
        assert!(msg.contains("config_update"));
    }

    #[test]
    fn check_taint_lists_multiple_sources() {
        let mut label = TaintLabel::untrusted(TaintSource::UserInput);
        label.sources.insert(TaintSource::ExternalApi);
        let warning = check_taint(&label, "policy_eval").unwrap();
        assert!(warning.contains("user_input"));
        assert!(warning.contains("external_api"));
    }

    #[test]
    fn source_for_tool_maps_correctly() {
        assert_eq!(
            TaintLabel::source_for_tool("shell"),
            TaintSource::ToolOutput
        );
        assert_eq!(
            TaintLabel::source_for_tool("web_fetch"),
            TaintSource::ExternalApi
        );
        assert_eq!(
            TaintLabel::source_for_tool("http_request"),
            TaintSource::ExternalApi
        );
        assert_eq!(
            TaintLabel::source_for_tool("file_read"),
            TaintSource::FileSystem
        );
        assert_eq!(
            TaintLabel::source_for_tool("memory_recall"),
            TaintSource::Memory
        );
        assert_eq!(
            TaintLabel::source_for_tool("unknown_tool"),
            TaintSource::ToolOutput
        );
    }

    #[test]
    fn serde_roundtrip_preserves_label() {
        let mut label = TaintLabel::untrusted(TaintSource::UserInput);
        label.sources.insert(TaintSource::ExternalApi);

        let json = serde_json::to_string(&label).unwrap();
        let parsed: TaintLabel = serde_json::from_str(&json).unwrap();

        assert!(parsed.contains(&TaintSource::UserInput));
        assert!(parsed.contains(&TaintSource::ExternalApi));
        assert_eq!(parsed.sources.len(), 2);
    }

    #[test]
    fn serde_default_deserializes_empty_label() {
        let json = "{}";
        let label: TaintLabel = serde_json::from_str(json).unwrap();
        assert!(!label.is_tainted());
        assert!(label.is_empty());
    }

    #[test]
    fn taint_source_serde_uses_snake_case() {
        let source = TaintSource::UserInput;
        let json = serde_json::to_string(&source).unwrap();
        assert_eq!(json, "\"user_input\"");

        let source = TaintSource::ExternalApi;
        let json = serde_json::to_string(&source).unwrap();
        assert_eq!(json, "\"external_api\"");
    }

    #[test]
    fn is_empty_returns_true_for_default() {
        let label = TaintLabel::default();
        assert!(label.is_empty());
        assert!(!label.is_tainted());
    }
}
