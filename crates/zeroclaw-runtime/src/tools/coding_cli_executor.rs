use crate::platform::RuntimeAdapter;
use crate::security::traits::Sandbox;
use async_trait::async_trait;
use std::ffi::{OsStr, OsString};
use std::process::Output;
use std::sync::Arc;
use std::time::Duration;
use zeroclaw_tools::coding_cli::{
    CodingCliCommand, CodingCliExecutionError, CodingCliExecutor, host_native_program,
};

pub(crate) struct RuntimeCodingCliExecutor {
    runtime: Arc<dyn RuntimeAdapter>,
    sandbox: Arc<dyn Sandbox>,
    use_native_argv: bool,
}

impl RuntimeCodingCliExecutor {
    pub(crate) fn shared(
        runtime: Arc<dyn RuntimeAdapter>,
        sandbox: Arc<dyn Sandbox>,
        use_native_argv: bool,
    ) -> Arc<dyn CodingCliExecutor> {
        Arc::new(Self {
            runtime,
            sandbox,
            use_native_argv,
        })
    }
}

#[async_trait]
impl CodingCliExecutor for RuntimeCodingCliExecutor {
    async fn output(&self, command: CodingCliCommand) -> Result<Output, CodingCliExecutionError> {
        if let Some(reason) = self.sandbox.coding_cli_unsupported_reason() {
            let backend = self.sandbox.name();
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "sandbox": backend,
                        "reason": reason,
                    })),
                "coding CLI execution rejected because sandbox backend is unsupported"
            );
            return Err(CodingCliExecutionError::Prepare(anyhow::Error::msg(
                format!("sandbox backend '{backend}' cannot run coding CLI tools: {reason}"),
            )));
        }

        let timeout_secs = command.timeout_secs;
        let mut process = if self.use_native_argv {
            native_command(&command)?
        } else {
            let env_keys: Vec<&OsStr> = command
                .runtime_env_keys
                .iter()
                .map(|key| key.as_os_str())
                .collect();
            self.runtime
                .build_shell_command_with_env_keys(
                    &shell_command(&command),
                    &command.working_dir,
                    &env_keys,
                )
                .map_err(CodingCliExecutionError::Prepare)?
        };

        self.sandbox
            .wrap_command(process.as_std_mut())
            .map_err(|error| CodingCliExecutionError::Prepare(error.into()))?;
        process.current_dir(&command.working_dir);

        let runtime_sandbox_env = command_env_snapshot(&process);
        process.env_clear();
        for (key, value) in runtime_sandbox_env {
            match value {
                Some(value) => {
                    process.env(key, value);
                }
                None => {
                    process.env_remove(key);
                }
            }
        }
        for (key, value) in command.env {
            process.env(key, value);
        }
        process.kill_on_drop(true);

        tokio::time::timeout(Duration::from_secs(timeout_secs), process.output())
            .await
            .map_err(|_| CodingCliExecutionError::Timeout)?
            .map_err(CodingCliExecutionError::Io)
    }
}

fn command_env_snapshot(process: &tokio::process::Command) -> Vec<(OsString, Option<OsString>)> {
    process
        .as_std()
        .get_envs()
        .map(|(key, value)| (key.to_os_string(), value.map(|value| value.to_os_string())))
        .collect()
}

fn native_command(
    command: &CodingCliCommand,
) -> Result<tokio::process::Command, CodingCliExecutionError> {
    let mut process = tokio::process::Command::new(host_native_program(&command.program)?);
    process.args(&command.args);
    process.current_dir(&command.working_dir);
    Ok(process)
}

fn shell_command(command: &CodingCliCommand) -> String {
    std::iter::once(command.program.as_os_str())
        .chain(command.args.iter().map(|arg| arg.as_os_str()))
        .map(shell_escape)
        .collect::<Vec<_>>()
        .join(" ")
}

fn shell_escape(value: &std::ffi::OsStr) -> String {
    let value = value.to_string_lossy();
    if value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/' | ':' | '=' | '+'))
    {
        value.into_owned()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::SecurityPolicy;
    use crate::security::traits::Sandbox;
    use serde_json::json;
    use std::path::PathBuf;
    use std::sync::Mutex;
    use zeroclaw_api::runtime_traits::RuntimeAdapter;
    use zeroclaw_api::tool::Tool;
    use zeroclaw_config::autonomy::AutonomyLevel;
    use zeroclaw_config::schema::CodexCliConfig;
    use zeroclaw_tools::codex_cli::CodexCliTool;

    #[test]
    fn shell_command_uses_posix_quoting_for_shell_runtimes() {
        let mut command = CodingCliCommand::new("codex", PathBuf::from("."), 1);
        command.args(["exec", "hello world", "it's safe; really"]);

        let rendered = shell_command(&command);
        assert_eq!(rendered, "codex exec 'hello world' 'it'\\''s safe; really'");
    }

    #[cfg(not(target_os = "windows"))]
    struct FakeRuntime {
        seen_command: Arc<Mutex<Option<String>>>,
    }

    #[cfg(not(target_os = "windows"))]
    impl RuntimeAdapter for FakeRuntime {
        fn name(&self) -> &str {
            "fake-runtime"
        }

        fn has_shell_access(&self) -> bool {
            true
        }

        fn has_filesystem_access(&self) -> bool {
            true
        }

        fn storage_path(&self) -> PathBuf {
            PathBuf::from("/tmp/fake-runtime")
        }

        fn supports_long_running(&self) -> bool {
            true
        }

        fn build_shell_command(
            &self,
            command: &str,
            workspace_dir: &std::path::Path,
        ) -> anyhow::Result<tokio::process::Command> {
            *self.seen_command.lock().expect("fake runtime mutex") = Some(command.to_string());
            let mut process = tokio::process::Command::new("/bin/sh");
            process
                .args([
                    "-c",
                    "printf '%s:%s:%s' \"$ZC_RUNTIME_SENTINEL\" \"$ZC_SANDBOX_SENTINEL\" \"$1\"",
                    "zc-runtime",
                ])
                .env("ZC_RUNTIME_SENTINEL", "runtime")
                .current_dir(workspace_dir);
            Ok(process)
        }
    }

    #[cfg(not(target_os = "windows"))]
    struct FakeSandbox;

    #[cfg(not(target_os = "windows"))]
    impl Sandbox for FakeSandbox {
        fn wrap_command(&self, cmd: &mut std::process::Command) -> std::io::Result<()> {
            cmd.env("ZC_SANDBOX_SENTINEL", "sandbox");
            cmd.arg("sandboxed");
            Ok(())
        }

        fn is_available(&self) -> bool {
            true
        }

        fn name(&self) -> &str {
            "fake-sandbox"
        }

        fn description(&self) -> &str {
            "test sandbox"
        }
    }

    #[cfg(not(target_os = "windows"))]
    struct NoopSandbox;

    #[cfg(not(target_os = "windows"))]
    impl Sandbox for NoopSandbox {
        fn wrap_command(&self, _cmd: &mut std::process::Command) -> std::io::Result<()> {
            Ok(())
        }

        fn is_available(&self) -> bool {
            true
        }

        fn name(&self) -> &str {
            "noop-sandbox"
        }

        fn description(&self) -> &str {
            "test sandbox"
        }
    }

    #[cfg(not(target_os = "windows"))]
    struct UnsupportedSandbox;

    #[cfg(not(target_os = "windows"))]
    impl Sandbox for UnsupportedSandbox {
        fn wrap_command(&self, _cmd: &mut std::process::Command) -> std::io::Result<()> {
            panic!("unsupported coding CLI sandbox should not wrap commands")
        }

        fn is_available(&self) -> bool {
            true
        }

        fn name(&self) -> &str {
            "unsupported-sandbox"
        }

        fn description(&self) -> &str {
            "test unsupported sandbox"
        }

        fn coding_cli_unsupported_reason(&self) -> Option<&'static str> {
            Some("test unsupported sandbox")
        }
    }

    #[cfg(not(target_os = "windows"))]
    struct ReplacingPwdSandbox;

    #[cfg(not(target_os = "windows"))]
    impl Sandbox for ReplacingPwdSandbox {
        fn wrap_command(&self, cmd: &mut std::process::Command) -> std::io::Result<()> {
            *cmd = std::process::Command::new("/bin/pwd");
            Ok(())
        }

        fn is_available(&self) -> bool {
            true
        }

        fn name(&self) -> &str {
            "replacing-pwd-sandbox"
        }

        fn description(&self) -> &str {
            "test sandbox"
        }
    }

    #[cfg(not(target_os = "windows"))]
    struct EnvForwardingRuntime {
        seen_env_keys: Arc<Mutex<Vec<OsString>>>,
    }

    #[cfg(not(target_os = "windows"))]
    impl RuntimeAdapter for EnvForwardingRuntime {
        fn name(&self) -> &str {
            "env-forwarding-runtime"
        }

        fn has_shell_access(&self) -> bool {
            true
        }

        fn has_filesystem_access(&self) -> bool {
            true
        }

        fn storage_path(&self) -> PathBuf {
            PathBuf::from("/tmp/env-forwarding-runtime")
        }

        fn supports_long_running(&self) -> bool {
            true
        }

        fn build_shell_command(
            &self,
            command: &str,
            workspace_dir: &std::path::Path,
        ) -> anyhow::Result<tokio::process::Command> {
            self.build_shell_command_with_env_keys(command, workspace_dir, &[])
        }

        fn build_shell_command_with_env_keys(
            &self,
            _command: &str,
            workspace_dir: &std::path::Path,
            env_keys: &[&OsStr],
        ) -> anyhow::Result<tokio::process::Command> {
            let mut seen_env_keys = self
                .seen_env_keys
                .lock()
                .expect("env-forwarding runtime mutex");
            *seen_env_keys = env_keys.iter().map(|key| key.to_os_string()).collect();
            let mut process = tokio::process::Command::new("/bin/sh");
            process
                .args(["-c", "printf '%s' \"$ZC_CLI_TOKEN\""])
                .current_dir(workspace_dir);
            Ok(process)
        }
    }

    #[tokio::test]
    #[cfg(not(target_os = "windows"))]
    async fn codex_cli_uses_runtime_and_sandbox_executor() {
        let workspace = tempfile::TempDir::new().expect("temp workspace");
        let seen_command = Arc::new(Mutex::new(None));
        let runtime = Arc::new(FakeRuntime {
            seen_command: Arc::clone(&seen_command),
        });
        let executor = RuntimeCodingCliExecutor::shared(runtime, Arc::new(FakeSandbox), false);
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Full,
            workspace_dir: workspace.path().to_path_buf(),
            ..SecurityPolicy::default()
        });
        let tool = CodexCliTool::new_with_executor(
            security,
            CodexCliConfig {
                timeout_secs: 5,
                ..CodexCliConfig::default()
            },
            executor,
        );

        let result = tool
            .execute(json!({"prompt": "prove runtime boundary"}))
            .await
            .expect("codex_cli should return a tool result");

        assert!(result.success, "unexpected error: {:?}", result.error);
        assert_eq!(result.output.trim(), "runtime:sandbox:sandboxed");
        let command = seen_command
            .lock()
            .expect("fake runtime mutex")
            .clone()
            .expect("runtime should receive the coding CLI command");
        assert!(command.contains("codex"), "command was {command:?}");
        assert!(command.contains("exec"), "command was {command:?}");
        assert!(
            command.contains("prove runtime boundary"),
            "command was {command:?}"
        );
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn native_runtime_executes_argv_without_shell_interpretation() {
        let workspace = tempfile::TempDir::new().expect("temp workspace");
        let runtime = Arc::new(FakeRuntime {
            seen_command: Arc::new(Mutex::new(None)),
        });
        let executor = RuntimeCodingCliExecutor::shared(runtime, Arc::new(FakeSandbox), true);
        let mut command = CodingCliCommand::new("/bin/echo", workspace.path().to_path_buf(), 5);
        command.arg("hello; exit 7");

        let output = executor
            .output(command)
            .await
            .expect("native argv command should execute");

        assert!(output.status.success());
        assert_eq!(
            String::from_utf8_lossy(&output.stdout).trim(),
            "hello; exit 7 sandboxed"
        );
    }

    #[tokio::test]
    #[cfg(not(target_os = "windows"))]
    async fn unsupported_sandbox_rejects_coding_cli_before_runtime_wrapping() {
        let workspace = tempfile::TempDir::new().expect("temp workspace");
        let seen_command = Arc::new(Mutex::new(None));
        let runtime = Arc::new(FakeRuntime {
            seen_command: Arc::clone(&seen_command),
        });
        let executor =
            RuntimeCodingCliExecutor::shared(runtime, Arc::new(UnsupportedSandbox), false);
        let command = CodingCliCommand::new("codex", workspace.path().to_path_buf(), 5);

        let error = executor
            .output(command)
            .await
            .expect_err("unsupported sandbox should fail during command preparation");

        let message = error.to_string();
        assert!(
            message.contains("unsupported-sandbox"),
            "unexpected error: {message}"
        );
        assert!(
            message.contains("test unsupported sandbox"),
            "unexpected error: {message}"
        );
        assert!(
            seen_command.lock().expect("fake runtime mutex").is_none(),
            "unsupported sandbox should fail before runtime command rendering"
        );
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn replacing_sandbox_preserves_validated_working_dir() {
        let workspace = tempfile::TempDir::new().expect("temp workspace");
        let workspace_path = workspace
            .path()
            .canonicalize()
            .expect("canonical workspace");
        let runtime = Arc::new(FakeRuntime {
            seen_command: Arc::new(Mutex::new(None)),
        });
        let executor =
            RuntimeCodingCliExecutor::shared(runtime, Arc::new(ReplacingPwdSandbox), true);
        let command = CodingCliCommand::new("/bin/false", workspace_path.clone(), 5);

        let output = executor
            .output(command)
            .await
            .expect("replacement sandbox command should execute");

        assert!(output.status.success());
        assert_eq!(
            String::from_utf8_lossy(&output.stdout).trim(),
            workspace_path.to_string_lossy()
        );
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn runtime_env_key_forwarding_delivers_selected_env_to_child() {
        let workspace = tempfile::TempDir::new().expect("temp workspace");
        let seen_env_keys = Arc::new(Mutex::new(Vec::new()));
        let runtime = Arc::new(EnvForwardingRuntime {
            seen_env_keys: Arc::clone(&seen_env_keys),
        });
        let executor = RuntimeCodingCliExecutor::shared(runtime, Arc::new(NoopSandbox), false);
        let mut command = CodingCliCommand::new("codex", workspace.path().to_path_buf(), 5);
        command.env("ZC_CLI_TOKEN", "secret-value-visible-only-to-child-env");
        command.runtime_env_key("ZC_CLI_TOKEN");

        let output = executor
            .output(command)
            .await
            .expect("runtime command should receive selected environment");

        assert!(output.status.success());
        assert_eq!(
            String::from_utf8_lossy(&output.stdout).trim(),
            "secret-value-visible-only-to-child-env"
        );
        assert_eq!(
            *seen_env_keys.lock().expect("env-forwarding runtime mutex"),
            vec![OsString::from("ZC_CLI_TOKEN")]
        );
    }

    #[tokio::test]
    #[cfg(not(target_os = "windows"))]
    async fn runtime_env_key_forwarding_omits_implicit_safe_env() {
        let workspace = tempfile::TempDir::new().expect("temp workspace");
        let seen_env_keys = Arc::new(Mutex::new(Vec::new()));
        let runtime = Arc::new(EnvForwardingRuntime {
            seen_env_keys: Arc::clone(&seen_env_keys),
        });
        let executor = RuntimeCodingCliExecutor::shared(runtime, Arc::new(NoopSandbox), false);
        let mut command = CodingCliCommand::new("codex", workspace.path().to_path_buf(), 5);
        command.env("PATH", "/host/bin");
        command.env("HOME", "/host/home");
        command.env("ZC_CLI_TOKEN", "secret-value-visible-only-to-child-env");
        command.runtime_env_key("ZC_CLI_TOKEN");

        let output = executor
            .output(command)
            .await
            .expect("runtime command should receive selected environment");

        assert!(output.status.success());
        assert_eq!(
            *seen_env_keys.lock().expect("env-forwarding runtime mutex"),
            vec![OsString::from("ZC_CLI_TOKEN")]
        );
    }
}
