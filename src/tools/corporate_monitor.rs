use std::collections::HashMap;
use std::fmt::Write;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::util::truncate_with_ellipsis;

use super::traits::{Tool, ToolResult};

// ── Configuration ──────────────────────────────────────────────

/// Configuration for corporate monitoring schedules (`[corporate_monitor]`).
///
/// When `enabled` is `true`, the agent will periodically check configured
/// corporate tool targets (Teams channels, Outlook inboxes, Jira boards, etc.)
/// and send prioritized notifications to the configured channel.
///
/// Defaults: disabled, no monitors, `"normal"` min priority, LLM classification on.
/// Backward-compatible: absent key uses defaults. Rollback: remove the section.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CorporateMonitorConfig {
    /// Enable corporate monitoring. Default: `false`.
    #[serde(default)]
    pub enabled: bool,

    /// Monitor entries (each watches a specific corporate tool target).
    #[serde(default)]
    pub monitors: Vec<MonitorEntry>,

    /// Notification channel: `"telegram"`, `"whatsapp"`, `"slack"`, `"discord"`, etc.
    #[serde(default)]
    pub notify_channel: String,

    /// Minimum priority level that triggers a notification.
    /// One of `"low"`, `"normal"`, `"urgent"`. Default: `"normal"`.
    #[serde(default = "default_min_priority")]
    pub min_notify_priority: String,

    /// Use LLM to classify message importance before notifying. Default: `true`.
    #[serde(default = "default_classify_importance")]
    pub classify_importance: bool,
}

impl Default for CorporateMonitorConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            monitors: Vec::new(),
            notify_channel: String::new(),
            min_notify_priority: default_min_priority(),
            classify_importance: default_classify_importance(),
        }
    }
}

/// A single monitor entry that watches a corporate tool target.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct MonitorEntry {
    /// Human-readable monitor name (e.g. `"eng-standup-channel"`).
    pub name: String,

    /// Monitor type: `"teams"`, `"outlook"`, `"jira"`, `"confluence"`, `"custom"`.
    pub monitor_type: String,

    /// Check interval in minutes. Default: `15`.
    #[serde(default = "default_check_interval")]
    pub interval_minutes: u64,

    /// Target: channel name, project key, URL, etc.
    pub target: String,

    /// Extra parameters (channel name, filter query, etc.).
    #[serde(default)]
    pub params: HashMap<String, String>,

    /// Whether this monitor is active. Default: `true`.
    #[serde(default = "default_monitor_active")]
    pub active: bool,
}

/// Default check interval for a monitor entry: 15 minutes.
fn default_check_interval() -> u64 {
    15
}

/// Default minimum notification priority: `"normal"`.
fn default_min_priority() -> String {
    "normal".into()
}

/// Default LLM importance classification toggle: enabled.
fn default_classify_importance() -> bool {
    true
}

/// Default active state for new monitor entries: enabled.
fn default_monitor_active() -> bool {
    true
}

// ── Notification data structs ──────────────────────────────────

/// Summary of a Teams message for notification formatting.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamMessage {
    /// Display name of the message sender.
    pub sender: String,
    /// Teams channel name where the message was posted.
    pub channel: String,
    /// Message body content.
    pub content: String,
    /// ISO 8601 timestamp of the message.
    pub timestamp: String,
    /// Priority level: `"low"`, `"normal"`, or `"urgent"`.
    pub priority: String,
}

/// Summary of an Outlook email for notification formatting.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmailSummary {
    /// Email sender address or display name.
    pub from: String,
    /// Email subject line.
    pub subject: String,
    /// Truncated body preview.
    pub preview: String,
    /// ISO 8601 timestamp of the email.
    pub timestamp: String,
    /// Priority level: `"low"`, `"normal"`, or `"urgent"`.
    pub priority: String,
}

/// Summary of a Jira ticket update for notification formatting.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JiraUpdate {
    /// Jira issue key (e.g. `"PROJ-123"`).
    pub ticket_id: String,
    /// Issue summary/title.
    pub title: String,
    /// Status transition description, if any (e.g. `"In Progress -> Review"`).
    pub status_change: Option<String>,
    /// Current assignee display name.
    pub assignee: String,
    /// Priority level: `"low"`, `"normal"`, or `"urgent"`.
    pub priority: String,
}

// ── Priority helpers ───────────────────────────────────────────

/// Numeric rank for priority comparison. Higher = more urgent.
fn priority_rank(priority: &str) -> u8 {
    match priority.to_lowercase().as_str() {
        "low" => 1,
        "urgent" | "high" | "critical" => 3,
        _ => 2,
    }
}

/// Returns `true` if `item_priority` meets or exceeds `min_priority`.
pub fn meets_priority(item_priority: &str, min_priority: &str) -> bool {
    priority_rank(item_priority) >= priority_rank(min_priority)
}

// ── Notification formatters ────────────────────────────────────

/// Format a notification for Teams messages.
pub fn format_teams_notification(messages: &[TeamMessage]) -> String {
    if messages.is_empty() {
        return "No new Teams messages.".into();
    }
    let mut out = String::from("Teams updates:\n");
    for msg in messages {
        let _ = writeln!(
            out,
            "- [{pri}] {sender} in #{channel}: {content} ({ts})",
            pri = msg.priority,
            sender = msg.sender,
            channel = msg.channel,
            content = truncate_with_ellipsis(&msg.content, 120),
            ts = msg.timestamp,
        );
    }
    out
}

/// Format a notification for Outlook emails.
pub fn format_outlook_notification(emails: &[EmailSummary]) -> String {
    if emails.is_empty() {
        return "No new Outlook emails.".into();
    }
    let mut out = String::from("Outlook updates:\n");
    for email in emails {
        let _ = writeln!(
            out,
            "- [{pri}] From {from}: {subject} — {preview} ({ts})",
            pri = email.priority,
            from = email.from,
            subject = email.subject,
            preview = truncate_with_ellipsis(&email.preview, 80),
            ts = email.timestamp,
        );
    }
    out
}

/// Format a notification for Jira updates.
pub fn format_jira_notification(tickets: &[JiraUpdate]) -> String {
    if tickets.is_empty() {
        return "No new Jira updates.".into();
    }
    let mut out = String::from("Jira updates:\n");
    for ticket in tickets {
        let status = ticket
            .status_change
            .as_deref()
            .unwrap_or("no status change");
        let _ = writeln!(
            out,
            "- [{pri}] {id}: {title} ({status}, assignee: {assignee})",
            pri = ticket.priority,
            id = ticket.ticket_id,
            title = ticket.title,
            assignee = ticket.assignee,
        );
    }
    out
}

// ── Tool implementation ────────────────────────────────────────

/// Tool for managing and querying corporate monitoring schedules.
///
/// This tool defines the configuration and management interface. Actual browser
/// interaction is handled by the `browser_delegate` tool (separate concern).
pub struct CorporateMonitorTool {
    config: CorporateMonitorConfig,
}

impl CorporateMonitorTool {
    /// Create a new `CorporateMonitorTool` with the given configuration.
    pub fn new(config: CorporateMonitorConfig) -> Self {
        Self { config }
    }

    /// List all configured monitors with their type, target, interval, and active state.
    fn handle_list(&self) -> ToolResult {
        if self.config.monitors.is_empty() {
            return ToolResult {
                success: true,
                output: "No corporate monitors configured.".into(),
                error: None,
            };
        }
        let mut out = String::from("Corporate monitors:\n");
        for entry in &self.config.monitors {
            let _ = writeln!(
                out,
                "- {name} ({mtype}, target: {target}, every {interval}m, active: {active})",
                name = entry.name,
                mtype = entry.monitor_type,
                target = entry.target,
                interval = entry.interval_minutes,
                active = entry.active,
            );
        }
        ToolResult {
            success: true,
            output: out,
            error: None,
        }
    }

    /// Return a summary of the monitoring configuration: enabled state, active count,
    /// notification channel, priority threshold, and LLM classification toggle.
    fn handle_status(&self) -> ToolResult {
        let total = self.config.monitors.len();
        let active = self.config.monitors.iter().filter(|m| m.active).count();
        let output = format!(
            "Corporate monitoring enabled: {enabled}\n\
             Monitors: {active}/{total} active\n\
             Notification channel: {channel}\n\
             Min priority: {pri}\n\
             LLM classification: {classify}",
            enabled = self.config.enabled,
            channel = if self.config.notify_channel.is_empty() {
                "(not set)"
            } else {
                &self.config.notify_channel
            },
            pri = self.config.min_notify_priority,
            classify = self.config.classify_importance,
        );
        ToolResult {
            success: true,
            output,
            error: None,
        }
    }

    /// Queue active monitors (optionally filtered by name) for browser-delegate checks.
    fn handle_check(&self, monitor_name: Option<&str>) -> ToolResult {
        // This tool orchestrates *when/what* to check, not *how*.
        // Actual browser interaction is delegated to browser_delegate.
        let targets: Vec<&MonitorEntry> = match monitor_name {
            Some(name) => self
                .config
                .monitors
                .iter()
                .filter(|m| m.name == name && m.active)
                .collect(),
            None => self.config.monitors.iter().filter(|m| m.active).collect(),
        };

        if targets.is_empty() {
            let msg = monitor_name.map_or_else(
                || "No active monitors to check.".to_string(),
                |name| format!("No active monitor found with name '{name}'."),
            );
            return ToolResult {
                success: true,
                output: msg,
                error: None,
            };
        }

        let mut out = String::from("Check targets queued for browser delegation:\n");
        for entry in &targets {
            let _ = writeln!(
                out,
                "- {name} ({mtype}): {target}",
                name = entry.name,
                mtype = entry.monitor_type,
                target = entry.target,
            );
        }
        out.push_str(
            "\nNote: actual browser interaction is handled by browser_delegate (separate tool).",
        );

        ToolResult {
            success: true,
            output: out,
            error: None,
        }
    }
}

#[async_trait]
impl Tool for CorporateMonitorTool {
    fn name(&self) -> &str {
        "corporate_monitor"
    }

    fn description(&self) -> &str {
        "Manage and query corporate monitoring schedules for Teams, Outlook, Jira, and Confluence. \
         Actions: list, check, status. Actual browser interaction is delegated to browser_delegate."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["list", "check", "status"],
                    "description": "Action to perform: list monitors, trigger a check, or show status"
                },
                "monitor_name": {
                    "type": "string",
                    "description": "Target a specific monitor by name (optional, used with 'check')"
                }
            },
            "required": ["action"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let action = args
            .get("action")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'action' parameter"))?;

        let monitor_name = args.get("monitor_name").and_then(|v| v.as_str());

        let result = match action {
            "list" => self.handle_list(),
            "status" => self.handle_status(),
            "check" => self.handle_check(monitor_name),
            other => ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Unknown action '{other}'. Valid actions: list, check, status"
                )),
            },
        };

        Ok(result)
    }
}

// ── Tests ──────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_config() -> CorporateMonitorConfig {
        CorporateMonitorConfig {
            enabled: true,
            monitors: vec![
                MonitorEntry {
                    name: "eng-standup".into(),
                    monitor_type: "teams".into(),
                    interval_minutes: 10,
                    target: "Engineering Standup".into(),
                    params: HashMap::new(),
                    active: true,
                },
                MonitorEntry {
                    name: "inbox-urgent".into(),
                    monitor_type: "outlook".into(),
                    interval_minutes: 5,
                    target: "Inbox".into(),
                    params: {
                        let mut m = HashMap::new();
                        m.insert("filter".into(), "is:unread importance:high".into());
                        m
                    },
                    active: true,
                },
                MonitorEntry {
                    name: "proj-board".into(),
                    monitor_type: "jira".into(),
                    interval_minutes: 30,
                    target: "PROJ".into(),
                    params: HashMap::new(),
                    active: false,
                },
            ],
            notify_channel: "telegram".into(),
            min_notify_priority: "normal".into(),
            classify_importance: true,
        }
    }

    #[test]
    fn config_defaults() {
        let cfg = CorporateMonitorConfig::default();
        assert!(!cfg.enabled);
        assert!(cfg.monitors.is_empty());
        assert!(cfg.notify_channel.is_empty());
        assert_eq!(cfg.min_notify_priority, "normal");
        assert!(cfg.classify_importance);
    }

    #[test]
    fn config_serde_roundtrip() {
        let cfg = sample_config();
        let json = serde_json::to_string(&cfg).unwrap();
        let parsed: CorporateMonitorConfig = serde_json::from_str(&json).unwrap();
        assert!(parsed.enabled);
        assert_eq!(parsed.monitors.len(), 3);
        assert_eq!(parsed.notify_channel, "telegram");
        assert_eq!(parsed.min_notify_priority, "normal");
        assert!(parsed.classify_importance);
    }

    #[test]
    fn monitor_entry_serde_defaults() {
        let json = r#"{"name":"test","monitor_type":"teams","target":"General"}"#;
        let entry: MonitorEntry = serde_json::from_str(json).unwrap();
        assert_eq!(entry.interval_minutes, 15);
        assert!(entry.active);
        assert!(entry.params.is_empty());
    }

    #[test]
    fn priority_filtering() {
        assert!(meets_priority("urgent", "normal"));
        assert!(meets_priority("normal", "normal"));
        assert!(!meets_priority("low", "normal"));
        assert!(meets_priority("low", "low"));
        assert!(meets_priority("urgent", "low"));
        assert!(meets_priority("high", "normal"));
        assert!(meets_priority("critical", "urgent"));
    }

    #[test]
    fn format_teams_empty() {
        assert_eq!(format_teams_notification(&[]), "No new Teams messages.");
    }

    #[test]
    fn format_teams_messages() {
        let msgs = vec![TeamMessage {
            sender: "user_a".into(),
            channel: "general".into(),
            content: "Build is broken".into(),
            timestamp: "2026-03-08T10:00:00Z".into(),
            priority: "urgent".into(),
        }];
        let out = format_teams_notification(&msgs);
        assert!(out.contains("user_a"));
        assert!(out.contains("general"));
        assert!(out.contains("Build is broken"));
    }

    #[test]
    fn format_outlook_empty() {
        assert_eq!(format_outlook_notification(&[]), "No new Outlook emails.");
    }

    #[test]
    fn format_outlook_emails() {
        let emails = vec![EmailSummary {
            from: "user_b".into(),
            subject: "Deployment approval needed".into(),
            preview: "Please review the deployment plan".into(),
            timestamp: "2026-03-08T11:00:00Z".into(),
            priority: "urgent".into(),
        }];
        let out = format_outlook_notification(&emails);
        assert!(out.contains("user_b"));
        assert!(out.contains("Deployment approval needed"));
    }

    #[test]
    fn format_jira_empty() {
        assert_eq!(format_jira_notification(&[]), "No new Jira updates.");
    }

    #[test]
    fn format_jira_updates() {
        let tickets = vec![JiraUpdate {
            ticket_id: "PROJ-123".into(),
            title: "Fix login flow".into(),
            status_change: Some("In Progress -> Review".into()),
            assignee: "user_c".into(),
            priority: "normal".into(),
        }];
        let out = format_jira_notification(&tickets);
        assert!(out.contains("PROJ-123"));
        assert!(out.contains("Fix login flow"));
        assert!(out.contains("In Progress -> Review"));
    }

    #[tokio::test]
    async fn tool_list_action() {
        let tool = CorporateMonitorTool::new(sample_config());
        let result = tool.execute(json!({"action": "list"})).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("eng-standup"));
        assert!(result.output.contains("inbox-urgent"));
        assert!(result.output.contains("proj-board"));
    }

    #[tokio::test]
    async fn tool_list_empty() {
        let tool = CorporateMonitorTool::new(CorporateMonitorConfig::default());
        let result = tool.execute(json!({"action": "list"})).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("No corporate monitors configured"));
    }

    #[tokio::test]
    async fn tool_status_action() {
        let tool = CorporateMonitorTool::new(sample_config());
        let result = tool.execute(json!({"action": "status"})).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("enabled: true"));
        assert!(result.output.contains("2/3 active"));
        assert!(result.output.contains("telegram"));
    }

    #[tokio::test]
    async fn tool_check_action() {
        let tool = CorporateMonitorTool::new(sample_config());
        let result = tool.execute(json!({"action": "check"})).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("eng-standup"));
        assert!(result.output.contains("inbox-urgent"));
        // proj-board is inactive, should not appear
        assert!(!result.output.contains("proj-board"));
        assert!(result.output.contains("browser_delegate"));
    }

    #[tokio::test]
    async fn tool_check_specific_monitor() {
        let tool = CorporateMonitorTool::new(sample_config());
        let result = tool
            .execute(json!({"action": "check", "monitor_name": "eng-standup"}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("eng-standup"));
        assert!(!result.output.contains("inbox-urgent"));
    }

    #[tokio::test]
    async fn tool_check_nonexistent_monitor() {
        let tool = CorporateMonitorTool::new(sample_config());
        let result = tool
            .execute(json!({"action": "check", "monitor_name": "nonexistent"}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("No active monitor found"));
    }

    #[tokio::test]
    async fn tool_unknown_action() {
        let tool = CorporateMonitorTool::new(sample_config());
        let result = tool.execute(json!({"action": "delete"})).await.unwrap();
        assert!(!result.success);
        assert!(result.error.as_ref().unwrap().contains("Unknown action"));
    }

    #[tokio::test]
    async fn tool_missing_action() {
        let tool = CorporateMonitorTool::new(sample_config());
        let result = tool.execute(json!({})).await;
        assert!(result.is_err());
    }
}
