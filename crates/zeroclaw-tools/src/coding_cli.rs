use async_trait::async_trait;
use std::ffi::{OsStr, OsString};
use std::path::PathBuf;
use std::process::Output;
use std::sync::Arc;
use std::time::Duration;
use tokio::process::Command;

/// Environment variables coding CLI subprocesses may inherit after the
/// executor clears the ambient environment.
///
/// Keep these lists shared so every adapter reconstructs the same base
/// environment without propagating variables that are irrelevant to the host
/// platform.
#[cfg(not(target_os = "windows"))]
const SAFE_ENV_VARS: &[&str] = &[
    "PATH", "HOME", "TERM", "LANG", "LC_ALL", "LC_CTYPE", "USER", "SHELL", "TMPDIR",
];

/// Windows process-startup variables plus the profile locations where coding
/// CLIs discover authentication and user configuration.
#[cfg(target_os = "windows")]
const SAFE_ENV_VARS: &[&str] = &[
    "PATH",
    "PATHEXT",
    "HOME",
    "USERPROFILE",
    "HOMEDRIVE",
    "HOMEPATH",
    "APPDATA",
    "LOCALAPPDATA",
    "SYSTEMROOT",
    "SYSTEMDRIVE",
    "WINDIR",
    "COMSPEC",
    "TEMP",
    "TMP",
    "TERM",
    "LANG",
    "USERNAME",
];

#[derive(Debug, Clone)]
pub struct CodingCliCommand {
    pub program: OsString,
    pub args: Vec<OsString>,
    pub env: Vec<(OsString, OsString)>,
    pub runtime_env_keys: Vec<OsString>,
    pub working_dir: PathBuf,
    pub timeout_secs: u64,
}

impl CodingCliCommand {
    pub fn new(program: impl Into<OsString>, working_dir: PathBuf, timeout_secs: u64) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            env: Vec::new(),
            runtime_env_keys: Vec::new(),
            working_dir,
            timeout_secs,
        }
    }

    pub fn arg(&mut self, arg: impl Into<OsString>) -> &mut Self {
        self.args.push(arg.into());
        self
    }

    pub fn args<I, S>(&mut self, args: I) -> &mut Self
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    pub fn env(&mut self, key: impl Into<OsString>, value: impl Into<OsString>) -> &mut Self {
        self.env.push((key.into(), value.into()));
        self
    }

    pub fn runtime_env_key(&mut self, key: impl Into<OsString>) -> &mut Self {
        let key = key.into();
        if !self.runtime_env_keys.contains(&key) {
            self.runtime_env_keys.push(key);
        }
        self
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CodingCliExecutionError {
    #[error("failed to execute command: {0}")]
    Io(#[from] std::io::Error),
    #[error("command timed out")]
    Timeout,
    #[error("failed to prepare command: {0}")]
    Prepare(#[from] anyhow::Error),
}

#[async_trait]
pub trait CodingCliExecutor: Send + Sync {
    async fn output(&self, command: CodingCliCommand) -> Result<Output, CodingCliExecutionError>;
}

#[derive(Debug, Default)]
pub struct DirectCodingCliExecutor;

impl DirectCodingCliExecutor {
    pub fn shared() -> Arc<dyn CodingCliExecutor> {
        Arc::new(Self)
    }
}

#[async_trait]
impl CodingCliExecutor for DirectCodingCliExecutor {
    async fn output(&self, command: CodingCliCommand) -> Result<Output, CodingCliExecutionError> {
        let mut process = Command::new(host_native_program(&command.program)?);
        process.args(&command.args);
        process.env_clear();
        for (key, value) in command.env {
            process.env(key, value);
        }
        process.current_dir(command.working_dir);
        process.kill_on_drop(true);

        tokio::time::timeout(Duration::from_secs(command.timeout_secs), process.output())
            .await
            .map_err(|_| CodingCliExecutionError::Timeout)?
            .map_err(CodingCliExecutionError::Io)
    }
}

pub fn host_native_program(program: &OsStr) -> Result<OsString, CodingCliExecutionError> {
    if cfg!(target_os = "windows") {
        host_native_windows_program(program)
    } else {
        host_native_non_windows_program(program)
    }
}

fn find_claude_on_path() -> Option<OsString> {
    which::which("claude")
        .ok()
        .map(|path| path.into_os_string())
}

fn missing_claude_error() -> CodingCliExecutionError {
    CodingCliExecutionError::Prepare(anyhow::Error::msg(
        "Claude Code executable 'claude' was not found on PATH before applying the workspace working directory",
    ))
}

fn host_native_non_windows_program(program: &OsStr) -> Result<OsString, CodingCliExecutionError> {
    host_native_non_windows_program_with(program, find_claude_on_path)
}

fn host_native_non_windows_program_with<F>(
    program: &OsStr,
    find_claude: F,
) -> Result<OsString, CodingCliExecutionError>
where
    F: FnOnce() -> Option<OsString>,
{
    match program.to_str() {
        Some("claude") => find_claude().ok_or_else(missing_claude_error),
        _ => Ok(program.to_os_string()),
    }
}

fn host_native_windows_program(program: &OsStr) -> Result<OsString, CodingCliExecutionError> {
    host_native_windows_program_with(program, find_claude_on_path)
}

fn host_native_windows_program_with<F>(
    program: &OsStr,
    find_claude: F,
) -> Result<OsString, CodingCliExecutionError>
where
    F: FnOnce() -> Option<OsString>,
{
    Ok(match program.to_str() {
        Some("codex") => OsString::from("codex.cmd"),
        Some("gemini") => OsString::from("gemini.cmd"),
        Some("claude") => find_claude().unwrap_or_else(|| OsString::from("claude.cmd")),
        _ => program.to_os_string(),
    })
}

pub fn add_safe_env(command: &mut CodingCliCommand, safe_vars: &[&str], passthrough: &[String]) {
    for var in safe_vars {
        if let Ok(val) = std::env::var(var) {
            command.env(*var, val);
        }
    }
    for var in passthrough {
        let trimmed = var.trim();
        if !trimmed.is_empty()
            && let Ok(val) = std::env::var(trimmed)
        {
            command.env(trimmed, val);
            command.runtime_env_key(trimmed);
        }
    }
}

/// Add the canonical base environment plus operator-configured passthrough
/// variables to a coding CLI command.
pub(crate) fn add_coding_cli_env(command: &mut CodingCliCommand, passthrough: &[String]) {
    add_safe_env(command, SAFE_ENV_VARS, passthrough);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claude_code::ClaudeCodeTool;
    use crate::codex_cli::CodexCliTool;
    use crate::gemini_cli::GeminiCliTool;
    use crate::opencode_cli::OpenCodeCliTool;
    use std::sync::Mutex;
    use zeroclaw_api::tool::Tool;
    use zeroclaw_config::autonomy::AutonomyLevel;
    use zeroclaw_config::policy::SecurityPolicy;
    use zeroclaw_config::schema::{
        ClaudeCodeConfig, CodexCliConfig, GeminiCliConfig, OpenCodeCliConfig,
    };

    #[derive(Default)]
    struct RecordingExecutor {
        commands: Mutex<Vec<CodingCliCommand>>,
    }

    #[async_trait]
    impl CodingCliExecutor for RecordingExecutor {
        async fn output(
            &self,
            command: CodingCliCommand,
        ) -> Result<Output, CodingCliExecutionError> {
            self.commands
                .lock()
                .expect("recorded command lock should not be poisoned")
                .push(command);
            Err(CodingCliExecutionError::Timeout)
        }
    }

    #[test]
    fn host_native_windows_program_preserves_known_cli_shims() {
        assert_eq!(
            host_native_windows_program_with(OsStr::new("codex"), || None).unwrap(),
            OsString::from("codex.cmd")
        );
        assert_eq!(
            host_native_windows_program_with(OsStr::new("gemini"), || None).unwrap(),
            OsString::from("gemini.cmd")
        );
        assert_eq!(
            host_native_windows_program_with(OsStr::new("claude"), || None).unwrap(),
            OsString::from("claude.cmd")
        );
        assert_eq!(
            host_native_windows_program_with(OsStr::new("claude"), || Some(OsString::from(
                r"C:\Tools\claude.exe"
            )))
            .unwrap(),
            OsString::from(r"C:\Tools\claude.exe")
        );
        assert_eq!(
            host_native_windows_program_with(OsStr::new("opencode"), || None).unwrap(),
            OsString::from("opencode")
        );
    }

    #[test]
    fn host_native_program_leaves_names_neutral_on_non_windows() {
        if !cfg!(target_os = "windows") {
            assert_eq!(
                host_native_program(OsStr::new("codex")).unwrap(),
                OsString::from("codex")
            );
        }
    }

    #[test]
    fn host_native_non_windows_program_resolves_claude_before_workspace_cwd() {
        assert_eq!(
            host_native_non_windows_program_with(OsStr::new("claude"), || Some(OsString::from(
                "/usr/local/bin/claude"
            )))
            .unwrap(),
            OsString::from("/usr/local/bin/claude")
        );
        let error = host_native_non_windows_program_with(OsStr::new("claude"), || None)
            .expect_err("unresolved Unix Claude path should fail closed");
        assert!(
            error.to_string().contains("was not found on PATH"),
            "unexpected error: {error}"
        );
        assert_eq!(
            host_native_non_windows_program_with(OsStr::new("codex"), || Some(OsString::from(
                "/tmp/claude"
            )))
            .unwrap(),
            OsString::from("codex")
        );
    }

    #[test]
    fn runtime_env_keys_are_explicit_and_deduplicated() {
        let mut command = CodingCliCommand::new("codex", PathBuf::from("."), 5);

        command.env("PATH", "/usr/bin");
        command.runtime_env_key("OPENAI_API_KEY");
        command.runtime_env_key("OPENAI_API_KEY");

        assert_eq!(
            command.env,
            vec![(OsString::from("PATH"), OsString::from("/usr/bin"))]
        );
        assert_eq!(
            command.runtime_env_keys,
            vec![OsString::from("OPENAI_API_KEY")]
        );
    }

    #[tokio::test]
    async fn every_adapter_applies_the_canonical_environment_boundary() {
        let workspace = tempfile::tempdir().expect("temporary workspace");
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Full,
            workspace_dir: workspace.path().to_path_buf(),
            ..SecurityPolicy::default()
        });
        let recorder = Arc::new(RecordingExecutor::default());
        let passthrough_key = if cfg!(target_os = "windows") {
            "PROCESSOR_ARCHITECTURE"
        } else {
            "PWD"
        };
        assert!(
            std::env::var_os(passthrough_key).is_some(),
            "test process should define {passthrough_key}"
        );
        let passthrough = vec![passthrough_key.to_string()];
        let tools: Vec<Box<dyn Tool>> = vec![
            Box::new(ClaudeCodeTool::new_with_executor(
                security.clone(),
                ClaudeCodeConfig {
                    env_passthrough: passthrough.clone(),
                    ..ClaudeCodeConfig::default()
                },
                recorder.clone(),
            )),
            Box::new(CodexCliTool::new_with_executor(
                security.clone(),
                CodexCliConfig {
                    env_passthrough: passthrough.clone(),
                    ..CodexCliConfig::default()
                },
                recorder.clone(),
            )),
            Box::new(GeminiCliTool::new_with_executor(
                security.clone(),
                GeminiCliConfig {
                    env_passthrough: passthrough.clone(),
                    ..GeminiCliConfig::default()
                },
                recorder.clone(),
            )),
            Box::new(OpenCodeCliTool::new_with_executor(
                security,
                OpenCodeCliConfig {
                    env_passthrough: passthrough,
                    ..OpenCodeCliConfig::default()
                },
                recorder.clone(),
            )),
        ];
        for tool in tools {
            tool.execute(serde_json::json!({"prompt": "environment probe"}))
                .await
                .unwrap_or_else(|error| panic!("{} adapter failed: {error}", tool.name()));
        }

        let commands = std::mem::take(
            &mut *recorder
                .commands
                .lock()
                .expect("recorded command lock should not be poisoned"),
        );
        assert_eq!(
            commands
                .iter()
                .map(|command| command.program.clone())
                .collect::<Vec<_>>(),
            ["claude", "codex", "gemini", "opencode"]
                .map(OsString::from)
                .to_vec()
        );

        // Compare names only so assertion failures cannot expose host paths.
        let mut expected_env_keys = SAFE_ENV_VARS
            .iter()
            .filter(|key| std::env::var_os(**key).is_some())
            .map(|key| OsString::from(*key))
            .collect::<std::collections::BTreeSet<_>>();
        expected_env_keys.insert(OsString::from(passthrough_key));

        for command in commands {
            assert_eq!(
                command
                    .env
                    .iter()
                    .map(|(key, _)| key.clone())
                    .collect::<std::collections::BTreeSet<_>>(),
                expected_env_keys,
                "{} must use the canonical coding CLI environment",
                command.program.to_string_lossy()
            );
            assert_eq!(
                command.runtime_env_keys,
                vec![OsString::from(passthrough_key)],
                "{} must keep configured passthrough explicit at runtime boundaries",
                command.program.to_string_lossy()
            );
        }
    }

    #[cfg(target_os = "windows")]
    #[tokio::test]
    async fn direct_executor_runs_windows_child_with_only_allowlisted_environment() {
        let comspec = std::env::var_os("COMSPEC").expect("Windows should define COMSPEC");
        let mut command = CodingCliCommand::new(comspec, std::env::temp_dir(), 10);
        command.args(["/d", "/c", "set"]);
        add_coding_cli_env(&mut command, &[]);

        let expected_keys = command
            .env
            .iter()
            .map(|(key, _)| key.to_string_lossy().to_ascii_uppercase())
            .collect::<std::collections::HashSet<_>>();
        for required_key in ["USERPROFILE", "APPDATA", "LOCALAPPDATA", "SYSTEMROOT"] {
            assert!(
                expected_keys.contains(required_key),
                "Windows test process should define {required_key}"
            );
        }

        let blocked_keys = [
            "PROCESSOR_ARCHITECTURE",
            "COMPUTERNAME",
            "OS",
            "NUMBER_OF_PROCESSORS",
        ]
        .into_iter()
        .filter(|key| std::env::var_os(key).is_some())
        .collect::<Vec<_>>();
        assert!(
            !blocked_keys.is_empty(),
            "Windows test process should define a non-allowlisted variable"
        );

        let output = DirectCodingCliExecutor
            .output(command)
            .await
            .expect("allowlisted environment should launch cmd.exe");
        assert!(
            output.status.success(),
            "cmd.exe environment probe failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );

        // cmd.exe synthesizes variables such as PROMPT, so assert the inherited
        // boundary instead of requiring exact equality with the command input.
        let child_keys = String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| {
                line.split_once('=')
                    .map(|(key, _)| key.to_ascii_uppercase())
            })
            .collect::<std::collections::HashSet<_>>();

        for expected_key in expected_keys {
            assert!(
                child_keys.contains(&expected_key),
                "allowlisted {expected_key} did not reach the Windows child"
            );
        }
        for blocked_key in blocked_keys {
            assert!(
                !child_keys.contains(blocked_key),
                "non-allowlisted {blocked_key} leaked through env_clear()"
            );
        }
    }

    #[test]
    #[cfg(not(target_os = "windows"))]
    fn safe_env_vars_match_the_unix_allowlist() {
        assert_eq!(
            SAFE_ENV_VARS,
            &[
                "PATH", "HOME", "TERM", "LANG", "LC_ALL", "LC_CTYPE", "USER", "SHELL", "TMPDIR",
            ],
            "coding CLI subprocess inheritance must remain an explicit security allowlist"
        );
    }

    #[test]
    #[cfg(target_os = "windows")]
    fn safe_env_vars_match_the_windows_allowlist() {
        assert_eq!(
            SAFE_ENV_VARS,
            &[
                "PATH",
                "PATHEXT",
                "HOME",
                "USERPROFILE",
                "HOMEDRIVE",
                "HOMEPATH",
                "APPDATA",
                "LOCALAPPDATA",
                "SYSTEMROOT",
                "SYSTEMDRIVE",
                "WINDIR",
                "COMSPEC",
                "TEMP",
                "TMP",
                "TERM",
                "LANG",
                "USERNAME",
            ],
            "coding CLI subprocess inheritance must remain an explicit security allowlist"
        );
    }
}
