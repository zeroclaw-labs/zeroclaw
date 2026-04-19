//! Audit logging for security events
//!
//! Each audit entry is chained via a Merkle hash: `entry_hash = SHA-256(prev_hash || canonical_json)`.
//! This makes the trail tamper-evident — modifying any entry invalidates all subsequent hashes.

use anyhow::{Result, bail};
use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use uuid::Uuid;
use zeroclaw_config::schema::AuditConfig;

/// Well-known seed for the genesis entry's `prev_hash`.
const GENESIS_PREV_HASH: &str = "0000000000000000000000000000000000000000000000000000000000000000";

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
    PluginExecution,
    CliExecution,
}

/// Actor information (who performed the action)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Actor {
    pub channel: String,
    pub user_id: Option<String>,
    pub username: Option<String>,
}

/// A single HTTP request observed during plugin execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HttpRequestEntry {
    pub method: String,
    pub url: String,
}

/// Action information (what was done)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Action {
    pub command: Option<String>,
    pub risk_level: Option<String>,
    pub approved: bool,
    pub allowed: bool,
    /// Redacted input parameters (present only for plugin execution events).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub redacted_input: Option<String>,
    /// HTTP requests made during plugin execution (URL and method only).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub http_requests: Option<Vec<HttpRequestEntry>>,
    /// WASM export name invoked (present only for plugin execution events).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub export_name: Option<String>,
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

/// Complete audit event with Merkle hash-chain fields.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEvent {
    pub timestamp: DateTime<Utc>,
    pub event_id: String,
    pub event_type: AuditEventType,
    pub actor: Option<Actor>,
    pub action: Option<Action>,
    pub result: Option<ExecutionResult>,
    pub security: SecurityContext,

    /// Monotonically increasing sequence number.
    #[serde(default)]
    pub sequence: u64,
    /// SHA-256 hash of the previous entry (genesis uses [`GENESIS_PREV_HASH`]).
    #[serde(default)]
    pub prev_hash: String,
    /// SHA-256 hash of (`prev_hash` || canonical JSON of this entry's content fields).
    #[serde(default)]
    pub entry_hash: String,

    /// Optional HMAC-SHA256 signature over entry_hash (present only when sign_events enabled)
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub signature: Option<String>,
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
            sequence: 0,
            prev_hash: String::new(),
            entry_hash: String::new(),
            signature: None,
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
            redacted_input: None,
            http_requests: None,
            export_name: None,
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

/// Compute the SHA-256 entry hash: `H(prev_hash || content_json)`.
///
/// `content_json` is the canonical JSON of the event *without* the chain fields
/// (`sequence`, `prev_hash`, `entry_hash`), so the hash covers only the payload.
fn compute_entry_hash(prev_hash: &str, event: &AuditEvent) -> String {
    // Build a canonical representation of the content fields only.
    let content = serde_json::json!({
        "timestamp": event.timestamp,
        "event_id": event.event_id,
        "event_type": event.event_type,
        "actor": event.actor,
        "action": event.action,
        "result": event.result,
        "security": event.security,
        "sequence": event.sequence,
    });
    let content_json = serde_json::to_string(&content).expect("serialize canonical content");

    let mut hasher = Sha256::new();
    hasher.update(prev_hash.as_bytes());
    hasher.update(content_json.as_bytes());
    hex::encode(hasher.finalize())
}

/// Internal chain state tracked across writes.
struct ChainState {
    prev_hash: String,
    sequence: u64,
}

/// Audit logger
pub struct AuditLogger {
    log_path: PathBuf,
    config: AuditConfig,
    #[allow(dead_code)] // WIP: buffered writes for batch flushing
    buffer: Mutex<Vec<AuditEvent>>,
    chain: Mutex<ChainState>,
    /// Signing key (loaded once at construction time if sign_events enabled)
    signing_key: Option<Vec<u8>>,
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

/// Structured plugin execution details for audit logging.
#[derive(Debug, Clone)]
pub struct PluginExecutionLog<'a> {
    pub plugin_name: &'a str,
    pub plugin_version: &'a str,
    pub tool_name: &'a str,
    /// WASM export function name invoked by the plugin.
    pub export_name: &'a str,
    pub success: bool,
    pub duration_ms: u64,
    pub error: Option<&'a str>,
    /// Redacted JSON representation of the input parameters.
    pub redacted_input: Option<String>,
    /// HTTP requests observed during execution (URL and method only).
    pub http_requests: Option<Vec<HttpRequestEntry>>,
}

/// Audit entry for CLI command executions from WASM plugins.
///
/// Captures all relevant details about a CLI command executed via the
/// `zeroclaw_cli_exec` host function, with sensitive arguments automatically
/// redacted for safe logging.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CliAuditEntry {
    /// Name of the plugin that executed the command.
    pub plugin_name: String,
    /// The command that was executed (e.g., "git", "npm").
    pub command: String,
    /// Command arguments with sensitive values redacted.
    pub args_redacted: Vec<String>,
    /// Working directory where the command was executed.
    pub working_dir: Option<String>,
    /// Exit code returned by the command.
    pub exit_code: i32,
    /// Execution duration in milliseconds.
    pub duration_ms: u64,
    /// Number of bytes in the combined stdout/stderr output.
    pub output_bytes: usize,
    /// Whether the output was truncated due to size limits.
    pub truncated: bool,
    /// Whether the command was terminated due to timeout.
    pub timed_out: bool,
    /// Timestamp when the command completed.
    pub timestamp: DateTime<Utc>,
}

impl CliAuditEntry {
    /// Create a new CLI audit entry with automatic argument redaction.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        plugin_name: impl Into<String>,
        command: impl Into<String>,
        args: &[String],
        working_dir: Option<String>,
        exit_code: i32,
        duration_ms: u64,
        output_bytes: usize,
        truncated: bool,
        timed_out: bool,
    ) -> Self {
        Self {
            plugin_name: plugin_name.into(),
            command: command.into(),
            args_redacted: args.iter().map(|a| redact_sensitive_arg(a)).collect(),
            working_dir,
            exit_code,
            duration_ms,
            output_bytes,
            truncated,
            timed_out,
            timestamp: Utc::now(),
        }
    }
}

/// Patterns for arguments that should be fully redacted.
///
/// These patterns match common sensitive argument forms like:
/// - `--password=secret` or `--token=abc123`
/// - `-p secret` style (detected by flag, value redacted separately)
/// - Values that look like API keys or tokens
const SENSITIVE_ARG_PREFIXES: &[&str] = &[
    "--password=",
    "--passwd=",
    "--token=",
    "--api-key=",
    "--apikey=",
    "--secret=",
    "--secret-key=",
    "--access-key=",
    "--auth=",
    "--authorization=",
    "--bearer=",
    "--credential=",
    "--private-key=",
    "-p=",
];

/// Short flags that indicate the next argument is sensitive.
const SENSITIVE_SHORT_FLAGS: &[&str] = &["-p", "-P", "--password", "--token", "--secret"];

/// Redact sensitive values from a single command-line argument.
///
/// Handles several patterns:
/// - `--password=value` → `--password=[REDACTED]`
/// - Values matching API key patterns → `[REDACTED]`
/// - Environment variable assignments with secrets → `VAR=[REDACTED]`
pub fn redact_sensitive_arg(arg: &str) -> String {
    // Check for --key=value patterns with sensitive keys
    for prefix in SENSITIVE_ARG_PREFIXES {
        if let Some(stripped) = arg.to_lowercase().strip_prefix(&prefix.to_lowercase())
            && !stripped.is_empty()
        {
            // Find the actual prefix length in the original string (case-preserved)
            let prefix_len = prefix.len();
            return format!("{}[REDACTED]", &arg[..prefix_len]);
        }
    }

    // Check for environment variable assignments with sensitive names
    if let Some(eq_pos) = arg.find('=') {
        let var_name = &arg[..eq_pos].to_uppercase();
        if is_sensitive_env_var(var_name) {
            return format!("{}=[REDACTED]", &arg[..eq_pos]);
        }
    }

    // Check if the value itself looks like a secret (API key, token, etc.)
    if looks_like_secret(arg) {
        return "[REDACTED]".to_string();
    }

    arg.to_string()
}

/// Check if an environment variable name suggests sensitive content.
fn is_sensitive_env_var(name: &str) -> bool {
    let sensitive_patterns = [
        "PASSWORD",
        "PASSWD",
        "SECRET",
        "TOKEN",
        "API_KEY",
        "APIKEY",
        "AUTH",
        "CREDENTIAL",
        "PRIVATE_KEY",
        "ACCESS_KEY",
        "AWS_SECRET",
    ];
    sensitive_patterns.iter().any(|p| name.contains(p))
}

/// Check if a value looks like a secret (API key, token, etc.).
///
/// Detects common patterns:
/// - Prefixes: sk-, pk-, xox[bpra]-, ghp_, gho_, ghu_, github_pat_
/// - Base64-encoded strings of sufficient length
/// - High-entropy alphanumeric strings
fn looks_like_secret(value: &str) -> bool {
    // Skip short values and paths
    if value.len() < 16 || value.starts_with('/') || value.starts_with('.') {
        return false;
    }

    // Common API key prefixes
    let secret_prefixes = [
        "sk-",
        "sk_",
        "pk-",
        "pk_",
        "xoxb-",
        "xoxp-",
        "xoxa-",
        "xoxr-",
        "ghp_",
        "gho_",
        "ghu_",
        "ghs_",
        "github_pat_",
        "glpat-",
        "AKIA", // AWS access key
        "bearer ",
        "Bearer ",
    ];

    if secret_prefixes.iter().any(|p| value.starts_with(p)) {
        return true;
    }

    // Check for high-entropy alphanumeric strings (likely tokens/keys)
    // Must be mostly alphanumeric and have good character variety
    if value.len() >= 24 {
        let alphanum_count = value.chars().filter(|c| c.is_ascii_alphanumeric()).count();
        let alphanum_ratio = alphanum_count as f64 / value.len() as f64;

        if alphanum_ratio > 0.85 {
            // Count unique characters - high variety suggests randomness
            let unique_chars: std::collections::HashSet<char> = value.chars().collect();
            let uniqueness_ratio = unique_chars.len() as f64 / value.len() as f64;

            // High uniqueness in a long alphanumeric string suggests a secret
            if uniqueness_ratio > 0.4 {
                return true;
            }
        }
    }

    false
}

/// Redact sensitive values from a sequence of arguments.
///
/// Handles positional sensitivity: if a flag like `-p` or `--password` is seen,
/// the following argument is also redacted.
pub fn redact_sensitive_args(args: &[String]) -> Vec<String> {
    let mut result = Vec::with_capacity(args.len());
    let mut redact_next = false;

    for arg in args {
        if redact_next {
            result.push("[REDACTED]".to_string());
            redact_next = false;
            continue;
        }

        // Check if this flag indicates the next arg is sensitive
        if SENSITIVE_SHORT_FLAGS.iter().any(|f| arg == *f) {
            result.push(arg.clone());
            redact_next = true;
            continue;
        }

        result.push(redact_sensitive_arg(arg));
    }

    result
}

impl AuditLogger {
    /// Create a new audit logger.
    ///
    /// If the log file already exists, the chain state is recovered from the last
    /// entry so that new writes continue the existing hash chain.
    ///
    /// If `config.sign_events` is true, requires `ZEROCLAW_AUDIT_SIGNING_KEY` env var
    /// to be set with a hex-encoded 32-byte key. Fails if key is missing or invalid.
    pub fn new(config: AuditConfig, zeroclaw_dir: PathBuf) -> Result<Self> {
        // Load and validate signing key if sign_events enabled
        let signing_key = if config.sign_events {
            let key_hex = std::env::var("ZEROCLAW_AUDIT_SIGNING_KEY").map_err(|_| {
                anyhow::anyhow!("sign_events enabled but ZEROCLAW_AUDIT_SIGNING_KEY not set")
            })?;

            let key_bytes = hex::decode(&key_hex)
                .map_err(|_| anyhow::anyhow!("ZEROCLAW_AUDIT_SIGNING_KEY must be hex-encoded"))?;

            if key_bytes.len() != 32 {
                bail!(
                    "ZEROCLAW_AUDIT_SIGNING_KEY must be 32 bytes (64 hex chars), got {}",
                    key_bytes.len()
                );
            }

            Some(key_bytes)
        } else {
            None
        };

        let log_path = zeroclaw_dir.join(&config.log_path);
        let chain_state = recover_chain_state(&log_path);
        Ok(Self {
            log_path,
            config,
            buffer: Mutex::new(Vec::new()),
            chain: Mutex::new(chain_state),
            signing_key,
        })
    }

    /// Compute HMAC-SHA256 signature over entry_hash when sign_events enabled.
    fn compute_signature(&self, entry_hash: &str) -> Result<Option<String>> {
        if let Some(ref key_bytes) = self.signing_key {
            use hmac::{Hmac, Mac};
            use sha2::Sha256;

            let mut mac = Hmac::<Sha256>::new_from_slice(key_bytes)
                .map_err(|_| anyhow::anyhow!("Invalid HMAC key length"))?;
            mac.update(entry_hash.as_bytes());

            Ok(Some(hex::encode(mac.finalize().into_bytes())))
        } else {
            Ok(None)
        }
    }

    /// Log an event
    pub fn log(&self, event: &AuditEvent) -> Result<()> {
        if !self.config.enabled {
            return Ok(());
        }

        // Check log size and rotate if needed
        self.rotate_if_needed()?;

        // Populate chain fields under the lock
        let mut chained = event.clone();
        {
            let mut state = self.chain.lock();
            chained.sequence = state.sequence;
            chained.prev_hash = state.prev_hash.clone();
            chained.entry_hash = compute_entry_hash(&state.prev_hash, &chained);

            // Compute signature if sign_events enabled
            chained.signature = self.compute_signature(&chained.entry_hash)?;

            state.prev_hash = chained.entry_hash.clone();
            state.sequence += 1;
        }

        // Serialize and write
        let line = serde_json::to_string(&chained)?;
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

    /// Log a plugin tool execution event.
    pub fn log_plugin_execution(&self, entry: PluginExecutionLog<'_>) -> Result<()> {
        let mut event = AuditEvent::new(AuditEventType::PluginExecution)
            .with_action(
                format!(
                    "{}@{}:{}",
                    entry.plugin_name, entry.plugin_version, entry.tool_name
                ),
                "low".to_string(),
                true,
                true,
            )
            .with_result(
                entry.success,
                None,
                entry.duration_ms,
                entry.error.map(|s| s.to_string()),
            );

        // Attach export name to the action.
        if let Some(action) = &mut event.action {
            action.export_name = Some(entry.export_name.to_string());
        }

        // Attach redacted input parameters if provided.
        if let (Some(action), Some(redacted)) = (&mut event.action, entry.redacted_input) {
            action.redacted_input = Some(redacted);
        }

        // Attach HTTP request log if provided.
        if let (Some(action), Some(requests)) = (&mut event.action, entry.http_requests)
            && !requests.is_empty()
        {
            action.http_requests = Some(requests);
        }

        self.log(&event)
    }

    /// Log a CLI command execution from a plugin.
    ///
    /// Records command, redacted arguments, exit code, duration, and output metadata.
    /// This is called after each `zeroclaw_cli_exec` invocation completes.
    pub fn log_cli(&self, entry: &CliAuditEntry) -> Result<()> {
        let success = entry.exit_code == 0 && !entry.timed_out;
        let error = if entry.timed_out {
            Some("command timed out".to_string())
        } else if entry.exit_code != 0 {
            Some(format!("exit code {}", entry.exit_code))
        } else {
            None
        };

        let mut event = AuditEvent::new(AuditEventType::CliExecution)
            .with_actor(entry.plugin_name.clone(), None, None)
            .with_action(
                entry.command.clone(),
                "medium".to_string(), // CLI execution is medium risk
                true,                 // approved (passed capability checks)
                true,                 // allowed (command was in allowlist)
            )
            .with_result(success, Some(entry.exit_code), entry.duration_ms, error);

        // Store redacted args and CLI metadata as JSON in the redacted_input field
        if let Some(action) = &mut event.action {
            let cli_details = serde_json::json!({
                "args": entry.args_redacted,
                "working_dir": entry.working_dir,
                "output_bytes": entry.output_bytes,
                "truncated": entry.truncated,
                "timed_out": entry.timed_out,
            });
            action.redacted_input = Some(cli_details.to_string());
        }

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

/// Recover chain state from an existing log file.
///
/// Returns the genesis state if the file does not exist or is empty.
fn recover_chain_state(log_path: &Path) -> ChainState {
    let file = match std::fs::File::open(log_path) {
        Ok(f) => f,
        Err(_) => {
            return ChainState {
                prev_hash: GENESIS_PREV_HASH.to_string(),
                sequence: 0,
            };
        }
    };

    let reader = BufReader::new(file);
    let mut last_entry: Option<AuditEvent> = None;
    for l in reader.lines().map_while(Result::ok) {
        if let Ok(entry) = serde_json::from_str::<AuditEvent>(&l) {
            last_entry = Some(entry);
        }
    }

    match last_entry {
        Some(entry) => ChainState {
            prev_hash: entry.entry_hash,
            sequence: entry.sequence + 1,
        },
        None => ChainState {
            prev_hash: GENESIS_PREV_HASH.to_string(),
            sequence: 0,
        },
    }
}

/// Verify the integrity of an audit log's Merkle hash chain.
///
/// Reads every entry from the log file and checks:
/// - Each `entry_hash` matches the recomputed `SHA-256(prev_hash || content)`.
/// - `prev_hash` links to the preceding entry (or the genesis seed for the first).
/// - Sequence numbers are contiguous starting from 0.
/// - If a record has a `signature` field and `ZEROCLAW_AUDIT_SIGNING_KEY` is available,
///   verifies the HMAC-SHA256 signature over `entry_hash`.
///
/// Returns `Ok(entry_count)` on success, or an error describing the first violation.
pub fn verify_chain(log_path: &Path) -> Result<u64> {
    let file = std::fs::File::open(log_path)?;
    let reader = BufReader::new(file);

    let mut expected_prev_hash = GENESIS_PREV_HASH.to_string();
    let mut expected_sequence: u64 = 0;

    // Attempt to load signing key from environment (optional)
    let signing_key = std::env::var("ZEROCLAW_AUDIT_SIGNING_KEY")
        .ok()
        .and_then(|key_hex| hex::decode(&key_hex).ok())
        .filter(|key_bytes| key_bytes.len() == 32);

    for (line_idx, line) in reader.lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let entry: AuditEvent = serde_json::from_str(&line)?;

        // Check sequence continuity
        if entry.sequence != expected_sequence {
            bail!(
                "sequence gap at line {}: expected {}, got {}",
                line_idx + 1,
                expected_sequence,
                entry.sequence
            );
        }

        // Check prev_hash linkage
        if entry.prev_hash != expected_prev_hash {
            bail!(
                "prev_hash mismatch at line {} (sequence {}): expected {}, got {}",
                line_idx + 1,
                entry.sequence,
                expected_prev_hash,
                entry.prev_hash
            );
        }

        // Recompute and verify entry_hash
        let recomputed = compute_entry_hash(&entry.prev_hash, &entry);
        if entry.entry_hash != recomputed {
            bail!(
                "entry_hash mismatch at line {} (sequence {}): expected {}, got {}",
                line_idx + 1,
                entry.sequence,
                recomputed,
                entry.entry_hash
            );
        }

        // Verify signature if present and key is available
        if let Some(ref signature) = entry.signature
            && let Some(ref key_bytes) = signing_key
        {
            use hmac::{Hmac, Mac};
            use sha2::Sha256;

            let mut mac = Hmac::<Sha256>::new_from_slice(key_bytes)
                .map_err(|_| anyhow::anyhow!("Invalid HMAC key length during verification"))?;
            mac.update(entry.entry_hash.as_bytes());
            let expected_sig = hex::encode(mac.finalize().into_bytes());

            if signature != &expected_sig {
                bail!(
                    "signature verification failed at line {} (sequence {}): signature mismatch",
                    line_idx + 1,
                    entry.sequence
                );
            }
        }
        // If signature present but key not available, skip verification (backward compat)

        expected_prev_hash = entry.entry_hash.clone();
        expected_sequence += 1;
    }

    Ok(expected_sequence)
}

#[cfg(test)]
mod tests {
    use super::*;
    use scopeguard::defer;
    use std::sync::Mutex;
    use tempfile::TempDir;

    /// Mutex to serialize tests that read/write ZEROCLAW_AUDIT_SIGNING_KEY env var.
    static ENV_MUTEX: Mutex<()> = Mutex::new(());

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
            Some("@zeroclaw_user".to_string()),
        );

        assert!(event.actor.is_some());
        let actor = event.actor.as_ref().unwrap();
        assert_eq!(actor.channel, "telegram");
        assert_eq!(actor.user_id, Some("123".to_string()));
        assert_eq!(actor.username, Some("@zeroclaw_user".to_string()));
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

    // ── Merkle hash-chain tests ─────────────────────────────

    #[test]
    fn merkle_chain_genesis_uses_well_known_seed() -> Result<()> {
        let tmp = TempDir::new()?;
        let config = AuditConfig {
            enabled: true,
            max_size_mb: 10,
            ..Default::default()
        };
        let logger = AuditLogger::new(config, tmp.path().to_path_buf())?;

        let event = AuditEvent::new(AuditEventType::SecurityEvent);
        logger.log(&event)?;

        let log_path = tmp.path().join("audit.log");
        let content = std::fs::read_to_string(&log_path)?;
        let parsed: AuditEvent = serde_json::from_str(content.trim())?;

        assert_eq!(parsed.sequence, 0);
        assert_eq!(parsed.prev_hash, GENESIS_PREV_HASH);
        assert!(!parsed.entry_hash.is_empty());
        Ok(())
    }

    #[test]
    fn merkle_chain_multiple_entries_verify() -> Result<()> {
        let tmp = TempDir::new()?;
        let config = AuditConfig {
            enabled: true,
            max_size_mb: 10,
            ..Default::default()
        };
        let logger = AuditLogger::new(config, tmp.path().to_path_buf())?;

        // Write several events
        for i in 0..5 {
            let event = AuditEvent::new(AuditEventType::CommandExecution).with_action(
                format!("cmd-{}", i),
                "low".to_string(),
                false,
                true,
            );
            logger.log(&event)?;
        }

        let log_path = tmp.path().join("audit.log");
        let count = verify_chain(&log_path)?;
        assert_eq!(count, 5);
        Ok(())
    }

    #[test]
    fn merkle_chain_detects_tampered_entry() -> Result<()> {
        let tmp = TempDir::new()?;
        let config = AuditConfig {
            enabled: true,
            max_size_mb: 10,
            ..Default::default()
        };
        let logger = AuditLogger::new(config, tmp.path().to_path_buf())?;

        for i in 0..3 {
            let event = AuditEvent::new(AuditEventType::CommandExecution).with_action(
                format!("cmd-{}", i),
                "low".to_string(),
                false,
                true,
            );
            logger.log(&event)?;
        }

        // Tamper with the second entry (change the command text)
        let log_path = tmp.path().join("audit.log");
        let content = std::fs::read_to_string(&log_path)?;
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 3);

        let mut entry: serde_json::Value = serde_json::from_str(lines[1])?;
        entry["action"]["command"] = serde_json::Value::String("TAMPERED".to_string());
        let tampered_line = serde_json::to_string(&entry)?;

        let tampered_content = format!("{}\n{}\n{}\n", lines[0], tampered_line, lines[2]);
        std::fs::write(&log_path, tampered_content)?;

        // Verification must fail
        let result = verify_chain(&log_path);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("entry_hash mismatch"),
            "expected entry_hash mismatch, got: {}",
            err_msg
        );
        Ok(())
    }

    #[test]
    fn merkle_chain_detects_sequence_gap() -> Result<()> {
        let tmp = TempDir::new()?;
        let config = AuditConfig {
            enabled: true,
            max_size_mb: 10,
            ..Default::default()
        };
        let logger = AuditLogger::new(config, tmp.path().to_path_buf())?;

        for i in 0..3 {
            let event = AuditEvent::new(AuditEventType::CommandExecution).with_action(
                format!("cmd-{}", i),
                "low".to_string(),
                false,
                true,
            );
            logger.log(&event)?;
        }

        // Remove the second entry to create a sequence gap
        let log_path = tmp.path().join("audit.log");
        let content = std::fs::read_to_string(&log_path)?;
        let lines: Vec<&str> = content.lines().collect();
        let gapped_content = format!("{}\n{}\n", lines[0], lines[2]);
        std::fs::write(&log_path, gapped_content)?;

        let result = verify_chain(&log_path);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("sequence gap"),
            "expected sequence gap, got: {}",
            err_msg
        );
        Ok(())
    }

    #[test]
    fn merkle_chain_recovery_continues_after_restart() -> Result<()> {
        let tmp = TempDir::new()?;
        let log_path = tmp.path().join("audit.log");

        // First logger writes 2 entries
        {
            let config = AuditConfig {
                enabled: true,
                max_size_mb: 10,
                ..Default::default()
            };
            let logger = AuditLogger::new(config, tmp.path().to_path_buf())?;
            for i in 0..2 {
                let event = AuditEvent::new(AuditEventType::CommandExecution).with_action(
                    format!("batch1-{}", i),
                    "low".to_string(),
                    false,
                    true,
                );
                logger.log(&event)?;
            }
        }

        // Second logger (simulating restart) continues the chain
        {
            let config = AuditConfig {
                enabled: true,
                max_size_mb: 10,
                ..Default::default()
            };
            let logger = AuditLogger::new(config, tmp.path().to_path_buf())?;
            for i in 0..2 {
                let event = AuditEvent::new(AuditEventType::CommandExecution).with_action(
                    format!("batch2-{}", i),
                    "low".to_string(),
                    false,
                    true,
                );
                logger.log(&event)?;
            }
        }

        // Full chain should verify (4 entries, sequences 0..3)
        let count = verify_chain(&log_path)?;
        assert_eq!(count, 4);
        Ok(())
    }

    // ── HMAC signing tests ──────────────────────────────────

    #[test]
    fn signature_present_when_sign_events_enabled() -> Result<()> {
        let _guard = ENV_MUTEX.lock().unwrap();
        let old_key = std::env::var("ZEROCLAW_AUDIT_SIGNING_KEY").ok();
        defer! {
            if let Some(key) = old_key {
                // SAFETY: test-only, single-threaded test runner.
                unsafe { std::env::set_var("ZEROCLAW_AUDIT_SIGNING_KEY", key) };
            } else {
                // SAFETY: test-only, single-threaded test runner.
                unsafe { std::env::remove_var("ZEROCLAW_AUDIT_SIGNING_KEY") };
            }
        }

        let tmp = TempDir::new()?;
        let test_key = "a".repeat(64); // 64 hex chars = 32 bytes
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var("ZEROCLAW_AUDIT_SIGNING_KEY", &test_key) };

        let config = AuditConfig {
            enabled: true,
            sign_events: true,
            ..Default::default()
        };
        let logger = AuditLogger::new(config, tmp.path().to_path_buf())?;
        let event = AuditEvent::new(AuditEventType::CommandExecution);

        logger.log(&event)?;

        let log_path = tmp.path().join("audit.log");
        let content = std::fs::read_to_string(&log_path)?;
        let parsed: AuditEvent = serde_json::from_str(content.trim())?;

        assert!(
            parsed.signature.is_some(),
            "signature must be present when sign_events=true"
        );
        let sig = parsed.signature.unwrap();
        assert_eq!(sig.len(), 64, "HMAC-SHA256 signature must be 64 hex chars");

        Ok(())
    }

    #[test]
    fn signature_absent_when_sign_events_disabled() -> Result<()> {
        let _guard = ENV_MUTEX.lock().unwrap();
        let tmp = TempDir::new()?;
        let config = AuditConfig {
            enabled: true,
            sign_events: false,
            ..Default::default()
        };
        let logger = AuditLogger::new(config, tmp.path().to_path_buf())?;
        let event = AuditEvent::new(AuditEventType::CommandExecution);

        logger.log(&event)?;

        let log_path = tmp.path().join("audit.log");
        let content = std::fs::read_to_string(&log_path)?;
        let parsed: AuditEvent = serde_json::from_str(content.trim())?;

        assert!(
            parsed.signature.is_none(),
            "signature must be absent when sign_events=false"
        );
        Ok(())
    }

    #[test]
    fn signature_computed_over_entry_hash() -> Result<()> {
        let _guard = ENV_MUTEX.lock().unwrap();
        let old_key = std::env::var("ZEROCLAW_AUDIT_SIGNING_KEY").ok();
        defer! {
            if let Some(key) = old_key {
                // SAFETY: test-only, single-threaded test runner.
                unsafe { std::env::set_var("ZEROCLAW_AUDIT_SIGNING_KEY", key) };
            } else {
                // SAFETY: test-only, single-threaded test runner.
                unsafe { std::env::remove_var("ZEROCLAW_AUDIT_SIGNING_KEY") };
            }
        }

        let tmp = TempDir::new()?;
        let test_key = "b".repeat(64);
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var("ZEROCLAW_AUDIT_SIGNING_KEY", &test_key) };

        let config = AuditConfig {
            enabled: true,
            sign_events: true,
            ..Default::default()
        };
        let logger = AuditLogger::new(config, tmp.path().to_path_buf())?;
        let event = AuditEvent::new(AuditEventType::CommandExecution);

        logger.log(&event)?;

        let log_path = tmp.path().join("audit.log");
        let content = std::fs::read_to_string(&log_path)?;
        let parsed: AuditEvent = serde_json::from_str(content.trim())?;

        // Manually recompute HMAC to verify correctness
        use hmac::{Hmac, Mac};
        use sha2::Sha256;
        let key_bytes = hex::decode(&test_key)?;
        let mut mac = Hmac::<Sha256>::new_from_slice(&key_bytes).unwrap();
        mac.update(parsed.entry_hash.as_bytes());
        let expected_sig = hex::encode(mac.finalize().into_bytes());

        assert_eq!(parsed.signature, Some(expected_sig));

        Ok(())
    }

    #[test]
    fn constructor_fails_if_sign_events_but_no_key() -> Result<()> {
        let _guard = ENV_MUTEX.lock().unwrap();
        let old_key = std::env::var("ZEROCLAW_AUDIT_SIGNING_KEY").ok();
        defer! {
            // Only restore if it was a valid 64-char key
            if let Some(key) = old_key.as_ref().filter(|k| k.len() == 64) {
                // SAFETY: test-only, single-threaded test runner.
                unsafe { std::env::set_var("ZEROCLAW_AUDIT_SIGNING_KEY", key) };
            } else {
                // SAFETY: test-only, single-threaded test runner.
                unsafe { std::env::remove_var("ZEROCLAW_AUDIT_SIGNING_KEY") };
            }
        }

        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::remove_var("ZEROCLAW_AUDIT_SIGNING_KEY") };

        let tmp = TempDir::new()?;
        let config = AuditConfig {
            enabled: true,
            sign_events: true,
            ..Default::default()
        };

        let result = AuditLogger::new(config, tmp.path().to_path_buf());
        assert!(result.is_err());
        if let Err(e) = result {
            let err_msg = e.to_string();
            assert!(
                err_msg.contains("ZEROCLAW_AUDIT_SIGNING_KEY not set"),
                "error: {}",
                err_msg
            );
        }

        Ok(())
    }

    #[test]
    fn constructor_fails_if_signing_key_invalid_hex() -> Result<()> {
        let _guard = ENV_MUTEX.lock().unwrap();
        let old_key = std::env::var("ZEROCLAW_AUDIT_SIGNING_KEY").ok();
        defer! {
            // Only restore if it was a valid 64-char key
            if let Some(key) = old_key.as_ref().filter(|k| k.len() == 64) {
                // SAFETY: test-only, single-threaded test runner.
                unsafe { std::env::set_var("ZEROCLAW_AUDIT_SIGNING_KEY", key) };
            } else {
                // SAFETY: test-only, single-threaded test runner.
                unsafe { std::env::remove_var("ZEROCLAW_AUDIT_SIGNING_KEY") };
            }
        }

        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var("ZEROCLAW_AUDIT_SIGNING_KEY", "not-valid-hex") };

        let tmp = TempDir::new()?;
        let config = AuditConfig {
            enabled: true,
            sign_events: true,
            ..Default::default()
        };

        let result = AuditLogger::new(config, tmp.path().to_path_buf());
        assert!(result.is_err());
        if let Err(e) = result {
            let err_msg = e.to_string();
            assert!(
                err_msg.contains("must be hex-encoded"),
                "error: {}",
                err_msg
            );
        }

        Ok(())
    }

    #[test]
    fn constructor_fails_if_signing_key_wrong_length() -> Result<()> {
        let _guard = ENV_MUTEX.lock().unwrap();
        let old_key = std::env::var("ZEROCLAW_AUDIT_SIGNING_KEY").ok();
        defer! {
            // Only restore if it was a valid 64-char key
            if let Some(key) = old_key.as_ref().filter(|k| k.len() == 64) {
                // SAFETY: test-only, single-threaded test runner.
                unsafe { std::env::set_var("ZEROCLAW_AUDIT_SIGNING_KEY", key) };
            } else {
                // SAFETY: test-only, single-threaded test runner.
                unsafe { std::env::remove_var("ZEROCLAW_AUDIT_SIGNING_KEY") };
            }
        }

        // 30 bytes = 60 hex chars (not 32 bytes)
        let short_key = "c".repeat(60);
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var("ZEROCLAW_AUDIT_SIGNING_KEY", &short_key) };
        let tmp = TempDir::new()?;
        let config = AuditConfig {
            enabled: true,
            sign_events: true,
            ..Default::default()
        };

        let result = AuditLogger::new(config, tmp.path().to_path_buf());
        assert!(result.is_err());
        if let Err(e) = result {
            let err_msg = e.to_string();
            assert!(err_msg.contains("must be 32 bytes"), "error: {}", err_msg);
        }

        Ok(())
    }

    #[test]
    fn different_keys_produce_different_signatures() -> Result<()> {
        let _guard = ENV_MUTEX.lock().unwrap();
        let old_key = std::env::var("ZEROCLAW_AUDIT_SIGNING_KEY").ok();
        defer! {
            if let Some(key) = old_key {
                // SAFETY: test-only, single-threaded test runner.
                unsafe { std::env::set_var("ZEROCLAW_AUDIT_SIGNING_KEY", key) };
            } else {
                // SAFETY: test-only, single-threaded test runner.
                unsafe { std::env::remove_var("ZEROCLAW_AUDIT_SIGNING_KEY") };
            }
        }

        let _tmp = TempDir::new()?;

        // Compute HMAC manually with key1
        let key1 = "d".repeat(64);
        let key1_bytes = hex::decode(&key1)?;

        // Compute HMAC manually with key2
        let key2 = "e".repeat(64);
        let key2_bytes = hex::decode(&key2)?;

        // Use a fixed entry_hash for testing
        let test_entry_hash = "test_hash_value";

        use hmac::{Hmac, Mac};
        use sha2::Sha256;

        let mut mac1 = Hmac::<Sha256>::new_from_slice(&key1_bytes).unwrap();
        mac1.update(test_entry_hash.as_bytes());
        let sig1 = hex::encode(mac1.finalize().into_bytes());

        let mut mac2 = Hmac::<Sha256>::new_from_slice(&key2_bytes).unwrap();
        mac2.update(test_entry_hash.as_bytes());
        let sig2 = hex::encode(mac2.finalize().into_bytes());

        assert_ne!(
            sig1, sig2,
            "different keys must produce different signatures"
        );

        Ok(())
    }

    #[test]
    fn signature_deterministic_for_same_entry_hash() -> Result<()> {
        let _guard = ENV_MUTEX.lock().unwrap();
        let old_key = std::env::var("ZEROCLAW_AUDIT_SIGNING_KEY").ok();
        defer! {
            if let Some(key) = old_key {
                // SAFETY: test-only, single-threaded test runner.
                unsafe { std::env::set_var("ZEROCLAW_AUDIT_SIGNING_KEY", key) };
            } else {
                // SAFETY: test-only, single-threaded test runner.
                unsafe { std::env::remove_var("ZEROCLAW_AUDIT_SIGNING_KEY") };
            }
        }

        let tmp = TempDir::new()?;
        let test_key = "f".repeat(64);
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var("ZEROCLAW_AUDIT_SIGNING_KEY", &test_key) };

        let config = AuditConfig {
            enabled: true,
            sign_events: true,
            ..Default::default()
        };
        let logger = AuditLogger::new(config, tmp.path().to_path_buf())?;

        // Log two events
        for _ in 0..2 {
            let event = AuditEvent::new(AuditEventType::CommandExecution).with_action(
                "cmd".to_string(),
                "low".to_string(),
                false,
                true,
            );
            logger.log(&event)?;
        }

        let log_path = tmp.path().join("audit.log");
        let content = std::fs::read_to_string(&log_path)?;
        let lines: Vec<&str> = content.lines().collect();
        let event1: AuditEvent = serde_json::from_str(lines[0])?;
        let event2: AuditEvent = serde_json::from_str(lines[1])?;

        // Different entry_hashes due to chaining, so signatures should differ
        assert_ne!(event1.entry_hash, event2.entry_hash);
        assert_ne!(event1.signature, event2.signature);

        // Manually verify determinism by recomputing signature for event1
        use hmac::{Hmac, Mac};
        use sha2::Sha256;
        let key_bytes = hex::decode(&test_key)?;
        let mut mac = Hmac::<Sha256>::new_from_slice(&key_bytes).unwrap();
        mac.update(event1.entry_hash.as_bytes());
        let expected_sig1 = hex::encode(mac.finalize().into_bytes());
        assert_eq!(event1.signature, Some(expected_sig1));

        Ok(())
    }

    #[test]
    fn verify_chain_accepts_mixed_signed_and_unsigned_records() -> Result<()> {
        let _guard = ENV_MUTEX.lock().unwrap();
        let old_key = std::env::var("ZEROCLAW_AUDIT_SIGNING_KEY").ok();
        defer! {
            if let Some(key) = old_key.as_ref().filter(|k| k.len() == 64) {
                // SAFETY: test-only, single-threaded test runner.
                unsafe { std::env::set_var("ZEROCLAW_AUDIT_SIGNING_KEY", key) };
            } else {
                // SAFETY: test-only, single-threaded test runner.
                unsafe { std::env::remove_var("ZEROCLAW_AUDIT_SIGNING_KEY") };
            }
        }

        let tmp = TempDir::new()?;
        let log_path = tmp.path().join("audit.log");
        let test_key = "a1".repeat(32); // 64 hex chars = 32 bytes

        // First logger with sign_events=false (unsigned records)
        {
            // SAFETY: test-only, single-threaded test runner.
            unsafe { std::env::remove_var("ZEROCLAW_AUDIT_SIGNING_KEY") };
            let config = AuditConfig {
                enabled: true,
                sign_events: false,
                ..Default::default()
            };
            let logger = AuditLogger::new(config, tmp.path().to_path_buf())?;
            for i in 0..2 {
                let event = AuditEvent::new(AuditEventType::CommandExecution).with_action(
                    format!("unsigned-{}", i),
                    "low".to_string(),
                    false,
                    true,
                );
                logger.log(&event)?;
            }
        }

        // Second logger with sign_events=true (signed records)
        {
            // SAFETY: test-only, single-threaded test runner.
            unsafe { std::env::set_var("ZEROCLAW_AUDIT_SIGNING_KEY", &test_key) };
            let config = AuditConfig {
                enabled: true,
                sign_events: true,
                ..Default::default()
            };
            let logger = AuditLogger::new(config, tmp.path().to_path_buf())?;
            for i in 0..2 {
                let event = AuditEvent::new(AuditEventType::CommandExecution).with_action(
                    format!("signed-{}", i),
                    "low".to_string(),
                    false,
                    true,
                );
                logger.log(&event)?;
            }
        }

        // Verify the full chain (4 records: 2 unsigned + 2 signed)
        // Set the key in env so verify_chain can check signatures
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var("ZEROCLAW_AUDIT_SIGNING_KEY", &test_key) };
        let count = verify_chain(&log_path)?;
        assert_eq!(count, 4, "should verify all 4 records");

        // Verify that first 2 records have no signature, last 2 have signatures
        let content = std::fs::read_to_string(&log_path)?;
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 4);

        let rec0: AuditEvent = serde_json::from_str(lines[0])?;
        let rec1: AuditEvent = serde_json::from_str(lines[1])?;
        let rec2: AuditEvent = serde_json::from_str(lines[2])?;
        let rec3: AuditEvent = serde_json::from_str(lines[3])?;

        assert!(rec0.signature.is_none(), "first unsigned record");
        assert!(rec1.signature.is_none(), "second unsigned record");
        assert!(rec2.signature.is_some(), "first signed record");
        assert!(rec3.signature.is_some(), "second signed record");

        Ok(())
    }

    // ── CliAuditEntry and argument redaction tests ─────────────────────────────

    #[test]
    fn redact_sensitive_arg_password_flag() {
        assert_eq!(
            redact_sensitive_arg("--password=secret123"),
            "--password=[REDACTED]"
        );
        assert_eq!(
            redact_sensitive_arg("--token=abc123def"),
            "--token=[REDACTED]"
        );
        assert_eq!(
            redact_sensitive_arg("--api-key=sk-12345"),
            "--api-key=[REDACTED]"
        );
    }

    #[test]
    fn redact_sensitive_arg_preserves_safe_args() {
        assert_eq!(redact_sensitive_arg("--verbose"), "--verbose");
        assert_eq!(redact_sensitive_arg("-v"), "-v");
        assert_eq!(redact_sensitive_arg("/path/to/file"), "/path/to/file");
        assert_eq!(redact_sensitive_arg("filename.txt"), "filename.txt");
    }

    #[test]
    fn redact_sensitive_arg_env_var_assignment() {
        assert_eq!(
            redact_sensitive_arg("API_KEY=sk_test_secret"),
            "API_KEY=[REDACTED]"
        );
        assert_eq!(
            redact_sensitive_arg("PASSWORD=hunter2"),
            "PASSWORD=[REDACTED]"
        );
        assert_eq!(
            redact_sensitive_arg("AWS_SECRET_ACCESS_KEY=abc123"),
            "AWS_SECRET_ACCESS_KEY=[REDACTED]"
        );
        // Non-sensitive env var should be preserved
        assert_eq!(redact_sensitive_arg("HOME=/home/user"), "HOME=/home/user");
    }

    #[test]
    fn redact_sensitive_arg_api_key_patterns() {
        // OpenAI-style keys
        assert_eq!(
            redact_sensitive_arg("sk-abc123def456ghi789jkl0"),
            "[REDACTED]"
        );
        // Stripe-style keys
        assert_eq!(
            redact_sensitive_arg("sk_test_1234567890abcdef"),
            "[REDACTED]"
        );
        // GitHub tokens
        assert_eq!(redact_sensitive_arg("ghp_abc123def456ghi789"), "[REDACTED]");
        assert_eq!(redact_sensitive_arg("gho_abc123def456ghi789"), "[REDACTED]");
        // AWS access key ID
        assert_eq!(redact_sensitive_arg("AKIAIOSFODNN7EXAMPLE"), "[REDACTED]");
    }

    #[test]
    fn redact_sensitive_args_positional() {
        let args = vec![
            "git".to_string(),
            "clone".to_string(),
            "-p".to_string(),
            "secret_password".to_string(),
            "https://github.com/example/repo".to_string(),
        ];
        let redacted = redact_sensitive_args(&args);
        assert_eq!(redacted[0], "git");
        assert_eq!(redacted[1], "clone");
        assert_eq!(redacted[2], "-p");
        assert_eq!(redacted[3], "[REDACTED]");
        assert_eq!(redacted[4], "https://github.com/example/repo");
    }

    #[test]
    fn cli_audit_entry_redacts_args_on_creation() {
        let entry = CliAuditEntry::new(
            "test-plugin",
            "curl",
            &[
                "-H".to_string(),
                "Authorization: Bearer sk-abc123def456ghi789jkl0".to_string(),
                "https://api.example.com".to_string(),
            ],
            Some("/tmp".to_string()),
            0,
            100,
            1024,
            false,
            false,
        );

        assert_eq!(entry.plugin_name, "test-plugin");
        assert_eq!(entry.command, "curl");
        assert_eq!(entry.args_redacted[0], "-H");
        // The Bearer token should be redacted
        assert!(entry.args_redacted[1].contains("[REDACTED]"));
        assert_eq!(entry.args_redacted[2], "https://api.example.com");
        assert_eq!(entry.exit_code, 0);
        assert_eq!(entry.duration_ms, 100);
        assert_eq!(entry.output_bytes, 1024);
        assert!(!entry.truncated);
        assert!(!entry.timed_out);
    }

    #[test]
    fn cli_audit_entry_serializes_to_json() {
        let entry = CliAuditEntry::new(
            "my-plugin",
            "ls",
            &["-la".to_string()],
            None,
            0,
            50,
            256,
            false,
            false,
        );

        let json = serde_json::to_string(&entry).expect("serialization should succeed");
        assert!(json.contains("\"plugin_name\":\"my-plugin\""));
        assert!(json.contains("\"command\":\"ls\""));
        assert!(json.contains("\"-la\""));
    }

    #[test]
    fn looks_like_secret_high_entropy_detection() {
        // Random-looking alphanumeric strings should be detected
        assert!(looks_like_secret("xQ7zK9mN4pR2sT6uV8wY0aB3cD5eF7gH"));
        // Short values should not be flagged
        assert!(!looks_like_secret("abc123"));
        // Paths should not be flagged
        assert!(!looks_like_secret("/usr/local/bin/program"));
        // Regular words should not be flagged
        assert!(!looks_like_secret("hello-world-config"));
    }
}
