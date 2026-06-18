use crate::cleanup_rule::{CleanupRule, resolve_cleanup_path};
use anyhow::Result;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Cleanup execution report.
pub struct EnforceReport {
    pub rules_executed: usize,
    pub files_deleted: usize,
    pub bytes_freed: u64,
    pub errors: Vec<(String, String)>, // (rule_path, error_msg)
}

/// Directory usage information.
pub struct UsageInfo {
    pub total_size_mb: f64,
    pub file_count: usize,
    pub oldest_file_age_hours: f64,
    pub newest_file_age_hours: f64,
}

use tokio_util::sync::CancellationToken;

fn temp_file_event(attrs: serde_json::Value) -> zeroclaw_log::Event {
    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(attrs)
}

pub struct TempFileManager {
    cleanup_root: PathBuf,
    rules: Vec<Arc<CleanupRule>>,
    enabled: bool,
    /// Scheduled cleanup task configuration.
    scheduled_cleanup_enabled: bool,
    scheduled_cleanup_interval_hours: f64,
}

impl TempFileManager {
    /// Build a cleanup manager from resolved cleanup settings.
    pub fn from_params<I, RulePath, RulePattern>(
        cleanup_root: &Path,
        enabled: bool,
        scheduled_cleanup_enabled: bool,
        scheduled_cleanup_interval_hours: f64,
        rules: I,
    ) -> Result<Self>
    where
        I: IntoIterator<Item = (RulePath, Option<RulePattern>, u64, u64)>,
        RulePath: AsRef<str>,
        RulePattern: AsRef<str>,
    {
        if !enabled {
            ::zeroclaw_log::record!(
                INFO,
                temp_file_event(json!({"enabled": false})),
                "Temporary file cleanup is disabled"
            );
            return Ok(Self {
                cleanup_root: cleanup_root.to_path_buf(),
                rules: vec![],
                enabled: false,
                scheduled_cleanup_enabled,
                scheduled_cleanup_interval_hours,
            });
        }

        let mut registered_rules = Vec::new();

        // Register only the rules explicitly declared by the caller.
        for (rule_path, rule_pattern, retention_hours, max_size_mb) in rules {
            let rule_path = rule_path.as_ref();
            let rule_pattern = rule_pattern.as_ref().map(|pattern| pattern.as_ref());
            match CleanupRule::new(
                cleanup_root,
                rule_path,
                rule_pattern,
                retention_hours,
                max_size_mb,
            ) {
                Ok(rule) => {
                    ::zeroclaw_log::record!(
                        INFO,
                        temp_file_event(json!({
                            "rule_path": rule.path_display(),
                            "retention_hours": retention_hours,
                            "max_size_mb": max_size_mb
                        })),
                        "Registered custom cleanup rule"
                    );
                    registered_rules.push(Arc::new(rule));
                }
                Err(e) => {
                    ::zeroclaw_log::record!(
                        WARN,
                        temp_file_event(json!({
                            "rule_path": rule_path,
                            "error": e.to_string()
                        }))
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure),
                        "Failed to create custom cleanup rule"
                    );
                }
            }
        }

        Ok(Self {
            cleanup_root: cleanup_root.to_path_buf(),
            rules: registered_rules,
            enabled: true,
            scheduled_cleanup_enabled,
            scheduled_cleanup_interval_hours,
        })
    }

    /// Register a file and trigger cleanup for matching rules.
    pub fn register(&self, path: &Path) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }

        // Find matching rules and run cleanup.
        for rule in &self.rules {
            if rule.matches(&self.cleanup_root, path) {
                match rule.register_and_enforce(&self.cleanup_root, path) {
                    Ok(deleted) => {
                        if !deleted.is_empty() {
                            ::zeroclaw_log::record!(
                                INFO,
                                temp_file_event(json!({
                                    "registered_file": path.display().to_string(),
                                    "deleted_count": deleted.len()
                                })),
                                "Registered file and triggered cleanup"
                            );
                        }
                    }
                    Err(e) => {
                        ::zeroclaw_log::record!(
                            WARN,
                            temp_file_event(json!({
                                "registered_file": path.display().to_string(),
                                "error": e.to_string()
                            }))
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure),
                            "Cleanup failed after registering file"
                        );
                    }
                }
            }
        }

        Ok(())
    }

    /// Run all cleanup rules manually.
    pub fn enforce_all(&self) -> Result<EnforceReport> {
        let mut report = EnforceReport {
            rules_executed: 0,
            files_deleted: 0,
            bytes_freed: 0,
            errors: vec![],
        };

        ::zeroclaw_log::record!(
            DEBUG,
            temp_file_event(json!({"rules_count": self.rules.len()})),
            "Starting cleanup scan"
        );

        for rule in &self.rules {
            report.rules_executed += 1;
            match rule.enforce(&self.cleanup_root) {
                Ok(deleted) => {
                    let count = deleted.len();
                    // Estimate reclaimed space; exact sizes are unavailable after deletion.
                    report.files_deleted += count;
                }
                Err(e) => {
                    report.errors.push((rule.path_display(), e.to_string()));
                }
            }
        }

        Ok(report)
    }

    /// Query usage information for a cleanup-relative path.
    pub fn get_usage(&self, cleanup_root: &Path, rel_path: &str) -> Result<UsageInfo> {
        use crate::dir_monitor::DirMonitor;

        let dir = resolve_cleanup_path(cleanup_root, rel_path)
            .map_err(|error| anyhow::Error::msg(error.to_string()))?;
        if !dir.exists() {
            return Ok(UsageInfo {
                total_size_mb: 0.0,
                file_count: 0,
                oldest_file_age_hours: 0.0,
                newest_file_age_hours: 0.0,
            });
        }

        let files = DirMonitor::enumerate_files(&dir, None)?;
        let total_size_bytes: u64 = files.iter().map(|f| f.size_bytes).sum();
        let total_size_mb = total_size_bytes as f64 / 1024.0 / 1024.0;

        let now = std::time::SystemTime::now();
        let ages_hours: Vec<f64> = files
            .iter()
            .filter_map(|f| {
                f.mtime
                    .duration_since(now)
                    .ok()
                    .map(|d| d.as_secs_f64() / 3600.0)
            })
            .collect();

        let oldest = ages_hours.iter().cloned().fold(f64::NAN, f64::max);
        let newest = ages_hours.iter().cloned().fold(f64::NAN, f64::min);

        Ok(UsageInfo {
            total_size_mb,
            file_count: files.len(),
            oldest_file_age_hours: if oldest.is_nan() { 0.0 } else { oldest },
            newest_file_age_hours: if newest.is_nan() { 0.0 } else { newest },
        })
    }

    /// Return the number of registered rules.
    pub fn rules_count(&self) -> usize {
        self.rules.len()
    }

    /// Return whether cleanup is enabled.
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Return whether scheduled cleanup is enabled.
    pub fn scheduled_cleanup_enabled(&self) -> bool {
        self.scheduled_cleanup_enabled
    }

    /// Return the scheduled cleanup interval in hours.
    pub fn scheduled_cleanup_interval_hours(&self) -> f64 {
        self.scheduled_cleanup_interval_hours
    }

    /// Calculate the interval duration with minimum enforcement (1 minute)
    fn calculate_interval_duration(&self) -> std::time::Duration {
        let minutes = self.scheduled_cleanup_interval_hours * 60.0;

        // Minimum enforcement: not less than 1 minute
        let effective_minutes = if minutes < 1.0 {
            ::zeroclaw_log::record!(
                WARN,
                temp_file_event(json!({
                    "configured_hours": self.scheduled_cleanup_interval_hours
                }))
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                "Cleanup interval too small (< 1 minute), adjusted to 1 minute"
            );
            1.0
        } else {
            minutes
        };

        std::time::Duration::from_secs_f64(effective_minutes * 60.0)
    }

    /// Start the scheduled cleanup task in the background.
    pub async fn start_scheduled_cleanup(
        self: Arc<Self>,
        cancel_token: CancellationToken,
    ) -> Result<()> {
        if !self.scheduled_cleanup_enabled || !self.enabled {
            ::zeroclaw_log::record!(
                DEBUG,
                temp_file_event(json!({
                    "scheduled_cleanup_enabled": self.scheduled_cleanup_enabled,
                    "enabled": self.enabled
                })),
                "Scheduled cleanup is disabled"
            );
            return Ok(());
        }

        if cancel_token.is_cancelled() {
            ::zeroclaw_log::record!(
                INFO,
                temp_file_event(json!({})),
                "Scheduled cleanup task cancelled"
            );
            return Ok(());
        }

        let interval = self.calculate_interval_duration();
        let interval_minutes = self.scheduled_cleanup_interval_hours * 60.0;
        ::zeroclaw_log::record!(
            INFO,
            temp_file_event(json!({
                "interval_hours": self.scheduled_cleanup_interval_hours,
                "interval_minutes": interval_minutes
            })),
            "Starting scheduled cleanup task"
        );

        // Run one cleanup pass immediately at startup.
        ::zeroclaw_log::record!(
            INFO,
            temp_file_event(json!({})),
            "Running initial startup cleanup scan"
        );

        match self.enforce_all() {
            Ok(report) => {
                if report.files_deleted > 0 {
                    ::zeroclaw_log::record!(
                        INFO,
                        temp_file_event(json!({
                            "rules_executed": report.rules_executed,
                            "files_deleted": report.files_deleted,
                            "bytes_freed": report.bytes_freed
                        })),
                        "Startup cleanup completed"
                    );
                } else {
                    ::zeroclaw_log::record!(
                        INFO,
                        temp_file_event(json!({"rules_scanned": report.rules_executed})),
                        "Startup cleanup scan completed, no files matched deletion criteria"
                    );
                }
            }
            Err(e) => {
                ::zeroclaw_log::record!(
                    WARN,
                    temp_file_event(json!({"error": e.to_string()}))
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure),
                    "Startup cleanup failed"
                );
            }
        }

        // Then enter the periodic cleanup loop.
        loop {
            tokio::select! {
                _ = tokio::time::sleep(interval) => {
                    ::zeroclaw_log::record!(
                        INFO,
                        temp_file_event(json!({})),
                        "Starting scheduled cleanup scan"
                    );

                    match self.enforce_all() {
                        Ok(report) => {
                            if report.files_deleted > 0 {
                                ::zeroclaw_log::record!(
                                    INFO,
                                    temp_file_event(json!({
                                        "rules_executed": report.rules_executed,
                                        "files_deleted": report.files_deleted,
                                        "bytes_freed": report.bytes_freed
                                    })),
                                    "Scheduled cleanup completed"
                                );
                            } else {
                                ::zeroclaw_log::record!(
                                    INFO,
                                    temp_file_event(json!({"rules_scanned": report.rules_executed})),
                                    "Scheduled cleanup scan completed, no files matched deletion criteria"
                                );
                            }
                        }
                        Err(e) => {
                            ::zeroclaw_log::record!(
                                WARN,
                                temp_file_event(json!({"error": e.to_string()}))
                                    .with_outcome(::zeroclaw_log::EventOutcome::Failure),
                                "Scheduled cleanup failed"
                            );
                        }
                    }
                }
                _ = cancel_token.cancelled() => {
                    ::zeroclaw_log::record!(
                        INFO,
                        temp_file_event(json!({})),
                        "Scheduled cleanup task cancelled"
                    );
                    break;
                }
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests;
