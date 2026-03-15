use super::traits::{Tool, ToolResult};
use crate::runtime::RuntimeAdapter;
use crate::security::policy::ToolOperation;
use crate::security::SecurityPolicy;
use async_trait::async_trait;
use serde_json::json;
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use uuid::Uuid;

const DEFAULT_READ_TIMEOUT_MS: u64 = 200;
const DEFAULT_MAX_OUTPUT_BYTES: usize = 8192;
const DEFAULT_SESSION_IDLE_SECS: u64 = 900;
const MAX_READ_TIMEOUT_MS: u64 = 5_000;
const MAX_SESSION_OUTPUT_BYTES: usize = 1_048_576;
const PROMPT_MARKER: &str = "__ZEROCLAW_TTY_PROMPT__# ";
const SAFE_ENV_VARS: &[&str] = &[
    "PATH", "HOME", "TERM", "LANG", "LC_ALL", "LC_CTYPE", "USER", "SHELL", "TMPDIR",
];

pub struct TtyShellTool {
    security: Arc<SecurityPolicy>,
    runtime: Arc<dyn RuntimeAdapter>,
    #[cfg(target_os = "linux")]
    sessions: Arc<Mutex<std::collections::HashMap<String, linux_impl::InteractiveSession>>>,
}

impl TtyShellTool {
    pub fn new(security: Arc<SecurityPolicy>, runtime: Arc<dyn RuntimeAdapter>) -> Self {
        Self {
            security,
            runtime,
            #[cfg(target_os = "linux")]
            sessions: Arc::new(Mutex::new(std::collections::HashMap::new())),
        }
    }
}

fn is_valid_env_var_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) if first.is_ascii_alphabetic() || first == '_' => {}
        _ => return false,
    }
    chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

fn collect_allowed_shell_env_vars(security: &SecurityPolicy) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for key in SAFE_ENV_VARS
        .iter()
        .copied()
        .chain(security.shell_env_passthrough.iter().map(|s| s.as_str()))
    {
        let candidate = key.trim();
        if candidate.is_empty() || !is_valid_env_var_name(candidate) {
            continue;
        }
        if seen.insert(candidate.to_string()) {
            out.push(candidate.to_string());
        }
    }
    out
}

#[cfg(target_os = "linux")]
mod linux_impl {
    use super::{
        collect_allowed_shell_env_vars, json, Arc, AtomicBool, Duration, Instant, Mutex, Ordering,
        StdMutex, ToolResult, TtyShellTool, Uuid, DEFAULT_MAX_OUTPUT_BYTES,
        DEFAULT_READ_TIMEOUT_MS, DEFAULT_SESSION_IDLE_SECS, MAX_SESSION_OUTPUT_BYTES,
        PROMPT_MARKER,
    };
    use anyhow::Context;
    use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
    use std::io::{Read, Write};

    pub(super) struct InteractiveSession {
        pub master: Box<dyn MasterPty + Send>,
        pub writer: Box<dyn Write + Send>,
        pub child: Option<Box<dyn Child + Send + Sync>>,
        pub output_buffer: Arc<StdMutex<Vec<u8>>>,
        pub stream_closed: Arc<AtomicBool>,
        pub last_input: Option<String>,
        pub last_active: Instant,
    }

    pub(super) async fn cleanup_expired(
        sessions: &Arc<Mutex<std::collections::HashMap<String, InteractiveSession>>>,
    ) {
        let now = Instant::now();
        let idle_limit = Duration::from_secs(DEFAULT_SESSION_IDLE_SECS);
        let mut expired_children: Vec<Box<dyn Child + Send + Sync>> = Vec::new();

        {
            let mut locked = sessions.lock().await;
            let expired_ids = locked
                .iter()
                .filter(|(_, session)| now.duration_since(session.last_active) > idle_limit)
                .map(|(id, _)| id.clone())
                .collect::<Vec<_>>();

            for id in expired_ids {
                if let Some(mut session) = locked.remove(&id) {
                    if let Some(child) = session.child.take() {
                        expired_children.push(child);
                    }
                }
            }
        }

        for mut child in expired_children {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    pub(super) async fn start_session(
        tool: &TtyShellTool,
        command: Option<&str>,
    ) -> anyhow::Result<ToolResult> {
        if tool.runtime.name() != "native" {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("tty_shell currently supports only the native Linux runtime".into()),
            });
        }

        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows: 24,
                cols: 120,
                pixel_width: 0,
                pixel_height: 0,
            })
            .context("Failed to allocate pseudo-terminal")?;

        let mut builder = CommandBuilder::new("sh");
        match command.map(str::trim).filter(|value| !value.is_empty()) {
            Some(command) => {
                builder.arg("-lc");
                builder.arg(format!("{command}\nexec sh -i"));
            }
            None => {
                builder.arg("-i");
            }
        }
        builder.cwd(&tool.security.workspace_dir);

        for var in collect_allowed_shell_env_vars(&tool.security) {
            match std::env::var(&var) {
                Ok(val) => {
                    builder.env(&var, &val);
                }
                Err(_) if var == "TERM" => {
                    builder.env("TERM", "xterm-256color");
                }
                Err(_) => {}
            }
        }
        builder.env("PS1", PROMPT_MARKER);

        let child = pair
            .slave
            .spawn_command(builder)
            .context("Failed to spawn tty session")?;
        let mut reader = pair.master.try_clone_reader()?;
        let writer = pair.master.take_writer()?;
        let session_id = Uuid::new_v4().to_string();
        let output_buffer = Arc::new(StdMutex::new(Vec::new()));
        let stream_closed = Arc::new(AtomicBool::new(false));

        {
            let reader_buffer = output_buffer.clone();
            let reader_closed = stream_closed.clone();
            std::thread::spawn(move || {
                let mut buf = vec![0_u8; DEFAULT_MAX_OUTPUT_BYTES];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => {
                            let stripped = strip_ansi_escapes::strip(&buf[..n]);
                            let mut locked = match reader_buffer.lock() {
                                Ok(locked) => locked,
                                Err(_) => break,
                            };

                            let remaining = MAX_SESSION_OUTPUT_BYTES.saturating_sub(locked.len());
                            if remaining == 0 {
                                continue;
                            }

                            let to_copy = stripped.len().min(remaining);
                            locked.extend_from_slice(&stripped[..to_copy]);
                        }
                        Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
                        Err(err) if err.raw_os_error() == Some(libc::EIO) => break,
                        Err(_) => break,
                    }
                }

                reader_closed.store(true, Ordering::Release);
            });
        }

        let mut sessions = tool.sessions.lock().await;
        sessions.insert(
            session_id.clone(),
            InteractiveSession {
                master: pair.master,
                writer: Box::new(writer),
                child: Some(child),
                output_buffer,
                stream_closed,
                last_input: None,
                last_active: Instant::now(),
            },
        );

        Ok(ToolResult {
            success: true,
            output: serde_json::to_string(&json!({
                "status": "started",
                "session_id": session_id,
            }))?,
            error: None,
        })
    }

    pub(super) async fn send_input(
        tool: &TtyShellTool,
        session_id: &str,
        input: &str,
    ) -> anyhow::Result<ToolResult> {
        let mut sessions = tool.sessions.lock().await;
        let Some(session) = sessions.get_mut(session_id) else {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("TTY session not found: {session_id}")),
            });
        };

        session.last_active = Instant::now();
        clear_output_buffer(session);
        session.last_input = Some(input.to_string());
        session
            .writer
            .write_all(input.as_bytes())
            .context("Failed to write tty input")?;
        session
            .writer
            .flush()
            .context("Failed to flush tty input")?;

        Ok(ToolResult {
            success: true,
            output: serde_json::to_string(&json!({
                "status": "sent",
                "session_id": session_id,
            }))?,
            error: None,
        })
    }

    pub(super) async fn read_output(
        tool: &TtyShellTool,
        session_id: &str,
        timeout_ms: u64,
        max_output: usize,
    ) -> anyhow::Result<ToolResult> {
        let mut sessions = tool.sessions.lock().await;
        let Some(session) = sessions.get_mut(session_id) else {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("TTY session not found: {session_id}")),
            });
        };

        session.last_active = Instant::now();
        let output = collect_output(session, timeout_ms, max_output)?;

        Ok(ToolResult {
            success: true,
            output: serde_json::to_string(&json!({
                "status": "read",
                "session_id": session_id,
                "output": output,
            }))?,
            error: None,
        })
    }

    pub(super) async fn close_session(
        tool: &TtyShellTool,
        session_id: &str,
    ) -> anyhow::Result<ToolResult> {
        let mut sessions = tool.sessions.lock().await;
        let Some(mut session) = sessions.remove(session_id) else {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("TTY session not found: {session_id}")),
            });
        };

        if let Some(mut child) = session.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }

        Ok(ToolResult {
            success: true,
            output: serde_json::to_string(&json!({
                "status": "closed",
                "session_id": session_id,
            }))?,
            error: None,
        })
    }

    fn collect_output(
        session: &mut InteractiveSession,
        timeout_ms: u64,
        max_output: usize,
    ) -> anyhow::Result<String> {
        let deadline =
            Instant::now() + Duration::from_millis(timeout_ms.max(DEFAULT_READ_TIMEOUT_MS));
        let mut output = String::new();
        let mut empty_rounds = 0_u8;
        let mut saw_meaningful_output = false;

        while Instant::now() < deadline && output.len() < max_output {
            let chunk = take_output_chunk(session, max_output.saturating_sub(output.len()));
            let filtered = sanitize_chunk(session, &chunk);

            if !filtered.trim().is_empty() {
                output.push_str(&filtered);
                empty_rounds = 0;
                saw_meaningful_output = true;
            } else if chunk.is_empty() {
                empty_rounds = empty_rounds.saturating_add(1);
                if saw_meaningful_output && empty_rounds >= 2 {
                    break;
                }
            }

            if chunk.is_empty() {
                if session.stream_closed.load(Ordering::Acquire) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(120));
            }
        }

        Ok(output)
    }

    fn sanitize_chunk(session: &mut InteractiveSession, chunk: &str) -> String {
        let without_prompt = chunk.replace(PROMPT_MARKER, "");
        let Some(last_input) = session.last_input.as_ref() else {
            return normalize_chunk(&without_prompt);
        };
        let last_line = last_input.trim().trim_end_matches('\n');
        if last_line.is_empty() {
            return normalize_chunk(&without_prompt);
        }

        without_prompt
            .lines()
            .filter_map(|line| {
                let trimmed = line.trim();
                if trimmed.is_empty() || trimmed == last_line || is_prompt_line(trimmed) {
                    None
                } else {
                    Some(trimmed.to_string())
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn normalize_chunk(chunk: &str) -> String {
        chunk
            .lines()
            .filter_map(|line| {
                let trimmed = line.trim();
                if trimmed.is_empty() || is_prompt_line(trimmed) {
                    None
                } else {
                    Some(trimmed.to_string())
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn is_prompt_line(line: &str) -> bool {
        line == PROMPT_MARKER.trim()
            || (!line.contains(' ')
                && (line.ends_with('$') || line.ends_with('#') || line.ends_with('>')))
    }

    fn clear_output_buffer(session: &mut InteractiveSession) {
        if let Ok(mut locked) = session.output_buffer.lock() {
            locked.clear();
        }
    }

    fn take_output_chunk(session: &mut InteractiveSession, max_output: usize) -> String {
        let mut locked = match session.output_buffer.lock() {
            Ok(locked) => locked,
            Err(_) => return String::new(),
        };

        if locked.is_empty() {
            return String::new();
        }

        let to_take = locked.len().min(max_output.max(1));
        let bytes = locked.drain(..to_take).collect::<Vec<_>>();
        String::from_utf8_lossy(&bytes).to_string()
    }
}

#[async_trait]
impl Tool for TtyShellTool {
    fn name(&self) -> &str {
        "tty_shell"
    }

    fn description(&self) -> &str {
        "Manage a Linux pseudo-terminal session with start, send, read, and close actions for interactive terminal programs"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["start", "send", "read", "close"],
                    "description": "TTY session action to perform"
                },
                "session_id": {
                    "type": "string",
                    "description": "Session identifier returned by the start action"
                },
                "command": {
                    "type": "string",
                    "description": "Optional initial shell command to run when action=start; if omitted, starts an interactive shell session"
                },
                "input": {
                    "type": "string",
                    "description": "Raw text sent to the TTY when action=send"
                },
                "timeout_ms": {
                    "type": "integer",
                    "description": "Read timeout in milliseconds for action=read",
                    "minimum": 50,
                    "maximum": MAX_READ_TIMEOUT_MS,
                    "default": DEFAULT_READ_TIMEOUT_MS
                },
                "max_output": {
                    "type": "integer",
                    "description": "Maximum bytes returned by action=read",
                    "minimum": 1,
                    "maximum": MAX_SESSION_OUTPUT_BYTES,
                    "default": DEFAULT_MAX_OUTPUT_BYTES
                },
                "approved": {
                    "type": "boolean",
                    "description": "Set true to explicitly approve medium/high-risk start commands in supervised mode",
                    "default": false
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

        #[cfg(target_os = "linux")]
        linux_impl::cleanup_expired(&self.sessions).await;

        match action {
            "start" => {
                let command = args.get("command").and_then(|v| v.as_str());
                let approved = args
                    .get("approved")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);

                if let Err(error) = self
                    .security
                    .enforce_tool_operation(ToolOperation::Act, "tty_shell")
                {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(error),
                    });
                }

                if let Some(command) = command.filter(|value| !value.trim().is_empty()) {
                    match self.security.validate_command_execution(command, approved) {
                        Ok(_) => {}
                        Err(reason) => {
                            return Ok(ToolResult {
                                success: false,
                                output: String::new(),
                                error: Some(reason),
                            });
                        }
                    }

                    if let Some(path) = self.security.forbidden_path_argument(command) {
                        return Ok(ToolResult {
                            success: false,
                            output: String::new(),
                            error: Some(format!("Path blocked by security policy: {path}")),
                        });
                    }
                }

                if !self.runtime.has_shell_access() {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some("Current runtime does not support shell access".into()),
                    });
                }

                #[cfg(target_os = "linux")]
                {
                    linux_impl::start_session(self, command).await
                }

                #[cfg(not(target_os = "linux"))]
                {
                    Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some("tty_shell is currently implemented only on Linux".into()),
                    })
                }
            }
            "send" => {
                let session_id =
                    args.get("session_id")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| {
                            anyhow::anyhow!("Missing 'session_id' parameter for send action")
                        })?;
                let input = args
                    .get("input")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("Missing 'input' parameter for send action"))?;

                if let Err(error) = self
                    .security
                    .enforce_tool_operation(ToolOperation::Act, "tty_shell")
                {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(error),
                    });
                }

                #[cfg(target_os = "linux")]
                {
                    linux_impl::send_input(self, session_id, input).await
                }

                #[cfg(not(target_os = "linux"))]
                {
                    let _ = (session_id, input);
                    Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some("tty_shell is currently implemented only on Linux".into()),
                    })
                }
            }
            "read" => {
                let session_id =
                    args.get("session_id")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| {
                            anyhow::anyhow!("Missing 'session_id' parameter for read action")
                        })?;
                let timeout_ms = args
                    .get("timeout_ms")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(DEFAULT_READ_TIMEOUT_MS)
                    .clamp(50, MAX_READ_TIMEOUT_MS);
                let max_output = args
                    .get("max_output")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(u64::try_from(DEFAULT_MAX_OUTPUT_BYTES).unwrap_or(u64::MAX))
                    .clamp(
                        1,
                        u64::try_from(MAX_SESSION_OUTPUT_BYTES).unwrap_or(u64::MAX),
                    );
                let max_output = usize::try_from(max_output).unwrap_or(MAX_SESSION_OUTPUT_BYTES);

                #[cfg(target_os = "linux")]
                {
                    linux_impl::read_output(self, session_id, timeout_ms, max_output).await
                }

                #[cfg(not(target_os = "linux"))]
                {
                    let _ = (session_id, timeout_ms, max_output);
                    Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some("tty_shell is currently implemented only on Linux".into()),
                    })
                }
            }
            "close" => {
                let session_id =
                    args.get("session_id")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| {
                            anyhow::anyhow!("Missing 'session_id' parameter for close action")
                        })?;

                if let Err(error) = self
                    .security
                    .enforce_tool_operation(ToolOperation::Act, "tty_shell")
                {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(error),
                    });
                }

                #[cfg(target_os = "linux")]
                {
                    linux_impl::close_session(self, session_id).await
                }

                #[cfg(not(target_os = "linux"))]
                {
                    let _ = session_id;
                    Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some("tty_shell is currently implemented only on Linux".into()),
                    })
                }
            }
            _ => anyhow::bail!("Unsupported tty_shell action: {action}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::{NativeRuntime, RuntimeAdapter};
    use crate::security::AutonomyLevel;

    fn test_runtime() -> Arc<dyn RuntimeAdapter> {
        Arc::new(NativeRuntime::new())
    }

    struct DummyRuntime {
        name: &'static str,
    }

    impl RuntimeAdapter for DummyRuntime {
        fn name(&self) -> &str {
            self.name
        }

        fn has_shell_access(&self) -> bool {
            true
        }

        fn has_filesystem_access(&self) -> bool {
            true
        }

        fn storage_path(&self) -> std::path::PathBuf {
            std::env::temp_dir()
        }

        fn supports_long_running(&self) -> bool {
            false
        }

        fn build_shell_command(
            &self,
            command: &str,
            workspace_dir: &std::path::Path,
        ) -> anyhow::Result<tokio::process::Command> {
            let mut process = tokio::process::Command::new("sh");
            process.arg("-c").arg(command).current_dir(workspace_dir);
            Ok(process)
        }
    }

    #[test]
    fn tty_shell_tool_name() {
        let tool = TtyShellTool::new(Arc::new(SecurityPolicy::default()), test_runtime());
        assert_eq!(tool.name(), "tty_shell");
    }

    #[test]
    fn tty_shell_tool_schema_has_actions() {
        let tool = TtyShellTool::new(Arc::new(SecurityPolicy::default()), test_runtime());
        let schema = tool.parameters_schema();
        assert!(schema["properties"]["action"].is_object());
        assert!(schema["properties"]["session_id"].is_object());
        assert!(schema["properties"]["command"].is_object());
    }

    #[tokio::test]
    async fn tty_shell_rejects_non_native_runtime() {
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Full,
            allowed_commands: vec!["echo".into()],
            workspace_dir: std::env::temp_dir(),
            ..SecurityPolicy::default()
        });
        let tool = TtyShellTool::new(security, Arc::new(DummyRuntime { name: "docker" }));

        let result = tool
            .execute(json!({"action": "start", "command": "echo hello"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result
            .error
            .as_deref()
            .unwrap_or("")
            .contains("native Linux runtime"));
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn tty_shell_read_missing_session_fails() {
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Full,
            workspace_dir: std::env::temp_dir(),
            ..SecurityPolicy::default()
        });
        let tool = TtyShellTool::new(security, test_runtime());

        let read = tool
            .execute(json!({"action": "read", "session_id": "missing"}))
            .await
            .unwrap();
        assert!(!read.success);
        assert!(read.error.as_deref().unwrap_or("").contains("not found"));
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn tty_shell_start_without_command_is_allowed() {
        let tool = TtyShellTool::new(Arc::new(SecurityPolicy::default()), test_runtime());
        let result = tool.execute(json!({"action": "start"})).await.unwrap();
        assert!(
            result.success,
            "unexpected start failure: {:?}",
            result.error
        );

        let start_json: serde_json::Value = serde_json::from_str(&result.output).unwrap();
        let session_id = start_json["session_id"].as_str().unwrap();
        let close = tool
            .execute(json!({"action": "close", "session_id": session_id}))
            .await
            .unwrap();
        assert!(close.success, "close error: {:?}", close.error);
    }
}
