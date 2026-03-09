//! Audit logging for security events

use crate::config::AuditConfig;
use anyhow::Result;
use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::fs::OpenOptions;
use std::io::{BufRead, Write};
use std::path::PathBuf;
use uuid::Uuid;

type HmacSha256 = Hmac<Sha256>;

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
    ToolExecution,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prev_hash: Option<String>,
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
            prev_hash: None,
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

/// Audit logger with HMAC-based tamper-evident chain
pub struct AuditLogger {
    log_path: PathBuf,
    config: AuditConfig,
    buffer: Mutex<Vec<AuditEvent>>,
    chain_key: [u8; 32],
    prev_hash: Mutex<String>,
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
    /// Create a new audit logger with a random HMAC chain key
    pub fn new(config: AuditConfig, zeroclaw_dir: PathBuf) -> Result<Self> {
        let log_path = zeroclaw_dir.join(&config.log_path);
        let mut chain_key = [0u8; 32];
        use rand::RngCore;
        rand::rng().fill_bytes(&mut chain_key);
        Ok(Self {
            log_path,
            config,
            buffer: Mutex::new(Vec::new()),
            chain_key,
            prev_hash: Mutex::new("genesis".to_string()),
        })
    }

    /// Create a logger with a specific chain key (for verification)
    pub fn with_chain_key(
        config: AuditConfig,
        zeroclaw_dir: PathBuf,
        chain_key: [u8; 32],
    ) -> Result<Self> {
        let log_path = zeroclaw_dir.join(&config.log_path);
        Ok(Self {
            log_path,
            config,
            buffer: Mutex::new(Vec::new()),
            chain_key,
            prev_hash: Mutex::new("genesis".to_string()),
        })
    }

    fn compute_hash(&self, data: &str) -> String {
        let mut mac =
            HmacSha256::new_from_slice(&self.chain_key).expect("HMAC accepts any key size");
        mac.update(data.as_bytes());
        hex::encode(mac.finalize().into_bytes())
    }

    /// Log an event with HMAC chain linking
    pub fn log(&self, event: &AuditEvent) -> Result<()> {
        if !self.config.enabled {
            return Ok(());
        }

        self.rotate_if_needed()?;

        let mut chained = event.clone();
        {
            let prev = self.prev_hash.lock();
            chained.prev_hash = Some(prev.clone());
        }

        let line = serde_json::to_string(&chained)?;
        let new_hash = self.compute_hash(&line);

        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log_path)?;

        writeln!(file, "{}", line)?;
        file.sync_all()?;

        *self.prev_hash.lock() = new_hash;

        Ok(())
    }

    /// Verify the HMAC chain integrity of the audit log
    pub fn verify_chain(&self) -> Result<bool> {
        let file = match std::fs::File::open(&self.log_path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(true),
            Err(e) => return Err(e.into()),
        };

        let reader = std::io::BufReader::new(file);
        let mut expected_prev = "genesis".to_string();

        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }

            let entry: AuditEvent = serde_json::from_str(&line)?;
            match &entry.prev_hash {
                Some(prev) if *prev == expected_prev => {}
                _ => return Ok(false),
            }

            expected_prev = self.compute_hash(&line);
        }

        Ok(true)
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

    /// Log a tool execution event for audit trail
    pub fn log_tool_execution(
        &self,
        tool_name: &str,
        args_hash: &str,
        success: bool,
        duration_ms: u64,
    ) -> Result<()> {
        let event = AuditEvent::new(AuditEventType::ToolExecution)
            .with_actor("agent".to_string(), None, None)
            .with_action(
                format!("{}({})", tool_name, args_hash),
                "medium".to_string(),
                true,
                true,
            )
            .with_result(success, None, duration_ms, None);

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

    #[test]
    fn audit_logger_writes_event_when_enabled() -> Result<()> {
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

        let content = std::fs::read_to_string(&log_path)?;
        assert!(!content.is_empty(), "audit log must not be empty");

        let parsed: AuditEvent = serde_json::from_str(content.trim())?;
        assert!(parsed.action.is_some());
        assert_eq!(parsed.prev_hash.as_deref(), Some("genesis"));
        Ok(())
    }

    #[test]
    fn audit_log_command_event_writes_structured_entry() -> Result<()> {
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
        let content = std::fs::read_to_string(&log_path)?;
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

    // ── Chain verification tests ─────────────────────────────

    #[test]
    fn verify_chain_valid_sequence() -> Result<()> {
        let tmp = TempDir::new()?;
        let key = [42u8; 32];
        let config = AuditConfig {
            enabled: true,
            max_size_mb: 10,
            ..Default::default()
        };
        let logger = AuditLogger::with_chain_key(config, tmp.path().to_path_buf(), key)?;

        for _ in 0..5 {
            let event = AuditEvent::new(AuditEventType::CommandExecution);
            logger.log(&event)?;
        }

        assert!(logger.verify_chain()?);
        Ok(())
    }

    #[test]
    fn verify_chain_detects_tampering() -> Result<()> {
        let tmp = TempDir::new()?;
        let key = [42u8; 32];
        let config = AuditConfig {
            enabled: true,
            max_size_mb: 10,
            ..Default::default()
        };
        let logger = AuditLogger::with_chain_key(config, tmp.path().to_path_buf(), key)?;

        for _ in 0..3 {
            let event = AuditEvent::new(AuditEventType::CommandExecution);
            logger.log(&event)?;
        }

        let log_path = tmp.path().join("audit.log");
        let content = std::fs::read_to_string(&log_path)?;
        let mut lines: Vec<&str> = content.trim().lines().collect();

        let mut tampered: serde_json::Value = serde_json::from_str(lines[1])?;
        tampered["event_type"] = serde_json::json!("policy_violation");
        let tampered_line = serde_json::to_string(&tampered)?;
        lines[1] = &tampered_line;

        std::fs::write(&log_path, lines.join("\n") + "\n")?;
        assert!(!logger.verify_chain()?);
        Ok(())
    }

    #[test]
    fn verify_chain_empty_log_is_valid() -> Result<()> {
        let tmp = TempDir::new()?;
        let config = AuditConfig {
            enabled: true,
            max_size_mb: 10,
            ..Default::default()
        };
        let logger = AuditLogger::new(config, tmp.path().to_path_buf())?;
        assert!(logger.verify_chain()?);
        Ok(())
    }

    // ── Tool execution audit tests ───────────────────────────

    #[test]
    fn log_tool_execution_writes_entry() -> Result<()> {
        let tmp = TempDir::new()?;
        let config = AuditConfig {
            enabled: true,
            max_size_mb: 10,
            ..Default::default()
        };
        let logger = AuditLogger::new(config, tmp.path().to_path_buf())?;

        logger.log_tool_execution("shell", "abc123", true, 50)?;

        let log_path = tmp.path().join("audit.log");
        let content = std::fs::read_to_string(&log_path)?;
        let parsed: AuditEvent = serde_json::from_str(content.trim())?;

        assert!(matches!(parsed.event_type, AuditEventType::ToolExecution));
        let action = parsed.action.unwrap();
        assert_eq!(action.command, Some("shell(abc123)".to_string()));
        Ok(())
    }
}
