//! Audit logging for security events

use crate::config::AuditConfig;
use anyhow::Result;
use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use uuid::Uuid;

/// Audit event types
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditEventType {
    CommandExecution,
    FileAccess,
    ConfigChange,
    AuthSuccess,
    AuthFailure,
    PolicyViolation,
    SecurityEvent,
    ToolCallAudit,
}

/// Actor information (who performed the action)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Actor {
    pub channel: String,
    pub user_id: Option<String>,
    pub username: Option<String>,
}

/// Action information (what was done)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Action {
    pub command: Option<String>,
    pub risk_level: Option<String>,
    pub approved: bool,
    pub allowed: bool,
}

/// Execution result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionResult {
    pub success: bool,
    pub exit_code: Option<i32>,
    pub duration_ms: Option<u64>,
    pub error: Option<String>,
}

/// Security context
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityContext {
    pub policy_violation: bool,
    pub rate_limit_remaining: Option<u32>,
    pub sandbox_backend: Option<String>,
}

/// Complete audit event
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEvent {
    pub timestamp: DateTime<Utc>,
    pub event_id: String,
    pub event_type: AuditEventType,
    pub actor: Option<Actor>,
    pub action: Option<Action>,
    pub result: Option<ExecutionResult>,
    pub security: SecurityContext,
}

impl AuditEvent {
    /// Create a new audit event
    pub fn new(event_type: AuditEventType) -> Self {
        Self {
            timestamp: Utc::now(),
            event_id: Uuid::new_v4().to_string(),
            event_type,
            actor: None,
            action: None,
            result: None,
            security: SecurityContext {
                policy_violation: false,
                rate_limit_remaining: None,
                sandbox_backend: None,
            },
        }
    }

    /// Set the actor
    pub fn with_actor(
        mut self,
        channel: String,
        user_id: Option<String>,
        username: Option<String>,
    ) -> Self {
        self.actor = Some(Actor {
            channel,
            user_id,
            username,
        });
        self
    }

    /// Set the action
    pub fn with_action(
        mut self,
        command: String,
        risk_level: String,
        approved: bool,
        allowed: bool,
    ) -> Self {
        self.action = Some(Action {
            command: Some(command),
            risk_level: Some(risk_level),
            approved,
            allowed,
        });
        self
    }

    /// Set the result
    pub fn with_result(
        mut self,
        success: bool,
        exit_code: Option<i32>,
        duration_ms: u64,
        error: Option<String>,
    ) -> Self {
        self.result = Some(ExecutionResult {
            success,
            exit_code,
            duration_ms: Some(duration_ms),
            error,
        });
        self
    }

    /// Set security context
    pub fn with_security(mut self, sandbox_backend: Option<String>) -> Self {
        self.security.sandbox_backend = sandbox_backend;
        self
    }
}

/// Audit logger
pub struct AuditLogger {
    log_path: PathBuf,
    config: AuditConfig,
    buffer: Mutex<Vec<AuditEvent>>,
}

/// Structured command execution details for audit logging.
#[derive(Debug, Clone)]
pub struct CommandExecutionLog<'a> {
    pub channel: &'a str,
    pub command: &'a str,
    pub risk_level: &'a str,
    pub approved: bool,
    pub allowed: bool,
    pub success: bool,
    pub duration_ms: u64,
}

impl AuditLogger {
    /// Create a new audit logger
    pub fn new(config: AuditConfig, zeroclaw_dir: PathBuf) -> Result<Self> {
        let log_path = zeroclaw_dir.join(&config.log_path);
        Ok(Self {
            log_path,
            config,
            buffer: Mutex::new(Vec::new()),
        })
    }

    /// Log an event
    pub fn log(&self, event: &AuditEvent) -> Result<()> {
        if !self.config.enabled {
            return Ok(());
        }

        // Check log size and rotate if needed
        self.rotate_if_needed()?;

        // Serialize and write
        let line = serde_json::to_string(event)?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log_path)?;

        writeln!(file, "{}", line)?;
        file.sync_all()?;

        Ok(())
    }

    /// Log a command execution event.
    pub fn log_command_event(&self, entry: CommandExecutionLog<'_>) -> Result<()> {
        let event = AuditEvent::new(AuditEventType::CommandExecution)
            .with_actor(entry.channel.to_string(), None, None)
            .with_action(
                entry.command.to_string(),
                entry.risk_level.to_string(),
                entry.approved,
                entry.allowed,
            )
            .with_result(entry.success, None, entry.duration_ms, None);

        self.log(&event)
    }

    /// Backward-compatible helper to log a command execution event.
    #[allow(clippy::too_many_arguments)]
    pub fn log_command(
        &self,
        channel: &str,
        command: &str,
        risk_level: &str,
        approved: bool,
        allowed: bool,
        success: bool,
        duration_ms: u64,
    ) -> Result<()> {
        self.log_command_event(CommandExecutionLog {
            channel,
            command,
            risk_level,
            approved,
            allowed,
            success,
            duration_ms,
        })
    }

    /// Log a tool call audit event.
    ///
    /// Records tool name, success/failure, duration, and an optional error label.
    /// The `channel` parameter identifies the session source (e.g. "cli", "daemon",
    /// "heartbeat", "goal-loop", "cron:<name>").
    /// Does NOT log raw arguments or output to avoid leaking sensitive data.
    pub fn log_tool_call(
        &self,
        channel: &str,
        tool_name: &str,
        success: bool,
        duration_ms: u64,
        error: Option<&str>,
    ) -> Result<()> {
        let risk_level = match tool_name {
            "shell" | "file_write" | "file_edit" => "medium",
            _ => "low",
        };

        let event = AuditEvent::new(AuditEventType::ToolCallAudit)
            .with_actor(channel.to_string(), None, None)
            .with_action(
                tool_name.to_string(),
                risk_level.to_string(),
                true, // approved (already past approval gate)
                true, // allowed (already past policy gate)
            )
            .with_result(success, None, duration_ms, error.map(|e| e.to_string()));

        self.log(&event)
    }

    /// Rotate log if it exceeds max size
    fn rotate_if_needed(&self) -> Result<()> {
        if let Ok(metadata) = std::fs::metadata(&self.log_path) {
            let current_size_mb = metadata.len() / (1024 * 1024);
            if current_size_mb >= u64::from(self.config.max_size_mb) {
                self.rotate()?;
            }
        }
        Ok(())
    }

    /// Rotate the log file
    fn rotate(&self) -> Result<()> {
        for i in (1..10).rev() {
            let old_name = format!("{}.{}.log", self.log_path.display(), i);
            let new_name = format!("{}.{}.log", self.log_path.display(), i + 1);
            let _ = std::fs::rename(&old_name, &new_name);
        }

        let rotated = format!("{}.1.log", self.log_path.display());
        std::fs::rename(&self.log_path, &rotated)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn audit_event_new_creates_unique_id() {
        let event1 = AuditEvent::new(AuditEventType::CommandExecution);
        let event2 = AuditEvent::new(AuditEventType::CommandExecution);
        assert_ne!(event1.event_id, event2.event_id);
    }

    #[test]
    fn audit_event_with_actor() {
        let event = AuditEvent::new(AuditEventType::CommandExecution).with_actor(
            "telegram".to_string(),
            Some("123".to_string()),
            Some("@alice".to_string()),
        );

        assert!(event.actor.is_some());
        let actor = event.actor.as_ref().unwrap();
        assert_eq!(actor.channel, "telegram");
        assert_eq!(actor.user_id, Some("123".to_string()));
        assert_eq!(actor.username, Some("@alice".to_string()));
    }

    #[test]
    fn audit_event_with_action() {
        let event = AuditEvent::new(AuditEventType::CommandExecution).with_action(
            "ls -la".to_string(),
            "low".to_string(),
            false,
            true,
        );

        assert!(event.action.is_some());
        let action = event.action.as_ref().unwrap();
        assert_eq!(action.command, Some("ls -la".to_string()));
        assert_eq!(action.risk_level, Some("low".to_string()));
    }

    #[test]
    fn audit_event_serializes_to_json() {
        let event = AuditEvent::new(AuditEventType::CommandExecution)
            .with_actor("telegram".to_string(), None, None)
            .with_action("ls".to_string(), "low".to_string(), false, true)
            .with_result(true, Some(0), 15, None);

        let json = serde_json::to_string(&event);
        assert!(json.is_ok());
        let json = json.expect("serialize");
        let parsed: AuditEvent = serde_json::from_str(json.as_str()).expect("parse");
        assert!(parsed.actor.is_some());
        assert!(parsed.action.is_some());
        assert!(parsed.result.is_some());
    }

    #[test]
    fn audit_logger_disabled_does_not_create_file() -> Result<()> {
        let tmp = TempDir::new()?;
        let config = AuditConfig {
            enabled: false,
            ..Default::default()
        };
        let logger = AuditLogger::new(config, tmp.path().to_path_buf())?;
        let event = AuditEvent::new(AuditEventType::CommandExecution);

        logger.log(&event)?;

        // File should not exist since logging is disabled
        assert!(!tmp.path().join("audit.log").exists());
        Ok(())
    }

    // ── §8.1 Log rotation tests ─────────────────────────────

    #[tokio::test]
    async fn audit_logger_writes_event_when_enabled() -> Result<()> {
        let tmp = TempDir::new()?;
        let config = AuditConfig {
            enabled: true,
            max_size_mb: 10,
            ..Default::default()
        };
        let logger = AuditLogger::new(config, tmp.path().to_path_buf())?;
        let event = AuditEvent::new(AuditEventType::CommandExecution)
            .with_actor("cli".to_string(), None, None)
            .with_action("ls".to_string(), "low".to_string(), false, true);

        logger.log(&event)?;

        let log_path = tmp.path().join("audit.log");
        assert!(log_path.exists(), "audit log file must be created");

        let content = tokio::fs::read_to_string(&log_path).await?;
        assert!(!content.is_empty(), "audit log must not be empty");

        let parsed: AuditEvent = serde_json::from_str(content.trim())?;
        assert!(parsed.action.is_some());
        Ok(())
    }

    #[tokio::test]
    async fn audit_log_command_event_writes_structured_entry() -> Result<()> {
        let tmp = TempDir::new()?;
        let config = AuditConfig {
            enabled: true,
            max_size_mb: 10,
            ..Default::default()
        };
        let logger = AuditLogger::new(config, tmp.path().to_path_buf())?;

        logger.log_command_event(CommandExecutionLog {
            channel: "telegram",
            command: "echo test",
            risk_level: "low",
            approved: false,
            allowed: true,
            success: true,
            duration_ms: 42,
        })?;

        let log_path = tmp.path().join("audit.log");
        let content = tokio::fs::read_to_string(&log_path).await?;
        let parsed: AuditEvent = serde_json::from_str(content.trim())?;

        let action = parsed.action.unwrap();
        assert_eq!(action.command, Some("echo test".to_string()));
        assert_eq!(action.risk_level, Some("low".to_string()));
        assert!(action.allowed);

        let result = parsed.result.unwrap();
        assert!(result.success);
        assert_eq!(result.duration_ms, Some(42));
        Ok(())
    }

    #[test]
    fn audit_rotation_creates_numbered_backup() -> Result<()> {
        let tmp = TempDir::new()?;
        let config = AuditConfig {
            enabled: true,
            max_size_mb: 0, // Force rotation on first write
            ..Default::default()
        };
        let logger = AuditLogger::new(config, tmp.path().to_path_buf())?;

        // Write initial content that triggers rotation
        let log_path = tmp.path().join("audit.log");
        std::fs::write(&log_path, "initial content\n")?;

        let event = AuditEvent::new(AuditEventType::CommandExecution);
        logger.log(&event)?;

        let rotated = format!("{}.1.log", log_path.display());
        assert!(
            std::path::Path::new(&rotated).exists(),
            "rotation must create .1.log backup"
        );
        Ok(())
    }

    // ── §8.2 ToolCallAudit tests ────────────────────────────

    #[test]
    fn tool_call_audit_serialization_roundtrip() {
        let event = AuditEvent::new(AuditEventType::ToolCallAudit)
            .with_action("shell".to_string(), "medium".to_string(), true, true)
            .with_result(true, None, 150, None);

        let json = serde_json::to_string(&event).expect("serialize");
        let parsed: AuditEvent = serde_json::from_str(&json).expect("deserialize");

        match parsed.event_type {
            AuditEventType::ToolCallAudit => {}
            other => panic!("expected ToolCallAudit, got {:?}", other),
        }
        let action = parsed.action.unwrap();
        assert_eq!(action.command, Some("shell".to_string()));
        assert_eq!(action.risk_level, Some("medium".to_string()));
    }

    #[tokio::test]
    async fn log_tool_call_writes_to_audit_file() -> Result<()> {
        let tmp = TempDir::new()?;
        let config = AuditConfig {
            enabled: true,
            max_size_mb: 10,
            ..Default::default()
        };
        let logger = AuditLogger::new(config, tmp.path().to_path_buf())?;

        logger.log_tool_call("cli", "file_read", true, 25, None)?;

        let log_path = tmp.path().join("audit.log");
        let content = tokio::fs::read_to_string(&log_path).await?;
        let parsed: AuditEvent = serde_json::from_str(content.trim())?;

        match parsed.event_type {
            AuditEventType::ToolCallAudit => {}
            other => panic!("expected ToolCallAudit, got {:?}", other),
        }
        let actor = parsed.actor.expect("actor must be set");
        assert_eq!(actor.channel, "cli");
        let action = parsed.action.unwrap();
        assert_eq!(action.command, Some("file_read".to_string()));
        assert_eq!(action.risk_level, Some("low".to_string()));
        assert!(parsed.result.unwrap().success);
        Ok(())
    }

    #[tokio::test]
    async fn log_tool_call_records_error_label_without_raw_args() -> Result<()> {
        let tmp = TempDir::new()?;
        let config = AuditConfig {
            enabled: true,
            max_size_mb: 10,
            ..Default::default()
        };
        let logger = AuditLogger::new(config, tmp.path().to_path_buf())?;

        logger.log_tool_call("daemon", "shell", false, 100, Some("permission_denied"))?;

        let log_path = tmp.path().join("audit.log");
        let content = tokio::fs::read_to_string(&log_path).await?;

        // Verify the raw content does not contain argument-like data
        assert!(!content.contains("arguments"));
        assert!(!content.contains("output"));

        let parsed: AuditEvent = serde_json::from_str(content.trim())?;
        let result = parsed.result.unwrap();
        assert!(!result.success);
        assert_eq!(result.error, Some("permission_denied".to_string()));
        assert_eq!(
            parsed.action.unwrap().risk_level,
            Some("medium".to_string())
        );
        Ok(())
    }

    #[test]
    fn log_tool_call_assigns_risk_levels_correctly() -> Result<()> {
        let tmp = TempDir::new()?;
        let config = AuditConfig {
            enabled: true,
            max_size_mb: 10,
            ..Default::default()
        };
        let logger = AuditLogger::new(config, tmp.path().to_path_buf())?;

        // Medium-risk tools
        for tool in &["shell", "file_write", "file_edit"] {
            logger.log_tool_call("cli", tool, true, 10, None)?;
        }
        // Low-risk tools
        for tool in &["file_read", "memory_recall", "browser_snapshot"] {
            logger.log_tool_call("cli", tool, true, 10, None)?;
        }

        let log_path = tmp.path().join("audit.log");
        let content = std::fs::read_to_string(&log_path)?;
        let events: Vec<AuditEvent> = content
            .lines()
            .map(|line| serde_json::from_str(line).expect("parse audit line"))
            .collect();

        assert_eq!(events.len(), 6);
        for event in &events[..3] {
            assert_eq!(
                event.action.as_ref().unwrap().risk_level,
                Some("medium".to_string())
            );
        }
        for event in &events[3..] {
            assert_eq!(
                event.action.as_ref().unwrap().risk_level,
                Some("low".to_string())
            );
        }
        Ok(())
    }

    // ── log_tool_call session source propagation ────────────────

    #[test]
    fn log_tool_call_propagates_various_session_sources() -> Result<()> {
        let tmp = TempDir::new()?;
        let config = AuditConfig {
            enabled: true,
            max_size_mb: 10,
            ..Default::default()
        };
        let logger = AuditLogger::new(config, tmp.path().to_path_buf())?;

        let sources = &["cli", "daemon", "heartbeat", "goal-loop", "cron:health-check"];
        for source in sources {
            logger.log_tool_call(source, "file_read", true, 10, None)?;
        }

        let log_path = tmp.path().join("audit.log");
        let content = std::fs::read_to_string(&log_path)?;
        let events: Vec<AuditEvent> = content
            .lines()
            .map(|line| serde_json::from_str(line).expect("parse audit line"))
            .collect();

        assert_eq!(events.len(), sources.len());
        for (event, expected_source) in events.iter().zip(sources.iter()) {
            let actor = event.actor.as_ref().expect("actor must be set");
            assert_eq!(actor.channel, *expected_source);
        }
        Ok(())
    }

    // ── AuditEvent full builder chain ───────────────────────────

    #[test]
    fn audit_event_full_builder_chain_sets_all_fields() {
        let event = AuditEvent::new(AuditEventType::ToolCallAudit)
            .with_actor(
                "telegram".to_string(),
                Some("u123".to_string()),
                Some("zeroclaw_user".to_string()),
            )
            .with_action("shell".to_string(), "medium".to_string(), true, true)
            .with_result(true, Some(0), 42, None)
            .with_security(Some("landlock".to_string()));

        let actor = event.actor.as_ref().unwrap();
        assert_eq!(actor.channel, "telegram");
        assert_eq!(actor.user_id, Some("u123".to_string()));
        assert_eq!(actor.username, Some("zeroclaw_user".to_string()));

        let action = event.action.as_ref().unwrap();
        assert_eq!(action.command, Some("shell".to_string()));
        assert_eq!(action.risk_level, Some("medium".to_string()));
        assert!(action.approved);
        assert!(action.allowed);

        let result = event.result.as_ref().unwrap();
        assert!(result.success);
        assert_eq!(result.exit_code, Some(0));
        assert_eq!(result.duration_ms, Some(42));
        assert!(result.error.is_none());

        assert_eq!(
            event.security.sandbox_backend,
            Some("landlock".to_string())
        );
    }

    #[test]
    fn audit_event_default_security_context() {
        let event = AuditEvent::new(AuditEventType::CommandExecution);
        assert!(!event.security.policy_violation);
        assert!(event.security.rate_limit_remaining.is_none());
        assert!(event.security.sandbox_backend.is_none());
    }

    // ── log_command backward compat ─────────────────────────────

    #[test]
    fn log_command_backward_compat_produces_command_execution_type() -> Result<()> {
        let tmp = TempDir::new()?;
        let config = AuditConfig {
            enabled: true,
            max_size_mb: 10,
            ..Default::default()
        };
        let logger = AuditLogger::new(config, tmp.path().to_path_buf())?;

        logger.log_command("telegram", "echo ok", "low", false, true, true, 15)?;

        let log_path = tmp.path().join("audit.log");
        let content = std::fs::read_to_string(&log_path)?;
        let parsed: AuditEvent = serde_json::from_str(content.trim())?;

        match parsed.event_type {
            AuditEventType::CommandExecution => {}
            other => panic!("expected CommandExecution, got {:?}", other),
        }
        let actor = parsed.actor.unwrap();
        assert_eq!(actor.channel, "telegram");
        Ok(())
    }

    // ── Edge cases ──────────────────────────────────────────────

    #[test]
    fn log_tool_call_with_none_error() -> Result<()> {
        let tmp = TempDir::new()?;
        let config = AuditConfig {
            enabled: true,
            max_size_mb: 10,
            ..Default::default()
        };
        let logger = AuditLogger::new(config, tmp.path().to_path_buf())?;

        logger.log_tool_call("cli", "file_read", true, 5, None)?;

        let log_path = tmp.path().join("audit.log");
        let content = std::fs::read_to_string(&log_path)?;
        let parsed: AuditEvent = serde_json::from_str(content.trim())?;
        assert!(parsed.result.unwrap().error.is_none());
        Ok(())
    }

    #[test]
    fn log_tool_call_with_empty_error_string() -> Result<()> {
        let tmp = TempDir::new()?;
        let config = AuditConfig {
            enabled: true,
            max_size_mb: 10,
            ..Default::default()
        };
        let logger = AuditLogger::new(config, tmp.path().to_path_buf())?;

        logger.log_tool_call("cli", "shell", false, 10, Some(""))?;

        let log_path = tmp.path().join("audit.log");
        let content = std::fs::read_to_string(&log_path)?;
        let parsed: AuditEvent = serde_json::from_str(content.trim())?;
        assert_eq!(parsed.result.unwrap().error, Some(String::new()));
        Ok(())
    }

    // ── Multiple events in one file ─────────────────────────────

    #[test]
    fn multiple_events_are_separate_json_lines() -> Result<()> {
        let tmp = TempDir::new()?;
        let config = AuditConfig {
            enabled: true,
            max_size_mb: 10,
            ..Default::default()
        };
        let logger = AuditLogger::new(config, tmp.path().to_path_buf())?;

        logger.log_tool_call("cli", "file_read", true, 10, None)?;
        logger.log_tool_call("daemon", "shell", false, 20, Some("denied"))?;
        logger.log_tool_call("heartbeat", "memory_recall", true, 30, None)?;

        let log_path = tmp.path().join("audit.log");
        let content = std::fs::read_to_string(&log_path)?;
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 3, "expected 3 JSON lines");

        let events: Vec<AuditEvent> = lines
            .iter()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(events[0].actor.as_ref().unwrap().channel, "cli");
        assert_eq!(events[1].actor.as_ref().unwrap().channel, "daemon");
        assert_eq!(events[2].actor.as_ref().unwrap().channel, "heartbeat");
        Ok(())
    }
}
