//! Security operations tool for managed cybersecurity service (MCSS) workflows.
//!
//! Provides alert triage, incident response playbook execution, vulnerability
//! scan parsing, and security report generation. All actions that modify state
//! enforce human approval gates unless explicitly configured otherwise.

use async_trait::async_trait;
use serde_json::json;
use std::path::PathBuf;

use super::traits::{Tool, ToolResult};
use crate::config::SecurityOpsConfig;
use crate::security::playbook::{
    Playbook, StepStatus, evaluate_step, load_playbooks, severity_level,
};
use crate::security::vulnerability::{generate_summary, parse_vulnerability_json};

/// Security operations tool — triage alerts, run playbooks, parse vulns, generate reports.
pub struct SecurityOpsTool {
    config: SecurityOpsConfig,
    playbooks: Vec<Playbook>,
}

impl SecurityOpsTool {
    pub fn new(config: SecurityOpsConfig) -> Self {
        let playbooks_dir = expand_tilde(&config.playbooks_dir);
        let playbooks = load_playbooks(&playbooks_dir);
        Self { config, playbooks }
    }

    /// Triage an alert: classify severity and recommend response.
    fn triage_alert(&self, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        let alert = args
            .get("alert")
            .ok_or_else(|| anyhow::anyhow!("Missing required 'alert' parameter"))?;

        // Extract key fields for classification
        let alert_type = alert
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let source = alert
            .get("source")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let severity = alert
            .get("severity")
            .and_then(|v| v.as_str())
            .unwrap_or("medium");
        let description = alert
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        // Classify and find matching playbooks
        let matching_playbooks: Vec<&Playbook> = self
            .playbooks
            .iter()
            .filter(|pb| {
                severity_level(severity) >= severity_level(&pb.severity_filter)
                    && (pb.name.contains(alert_type)
                        || alert_type.contains(&pb.name)
                        || description
                            .to_lowercase()
                            .contains(&pb.name.replace('_', " ")))
            })
            .collect();

        let playbook_names: Vec<&str> =
            matching_playbooks.iter().map(|p| p.name.as_str()).collect();

        let output = json!({
            "classification": {
                "alert_type": alert_type,
                "source": source,
                "severity": severity,
                "severity_level": severity_level(severity),
                "priority": if severity_level(severity) >= 3 { "immediate" } else { "standard" },
            },
            "recommended_playbooks": playbook_names,
            "recommended_action": if matching_playbooks.is_empty() {
                "Manual investigation required — no matching playbook found"
            } else {
                "Execute recommended playbook(s)"
            },
            "auto_triage": self.config.auto_triage,
        });

        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&output)?,
            error: None,
        })
    }

    /// Execute a playbook step with approval gating.
    fn run_playbook(&self, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        let playbook_name = args
            .get("playbook")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing required 'playbook' parameter"))?;

        let step_index =
            usize::try_from(args.get("step").and_then(|v| v.as_u64()).ok_or_else(|| {
                anyhow::anyhow!("Missing required 'step' parameter (0-based index)")
            })?)
            .map_err(|_| anyhow::anyhow!("'step' parameter value too large for this platform"))?;

        let alert_severity = args
            .get("alert_severity")
            .and_then(|v| v.as_str())
            .unwrap_or("medium");

        let playbook = self
            .playbooks
            .iter()
            .find(|p| p.name == playbook_name)
            .ok_or_else(|| anyhow::anyhow!("Playbook '{}' not found", playbook_name))?;

        let result = evaluate_step(
            playbook,
            step_index,
            alert_severity,
            &self.config.max_auto_severity,
            self.config.require_approval_for_actions,
        );

        let output = json!({
            "playbook": playbook_name,
            "step_index": result.step_index,
            "action": result.action,
            "status": result.status.to_string(),
            "message": result.message,
            "requires_manual_approval": result.status == StepStatus::PendingApproval,
        });

        Ok(ToolResult {
            success: result.status != StepStatus::Failed,
            output: serde_json::to_string_pretty(&output)?,
            error: if result.status == StepStatus::Failed {
                Some(result.message)
            } else {
                None
            },
        })
    }

    /// Parse vulnerability scan results.
    fn parse_vulnerability(&self, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        let scan_data = args
            .get("scan_data")
            .ok_or_else(|| anyhow::anyhow!("Missing required 'scan_data' parameter"))?;

        let json_str = if scan_data.is_string() {
            scan_data.as_str().unwrap().to_string()
        } else {
            serde_json::to_string(scan_data)?
        };

        let report = parse_vulnerability_json(&json_str)?;
        let summary = generate_summary(&report);

        let output = json!({
            "scanner": report.scanner,
            "scan_date": report.scan_date.to_rfc3339(),
            "total_findings": report.findings.len(),
            "by_severity": {
                "critical": report.findings.iter().filter(|f| f.severity == "critical").count(),
                "high": report.findings.iter().filter(|f| f.severity == "high").count(),
                "medium": report.findings.iter().filter(|f| f.severity == "medium").count(),
                "low": report.findings.iter().filter(|f| f.severity == "low").count(),
            },
            "summary": summary,
        });

        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&output)?,
            error: None,
        })
    }

    /// Generate a client-facing security posture report.
    fn generate_report(&self, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        let client_name = args
            .get("client_name")
            .and_then(|v| v.as_str())
            .unwrap_or("Client");
        let period = args
            .get("period")
            .and_then(|v| v.as_str())
            .unwrap_or("current");
        let alert_stats = args.get("alert_stats");
        let vuln_summary = args
            .get("vuln_summary")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let report = format!(
            "# Security Posture Report — {client_name}\n\
             **Period:** {period}\n\
             **Generated:** {}\n\n\
             ## Executive Summary\n\n\
             This report provides an overview of the security posture for {client_name} \
             during the {period} period.\n\n\
             ## Alert Summary\n\n\
             {}\n\n\
             ## Vulnerability Assessment\n\n\
             {}\n\n\
             ## Recommendations\n\n\
             1. Address all critical and high-severity findings immediately\n\
             2. Review and update incident response playbooks quarterly\n\
             3. Conduct regular vulnerability scans on all internet-facing assets\n\
             4. Ensure all endpoints have current security patches\n\n\
             ---\n\
             *Report generated by ZeroClaw MCSS Agent*\n",
            chrono::Utc::now().format("%Y-%m-%d %H:%M UTC"),
            alert_stats
                .map(|s| serde_json::to_string_pretty(s).unwrap_or_default())
                .unwrap_or_else(|| "No alert statistics provided.".into()),
            if vuln_summary.is_empty() {
                "No vulnerability data provided."
            } else {
                vuln_summary
            },
        );

        Ok(ToolResult {
            success: true,
            output: report,
            error: None,
        })
    }

    /// List available playbooks.
    fn list_playbooks(&self) -> anyhow::Result<ToolResult> {
        if self.playbooks.is_empty() {
            return Ok(ToolResult {
                success: true,
                output: "No playbooks available.".into(),
                error: None,
            });
        }

        let playbook_list: Vec<serde_json::Value> = self
            .playbooks
            .iter()
            .map(|pb| {
                json!({
                    "name": pb.name,
                    "description": pb.description,
                    "steps": pb.steps.len(),
                    "severity_filter": pb.severity_filter,
                    "auto_approve_steps": pb.auto_approve_steps,
                })
            })
            .collect();

        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&playbook_list)?,
            error: None,
        })
    }

    /// Summarize alert volume, categories, and resolution times.
    fn alert_stats(&self, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        let alerts = args
            .get("alerts")
            .and_then(|v| v.as_array())
            .ok_or_else(|| anyhow::anyhow!("Missing required 'alerts' array parameter"))?;

        let total = alerts.len();
        let mut by_severity = std::collections::HashMap::new();
        let mut by_category = std::collections::HashMap::new();
        let mut resolved_count = 0u64;
        let mut total_resolution_secs = 0u64;

        for alert in alerts {
            let severity = alert
                .get("severity")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            *by_severity.entry(severity.to_string()).or_insert(0u64) += 1;

            let category = alert
                .get("category")
                .and_then(|v| v.as_str())
                .unwrap_or("uncategorized");
            *by_category.entry(category.to_string()).or_insert(0u64) += 1;

            if let Some(resolution_secs) = alert.get("resolution_secs").and_then(|v| v.as_u64()) {
                resolved_count += 1;
                total_resolution_secs += resolution_secs;
            }
        }

        let avg_resolution = if resolved_count > 0 {
            total_resolution_secs as f64 / resolved_count as f64
        } else {
            0.0
        };

        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let avg_resolution_secs_u64 = avg_resolution.max(0.0) as u64;

        let output = json!({
            "total_alerts": total,
            "resolved": resolved_count,
            "unresolved": total as u64 - resolved_count,
            "by_severity": by_severity,
            "by_category": by_category,
            "avg_resolution_secs": avg_resolution,
            "avg_resolution_human": format_duration_secs(avg_resolution_secs_u64),
        });

        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&output)?,
            error: None,
        })
    }
}

fn format_duration_secs(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m {}s", secs / 60, secs % 60)
    } else {
        format!("{}h {}m", secs / 3600, (secs % 3600) / 60)
    }
}

/// Expand ~ to home directory.
fn expand_tilde(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(user_dirs) = directories::UserDirs::new() {
            return user_dirs.home_dir().join(rest);
        }
    }
    PathBuf::from(path)
}

#[async_trait]
impl Tool for SecurityOpsTool {
    fn name(&self) -> &str {
        "security_ops"
    }

    fn description(&self) -> &str {
        "Security operations tool for managed cybersecurity services. Actions: \
         triage_alert (classify/prioritize alerts), run_playbook (execute incident response steps), \
         parse_vulnerability (parse scan results), generate_report (create security posture reports), \
         list_playbooks (list available playbooks), alert_stats (summarize alert metrics)."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "required": ["action"],
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["triage_alert", "run_playbook", "parse_vulnerability", "generate_report", "list_playbooks", "alert_stats"],
                    "description": "The security operation to perform"
                },
                "alert": {
                    "type": "object",
                    "description": "Alert JSON for triage_alert (requires: type, severity; optional: source, description)"
                },
                "playbook": {
                    "type": "string",
                    "description": "Playbook name for run_playbook"
                },
                "step": {
                    "type": "integer",
                    "description": "0-based step index for run_playbook"
                },
                "alert_severity": {
                    "type": "string",
                    "description": "Alert severity context for run_playbook"
                },
                "scan_data": {
                    "description": "Vulnerability scan data (JSON string or object) for parse_vulnerability"
                },
                "client_name": {
                    "type": "string",
                    "description": "Client name for generate_report"
                },
                "period": {
                    "type": "string",
                    "description": "Reporting period for generate_report"
                },
                "alert_stats": {
                    "type": "object",
                    "description": "Alert statistics to include in generate_report"
                },
                "vuln_summary": {
                    "type": "string",
                    "description": "Vulnerability summary to include in generate_report"
                },
                "alerts": {
                    "type": "array",
                    "description": "Array of alert objects for alert_stats"
                }
            }
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let action = args
            .get("action")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing required 'action' parameter"))?;

        match action {
            "triage_alert" => self.triage_alert(&args),
            "run_playbook" => self.run_playbook(&args),
            "parse_vulnerability" => self.parse_vulnerability(&args),
            "generate_report" => self.generate_report(&args),
            "list_playbooks" => self.list_playbooks(),
            "alert_stats" => self.alert_stats(&args),
            _ => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Unknown action '{action}'. Valid: triage_alert, run_playbook, \
                     parse_vulnerability, generate_report, list_playbooks, alert_stats"
                )),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> SecurityOpsConfig {
        SecurityOpsConfig {
            enabled: true,
            playbooks_dir: "/nonexistent".into(),
            auto_triage: false,
            require_approval_for_actions: true,
            max_auto_severity: "low".into(),
            report_output_dir: "/tmp/reports".into(),
            siem_integration: None,
        }
    }

    fn test_tool() -> SecurityOpsTool {
        SecurityOpsTool::new(test_config())
    }

    #[test]
    fn tool_name_and_schema() {
        let tool = test_tool();
        assert_eq!(tool.name(), "security_ops");
        let schema = tool.parameters_schema();
        assert!(schema["properties"]["action"].is_object());
        assert!(
            schema["required"]
                .as_array()
                .unwrap()
                .contains(&json!("action"))
        );
    }

    #[tokio::test]
    async fn triage_alert_classifies_severity() {
        let tool = test_tool();
        let result = tool
            .execute(json!({
                "action": "triage_alert",
                "alert": {
                    "type": "suspicious_login",
                    "source": "siem",
                    "severity": "high",
                    "description": "Multiple failed login attempts followed by successful login"
                }
            }))
            .await
            .unwrap();

        assert!(result.success);
        let output: serde_json::Value = serde_json::from_str(&result.output).unwrap();
        assert_eq!(output["classification"]["severity"], "high");
        assert_eq!(output["classification"]["priority"], "immediate");
        // Should match suspicious_login playbook
        let playbooks = output["recommended_playbooks"].as_array().unwrap();
        assert!(playbooks.iter().any(|p| p == "suspicious_login"));
    }

    #[tokio::test]
    async fn triage_alert_missing_alert_param() {
        let tool = test_tool();
        let result = tool.execute(json!({"action": "triage_alert"})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn run_playbook_requires_approval() {
        let tool = test_tool();
        let result = tool
            .execute(json!({
                "action": "run_playbook",
                "playbook": "suspicious_login",
                "step": 2,
                "alert_severity": "high"
            }))
            .await
            .unwrap();

        assert!(result.success);
        let output: serde_json::Value = serde_json::from_str(&result.output).unwrap();
        assert_eq!(output["status"], "pending_approval");
        assert_eq!(output["requires_manual_approval"], true);
    }

    #[tokio::test]
    async fn run_playbook_executes_safe_step() {
        let tool = test_tool();
        let result = tool
            .execute(json!({
                "action": "run_playbook",
                "playbook": "suspicious_login",
                "step": 0,
                "alert_severity": "medium"
            }))
            .await
            .unwrap();

        assert!(result.success);
        let output: serde_json::Value = serde_json::from_str(&result.output).unwrap();
        assert_eq!(output["status"], "completed");
    }

    #[tokio::test]
    async fn run_playbook_not_found() {
        let tool = test_tool();
        let result = tool
            .execute(json!({
                "action": "run_playbook",
                "playbook": "nonexistent",
                "step": 0
            }))
            .await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn parse_vulnerability_valid_report() {
        let tool = test_tool();
        let scan_data = json!({
            "scan_date": "2025-01-15T10:00:00Z",
            "scanner": "nessus",
            "findings": [
                {
                    "cve_id": "CVE-2024-0001",
                    "cvss_score": 9.8,
                    "severity": "critical",
                    "affected_asset": "web-01",
                    "description": "RCE in web framework",
                    "remediation": "Upgrade",
                    "internet_facing": true,
                    "production": true
                }
            ]
        });

        let result = tool
            .execute(json!({
                "action": "parse_vulnerability",
                "scan_data": scan_data
            }))
            .await
            .unwrap();

        assert!(result.success);
        let output: serde_json::Value = serde_json::from_str(&result.output).unwrap();
        assert_eq!(output["total_findings"], 1);
        assert_eq!(output["by_severity"]["critical"], 1);
    }

    #[tokio::test]
    async fn generate_report_produces_markdown() {
        let tool = test_tool();
        let result = tool
            .execute(json!({
                "action": "generate_report",
                "client_name": "ZeroClaw Corp",
                "period": "Q1 2025"
            }))
            .await
            .unwrap();

        assert!(result.success);
        assert!(result.output.contains("ZeroClaw Corp"));
        assert!(result.output.contains("Q1 2025"));
        assert!(result.output.contains("Security Posture Report"));
    }

    #[tokio::test]
    async fn list_playbooks_returns_builtins() {
        let tool = test_tool();
        let result = tool
            .execute(json!({"action": "list_playbooks"}))
            .await
            .unwrap();

        assert!(result.success);
        let output: Vec<serde_json::Value> = serde_json::from_str(&result.output).unwrap();
        assert_eq!(output.len(), 4);
        let names: Vec<&str> = output.iter().map(|p| p["name"].as_str().unwrap()).collect();
        assert!(names.contains(&"suspicious_login"));
        assert!(names.contains(&"malware_detected"));
    }

    #[tokio::test]
    async fn alert_stats_computes_summary() {
        let tool = test_tool();
        let result = tool
            .execute(json!({
                "action": "alert_stats",
                "alerts": [
                    {"severity": "critical", "category": "malware", "resolution_secs": 3600},
                    {"severity": "high", "category": "phishing", "resolution_secs": 1800},
                    {"severity": "medium", "category": "malware"},
                    {"severity": "low", "category": "policy_violation", "resolution_secs": 600}
                ]
            }))
            .await
            .unwrap();

        assert!(result.success);
        let output: serde_json::Value = serde_json::from_str(&result.output).unwrap();
        assert_eq!(output["total_alerts"], 4);
        assert_eq!(output["resolved"], 3);
        assert_eq!(output["unresolved"], 1);
        assert_eq!(output["by_severity"]["critical"], 1);
        assert_eq!(output["by_category"]["malware"], 2);
    }

    #[tokio::test]
    async fn unknown_action_returns_error() {
        let tool = test_tool();
        let result = tool.execute(json!({"action": "bad_action"})).await.unwrap();

        assert!(!result.success);
        assert!(result.error.unwrap().contains("Unknown action"));
    }

    #[test]
    fn format_duration_secs_readable() {
        assert_eq!(format_duration_secs(45), "45s");
        assert_eq!(format_duration_secs(125), "2m 5s");
        assert_eq!(format_duration_secs(3665), "1h 1m");
    }
}
