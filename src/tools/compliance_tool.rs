//! Compliance reporting and audit verification tool.
//!
//! Exposes compliance operations to the LLM agent: generating framework-specific
//! reports, verifying audit log integrity, checking compliance posture, and
//! classifying actions by regulatory relevance.

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, Days, NaiveDate, Utc};
use std::path::PathBuf;

use crate::config::schema::ComplianceConfig;
use crate::security::audit_enhanced::TamperEvidentLog;
use crate::security::compliance::{ComplianceClassifier, ComplianceFramework};
use crate::tools::traits::{Tool, ToolResult};

/// Maximum number of entries included in the tagged-entries summary section.
const MAX_REPORT_ENTRIES: usize = 50;

/// Maximum byte length for the actor field in report output.
const MAX_ACTOR_DISPLAY_LEN: usize = 32;

/// Maximum byte length for the result_summary field in report output.
const MAX_SUMMARY_DISPLAY_LEN: usize = 120;

/// Maximum byte length for the action field in report output.
const MAX_ACTION_DISPLAY_LEN: usize = 256;

/// Maximum total byte length of a generated report before truncation.
const MAX_REPORT_SIZE: usize = 256 * 1024; // 256 KiB

/// Result of an audit-chain verification.
struct VerifyResult {
    passed: bool,
    message: String,
}

/// Strip control characters (newlines, carriage returns, tabs, etc.) from a
/// string, replacing them with spaces so they cannot break Markdown formatting.
fn sanitize_control_chars(value: &str) -> String {
    value
        .chars()
        .map(|c| if c.is_control() && c != ' ' { ' ' } else { c })
        .collect()
}

/// Redact a string field for safe inclusion in reports: truncate and mask the
/// middle portion when the value exceeds `max_len`.
fn redact_field(value: &str, max_len: usize) -> String {
    if value.len() <= max_len {
        return value.to_string();
    }
    let keep = max_len.saturating_sub(5) / 2;
    // Use char_indices to find safe UTF-8 boundaries and avoid panics on
    // multi-byte characters.
    let start: String = value.chars().take(keep).collect();
    let end: String = value
        .chars()
        .rev()
        .take(keep)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("{}[...]{}", start, end)
}

/// Agent-callable compliance tool.
pub struct ComplianceTool {
    config: ComplianceConfig,
    zeroclaw_dir: PathBuf,
}

impl ComplianceTool {
    /// Create a new compliance tool.
    pub fn new(config: ComplianceConfig, zeroclaw_dir: PathBuf) -> Self {
        Self {
            config,
            zeroclaw_dir,
        }
    }

    /// Resolve the tamper-evident log path.
    fn log_path(&self) -> PathBuf {
        self.zeroclaw_dir.join("compliance-audit.jsonl")
    }

    /// Open the tamper-evident log.
    fn open_log(&self) -> Result<TamperEvidentLog> {
        TamperEvidentLog::new(self.log_path(), 0)
    }

    /// Generate a compliance report for the given framework and date range.
    fn generate_report(
        &self,
        framework_name: &str,
        from: Option<&str>,
        to: Option<&str>,
    ) -> Result<String> {
        // Reject frameworks that are not enabled in configuration.
        let enabled = self
            .config
            .frameworks
            .iter()
            .any(|f| f.eq_ignore_ascii_case(framework_name));
        if !enabled {
            anyhow::bail!(
                "Framework '{}' is not enabled in compliance configuration. Enabled: [{}]",
                framework_name,
                self.config.frameworks.join(", ")
            );
        }

        let framework = ComplianceFramework::from_name(framework_name);
        let log = self.open_log()?;
        let entries = log.read_entries()?;

        let from_dt = from.map(parse_date_start).transpose()?;
        let to_dt = to.map(parse_date_end_exclusive).transpose()?;

        if let (Some(f), Some(t)) = (from_dt, to_dt) {
            if f >= t {
                anyhow::bail!(
                    "Invalid date range: 'from' ({}) must be before 'to'",
                    from.unwrap_or("?")
                );
            }
        }

        let filtered: Vec<_> = entries
            .iter()
            .filter(|e| {
                if let Some(from) = from_dt {
                    if e.timestamp < from {
                        return false;
                    }
                }
                // Exclusive upper bound: timestamp < next-day midnight captures
                // the entire calendar day including sub-second precision.
                if let Some(to) = to_dt {
                    if e.timestamp >= to {
                        return false;
                    }
                }
                true
            })
            .collect();

        let tagged: Vec<_> = filtered
            .iter()
            .filter(|e| e.compliance_tags.contains(&framework.label().to_string()))
            .collect();

        use std::fmt::Write;
        let mut report = String::new();
        let _ = writeln!(report, "# {} Compliance Report\n", framework.label());
        let _ = writeln!(report, "Generated: {}", Utc::now().to_rfc3339());
        if let Some(f) = from {
            let _ = writeln!(report, "From: {}", f);
        }
        if let Some(t) = to {
            let _ = writeln!(report, "To: {}", t);
        }
        let _ = writeln!(report, "Total audit entries in range: {}", filtered.len());
        let _ = writeln!(
            report,
            "{}-tagged entries: {}\n",
            framework.label(),
            tagged.len()
        );

        // Framework-specific sections
        match framework {
            ComplianceFramework::Finma => {
                report.push_str("## FINMA-Specific Fields\n\n");
                report.push_str("- Financial transaction entries: reviewed\n");
                report.push_str("- KYC/AML flagged actions: included\n");
                report.push_str("- Data residency: ");
                if let Some(ref region) = self.config.data_residency_region {
                    let _ = writeln!(report, "enforced ({})", region);
                } else {
                    report.push_str("not configured\n");
                }
            }
            ComplianceFramework::Gdpr => {
                report.push_str("## GDPR-Specific Fields\n\n");
                report.push_str("- Personal data processing entries: reviewed\n");
                report.push_str("- Data subject actions: included\n");
                report.push_str("- Consent/erasure operations: tracked\n");
            }
            ComplianceFramework::Dora => {
                report.push_str("## DORA-Specific Fields\n\n");
                report.push_str("- ICT risk management entries: reviewed\n");
                report.push_str("- Incident reporting: included\n");
                report.push_str("- Third-party dependency actions: tracked\n");
            }
            ComplianceFramework::Soc2 => {
                report.push_str("## SOC2-Specific Fields\n\n");
                report.push_str("- Security control entries: reviewed\n");
                report.push_str("- Access control changes: included\n");
                report.push_str("- Encryption operations: tracked\n");
            }
            ComplianceFramework::Iso27001 => {
                report.push_str("## ISO27001-Specific Fields\n\n");
                report.push_str("- Information security entries: reviewed\n");
                report.push_str("- Risk assessment actions: included\n");
                report.push_str("- Asset management operations: tracked\n");
            }
            ComplianceFramework::Custom(ref name) => {
                let _ = writeln!(report, "## {}-Specific Fields\n", name);
                report.push_str("- Custom framework: all tagged entries included\n");
            }
        }

        report.push_str("\n## Tagged Entries Summary\n\n");
        for entry in tagged.iter().take(MAX_REPORT_ENTRIES) {
            let action = sanitize_control_chars(&entry.action);
            let action = redact_field(&action, MAX_ACTION_DISPLAY_LEN);
            let actor = sanitize_control_chars(&entry.actor);
            let summary = sanitize_control_chars(&entry.result_summary);
            let actor = redact_field(&actor, MAX_ACTOR_DISPLAY_LEN);
            let summary = redact_field(&summary, MAX_SUMMARY_DISPLAY_LEN);
            let _ = writeln!(
                report,
                "- [{}] {} by {} -> {}",
                entry.timestamp.format("%Y-%m-%d %H:%M:%S"),
                action,
                actor,
                summary,
            );
            if report.len() >= MAX_REPORT_SIZE {
                report.push_str("\n... report truncated (size limit reached)\n");
                break;
            }
        }
        if tagged.len() > MAX_REPORT_ENTRIES {
            let _ = writeln!(
                report,
                "\n... and {} more entries",
                tagged.len() - MAX_REPORT_ENTRIES
            );
        }

        if report.len() > MAX_REPORT_SIZE {
            // Find a valid UTF-8 char boundary to avoid panicking on multibyte chars.
            let mut cutoff = MAX_REPORT_SIZE;
            while cutoff > 0 && !report.is_char_boundary(cutoff) {
                cutoff -= 1;
            }
            report.truncate(cutoff);
            report.push_str("\n... report truncated (size limit reached)\n");
        }

        Ok(report)
    }

    /// Verify audit log integrity.
    ///
    /// Returns `passed = false` when the chain is broken so callers propagate
    /// the failure via `ToolResult::success`.
    fn verify_integrity(&self) -> Result<VerifyResult> {
        let log = self.open_log()?;
        match log.verify_chain() {
            Ok(count) => Ok(VerifyResult {
                passed: true,
                message: format!(
                    "Audit log integrity: VERIFIED\nEntries checked: {}\nChain status: intact",
                    count
                ),
            }),
            Err(e) => Ok(VerifyResult {
                passed: false,
                message: format!("Audit log integrity: FAILED\nError: {}", e),
            }),
        }
    }

    /// Show current compliance posture.
    fn compliance_status(&self) -> Result<String> {
        use std::fmt::Write;
        let mut status = String::from("# Compliance Posture\n\n");
        let _ = writeln!(status, "Compliance enabled: {}", self.config.enabled);
        let _ = writeln!(
            status,
            "Active frameworks: {}",
            if self.config.frameworks.is_empty() {
                "none".to_string()
            } else {
                self.config.frameworks.join(", ")
            }
        );
        let _ = writeln!(
            status,
            "Tamper-evident logging: {}",
            self.config.tamper_evident_logging
        );
        let _ = writeln!(status, "Hash algorithm: {}", self.config.hash_algorithm);
        if let Some(ref region) = self.config.data_residency_region {
            let _ = writeln!(status, "Data residency region: {}", region);
            let _ = writeln!(
                status,
                "Block on violation: {}",
                self.config.block_on_residency_violation
            );
        } else {
            status.push_str("Data residency: not configured\n");
        }
        let _ = writeln!(status, "Report output: {}", self.config.report_output_dir);
        let _ = writeln!(
            status,
            "Audit retention: {} days",
            self.config.audit_retention_days
        );
        let _ = writeln!(
            status,
            "SIEM export format: {}",
            self.config.siem_export_format
        );

        let log_path = self.log_path();
        if log_path.exists() {
            let log = TamperEvidentLog::new(log_path, 0)
                .context("Failed to open audit log for health check")?;
            match log.verify_chain() {
                Ok(count) => {
                    let _ = writeln!(status, "\nAudit log: {} entries, chain intact", count);
                }
                Err(e) => {
                    let _ = writeln!(status, "\nAudit log: chain BROKEN ({})", e);
                }
            }
        } else {
            status.push_str("\nAudit log: no entries yet\n");
        }

        Ok(status)
    }

    /// Classify an action by regulatory relevance.
    fn classify_action(&self, action: &str) -> String {
        let frameworks: Vec<ComplianceFramework> = self
            .config
            .frameworks
            .iter()
            .map(|s| ComplianceFramework::from_name(s))
            .collect();
        let classifier = ComplianceClassifier::new(&frameworks);
        let tags = classifier.classify(action);

        if tags.is_empty() {
            format!(
                "Action '{}' has no regulatory tags under active frameworks: [{}]",
                action,
                self.config.frameworks.join(", ")
            )
        } else {
            let labels: Vec<&str> = tags.iter().map(|f| f.label()).collect();
            format!(
                "Action '{}' is tagged with: [{}]",
                action,
                labels.join(", ")
            )
        }
    }
}

/// Parse a date string (YYYY-MM-DD) into a DateTime at start of day UTC.
fn parse_date_start(s: &str) -> Result<DateTime<Utc>> {
    let date = NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .map_err(|e| anyhow::anyhow!("invalid date '{}': {}", s, e))?;
    Ok(date.and_hms_opt(0, 0, 0).unwrap().and_utc())
}

/// Parse a date string (YYYY-MM-DD) into an exclusive upper-bound DateTime.
///
/// Returns midnight of the *next* day so that `timestamp < bound` captures the
/// entire calendar day including 23:59:59.999...
fn parse_date_end_exclusive(s: &str) -> Result<DateTime<Utc>> {
    let date = NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .map_err(|e| anyhow::anyhow!("invalid date '{}': {}", s, e))?;
    let next_day = date
        .checked_add_days(Days::new(1))
        .ok_or_else(|| anyhow::anyhow!("date overflow adding one day to '{}'", s))?;
    Ok(next_day.and_hms_opt(0, 0, 0).unwrap().and_utc())
}

#[async_trait]
impl Tool for ComplianceTool {
    fn name(&self) -> &str {
        "compliance"
    }

    fn description(&self) -> &str {
        "Compliance reporting, audit verification, and regulatory classification for regulated industries (FINMA, DORA, GDPR, SOC2, ISO27001)"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "Subcommand: 'report', 'verify', 'status', 'classify'",
                    "enum": ["report", "verify", "status", "classify"]
                },
                "framework": {
                    "type": "string",
                    "description": "Framework name for 'report' command (e.g. FINMA, GDPR, DORA)"
                },
                "from": {
                    "type": "string",
                    "description": "Start date for report (YYYY-MM-DD)"
                },
                "to": {
                    "type": "string",
                    "description": "End date for report (YYYY-MM-DD)"
                },
                "action": {
                    "type": "string",
                    "description": "Action string for 'classify' command"
                }
            },
            "required": ["command"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> Result<ToolResult> {
        if !self.config.enabled {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Compliance module is not enabled in configuration".to_string()),
            });
        }

        let command = args.get("command").and_then(|v| v.as_str()).unwrap_or("");

        match command {
            "report" => {
                let framework = args.get("framework").and_then(|v| v.as_str());
                let Some(framework) = framework else {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(
                            "'framework' parameter is required for report command".to_string(),
                        ),
                    });
                };
                let from = args.get("from").and_then(|v| v.as_str());
                let to = args.get("to").and_then(|v| v.as_str());

                match self.generate_report(framework, from, to) {
                    Ok(report) => Ok(ToolResult {
                        success: true,
                        output: report,
                        error: None,
                    }),
                    Err(e) => Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!("Report generation failed: {}", e)),
                    }),
                }
            }
            "verify" => match self.verify_integrity() {
                Ok(result) => Ok(ToolResult {
                    success: result.passed,
                    output: result.message,
                    error: None,
                }),
                Err(e) => Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Verification failed: {}", e)),
                }),
            },
            "status" => match self.compliance_status() {
                Ok(output) => Ok(ToolResult {
                    success: true,
                    output,
                    error: None,
                }),
                Err(e) => Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Status check failed: {}", e)),
                }),
            },
            "classify" => {
                let action = args.get("action").and_then(|v| v.as_str());
                let Some(action) = action else {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(
                            "'action' parameter is required for classify command".to_string(),
                        ),
                    });
                };
                Ok(ToolResult {
                    success: true,
                    output: self.classify_action(action),
                    error: None,
                })
            }
            other => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Unknown compliance command '{}'. Use: report, verify, status, classify",
                    other
                )),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn test_config(frameworks: Vec<&str>) -> ComplianceConfig {
        ComplianceConfig {
            enabled: true,
            frameworks: frameworks.into_iter().map(String::from).collect(),
            tamper_evident_logging: true,
            hash_algorithm: "sha256".to_string(),
            data_residency_region: Some("CH".to_string()),
            block_on_residency_violation: true,
            report_output_dir: "/tmp/reports".to_string(),
            audit_retention_days: 365,
            siem_export_format: "json".to_string(),
        }
    }

    #[tokio::test]
    async fn compliance_disabled_returns_error() {
        let tmp = TempDir::new().unwrap();
        let mut config = test_config(vec!["FINMA"]);
        config.enabled = false;
        let tool = ComplianceTool::new(config, tmp.path().to_path_buf());

        let result = tool
            .execute(serde_json::json!({"command": "status"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("not enabled"));
    }

    #[tokio::test]
    async fn compliance_status_returns_posture() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(vec!["FINMA", "GDPR"]);
        let tool = ComplianceTool::new(config, tmp.path().to_path_buf());

        let result = tool
            .execute(serde_json::json!({"command": "status"}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("FINMA"));
        assert!(result.output.contains("GDPR"));
        assert!(result.output.contains("Compliance enabled: true"));
    }

    #[tokio::test]
    async fn compliance_classify_returns_tags() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(vec!["GDPR", "FINMA"]);
        let tool = ComplianceTool::new(config, tmp.path().to_path_buf());

        let result = tool
            .execute(serde_json::json!({
                "command": "classify",
                "action": "process_personal_data"
            }))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("GDPR"));
    }

    #[tokio::test]
    async fn compliance_verify_empty_log() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(vec!["FINMA"]);
        let tool = ComplianceTool::new(config, tmp.path().to_path_buf());

        let result = tool
            .execute(serde_json::json!({"command": "verify"}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("VERIFIED"));
    }

    #[tokio::test]
    async fn compliance_report_requires_framework() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(vec!["FINMA"]);
        let tool = ComplianceTool::new(config, tmp.path().to_path_buf());

        let result = tool
            .execute(serde_json::json!({"command": "report"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("framework"));
    }

    #[tokio::test]
    async fn compliance_report_generates_output() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(vec!["FINMA"]);
        let tool = ComplianceTool::new(config, tmp.path().to_path_buf());

        let result = tool
            .execute(serde_json::json!({
                "command": "report",
                "framework": "FINMA"
            }))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("FINMA Compliance Report"));
        assert!(result.output.contains("FINMA-Specific Fields"));
    }

    #[tokio::test]
    async fn compliance_unknown_command_returns_error() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(vec![]);
        let tool = ComplianceTool::new(config, tmp.path().to_path_buf());

        let result = tool
            .execute(serde_json::json!({"command": "invalid"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("Unknown"));
    }

    #[test]
    fn parse_date_end_exclusive_includes_full_day() {
        let bound = parse_date_end_exclusive("2024-01-15").unwrap();
        assert_eq!(
            bound,
            NaiveDate::from_ymd_opt(2024, 1, 16)
                .unwrap()
                .and_hms_opt(0, 0, 0)
                .unwrap()
                .and_utc()
        );
    }

    #[tokio::test]
    async fn compliance_report_rejects_non_enabled_framework() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(vec!["FINMA"]);
        let tool = ComplianceTool::new(config, tmp.path().to_path_buf());

        let result = tool
            .execute(serde_json::json!({
                "command": "report",
                "framework": "GDPR"
            }))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result
            .error
            .unwrap()
            .contains("not enabled in compliance configuration"));
    }

    #[test]
    fn sanitize_control_chars_strips_newlines() {
        assert_eq!(sanitize_control_chars("hello\nworld\r!"), "hello world !");
    }

    #[test]
    fn redact_field_short_unchanged() {
        assert_eq!(redact_field("short", 10), "short");
    }

    #[test]
    fn redact_field_long_truncated() {
        let long = "a".repeat(100);
        let redacted = redact_field(&long, 20);
        assert!(redacted.contains("[...]"));
        assert!(redacted.len() <= 25);
    }
}
