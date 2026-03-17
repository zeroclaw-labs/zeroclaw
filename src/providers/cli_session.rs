//! CLI session manager for persistent AI CLI sessions.
//!
//! Manages long-lived sessions for CLI tools (Claude Code, Gemini CLI, KiloCLI)
//! via stdin/stdout pipes, allowing multiple prompts to be sent to the same
//! session without restarting the subprocess each time.
//!
//! # Session lifecycle
//!
//! Sessions are created on first use and kept alive until:
//! - The idle timeout expires (no prompts sent within the window)
//! - The maximum lifetime is reached
//! - The session is explicitly killed
//! - The child process crashes (optionally restarted)
//!
//! # Thread safety
//!
//! The [`CliSessionManager`] uses a two-level locking strategy:
//! - An outer `RwLock<HashMap>` protects the session map (short-lived locks).
//! - Each session is wrapped in `Arc<Mutex<CliSession>>` for independent I/O.
//!
//! This avoids holding the map lock during long-running stdin/stdout operations.

use anyhow::{bail, Result};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{Mutex, RwLock};

// ── Configuration ────────────────────────────────────────────────

/// Configuration for the CLI session manager (`[cli_sessions]`).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CliSessionConfig {
    /// Enable session management. When false, no sessions are created.
    #[serde(default)]
    pub enabled: bool,
    /// Maximum concurrent sessions (across all CLI types).
    #[serde(default = "default_max_sessions")]
    pub max_sessions: usize,
    /// Session idle timeout in seconds — kill after inactivity.
    #[serde(default = "default_session_idle_timeout")]
    pub idle_timeout_secs: u64,
    /// Maximum session lifetime in seconds regardless of activity.
    #[serde(default = "default_session_max_lifetime")]
    pub max_lifetime_secs: u64,
    /// Restart a session automatically if the child process crashes.
    #[serde(default = "default_restart_on_crash")]
    pub restart_on_crash: bool,
}

fn default_max_sessions() -> usize {
    3
}
fn default_session_idle_timeout() -> u64 {
    300
}
fn default_session_max_lifetime() -> u64 {
    3600
}
fn default_restart_on_crash() -> bool {
    true
}

impl Default for CliSessionConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            max_sessions: default_max_sessions(),
            idle_timeout_secs: default_session_idle_timeout(),
            max_lifetime_secs: default_session_max_lifetime(),
            restart_on_crash: default_restart_on_crash(),
        }
    }
}

impl CliSessionConfig {
    /// Validate configuration values. Returns an error if any value is out of
    /// acceptable range.
    pub fn validate(&self) -> Result<()> {
        if self.max_sessions < 1 {
            bail!(
                "cli_sessions.max_sessions must be >= 1, got {}",
                self.max_sessions
            );
        }
        if self.idle_timeout_secs < 1 {
            bail!(
                "cli_sessions.idle_timeout_secs must be >= 1, got {}",
                self.idle_timeout_secs
            );
        }
        if self.max_lifetime_secs < 1 {
            bail!(
                "cli_sessions.max_lifetime_secs must be >= 1, got {}",
                self.max_lifetime_secs
            );
        }
        Ok(())
    }
}

// ── Session types ────────────────────────────────────────────────

/// A live CLI session backed by a child process with piped stdin/stdout.
struct CliSession {
    id: String,
    cli_name: String,
    child: Child,
    started_at: chrono::DateTime<chrono::Utc>,
    last_used: chrono::DateTime<chrono::Utc>,
    message_count: u64,
    /// The binary path used to spawn this session (for restart).
    binary: String,
    /// The args used to spawn this session (for restart).
    args: Vec<String>,
}

/// Public session info (no internal handles exposed).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionInfo {
    pub id: String,
    pub cli_name: String,
    pub started_at: String,
    pub last_used: String,
    pub message_count: u64,
}

impl SessionInfo {
    fn from_session(s: &CliSession) -> Self {
        Self {
            id: s.id.clone(),
            cli_name: s.cli_name.clone(),
            started_at: s.started_at.to_rfc3339(),
            last_used: s.last_used.to_rfc3339(),
            message_count: s.message_count,
        }
    }
}

// ── End-of-response sentinel ─────────────────────────────────────

/// Unique sentinel written after each prompt so we can detect response boundaries.
const RESPONSE_SENTINEL: &str = "__ZEROCLAW_CLI_SESSION_EOF__";

/// Maximum time to wait for a single prompt response before giving up.
const PROMPT_RESPONSE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

// ── Manager ──────────────────────────────────────────────────────

/// Manages long-lived CLI sessions with idle/lifetime pruning.
///
/// Sessions are stored as `Arc<Mutex<CliSession>>` so that the outer map lock
/// can be released before performing I/O on a specific session.
pub struct CliSessionManager {
    sessions: Arc<RwLock<HashMap<String, Arc<Mutex<CliSession>>>>>,
    config: CliSessionConfig,
}

impl CliSessionManager {
    /// Create a new manager with the given configuration.
    pub fn new(config: CliSessionConfig) -> Self {
        Self {
            sessions: Arc::new(RwLock::new(HashMap::new())),
            config,
        }
    }

    /// Get or create a session for the given CLI.
    ///
    /// If a live session for `cli_name` already exists, its ID is returned.
    /// Otherwise a new child process is spawned with the given `binary` and `args`.
    ///
    /// The `max_sessions` limit is enforced *before* checking for an existing
    /// session, so it correctly caps the total number of concurrent sessions.
    ///
    /// A single write lock is held for the entire check-then-create operation
    /// to prevent TOCTOU races that could spawn duplicate sessions or exceed
    /// `max_sessions`.
    ///
    /// Returns the session ID on success, or an error if the CLI cannot be
    /// spawned or does not support interactive mode (no stdin pipe).
    pub async fn get_or_create(
        &self,
        cli_name: &str,
        binary: &str,
        args: &[&str],
    ) -> Result<String> {
        let mut sessions = self.sessions.write().await;

        // Check for an existing live session for this CLI first.
        for entry in sessions.values() {
            let session = entry.lock().await;
            if session.cli_name == cli_name {
                return Ok(session.id.clone());
            }
        }

        // Enforce max-sessions limit (total across all CLI types).
        if sessions.len() >= self.config.max_sessions {
            bail!(
                "Maximum concurrent sessions ({}) reached — cannot create new session for CLI '{cli_name}'",
                self.config.max_sessions
            );
        }

        let session = Self::spawn_session(cli_name, binary, args)?;
        let id = session.id.clone();
        sessions.insert(id.clone(), Arc::new(Mutex::new(session)));
        Ok(id)
    }

    /// Send a prompt to an existing session via its stdin pipe and read
    /// the response from stdout until the sentinel line is detected.
    ///
    /// The outer map lock is held only briefly to clone the `Arc<Mutex<CliSession>>`.
    /// The actual I/O operates on the per-session mutex, allowing other sessions
    /// to be accessed concurrently.
    ///
    /// Returns the collected response text (sentinel stripped).
    pub async fn send_prompt(&self, session_id: &str, prompt: &str) -> Result<String> {
        // Brief read lock to grab the Arc handle.
        let session_arc = {
            let sessions = self.sessions.read().await;
            sessions
                .get(session_id)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("Session '{session_id}' not found"))?
        };
        // Map lock is now dropped — operate on the session independently.

        let mut session = session_arc.lock().await;

        // Check the child is still alive.
        if let Some(status) = session.child.try_wait()? {
            let cli = session.cli_name.clone();
            let binary = session.binary.clone();
            let args = session.args.clone();
            drop(session); // release session mutex before acquiring map write lock

            if self.config.restart_on_crash {
                let new_session = Self::spawn_session(
                    &cli,
                    &binary,
                    &args.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
                )?;
                let new_id = new_session.id.clone();
                let mut sessions = self.sessions.write().await;
                sessions.remove(session_id);
                sessions.insert(new_id.clone(), Arc::new(Mutex::new(new_session)));
                bail!(
                    "Session '{session_id}' crashed (exit: {status}). \
                     Restarted as '{new_id}'. Please retry the prompt."
                );
            }
            let mut sessions = self.sessions.write().await;
            sessions.remove(session_id);
            bail!(
                "Session '{session_id}' for CLI '{cli}' exited with status {status} \
                 and restart_on_crash is disabled"
            );
        }

        // Clone cli_name before taking mutable borrows on child's pipes.
        let cli_name = session.cli_name.clone();

        // Take stdin/stdout out of the child to avoid overlapping mutable borrows.
        // They are restored after I/O completes.
        let mut stdin = match session.child.stdin.take() {
            Some(s) => s,
            None => {
                bail!("CLI '{cli_name}' session has no stdin pipe — interactive mode not supported")
            }
        };

        let stdout = match session.child.stdout.take() {
            Some(s) => s,
            None => {
                session.child.stdin = Some(stdin);
                bail!(
                    "CLI '{cli_name}' session has no stdout pipe — interactive mode not supported"
                );
            }
        };

        // Write the prompt with an instruction to emit the sentinel at the end
        // of the response. This works for AI CLIs that interpret stdin as prompt
        // text (rather than shell commands), unlike `echo <sentinel>` which only
        // works in actual shell sessions.
        let payload = format!(
            "{prompt}\n\nAfter your complete response, print exactly this on its own line: {RESPONSE_SENTINEL}\n"
        );
        stdin.write_all(payload.as_bytes()).await?;
        stdin.flush().await?;

        let mut reader = BufReader::new(stdout);
        let mut response_lines: Vec<String> = Vec::new();

        let read_result = tokio::time::timeout(PROMPT_RESPONSE_TIMEOUT, async {
            let mut line = String::new();
            loop {
                line.clear();
                let bytes_read = reader.read_line(&mut line).await?;
                if bytes_read == 0 {
                    // EOF — process likely exited.
                    break;
                }
                let trimmed = line.trim_end();
                if trimmed.contains(RESPONSE_SENTINEL) {
                    break;
                }
                response_lines.push(trimmed.to_string());
            }
            Ok::<(), anyhow::Error>(())
        })
        .await;

        // Restore stdin/stdout back into the child for future prompts.
        session.child.stdin = Some(stdin);
        session.child.stdout = Some(reader.into_inner());

        match read_result {
            Ok(Ok(())) => {}
            Ok(Err(e)) => bail!("Error reading from CLI session stdout: {e}"),
            Err(_) => bail!(
                "Timed out waiting for response from CLI session '{session_id}' after {:?}",
                PROMPT_RESPONSE_TIMEOUT
            ),
        }

        session.last_used = chrono::Utc::now();
        session.message_count += 1;

        Ok(response_lines.join("\n").trim().to_string())
    }

    /// Kill a specific session by ID.
    pub async fn kill_session(&self, session_id: &str) -> Result<()> {
        let session_arc = {
            let mut sessions = self.sessions.write().await;
            sessions
                .remove(session_id)
                .ok_or_else(|| anyhow::anyhow!("Session '{session_id}' not found"))?
        };
        let mut session = session_arc.lock().await;
        let _ = session.child.kill().await;
        Ok(())
    }

    /// Kill all managed sessions.
    pub async fn kill_all(&self) -> Result<()> {
        let drained: Vec<Arc<Mutex<CliSession>>> = {
            let mut sessions = self.sessions.write().await;
            sessions.drain().map(|(_, v)| v).collect()
        };
        for entry in drained {
            let mut session = entry.lock().await;
            let _ = session.child.kill().await;
        }
        Ok(())
    }

    /// Prune sessions that have exceeded idle timeout or max lifetime.
    /// Returns the IDs of pruned sessions.
    pub async fn prune_stale(&self) -> Vec<String> {
        self.prune_stale_at(chrono::Utc::now()).await
    }

    /// Prune stale sessions relative to the given `now` timestamp.
    /// Exposed for deterministic testing.
    async fn prune_stale_at(&self, now: chrono::DateTime<chrono::Utc>) -> Vec<String> {
        let idle_limit = chrono::Duration::seconds(self.config.idle_timeout_secs as i64);
        let lifetime_limit = chrono::Duration::seconds(self.config.max_lifetime_secs as i64);

        // Collect stale IDs under read lock.
        let stale_ids: Vec<String> = {
            let sessions = self.sessions.read().await;
            let mut ids = Vec::new();
            for entry in sessions.values() {
                let s = entry.lock().await;
                let idle = now.signed_duration_since(s.last_used) > idle_limit;
                let expired = now.signed_duration_since(s.started_at) > lifetime_limit;
                if idle || expired {
                    ids.push(s.id.clone());
                }
            }
            ids
        };

        if stale_ids.is_empty() {
            return Vec::new();
        }

        // Remove and kill under write lock.
        let mut pruned = Vec::new();
        let mut sessions = self.sessions.write().await;
        for id in stale_ids {
            if let Some(entry) = sessions.remove(&id) {
                let mut session = entry.lock().await;
                let _ = session.child.kill().await;
                pruned.push(id);
            }
        }

        pruned
    }

    /// List all active sessions (public info only).
    pub async fn list_sessions(&self) -> Vec<SessionInfo> {
        let sessions = self.sessions.read().await;
        let mut infos = Vec::with_capacity(sessions.len());
        for entry in sessions.values() {
            let s = entry.lock().await;
            infos.push(SessionInfo::from_session(&s));
        }
        infos
    }

    /// Return the current config.
    pub fn config(&self) -> &CliSessionConfig {
        &self.config
    }

    // ── Internal helpers ─────────────────────────────────────────

    fn spawn_session(cli_name: &str, binary: &str, args: &[&str]) -> Result<CliSession> {
        let mut cmd = Command::new(binary);
        cmd.args(args);
        cmd.kill_on_drop(true);
        cmd.stdin(std::process::Stdio::piped());
        cmd.stdout(std::process::Stdio::piped());
        // Discard stderr to avoid deadlock: a piped stderr that is never read
        // will block the child once the OS pipe buffer fills (~64 KB on Linux).
        cmd.stderr(std::process::Stdio::null());

        let child = cmd.spawn().map_err(|err| {
            anyhow::anyhow!(
                "Failed to spawn CLI session for '{cli_name}' (binary: {binary}): {err}. \
                 Ensure the binary is installed and in PATH."
            )
        })?;

        let now = chrono::Utc::now();
        Ok(CliSession {
            id: uuid::Uuid::new_v4().to_string(),
            cli_name: cli_name.to_string(),
            child,
            started_at: now,
            last_used: now,
            message_count: 0,
            binary: binary.to_string(),
            args: args.iter().map(|s| s.to_string()).collect(),
        })
    }
}

impl Drop for CliSessionManager {
    fn drop(&mut self) {
        // Best-effort synchronous cleanup: try to kill child processes.
        // Because Drop is sync, we use try_lock and synchronous kill.
        if let Ok(sessions) = self.sessions.try_write() {
            for entry in sessions.values() {
                if let Ok(mut session) = entry.try_lock() {
                    // Child::start_kill is non-async and signals the process to exit.
                    let _ = session.child.start_kill();
                }
            }
        }
    }
}

// ── Tests ────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_defaults_are_sensible() {
        let config = CliSessionConfig::default();
        assert!(!config.enabled);
        assert_eq!(config.max_sessions, 3);
        assert_eq!(config.idle_timeout_secs, 300);
        assert_eq!(config.max_lifetime_secs, 3600);
        assert!(config.restart_on_crash);
    }

    #[test]
    fn config_deserializes_from_empty_table() {
        let config: CliSessionConfig = toml::from_str("").unwrap();
        assert!(!config.enabled);
        assert_eq!(config.max_sessions, 3);
    }

    #[test]
    fn config_deserializes_with_overrides() {
        let toml_str = r#"
enabled = true
max_sessions = 5
idle_timeout_secs = 600
max_lifetime_secs = 7200
restart_on_crash = false
"#;
        let config: CliSessionConfig = toml::from_str(toml_str).unwrap();
        assert!(config.enabled);
        assert_eq!(config.max_sessions, 5);
        assert_eq!(config.idle_timeout_secs, 600);
        assert_eq!(config.max_lifetime_secs, 7200);
        assert!(!config.restart_on_crash);
    }

    #[test]
    fn config_validate_accepts_defaults() {
        let config = CliSessionConfig::default();
        assert!(config.validate().is_ok());
    }

    #[test]
    fn config_validate_rejects_zero_max_sessions() {
        let config = CliSessionConfig {
            max_sessions: 0,
            ..Default::default()
        };
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("max_sessions"), "unexpected error: {err}");
    }

    #[test]
    fn config_validate_rejects_zero_idle_timeout() {
        let config = CliSessionConfig {
            idle_timeout_secs: 0,
            ..Default::default()
        };
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("idle_timeout_secs"), "unexpected error: {err}");
    }

    #[test]
    fn config_validate_rejects_zero_max_lifetime() {
        let config = CliSessionConfig {
            max_lifetime_secs: 0,
            ..Default::default()
        };
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("max_lifetime_secs"), "unexpected error: {err}");
    }

    #[test]
    fn session_info_serializes_to_json() {
        let info = SessionInfo {
            id: "test-id".to_string(),
            cli_name: "claude-code".to_string(),
            started_at: "2025-01-01T00:00:00+00:00".to_string(),
            last_used: "2025-01-01T00:05:00+00:00".to_string(),
            message_count: 3,
        };
        let json = serde_json::to_string(&info).unwrap();
        assert!(json.contains("\"cli_name\":\"claude-code\""));
        assert!(json.contains("\"message_count\":3"));
    }

    #[test]
    fn session_info_roundtrips_through_json() {
        let info = SessionInfo {
            id: "roundtrip-id".to_string(),
            cli_name: "gemini-cli".to_string(),
            started_at: "2025-06-01T12:00:00+00:00".to_string(),
            last_used: "2025-06-01T12:10:00+00:00".to_string(),
            message_count: 7,
        };
        let json = serde_json::to_string(&info).unwrap();
        let recovered: SessionInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(recovered.id, info.id);
        assert_eq!(recovered.cli_name, info.cli_name);
        assert_eq!(recovered.message_count, info.message_count);
    }

    #[tokio::test]
    async fn manager_creation_succeeds() {
        let manager = CliSessionManager::new(CliSessionConfig::default());
        let sessions = manager.list_sessions().await;
        assert!(sessions.is_empty());
    }

    #[tokio::test]
    async fn get_or_create_fails_for_missing_binary() {
        let manager = CliSessionManager::new(CliSessionConfig {
            enabled: true,
            ..Default::default()
        });
        let result = manager
            .get_or_create("test-cli", "/nonexistent/binary/path", &[])
            .await;
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("Failed to spawn CLI session"),
            "unexpected error: {msg}"
        );
    }

    #[tokio::test]
    async fn kill_nonexistent_session_returns_error() {
        let manager = CliSessionManager::new(CliSessionConfig::default());
        let result = manager.kill_session("nonexistent-id").await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }

    #[tokio::test]
    async fn kill_all_on_empty_manager_succeeds() {
        let manager = CliSessionManager::new(CliSessionConfig::default());
        let result = manager.kill_all().await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn prune_stale_on_empty_manager_returns_empty() {
        let manager = CliSessionManager::new(CliSessionConfig::default());
        let pruned = manager.prune_stale().await;
        assert!(pruned.is_empty());
    }

    #[tokio::test]
    async fn send_prompt_to_nonexistent_session_returns_error() {
        let manager = CliSessionManager::new(CliSessionConfig::default());
        let result = manager.send_prompt("no-such-session", "hello").await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn session_lifecycle_with_cat() {
        // Use `cat` as a simple interactive process on Unix.
        let config = CliSessionConfig {
            enabled: true,
            max_sessions: 2,
            idle_timeout_secs: 60,
            max_lifetime_secs: 300,
            restart_on_crash: false,
        };
        let manager = CliSessionManager::new(config);

        // Create a session.
        let id = manager.get_or_create("test-cat", "cat", &[]).await.unwrap();
        assert!(!id.is_empty());

        // Session should appear in list.
        let sessions = manager.list_sessions().await;
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].cli_name, "test-cat");

        // Kill the session.
        manager.kill_session(&id).await.unwrap();
        let sessions = manager.list_sessions().await;
        assert!(sessions.is_empty());
    }

    #[tokio::test]
    async fn prune_stale_removes_expired_sessions() {
        let config = CliSessionConfig {
            enabled: true,
            max_sessions: 5,
            idle_timeout_secs: 10,
            max_lifetime_secs: 20,
            restart_on_crash: false,
        };
        let manager = CliSessionManager::new(config);

        #[cfg(unix)]
        {
            let id = manager
                .get_or_create("prune-test", "cat", &[])
                .await
                .unwrap();

            // Simulate far-future time to trigger expiry.
            let future = chrono::Utc::now() + chrono::Duration::seconds(3700);
            let pruned = manager.prune_stale_at(future).await;
            assert_eq!(pruned.len(), 1);
            assert_eq!(pruned[0], id);

            let sessions = manager.list_sessions().await;
            assert!(sessions.is_empty());
        }

        #[cfg(not(unix))]
        {
            let pruned = manager.prune_stale().await;
            assert!(pruned.is_empty());
        }
    }

    #[tokio::test]
    async fn max_sessions_enforced() {
        let config = CliSessionConfig {
            enabled: true,
            max_sessions: 1,
            ..Default::default()
        };
        #[allow(unused_variables)]
        let manager = CliSessionManager::new(config);

        #[cfg(unix)]
        {
            // First session should succeed.
            let _id1 = manager
                .get_or_create("limit-cli-a", "cat", &[])
                .await
                .unwrap();

            // Second session for a *different* CLI should fail (global cap = 1).
            let result = manager.get_or_create("limit-cli-b", "cat", &[]).await;
            assert!(result.is_err());
            let msg = result.unwrap_err().to_string();
            assert!(
                msg.contains("Maximum concurrent sessions"),
                "unexpected error: {msg}"
            );

            // Same CLI reuses existing session (not blocked by limit).
            let id_reuse = manager
                .get_or_create("limit-cli-a", "cat", &[])
                .await
                .unwrap();
            assert_eq!(_id1, id_reuse);

            manager.kill_all().await.unwrap();
        }
    }
}
