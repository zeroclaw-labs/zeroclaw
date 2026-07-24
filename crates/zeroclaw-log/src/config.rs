//! Policy types parsed from the runtime's observability config.

use std::path::{Path, PathBuf};

/// Minimal observability config shape used by the writer + tool-io
/// capturer. Mirrors the relevant `[observability]` fields of
/// `zeroclaw_config::schema::ObservabilityConfig`.
#[derive(Debug, Clone)]
pub struct LogConfig {
    pub log_persistence: String,
    pub log_persistence_path: String,
    pub log_persistence_max_entries: usize,
    /// Size threshold (bytes) that triggers an archive rotation in `rotating`
    /// mode. `0` disables size-based rotation.
    pub log_persistence_max_bytes: u64,
    /// Rotate on a UTC day boundary in `rotating` mode.
    pub log_persistence_rotate_daily: bool,
    /// Max rotated archive files to keep in `rotating` mode. `0` keeps all.
    pub log_persistence_retention_max_files: usize,
    /// Max age (days) of rotated archives in `rotating` mode. `0` disables.
    pub log_persistence_retention_max_age_days: u64,
    pub log_tool_io: String,
    pub log_tool_io_truncate_bytes: usize,
    pub log_tool_io_denylist: Vec<String>,
    pub log_llm_request_payload: String,
}

impl Default for LogConfig {
    fn default() -> Self {
        Self {
            log_persistence: "rolling".into(),
            log_persistence_path: String::new(),
            log_persistence_max_entries: 10_000,
            log_persistence_max_bytes: 0,
            log_persistence_rotate_daily: true,
            log_persistence_retention_max_files: 7,
            log_persistence_retention_max_age_days: 0,
            log_tool_io: "redacted".into(),
            log_tool_io_truncate_bytes: 40960,
            log_tool_io_denylist: Vec::new(),
            log_llm_request_payload: "off".into(),
        }
    }
}

const DEFAULT_LOG_REL_PATH: &str = "state/runtime-trace.jsonl";

/// JSONL persistence policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoragePolicy {
    /// Do not persist; in-process broadcast only.
    None,
    /// Persist with rolling trim once `max_entries` is exceeded.
    Rolling,
    /// Persist all events forever (operator manages rotation).
    Full,
    /// Persist all events, rotating the active file to timestamped archives on
    /// a size and/or daily boundary and pruning old archives by count and age.
    Rotating,
}

impl StoragePolicy {
    pub fn from_raw(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "rolling" => Self::Rolling,
            "full" => Self::Full,
            "rotating" => Self::Rotating,
            _ => Self::None,
        }
    }

    pub fn is_enabled(self) -> bool {
        !matches!(self, Self::None)
    }
}

/// Tool input/output capture policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolIoPolicy {
    /// Tool name + outcome + duration only. No I/O bodies.
    Off,
    /// Leak-scan + truncate to `truncate_bytes`. Default.
    Redacted,
    /// Full I/O, still leak-scanned. No truncation.
    Full,
}

impl ToolIoPolicy {
    pub fn from_raw(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "off" => Self::Off,
            "full" => Self::Full,
            _ => Self::Redacted,
        }
    }

    pub fn captures_io(self) -> bool {
        !matches!(self, Self::Off)
    }
}

/// LLM request payload capture policy. Mirrors [`ToolIoPolicy`] but gates the
/// prompt/messages on each `llm_request`. Unlike tool I/O, an unknown or
/// empty value resolves to [`Self::Off`] so the prompt is never captured
/// unless the operator explicitly opts in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LlmRequestPayloadPolicy {
    /// Only `messages_count` on the request event. No payload.
    Off,
    /// Leak-scan + truncate to `truncate_bytes`.
    Redacted,
    /// Full payload, still leak-scanned. No truncation.
    Full,
}

impl LlmRequestPayloadPolicy {
    pub fn from_raw(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "redacted" => Self::Redacted,
            "full" => Self::Full,
            _ => Self::Off,
        }
    }

    pub fn captures_payload(self) -> bool {
        !matches!(self, Self::Off)
    }
}

/// Resolved policy bundle the writer + tool-io capturers read at runtime.
#[derive(Debug, Clone)]
pub struct ResolvedPolicy {
    pub storage: StoragePolicy,
    pub path: PathBuf,
    pub max_entries: usize,
    /// Size threshold (bytes) that triggers a rotation in `Rotating` mode.
    /// `0` disables size-based rotation.
    pub max_bytes: u64,
    /// Rotate on a UTC day boundary in `Rotating` mode.
    pub rotate_daily: bool,
    /// Max rotated archive files to keep in `Rotating` mode. `0` keeps all.
    pub retention_max_files: usize,
    /// Max age (days) of rotated archives in `Rotating` mode. `0` disables.
    pub retention_max_age_days: u64,
    pub tool_io: ToolIoPolicy,
    pub tool_io_truncate_bytes: usize,
    pub tool_io_denylist: Vec<String>,
    pub llm_request_payload: LlmRequestPayloadPolicy,
}

impl ResolvedPolicy {
    pub fn from_config(config: &LogConfig, workspace_dir: &Path) -> Self {
        Self {
            storage: StoragePolicy::from_raw(&config.log_persistence),
            path: resolve_path(&config.log_persistence_path, workspace_dir),
            max_entries: config.log_persistence_max_entries.max(1),
            max_bytes: config.log_persistence_max_bytes,
            rotate_daily: config.log_persistence_rotate_daily,
            retention_max_files: config.log_persistence_retention_max_files,
            retention_max_age_days: config.log_persistence_retention_max_age_days,
            tool_io: ToolIoPolicy::from_raw(&config.log_tool_io),
            tool_io_truncate_bytes: config.log_tool_io_truncate_bytes,
            tool_io_denylist: config.log_tool_io_denylist.clone(),
            llm_request_payload: LlmRequestPayloadPolicy::from_raw(&config.log_llm_request_payload),
        }
    }

    pub fn is_tool_denylisted(&self, tool: &str) -> bool {
        self.tool_io_denylist.iter().any(|t| t == tool)
    }
}

fn resolve_path(raw: &str, workspace_dir: &Path) -> PathBuf {
    let raw = raw.trim();
    if raw.is_empty() {
        return workspace_dir.join(DEFAULT_LOG_REL_PATH);
    }
    let p = PathBuf::from(raw);
    if p.is_absolute() {
        p
    } else {
        workspace_dir.join(p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_config() -> LogConfig {
        LogConfig::default()
    }

    #[test]
    fn storage_policy_parses_known() {
        assert_eq!(StoragePolicy::from_raw("none"), StoragePolicy::None);
        assert_eq!(StoragePolicy::from_raw("rolling"), StoragePolicy::Rolling);
        assert_eq!(StoragePolicy::from_raw("full"), StoragePolicy::Full);
        assert_eq!(StoragePolicy::from_raw("rotating"), StoragePolicy::Rotating);
        assert_eq!(StoragePolicy::from_raw("xyz"), StoragePolicy::None);
        // Rotating still counts as an enabled (persisting) policy.
        assert!(StoragePolicy::Rotating.is_enabled());
    }

    #[test]
    fn tool_io_policy_defaults_to_redacted() {
        assert_eq!(ToolIoPolicy::from_raw(""), ToolIoPolicy::Redacted);
        assert_eq!(ToolIoPolicy::from_raw("redacted"), ToolIoPolicy::Redacted);
        assert_eq!(ToolIoPolicy::from_raw("off"), ToolIoPolicy::Off);
        assert_eq!(ToolIoPolicy::from_raw("full"), ToolIoPolicy::Full);
    }

    #[test]
    fn resolved_policy_uses_workspace_default_when_path_empty() {
        let mut c = make_config();
        c.log_persistence_path = String::new();
        let tmp = tempfile::tempdir().unwrap();
        let p = ResolvedPolicy::from_config(&c, tmp.path());
        assert_eq!(p.path, tmp.path().join(DEFAULT_LOG_REL_PATH));
    }

    #[test]
    fn resolved_policy_respects_denylist() {
        let mut c = make_config();
        c.log_tool_io_denylist = vec!["memory_recall_personal".to_string()];
        let p = ResolvedPolicy::from_config(&c, std::path::Path::new("/"));
        assert!(p.is_tool_denylisted("memory_recall_personal"));
        assert!(!p.is_tool_denylisted("shell"));
    }

    #[test]
    fn storage_policy_from_raw_trims_and_ignores_case() {
        assert_eq!(
            StoragePolicy::from_raw("  ROLLING  "),
            StoragePolicy::Rolling
        );
        assert_eq!(StoragePolicy::from_raw("Full"), StoragePolicy::Full);
    }

    #[test]
    fn storage_policy_is_enabled_only_when_persisting() {
        assert!(!StoragePolicy::None.is_enabled());
        assert!(StoragePolicy::Rolling.is_enabled());
        assert!(StoragePolicy::Full.is_enabled());
    }

    #[test]
    fn tool_io_policy_from_raw_trims_and_ignores_case() {
        assert_eq!(ToolIoPolicy::from_raw("  OFF "), ToolIoPolicy::Off);
        assert_eq!(ToolIoPolicy::from_raw("Full"), ToolIoPolicy::Full);
    }

    #[test]
    fn tool_io_policy_captures_io_unless_off() {
        assert!(!ToolIoPolicy::Off.captures_io());
        assert!(ToolIoPolicy::Redacted.captures_io());
        assert!(ToolIoPolicy::Full.captures_io());
    }

    #[test]
    fn resolved_policy_clamps_max_entries_to_at_least_one() {
        let mut c = make_config();
        c.log_persistence_max_entries = 0;
        let p = ResolvedPolicy::from_config(&c, std::path::Path::new("/"));
        assert_eq!(p.max_entries, 1);
    }

    #[test]
    fn resolved_policy_maps_storage_and_tool_io_fields() {
        let mut c = make_config();
        c.log_persistence = "full".to_string();
        c.log_tool_io = "off".to_string();
        c.log_tool_io_truncate_bytes = 123;
        let p = ResolvedPolicy::from_config(&c, std::path::Path::new("/"));
        assert_eq!(p.storage, StoragePolicy::Full);
        assert_eq!(p.tool_io, ToolIoPolicy::Off);
        assert_eq!(p.tool_io_truncate_bytes, 123);
    }
}
