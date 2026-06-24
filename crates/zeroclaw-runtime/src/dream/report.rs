//! Dream report persistence — manages the "While you were away..." summary.
//!
//! After each dream cycle, a report is saved to `dream_report.json` in the
//! workspace directory. On the next user interaction, the agent runtime
//! loads and delivers the report, then marks it as delivered.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::Path;

const REPORT_FILENAME: &str = "dream_report.json";

/// A dream cycle report persisted for delivery on next user interaction.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DreamReport {
    /// Human-readable summary of the dream cycle.
    pub summary: String,
    /// Number of new insights consolidated.
    pub insights_count: usize,
    /// Number of stale memories pruned.
    pub pruned_count: usize,
    /// When the dream cycle completed.
    pub timestamp: DateTime<Utc>,
    /// Whether this report has been shown to the user.
    pub delivered: bool,
}

impl DreamReport {
    /// Persist the report to `dream_report.json` in the workspace directory.
    pub fn save(&self, workspace_dir: &Path) -> Result<()> {
        let path = workspace_dir.join(REPORT_FILENAME);
        let json =
            serde_json::to_string_pretty(self).context("dream report: failed to serialize")?;
        std::fs::write(&path, json)
            .with_context(|| format!("dream report: failed to write {}", path.display()))?;
        Ok(())
    }

    /// Load a pending (undelivered) dream report, if one exists.
    ///
    /// Returns `None` if the file doesn't exist or the report was already delivered.
    pub fn load_pending(workspace_dir: &Path) -> Result<Option<DreamReport>> {
        let path = workspace_dir.join(REPORT_FILENAME);
        if !path.exists() {
            return Ok(None);
        }

        let data = std::fs::read_to_string(&path)
            .with_context(|| format!("dream report: failed to read {}", path.display()))?;
        let report: DreamReport = serde_json::from_str(&data)
            .with_context(|| format!("dream report: failed to parse {}", path.display()))?;

        if report.delivered {
            return Ok(None);
        }

        Ok(Some(report))
    }

    /// Mark the report as delivered and persist the update.
    pub fn mark_delivered(workspace_dir: &Path) -> Result<()> {
        let path = workspace_dir.join(REPORT_FILENAME);
        if !path.exists() {
            return Ok(());
        }

        let data = std::fs::read_to_string(&path)
            .with_context(|| format!("dream report: failed to read {}", path.display()))?;
        let mut report: DreamReport = serde_json::from_str(&data)
            .with_context(|| format!("dream report: failed to parse {}", path.display()))?;

        report.delivered = true;

        let json =
            serde_json::to_string_pretty(&report).context("dream report: failed to serialize")?;
        std::fs::write(&path, json)
            .with_context(|| format!("dream report: failed to write {}", path.display()))?;

        Ok(())
    }

    /// Format the report as a user-facing "While you were away..." message.
    ///
    /// User-facing strings are routed through Fluent (`cli-dream-report-*`)
    /// when the i18n bundle is initialized; falls back to inline English when
    /// called outside the CLI (e.g. in unit tests).
    pub fn format_message(&self) -> String {
        let timestamp = self.timestamp.format("%Y-%m-%d %H:%M UTC").to_string();
        let header = crate::i18n::get_cli_string_with_args(
            "cli-dream-report-header",
            &[("timestamp", timestamp.as_str())],
        )
        .unwrap_or_else(|| format!("While you were away... ({timestamp})"));

        let mut msg = format!("{header}\n\n{}", self.summary);

        if self.insights_count > 0 || self.pruned_count > 0 {
            let insights_str = self.insights_count.to_string();
            let pruned_str = self.pruned_count.to_string();
            let counts = crate::i18n::get_cli_string_with_args(
                "cli-dream-report-counts",
                &[
                    ("insights", insights_str.as_str()),
                    ("pruned", pruned_str.as_str()),
                ],
            )
            .unwrap_or_else(|| {
                format!(
                    "({} insights consolidated, {} stale memories pruned)",
                    self.insights_count, self.pruned_count
                )
            });
            msg.push_str("\n\n");
            msg.push_str(&counts);
        }
        msg
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_roundtrip() {
        let temp = tempfile::tempdir().unwrap();
        let report = DreamReport {
            summary: "Learned user prefers Rust.".into(),
            insights_count: 3,
            pruned_count: 1,
            timestamp: Utc::now(),
            delivered: false,
        };

        report.save(temp.path()).unwrap();

        let loaded = DreamReport::load_pending(temp.path()).unwrap();
        assert!(loaded.is_some());
        let loaded = loaded.unwrap();
        assert_eq!(loaded.summary, "Learned user prefers Rust.");
        assert!(!loaded.delivered);
    }

    #[test]
    fn delivered_report_is_not_pending() {
        let temp = tempfile::tempdir().unwrap();
        let report = DreamReport {
            summary: "Done.".into(),
            insights_count: 0,
            pruned_count: 0,
            timestamp: Utc::now(),
            delivered: true,
        };

        report.save(temp.path()).unwrap();

        let loaded = DreamReport::load_pending(temp.path()).unwrap();
        assert!(loaded.is_none());
    }

    #[test]
    fn mark_delivered_updates_file() {
        let temp = tempfile::tempdir().unwrap();
        let report = DreamReport {
            summary: "Test.".into(),
            insights_count: 1,
            pruned_count: 0,
            timestamp: Utc::now(),
            delivered: false,
        };

        report.save(temp.path()).unwrap();
        DreamReport::mark_delivered(temp.path()).unwrap();

        let loaded = DreamReport::load_pending(temp.path()).unwrap();
        assert!(loaded.is_none());
    }

    #[test]
    fn format_message_includes_summary() {
        let report = DreamReport {
            summary: "User prefers dark themes.".into(),
            insights_count: 2,
            pruned_count: 1,
            timestamp: Utc::now(),
            delivered: false,
        };

        // Tests run without the Fluent bundle initialized, so the formatter
        // falls back to the inline English defaults.
        let msg = report.format_message();
        assert!(msg.contains("While you were away..."));
        assert!(msg.contains("User prefers dark themes."));
        assert!(msg.contains("2 insights consolidated"));
        assert!(msg.contains("1 stale memories pruned"));
    }

    #[test]
    fn no_report_file_returns_none() {
        let temp = tempfile::tempdir().unwrap();
        let loaded = DreamReport::load_pending(temp.path()).unwrap();
        assert!(loaded.is_none());
    }
}
