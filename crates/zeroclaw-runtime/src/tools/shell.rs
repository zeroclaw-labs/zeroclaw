use crate::platform::RuntimeAdapter;
use crate::security::SecurityPolicy;
use crate::security::traits::{Sandbox, SandboxShellProgram};
use crate::tools::shell_env::SAFE_SHELL_ENV_VARS;
use async_trait::async_trait;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use zeroclaw_api::platform::is_android;
use zeroclaw_api::runtime_traits::ShellExecutionDomain;
use zeroclaw_api::tool::{Tool, ToolOutput, ToolResult, with_ephemeral_workspace_warning};

/// Maximum output size in bytes (1MB).
const MAX_OUTPUT_BYTES: usize = 1_048_576;
const POST_EXIT_DRAIN: Duration = Duration::from_millis(250);

/// Drop guard that SIGKILLs the child's process group on cancel/timeout paths.
/// Disarmed after `child.wait()` returns so it never signals a recycled PID.
#[cfg(unix)]
struct ChildGroupGuard {
    pgid: std::sync::atomic::AtomicI32,
}

#[cfg(unix)]
impl ChildGroupGuard {
    fn new(child_pid: Option<u32>) -> Self {
        let pgid = child_pid.and_then(|p| i32::try_from(p).ok()).unwrap_or(0);
        Self {
            pgid: std::sync::atomic::AtomicI32::new(pgid),
        }
    }

    fn disarm(&self) {
        self.pgid.store(0, std::sync::atomic::Ordering::Release);
    }
}

#[cfg(unix)]
impl Drop for ChildGroupGuard {
    fn drop(&mut self) {
        let pgid = self.pgid.load(std::sync::atomic::Ordering::Acquire);
        if pgid <= 0 {
            return;
        }
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Kill)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                .with_attrs(::serde_json::json!({ "pgid": pgid, "signal": "SIGKILL" })),
            "shell tool reaping child process group"
        );
        // SAFETY: `pgid` was published only after the spawned child created
        // its own process group; a negative PID targets that group, and this
        // best-effort signal call passes no pointers.
        unsafe {
            libc::kill(-pgid, libc::SIGKILL);
        }
    }
}

/// Shell command execution tool with sandboxing
pub struct ShellTool {
    security: Arc<SecurityPolicy>,
    runtime: Arc<dyn RuntimeAdapter>,
    sandbox: Arc<dyn Sandbox>,
    timeout_secs: u64,
    /// Environment forwarded from the connected TUI client. When set, these
    /// vars are overlaid on top of the safe-env snapshot, letting the user's
    /// real shell environment (PATH, credentials, etc.) reach subprocesses
    /// even though the daemon itself may have a stripped-down env.
    tui_env: Option<Arc<HashMap<String, String>>>,
    persistent_writes: bool,
}

impl ShellTool {
    pub fn new(security: Arc<SecurityPolicy>, runtime: Arc<dyn RuntimeAdapter>) -> Self {
        let timeout_secs = security.shell_timeout_secs;
        Self {
            security,
            runtime,
            sandbox: Arc::new(crate::security::NoopSandbox),
            timeout_secs,
            tui_env: None,
            persistent_writes: true,
        }
    }

    pub fn new_with_sandbox(
        security: Arc<SecurityPolicy>,
        runtime: Arc<dyn RuntimeAdapter>,
        sandbox: Arc<dyn Sandbox>,
    ) -> Self {
        let timeout_secs = security.shell_timeout_secs;
        Self {
            security,
            runtime,
            sandbox,
            timeout_secs,
            tui_env: None,
            persistent_writes: true,
        }
    }

    pub fn with_persistent_writes(mut self, persistent: bool) -> Self {
        self.persistent_writes = persistent;
        self
    }

    /// Override the command execution timeout (in seconds).
    pub fn with_timeout_secs(mut self, secs: u64) -> Self {
        self.timeout_secs = secs;
        self
    }

    /// Overlay the TUI client's environment on top of the safe-env snapshot.
    /// Pass `Some(env)` to enable forwarding; `None` is a no-op (same as not
    /// calling this method at all).
    pub fn with_tui_env(mut self, env: Option<HashMap<String, String>>) -> Self {
        self.tui_env = env.map(Arc::new);
        self
    }

    /// Return an on-demand view over the exact runtime inputs this shell tool
    /// uses. Approval stores this resolver, not a snapshot of its derived
    /// facts, so mint and execution-time validation cannot drift.
    pub(crate) fn execution_facts_resolver(&self) -> Arc<ShellExecutionFactsResolver> {
        Arc::new(ShellExecutionFactsResolver {
            security: Arc::clone(&self.security),
            runtime: Arc::clone(&self.runtime),
            sandbox: Arc::clone(&self.sandbox),
            tui_env: self.tui_env.as_ref().map(Arc::clone),
        })
    }
}

/// Shell-v1 execution fact resolver shared by the approval gate and the
/// concrete shell tool. It owns only shared handles to the canonical runtime
/// objects and materializes a fresh command for every check.
pub struct ShellExecutionFactsResolver {
    security: Arc<SecurityPolicy>,
    runtime: Arc<dyn RuntimeAdapter>,
    sandbox: Arc<dyn Sandbox>,
    tui_env: Option<Arc<HashMap<String, String>>>,
}

pub(crate) struct PreparedShellExecution {
    pub command: tokio::process::Command,
    pub facts: serde_json::Value,
}

impl ShellExecutionFactsResolver {
    pub(crate) fn prepare(&self, source: &str) -> anyhow::Result<PreparedShellExecution> {
        self.prepare_with_binding(source)
    }

    fn prepare_static_allow(&self, source: &str) -> anyhow::Result<PreparedShellExecution> {
        let mut command = self
            .runtime
            .build_shell_command(source, &self.security.workspace_dir)?;
        self.sandbox.wrap_command(command.as_std_mut())?;
        let child_environment = self.child_environment();
        Self::finalize_command(&mut command, &child_environment);
        Ok(PreparedShellExecution {
            command,
            facts: serde_json::Value::Null,
        })
    }

    fn prepare_with_binding(&self, source: &str) -> anyhow::Result<PreparedShellExecution> {
        let action = zeroclaw_config::tool_policy::extract_shell_action(
            source,
            self.runtime.shell_dialect(),
            Some(&self.security.workspace_dir),
        );
        let zeroclaw_config::tool_policy::ToolAction::Shell(shell_action) = &action;

        let preliminary = self
            .runtime
            .build_shell_command(source, &self.security.workspace_dir)?;
        let runtime_cwd = preliminary
            .as_std()
            .get_current_dir()
            .unwrap_or(&self.security.workspace_dir)
            .canonicalize()?;
        let original_interpreter_program = preliminary.as_std().get_program().to_os_string();
        let child_environment = self.child_environment();
        let interpreter_program = resolve_program(
            preliminary.as_std().get_program(),
            &runtime_cwd,
            child_environment_value(&child_environment, "PATH"),
            child_environment_value(&child_environment, "PATHEXT"),
        )?;
        let mut command = self.runtime.build_shell_command_with_program(
            source,
            &self.security.workspace_dir,
            &interpreter_program,
        )?;
        self.runtime.pin_shell_command(
            command.as_std_mut(),
            &interpreter_program,
            &self.security.workspace_dir,
        )?;
        if command.as_std().get_current_dir().is_none() {
            command.current_dir(&runtime_cwd);
        }
        let interpreter_program = command.as_std().get_program().to_os_string();
        let interpreter_arguments: Vec<OsString> = command
            .as_std()
            .get_args()
            .map(OsStr::to_os_string)
            .collect();

        let sandbox_program = self
            .sandbox
            .wrap_shell_command(command.as_std_mut(), &original_interpreter_program)?;
        let sandbox_launcher_program = command.as_std().get_program().to_os_string();
        let sandbox_launcher = resolve_program(
            &sandbox_launcher_program,
            &runtime_cwd,
            child_environment_value(&child_environment, "PATH"),
            child_environment_value(&child_environment, "PATHEXT"),
        )?;
        ensure_program_is_pinned(&sandbox_launcher_program, &sandbox_launcher)?;
        self.sandbox
            .pin_shell_command(command.as_std_mut(), &sandbox_launcher)?;
        if command.as_std().get_current_dir().is_none() {
            command.current_dir(&runtime_cwd);
        }
        Self::finalize_command(&mut command, &child_environment);

        let std_command = command.as_std();
        let cwd = std_command.get_current_dir().ok_or_else(|| {
            anyhow::Error::msg("shell launch did not provide a working directory")
        })?;
        let resolved_cwd = cwd.canonicalize()?;
        let effective_path = environment_variable(std_command, "PATH");
        let host_interpreter = process_identity(
            &interpreter_program,
            &interpreter_arguments,
            &resolved_cwd,
            effective_path,
            environment_variable(std_command, "PATHEXT"),
        )?;
        ensure_program_is_pinned(&interpreter_program, &host_interpreter.resolved_program)?;
        let execution_domain = match &sandbox_program {
            SandboxShellProgram::Host => self.runtime.shell_execution_domain(),
            SandboxShellProgram::Isolated { mutable_mount, .. } => ShellExecutionDomain::Isolated {
                name: "sandbox",
                mutable_mount: *mutable_mount,
            },
        };
        let interpreter = match (&sandbox_program, execution_domain) {
            (SandboxShellProgram::Isolated { program, .. }, _) => json!({
                "program": os_identity(program),
                "resolved_program": serde_json::Value::Null,
                "execution_domain": "sandbox",
                "arguments": interpreter_arguments
                    .iter()
                    .map(|arg| os_identity(arg))
                    .collect::<Vec<_>>(),
            }),
            (SandboxShellProgram::Host, ShellExecutionDomain::Isolated { name: domain, .. }) => {
                let profile = self.runtime.shell_profile().ok_or_else(|| {
                    anyhow::Error::msg(
                        "isolated shell runtime did not declare its inner interpreter",
                    )
                })?;
                json!({
                    "program": os_identity(OsStr::new(&profile.name)),
                    "resolved_program": serde_json::Value::Null,
                    "execution_domain": domain,
                    "arguments_source": "sandbox.launch.arguments",
                })
            }
            (SandboxShellProgram::Host, ShellExecutionDomain::Host) => host_interpreter.facts,
        };
        let launch_arguments: Vec<OsString> =
            std_command.get_args().map(OsStr::to_os_string).collect();
        let launch = process_identity(
            std_command.get_program(),
            &launch_arguments,
            &resolved_cwd,
            effective_path,
            environment_variable(std_command, "PATHEXT"),
        )?;
        ensure_program_is_pinned(std_command.get_program(), &launch.resolved_program)?;
        let sandbox_material = self
            .sandbox
            .execution_fingerprint_material(&launch.resolved_program)?;
        let sandbox_policy_digest = hex::encode(Sha256::digest(&sandbox_material));
        let mut environment: Vec<serde_json::Value> = std_command
            .get_envs()
            .map(|(key, value)| {
                json!({
                    "name": os_identity(key),
                    "value": value.map(os_identity),
                })
            })
            .collect();
        environment.sort_by_key(|entry| entry.to_string());

        let mut facts = shell_action.fingerprint_facts();
        let object = facts
            .as_object_mut()
            .ok_or_else(|| anyhow::Error::msg("shell fingerprint facts must be a JSON object"))?;
        if !matches!(
            shell_action.parse_status,
            zeroclaw_config::tool_policy::ParseStatus::Clean
        ) {
            anyhow::bail!("shell syntax cannot be normalized into a complete execution identity");
        }
        if execution_domain == ShellExecutionDomain::Host {
            ensure_static_shell_startup(
                self.runtime.shell_dialect(),
                &interpreter_program,
                std_command,
                shell_action,
            )?;
        } else {
            ensure_isolated_shell_startup(
                self.runtime.as_ref(),
                self.runtime.shell_dialect(),
                std_command,
            )?;
        }
        let segment_identities = segment_executable_identities(
            shell_action,
            self.runtime.shell_dialect(),
            execution_domain,
            &interpreter_program,
            &resolved_cwd,
            effective_path,
            environment_variable(std_command, "PATHEXT"),
        )?;
        let fact_segments = object
            .get_mut("segments")
            .and_then(serde_json::Value::as_array_mut)
            .ok_or_else(|| anyhow::Error::msg("shell fingerprint segments must be an array"))?;
        for (segment, identity) in fact_segments.iter_mut().zip(segment_identities) {
            let segment = segment
                .as_object_mut()
                .ok_or_else(|| anyhow::Error::msg("shell fingerprint segment must be an object"))?;
            segment.insert("resolved_executable".to_string(), identity);
        }
        // The source is not an authority or display match. It closes the gap
        // for shell syntax (notably inline redirects) that the conservative
        // v1 normalizer deliberately marks degraded instead of interpreting.
        object.insert("source".to_string(), json!(source));
        object.insert("cwd".to_string(), json!(path_identity(&resolved_cwd)));
        object.insert("runtime".to_string(), json!(self.runtime.name()));
        object.insert("interpreter".to_string(), interpreter);
        object.insert(
            "environment".to_string(),
            json!({
                "inherit": false,
                "changes": environment,
            }),
        );
        object.insert(
            "redirections".to_string(),
            json!({
                "shell": "embedded_in_source",
                "process": {"stdin": "null", "stdout": "piped", "stderr": "piped"},
            }),
        );
        object.insert(
            "stdin".to_string(),
            json!({"process": "null", "shell": "embedded_in_source"}),
        );
        // Sandbox escape approval is explicitly outside shell v1. A closed
        // enum value records that no escalation request can exist here.
        object.insert("requested_escalation".to_string(), json!("none"));
        object.insert(
            "sandbox".to_string(),
            json!({
                "backend": self.sandbox.name(),
                "prepared": true,
                "policy_sha256": sandbox_policy_digest,
                "launch": launch.facts,
            }),
        );
        // No distinct authenticated principal reaches the current turn. The
        // canonical authority is the API's shared-operator sentinel; agent,
        // channel, sender, and model aliases are not identity substitutes.
        object.insert(
            "originating_principal".to_string(),
            json!(zeroclaw_api::principal::Principal::shared_operator()),
        );

        Ok(PreparedShellExecution { command, facts })
    }

    fn child_environment(&self) -> HashMap<String, String> {
        let mut environment = HashMap::new();
        for var in collect_allowed_shell_env_vars(&self.security) {
            if let Ok(value) = std::env::var(&var) {
                environment.insert(var, value);
            }
        }
        if let Some(session_id) = get_session_id() {
            environment.insert(SESSION_ID_ENV_VAR.to_string(), session_id);
        }
        if let Some(tui_env) = &self.tui_env {
            environment.extend(
                tui_env
                    .iter()
                    .map(|(key, value)| (key.clone(), value.clone())),
            );
        }
        if is_android() {
            let ambient = std::env::var("PATH").unwrap_or_default();
            let tui_path = self
                .tui_env
                .as_ref()
                .and_then(|env| env.get("PATH"))
                .map(String::as_str);
            environment.insert("PATH".to_string(), android_child_path(tui_path, &ambient));
        }
        environment
    }

    fn finalize_command(
        command: &mut tokio::process::Command,
        child_environment: &HashMap<String, String>,
    ) {
        command.env_clear();
        for (key, value) in child_environment {
            command.env(key, value);
        }
        #[cfg(unix)]
        command.process_group(0);
        command.kill_on_drop(true);
        command.stdout(std::process::Stdio::piped());
        command.stderr(std::process::Stdio::piped());
        command.stdin(std::process::Stdio::null());
    }
}

fn ensure_static_shell_startup(
    dialect: crate::platform::ShellDialect,
    interpreter_program: &OsStr,
    command: &std::process::Command,
    action: &zeroclaw_config::tool_policy::ShellAction,
) -> anyhow::Result<()> {
    if dialect != crate::platform::ShellDialect::Posix {
        return Ok(());
    }
    let shell = shell_program_name(Path::new(interpreter_program));
    let nonempty_env =
        |name| environment_variable(command, name).is_some_and(|value| !value.is_empty());
    if !modeled_posix_shell(&shell) {
        anyhow::bail!(
            "shell startup behavior for {shell} is not modeled completely; fresh approval cannot be bound safely"
        );
    }
    if nonempty_env("ENV") || nonempty_env("BASH_ENV") {
        anyhow::bail!(
            "shell startup environment can change executable resolution; fresh approval cannot be bound safely"
        );
    }
    if command.get_envs().any(|(key, value)| {
        let key = key.to_string_lossy();
        value.is_some() && key.starts_with("BASH_FUNC_") && key.ends_with("%%")
    }) {
        anyhow::bail!(
            "exported shell functions can change executable resolution; fresh approval cannot be bound safely"
        );
    }
    if nonempty_env("CDPATH") && action.segments.iter().any(|segment| segment.base == "cd") {
        anyhow::bail!(
            "CDPATH can change a literal directory target; fresh approval cannot be bound safely"
        );
    }
    if let Some(segment) = action
        .segments
        .iter()
        .find(|segment| unmodeled_posix_interpreter_command(&segment.base))
    {
        anyhow::bail!(
            "shell command '{}' can change executable lookup or working directory state; fresh approval cannot be bound safely",
            segment.base
        );
    }
    Ok(())
}

fn modeled_posix_shell(shell: &str) -> bool {
    matches!(shell, "sh" | "dash" | "ash" | "bash")
}

fn ensure_isolated_shell_startup(
    runtime: &dyn RuntimeAdapter,
    dialect: crate::platform::ShellDialect,
    command: &std::process::Command,
) -> anyhow::Result<()> {
    let profile = runtime.shell_profile().ok_or_else(|| {
        anyhow::Error::msg("isolated shell runtime did not declare an interpreter")
    })?;
    if profile.dialect != dialect {
        anyhow::bail!("isolated shell runtime profile does not match its declared shell dialect");
    }
    if dialect != crate::platform::ShellDialect::Posix {
        return Ok(());
    }
    let shell = profile.name.trim_end_matches(".exe").to_ascii_lowercase();
    if !modeled_posix_shell(&shell) {
        anyhow::bail!(
            "isolated shell startup behavior for {shell} is not modeled completely; fresh approval cannot be bound safely"
        );
    }
    let nonempty_env =
        |name| environment_variable(command, name).is_some_and(|value| !value.is_empty());
    if nonempty_env("ENV") || nonempty_env("BASH_ENV") {
        anyhow::bail!(
            "isolated shell startup environment cannot be bound safely to the inner interpreter"
        );
    }
    if command.get_envs().any(|(key, value)| {
        let key = key.to_string_lossy();
        value.is_some() && key.starts_with("BASH_FUNC_") && key.ends_with("%%")
    }) {
        anyhow::bail!(
            "isolated bash can import shell functions; fresh approval cannot be bound safely"
        );
    }
    Ok(())
}

fn shell_program_name(program: &Path) -> String {
    program
        .file_name()
        .and_then(OsStr::to_str)
        .unwrap_or_default()
        .trim_end_matches(".exe")
        .to_ascii_lowercase()
}

fn unmodeled_posix_shell(shell: &str) -> bool {
    matches!(
        shell,
        "zsh"
            | "fish"
            | "ksh"
            | "mksh"
            | "csh"
            | "tcsh"
            | "rc"
            | "es"
            | "oil"
            | "osh"
            | "xonsh"
            | "elvish"
            | "nu"
            | "nushell"
            | "pwsh"
            | "powershell"
            | "cmd"
            | "cmd.exe"
    )
}

fn ensure_nested_shell_startup(
    segment: &zeroclaw_config::tool_policy::ShellSegment,
    resolved_program: Option<&Path>,
) -> anyhow::Result<()> {
    let shell =
        resolved_program.map_or_else(|| segment.base.to_ascii_lowercase(), shell_program_name);
    let inline_env = |name| {
        segment_environment_value(segment, name, crate::platform::ShellDialect::Posix)
            .is_some_and(|value| !value.is_empty())
    };
    if inline_env("ENV") || inline_env("BASH_ENV") {
        anyhow::bail!(
            "external command has a dynamic shell startup environment; fresh approval cannot be bound safely"
        );
    }
    if unmodeled_posix_shell(&shell) {
        anyhow::bail!(
            "nested shell '{shell}' has unmodeled startup behavior; fresh approval cannot be bound safely"
        );
    }
    if matches!(shell.as_str(), "busybox" | "toybox")
        && segment.arguments.first().is_some_and(|argument| {
            let applet = shell_program_name(Path::new(argument));
            modeled_posix_shell(&applet) || unmodeled_posix_shell(&applet)
        })
    {
        anyhow::bail!(
            "multi-call executable '{shell}' selects a nested shell with an unmodeled code source"
        );
    }
    if !modeled_posix_shell(&shell) {
        return Ok(());
    }
    if !segment.arguments.is_empty() {
        anyhow::bail!(
            "nested shell '{shell}' has an unmodeled code source; fresh approval cannot be bound safely"
        );
    }
    Ok(())
}

fn unmodeled_posix_interpreter_command(base: &str) -> bool {
    matches!(
        base,
        // Bash builtins not included in the closed, stateless identity set.
        "."
            | "alias"
            | "bind"
            | "builtin"
            | "caller"
            | "compgen"
            | "complete"
            | "compopt"
            | "declare"
            | "dirs"
            | "disown"
            | "enable"
            | "eval"
            | "exec"
            | "export"
            | "fc"
            | "getopts"
            | "hash"
            | "help"
            | "history"
            | "let"
            | "local"
            | "logout"
            | "mapfile"
            | "popd"
            | "pushd"
            | "readarray"
            | "read"
            | "readonly"
            | "set"
            | "shopt"
            | "source"
            | "suspend"
            | "typeset"
            | "unalias"
            | "unset"
            // Reserved words and control operators must never fall through to
            // PATH lookup if extraction produced them as a segment.
            | "!"
            | "[["
            | "]]"
            | "{"
            | "}"
            | "case"
            | "coproc"
            | "do"
            | "done"
            | "elif"
            | "else"
            | "esac"
            | "fi"
            | "for"
            | "function"
            | "if"
            | "in"
            | "select"
            | "then"
            | "time"
            | "until"
            | "while"
    )
}

fn segment_executable_identities(
    action: &zeroclaw_config::tool_policy::ShellAction,
    dialect: crate::platform::ShellDialect,
    domain: ShellExecutionDomain,
    interpreter_program: &OsStr,
    initial_cwd: &Path,
    path: Option<&OsStr>,
    pathext: Option<&OsStr>,
) -> anyhow::Result<Vec<serde_json::Value>> {
    if !action.execution_context_static {
        anyhow::bail!(
            "shell execution context cannot be reconstructed safely; use literal inline environment assignments and directory changes"
        );
    }

    let mut cwd = initial_cwd.to_path_buf();
    let mut identities = Vec::with_capacity(action.segments.len());
    for segment in &action.segments {
        if dialect == crate::platform::ShellDialect::Posix
            && segment.base == "cd"
            && matches!(
                segment.connector,
                Some(
                    zeroclaw_config::tool_policy::ShellConnector::And
                        | zeroclaw_config::tool_policy::ShellConnector::Or
                        | zeroclaw_config::tool_policy::ShellConnector::Pipeline
                )
            )
        {
            anyhow::bail!("conditional shell directory changes cannot be reconstructed safely");
        }
        let segment_path = segment_environment_value(segment, "PATH", dialect).or(path);
        let segment_pathext = segment_environment_value(segment, "PATHEXT", dialect).or(pathext);
        identities.push(segment_executable_identity(
            segment,
            dialect,
            domain,
            interpreter_program,
            &cwd,
            segment_path,
            segment_pathext,
        )?);
        if dialect == crate::platform::ShellDialect::Posix && segment.base == "cd" {
            let target = Path::new(&segment.arguments[0]);
            let target = if target.is_absolute() {
                target.to_path_buf()
            } else {
                cwd.join(target)
            };
            cwd = target.canonicalize().map_err(|error| {
                anyhow::Error::msg(format!(
                    "shell directory change target '{}' cannot be resolved: {error}",
                    target.display()
                ))
            })?;
            if !cwd.is_dir() {
                anyhow::bail!(
                    "shell directory change target '{}' is not a directory",
                    cwd.display()
                );
            }
        }
    }
    Ok(identities)
}

fn segment_executable_identity(
    segment: &zeroclaw_config::tool_policy::ShellSegment,
    dialect: crate::platform::ShellDialect,
    domain: ShellExecutionDomain,
    interpreter_program: &OsStr,
    cwd: &Path,
    path: Option<&OsStr>,
    pathext: Option<&OsStr>,
) -> anyhow::Result<serde_json::Value> {
    if interpreter_builtin(dialect, interpreter_program, &segment.base) {
        return Ok(json!({
            "kind": "interpreter_builtin",
            "name": segment.base,
            "interpreter": os_identity(interpreter_program),
        }));
    }
    if let ShellExecutionDomain::Isolated {
        name: domain,
        mutable_mount,
    } = domain
    {
        if mutable_mount {
            anyhow::bail!(
                "isolated shell external executable '{}' may resolve through mutable mounted content",
                segment.executable
            );
        }
        if dialect == crate::platform::ShellDialect::Posix {
            ensure_nested_shell_startup(segment, None)?;
        }
        return Ok(json!({
            "kind": "isolated_runtime",
            "domain": domain,
            "program": segment.executable,
        }));
    }
    if dialect == crate::platform::ShellDialect::PowerShell {
        let requested = Path::new(&segment.executable);
        if !requested.is_absolute() && requested.components().count() <= 1 {
            anyhow::bail!(
                "PowerShell command '{}' is not a closed interpreter command or explicit executable path",
                segment.executable
            );
        }
    }
    let resolved = resolve_program(OsStr::new(&segment.executable), cwd, path, pathext)?;
    if dialect == crate::platform::ShellDialect::Posix {
        ensure_nested_shell_startup(segment, Some(&resolved))?;
    }
    Ok(json!({
        "kind": "host_executable",
        "path": path_identity(&resolved),
        "content_sha256": executable_content_sha256(&resolved)?,
    }))
}

fn segment_environment_value<'a>(
    segment: &'a zeroclaw_config::tool_policy::ShellSegment,
    name: &str,
    dialect: crate::platform::ShellDialect,
) -> Option<&'a OsStr> {
    segment
        .env_assignments
        .iter()
        .rev()
        .find_map(|(key, value)| {
            let matches = if dialect == crate::platform::ShellDialect::WindowsCmd {
                key.eq_ignore_ascii_case(name)
            } else {
                key == name
            };
            matches.then_some(OsStr::new(strip_assignment_quotes(value)))
        })
}

fn strip_assignment_quotes(value: &str) -> &str {
    if value.len() >= 2 {
        let bytes = value.as_bytes();
        if matches!(
            (bytes[0], bytes[value.len() - 1]),
            (b'\'', b'\'') | (b'"', b'"')
        ) {
            return &value[1..value.len() - 1];
        }
    }
    value
}

fn interpreter_builtin(
    dialect: crate::platform::ShellDialect,
    interpreter_program: &OsStr,
    base: &str,
) -> bool {
    match dialect {
        crate::platform::ShellDialect::Posix => {
            let required_builtin = matches!(
                base,
                ":" | "."
                    | "break"
                    | "cd"
                    | "command"
                    | "continue"
                    | "eval"
                    | "exec"
                    | "exit"
                    | "export"
                    | "readonly"
                    | "return"
                    | "set"
                    | "shift"
                    | "trap"
                    | "unset"
            );
            let shell = Path::new(interpreter_program)
                .file_name()
                .and_then(OsStr::to_str)
                .unwrap_or_default()
                .trim_end_matches(".exe")
                .to_ascii_lowercase();
            required_builtin
                || (modeled_posix_shell(&shell)
                    && matches!(
                        base,
                        "[" | "alias"
                            | "bg"
                            | "echo"
                            | "false"
                            | "fc"
                            | "fg"
                            | "getopts"
                            | "hash"
                            | "jobs"
                            | "kill"
                            | "printf"
                            | "pwd"
                            | "read"
                            | "test"
                            | "times"
                            | "true"
                            | "type"
                            | "ulimit"
                            | "umask"
                            | "unalias"
                            | "wait"
                    ))
        }
        crate::platform::ShellDialect::WindowsCmd => {
            let shell = Path::new(interpreter_program)
                .file_name()
                .and_then(OsStr::to_str)
                .unwrap_or_default()
                .trim_end_matches(".exe");
            shell.eq_ignore_ascii_case("cmd")
                && matches!(
                    base,
                    "assoc"
                        | "break"
                        | "call"
                        | "cd"
                        | "chdir"
                        | "cls"
                        | "color"
                        | "copy"
                        | "date"
                        | "del"
                        | "dir"
                        | "echo"
                        | "endlocal"
                        | "erase"
                        | "exit"
                        | "for"
                        | "ftype"
                        | "goto"
                        | "if"
                        | "md"
                        | "mkdir"
                        | "mklink"
                        | "move"
                        | "path"
                        | "pause"
                        | "popd"
                        | "prompt"
                        | "pushd"
                        | "rd"
                        | "rem"
                        | "ren"
                        | "rename"
                        | "rmdir"
                        | "set"
                        | "setlocal"
                        | "shift"
                        | "start"
                        | "time"
                        | "title"
                        | "type"
                        | "ver"
                        | "verify"
                        | "vol"
                )
        }
        crate::platform::ShellDialect::PowerShell => powershell_interpreter_command(base),
        crate::platform::ShellDialect::None => false,
    }
}

fn powershell_interpreter_command(base: &str) -> bool {
    matches!(
        base.to_ascii_lowercase().as_str(),
        "add-content"
            | "clear-content"
            | "compare-object"
            | "convertfrom-json"
            | "convertto-json"
            | "copy-item"
            | "echo"
            | "foreach-object"
            | "format-list"
            | "format-table"
            | "get-childitem"
            | "get-content"
            | "get-item"
            | "get-location"
            | "join-path"
            | "measure-object"
            | "move-item"
            | "new-item"
            | "out-file"
            | "pop-location"
            | "push-location"
            | "read-host"
            | "remove-item"
            | "rename-item"
            | "select-object"
            | "set-content"
            | "set-location"
            | "sort-object"
            | "split-path"
            | "tee-object"
            | "test-path"
            | "where-object"
            | "write-error"
            | "write-host"
            | "write-output"
            | "write-warning"
            | "cat"
            | "cd"
            | "cls"
            | "copy"
            | "cp"
            | "del"
            | "dir"
            | "erase"
            | "gc"
            | "gci"
            | "gi"
            | "gl"
            | "ls"
            | "md"
            | "mkdir"
            | "move"
            | "mv"
            | "popd"
            | "pushd"
            | "pwd"
            | "rd"
            | "ren"
            | "ri"
            | "rm"
            | "rmdir"
            | "select"
            | "sl"
            | "type"
    )
}

fn child_environment_value<'a>(
    environment: &'a HashMap<String, String>,
    name: &str,
) -> Option<&'a OsStr> {
    environment.iter().find_map(|(key, value)| {
        environment_name_matches(OsStr::new(key), name).then_some(value.as_ref())
    })
}

#[cfg(windows)]
fn environment_name_matches(actual: &OsStr, expected: &str) -> bool {
    actual.to_string_lossy().eq_ignore_ascii_case(expected)
}

#[cfg(not(windows))]
fn environment_name_matches(actual: &OsStr, expected: &str) -> bool {
    actual == OsStr::new(expected)
}

struct ProcessIdentity {
    facts: serde_json::Value,
    resolved_program: PathBuf,
}

fn process_identity(
    program: &OsStr,
    arguments: &[OsString],
    cwd: &Path,
    path: Option<&OsStr>,
    pathext: Option<&OsStr>,
) -> anyhow::Result<ProcessIdentity> {
    let resolved_program = resolve_program(program, cwd, path, pathext)?;
    let content_sha256 = executable_content_sha256(&resolved_program)?;
    Ok(ProcessIdentity {
        facts: json!({
            "program": os_identity(program),
            "resolved_program": path_identity(&resolved_program),
            "content_sha256": content_sha256,
            "arguments": arguments.iter().map(|arg| os_identity(arg)).collect::<Vec<_>>(),
        }),
        resolved_program,
    })
}

fn executable_content_sha256(path: &Path) -> anyhow::Result<String> {
    let mut file = std::fs::File::open(path).map_err(|error| {
        anyhow::Error::msg(format!(
            "shell execution program '{}' cannot be opened for fingerprinting: {error}",
            path.display()
        ))
    })?;
    let mut hasher = Sha256::new();
    let mut prefix = [0_u8; 2];
    let mut prefix_len = 0;
    while prefix_len < prefix.len() {
        let read = file.read(&mut prefix[prefix_len..]).map_err(|error| {
            anyhow::Error::msg(format!(
                "shell execution program '{}' cannot be read for fingerprinting: {error}",
                path.display()
            ))
        })?;
        if read == 0 {
            break;
        }
        prefix_len += read;
    }
    if prefix_len == prefix.len() && prefix == *b"#!" {
        anyhow::bail!(
            "shell execution program '{}' uses an unpinned shebang interpreter",
            path.display()
        );
    }
    hasher.update(&prefix[..prefix_len]);
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(|error| {
            anyhow::Error::msg(format!(
                "shell execution program '{}' cannot be read for fingerprinting: {error}",
                path.display()
            ))
        })?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex::encode(hasher.finalize()))
}

fn ensure_program_is_pinned(program: &OsStr, resolved_program: &Path) -> anyhow::Result<()> {
    if Path::new(program) == resolved_program {
        return Ok(());
    }
    anyhow::bail!(
        "shell execution program '{}' was not pinned to its resolved executable '{}'",
        Path::new(program).display(),
        resolved_program.display()
    )
}

fn environment_variable<'a>(command: &'a std::process::Command, name: &str) -> Option<&'a OsStr> {
    command.get_envs().find_map(|(key, value)| {
        environment_name_matches(key, name)
            .then_some(value)
            .flatten()
    })
}

fn resolve_program(
    program: &OsStr,
    cwd: &Path,
    path: Option<&OsStr>,
    pathext: Option<&OsStr>,
) -> anyhow::Result<PathBuf> {
    #[cfg(not(windows))]
    let _ = pathext;
    let requested = Path::new(program);
    if requested.components().count() > 1 || requested.is_absolute() {
        let candidate = if requested.is_absolute() {
            requested.to_path_buf()
        } else {
            cwd.join(requested)
        };
        return executable_candidate(&candidate).ok_or_else(|| {
            anyhow::Error::msg(format!(
                "shell execution program '{}' cannot be resolved as an executable",
                candidate.display()
            ))
        });
    }

    #[cfg(windows)]
    if requested
        .to_string_lossy()
        .trim_end_matches(".exe")
        .eq_ignore_ascii_case("cmd")
        && let Some(system_cmd) = windows_command_interpreter()
    {
        return Ok(system_cmd);
    }

    let path = path.ok_or_else(|| {
        anyhow::Error::msg(format!(
            "shell execution program '{}' is relative but the final environment has no PATH",
            requested.display()
        ))
    })?;
    #[cfg(not(windows))]
    let directories: Vec<PathBuf> = std::env::split_paths(path).collect();
    #[cfg(windows)]
    let mut directories: Vec<PathBuf> = std::env::split_paths(path).collect();
    #[cfg(windows)]
    directories.insert(0, cwd.to_path_buf());
    for directory in directories {
        let directory = if directory.is_absolute() {
            directory
        } else {
            cwd.join(directory)
        };
        let candidate = directory.join(requested);
        if let Some(resolved) = executable_candidate(&candidate) {
            return Ok(resolved);
        }
        #[cfg(windows)]
        if requested.extension().is_none() {
            let extensions = pathext
                .map(|value| value.to_string_lossy().into_owned())
                .unwrap_or_else(|| ".COM;.EXE;.BAT;.CMD".to_string());
            for extension in extensions.split(';').filter(|part| !part.is_empty()) {
                let extension = extension.trim_start_matches('.');
                if let Some(resolved) = executable_candidate(&candidate.with_extension(extension)) {
                    return Ok(resolved);
                }
            }
        }
    }
    Err(anyhow::Error::msg(format!(
        "shell execution program '{}' cannot be resolved through the final PATH",
        requested.display()
    )))
}

#[cfg(windows)]
fn windows_command_interpreter() -> Option<PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    use windows::Win32::System::SystemInformation::GetSystemDirectoryW;

    let mut buffer = vec![0u16; 32_768];
    // SAFETY: `buffer` is a valid writable UTF-16 slice for the duration of
    // the Win32 call. A zero or oversized result is rejected below.
    let len = unsafe { GetSystemDirectoryW(Some(&mut buffer)) } as usize;
    if len == 0 || len >= buffer.len() {
        return None;
    }
    let system_dir = OsString::from_wide(&buffer[..len]);
    executable_candidate(&PathBuf::from(system_dir).join("cmd.exe"))
}

fn executable_candidate(candidate: &Path) -> Option<PathBuf> {
    let resolved = candidate.canonicalize().ok()?;
    let metadata = resolved.metadata().ok()?;
    if !metadata.is_file() {
        return None;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o111 == 0 {
            return None;
        }
    }
    Some(resolved)
}

#[cfg(unix)]
fn os_identity(value: &OsStr) -> serde_json::Value {
    use std::os::unix::ffi::OsStrExt;
    json!({"encoding": "unix_bytes_hex", "value": hex::encode(value.as_bytes())})
}

#[cfg(windows)]
fn os_identity(value: &OsStr) -> serde_json::Value {
    use std::os::windows::ffi::OsStrExt;
    let encoded = value
        .encode_wide()
        .map(|unit| format!("{unit:04x}"))
        .collect::<String>();
    json!({"encoding": "windows_wide_hex", "value": encoded})
}

#[cfg(not(any(unix, windows)))]
fn os_identity(value: &OsStr) -> serde_json::Value {
    json!({"encoding": "utf8_lossy", "value": value.to_string_lossy()})
}

fn path_identity(path: &Path) -> serde_json::Value {
    os_identity(path.as_os_str())
}

#[cfg(target_os = "windows")]
fn decode_output(bytes: &[u8]) -> String {
    use windows::Win32::Globalization::GetACP;
    use windows::Win32::System::Console::GetConsoleOutputCP;

    // SAFETY: both Win32 functions are parameter-free code-page queries. A
    // zero console code page selects the documented system ANSI fallback.
    let cp = unsafe {
        let console_cp = GetConsoleOutputCP();
        if console_cp == 0 {
            GetACP()
        } else {
            console_cp
        }
    };

    decode_output_with_code_page(bytes, cp)
}

#[cfg(any(target_os = "windows", test))]
fn decode_output_with_code_page(bytes: &[u8], cp: u32) -> String {
    let encoding = windows_code_page_to_encoding(cp);
    if std::ptr::eq(encoding, encoding_rs::UTF_8) {
        String::from_utf8_lossy(bytes).into_owned()
    } else {
        let (cow, _enc_used, _had_errors) = encoding.decode(bytes);
        cow.into_owned()
    }
}

/// Map a Windows code page identifier to an `encoding_rs` `Encoding`.
/// Falls back to UTF-8 (lossy) for unknown code pages.
#[cfg(any(target_os = "windows", test))]
fn windows_code_page_to_encoding(cp: u32) -> &'static encoding_rs::Encoding {
    match cp {
        932 => encoding_rs::SHIFT_JIS,
        936 | 54936 => encoding_rs::GBK,
        949 => encoding_rs::EUC_KR,
        950 => encoding_rs::BIG5,
        1250 => encoding_rs::WINDOWS_1250,
        1251 => encoding_rs::WINDOWS_1251,
        1252 => encoding_rs::WINDOWS_1252,
        1253 => encoding_rs::WINDOWS_1253,
        1254 => encoding_rs::WINDOWS_1254,
        1255 => encoding_rs::WINDOWS_1255,
        1256 => encoding_rs::WINDOWS_1256,
        1257 => encoding_rs::WINDOWS_1257,
        1258 => encoding_rs::WINDOWS_1258,
        20127 | 65001 => encoding_rs::UTF_8,
        _ => encoding_rs::UTF_8,
    }
}

#[cfg(not(target_os = "windows"))]
fn decode_output(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
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
    for key in SAFE_SHELL_ENV_VARS
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

/// Name of the environment variable that carries the in-flight session key
/// into shell tools.
pub(crate) const SESSION_ID_ENV_VAR: &str = "ZEROCLAW_SESSION_ID";

fn get_session_id() -> Option<String> {
    zeroclaw_api::TOOL_LOOP_SESSION_KEY
        .try_with(Clone::clone)
        .ok()
        .flatten()
        .filter(|key| !key.is_empty())
}

#[async_trait]
impl Tool for ShellTool {
    fn name(&self) -> &str {
        "shell"
    }

    fn description(&self) -> &str {
        "Execute a shell command in the workspace directory"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The shell command to execute"
                },
                "intent": {
                    "type": "string",
                    "maxLength": 200,
                    "description": "What this command is meant to accomplish, in one sentence. Shown to the operator next to the real command during approval; never used for authorization."
                }
            },
            "required": ["command"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let command = args
            .get("command")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"param": "command"})),
                    "tool argument validation failed"
                );

                anyhow::Error::msg("Missing 'command' parameter")
            })?;
        // These keys are runtime plumbing stripped before the approval gate.
        // A confirmation and a dispatch-time policy/session Allow remain
        // distinct authorities even though both bind the same fresh facts.
        if args
            .get(crate::agent::RUNTIME_AUTHORIZATION_REJECTED_ARG)
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
        {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(crate::i18n::get_required_cli_string(
                    "tool-shell-execution-context-unverified",
                )),
            });
        }
        let confirmed = args
            .get("__zeroclaw_confirmation_consumed")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let policy_allowed = args
            .get(crate::agent::RUNTIME_POLICY_ALLOW_ARG)
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        let confirmation_expires_at = args
            .get(crate::agent::RUNTIME_CONFIRMATION_EXPIRES_AT_ARG)
            .and_then(serde_json::Value::as_u64);

        if !confirmed
            && !policy_allowed
            && let Err(reason) = self.security.validate_command_execution_confirmed(
                command,
                false,
                self.runtime.shell_dialect(),
            )
        {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(reason),
            });
        }

        let resolver = self.execution_facts_resolver();
        let prepared = match if confirmed || policy_allowed {
            resolver.prepare(command)
        } else {
            resolver.prepare_static_allow(command)
        } {
            Ok(prepared) => prepared,
            Err(error) => {
                return Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some(super::runtime_command_error::format_runtime_command_error(
                        &error,
                    )),
                });
            }
        };
        if confirmed || policy_allowed {
            let expected = args
                .get(crate::agent::RUNTIME_CONFIRMATION_FINGERPRINT_ARG)
                .and_then(serde_json::Value::as_str);
            let actual = zeroclaw_api::permission::ActionFingerprint::compute(&prepared.facts);
            if expected != Some(actual.as_hex().as_str()) {
                return Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some(crate::i18n::get_required_cli_string(
                        "tool-shell-execution-context-changed",
                    )),
                });
            }
        }
        if confirmed && confirmation_expires_at.is_none() {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(crate::i18n::get_required_cli_string(
                    "tool-shell-confirmation-validity-unverified",
                )),
            });
        }

        if !policy_allowed {
            match self.security.validate_command_execution_confirmed(
                command,
                confirmed,
                self.runtime.shell_dialect(),
            ) {
                Ok(_) => {}
                Err(reason) => {
                    return Ok(ToolResult {
                        success: false,
                        output: ToolOutput::default(),
                        error: Some(reason),
                    });
                }
            }
        }

        // Own the workspace-resolving forbidden-path scan here, dialect-aware,
        // rather than relying on an outer generic path guard that defaults to
        // POSIX. This keeps symlink-escape hardening while allowing cmd.exe's
        // null device only on the native Windows execution path.
        if let Some(path) = self
            .security
            .forbidden_workspace_path_argument_for_shell(command, self.runtime.shell_dialect())
        {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(format!("Path blocked by security policy: {path}")),
            });
        }

        if confirmation_expires_at
            .is_some_and(|expires_at| chrono::Utc::now().timestamp().max(0) as u64 >= expires_at)
        {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(crate::i18n::get_required_cli_string(
                    "tool-shell-confirmation-expired",
                )),
            });
        }

        let mut cmd = prepared.command;
        let timeout_secs = self.timeout_secs;
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some(format!("Failed to spawn command: {e}")),
                });
            }
        };

        #[cfg(unix)]
        let group_guard = ChildGroupGuard::new(child.id());

        let stdout_handle = child.stdout.take();
        let stderr_handle = child.stderr.take();

        let stdout_drain = spawn_drain(stdout_handle, MAX_OUTPUT_BYTES);
        let stderr_drain = spawn_drain(stderr_handle, MAX_OUTPUT_BYTES);

        let mut result =
            match tokio::time::timeout(Duration::from_secs(timeout_secs), child.wait()).await {
                Ok(Ok(status)) => {
                    #[cfg(unix)]
                    group_guard.disarm();
                    let (stdout_capture, stderr_capture) =
                        tokio::join!(finish_drain(stdout_drain), finish_drain(stderr_drain));

                    let mut stdout = decode_output(&stdout_capture.bytes);
                    let mut stderr = decode_output(&stderr_capture.bytes);

                    if stdout_capture.truncated || stdout.len() > MAX_OUTPUT_BYTES {
                        append_truncation_marker(&mut stdout, "\n... [output truncated at 1MB]");
                    }
                    if stderr_capture.truncated || stderr.len() > MAX_OUTPUT_BYTES {
                        append_truncation_marker(&mut stderr, "\n... [stderr truncated at 1MB]");
                    }

                    ToolResult {
                        success: status.success(),
                        output: stdout.into(),
                        error: if stderr.is_empty() {
                            None
                        } else {
                            Some(stderr)
                        },
                    }
                }
                Ok(Err(e)) => {
                    tokio::join!(abort_drain(stdout_drain), abort_drain(stderr_drain));
                    ToolResult {
                        success: false,
                        output: ToolOutput::default(),
                        error: Some(format!("Failed to execute command: {e}")),
                    }
                }
                Err(_) => {
                    let _ = child.start_kill();
                    tokio::join!(abort_drain(stdout_drain), abort_drain(stderr_drain));
                    ToolResult {
                        success: false,
                        output: ToolOutput::default(),
                        error: Some(format!(
                            "Command timed out after {timeout_secs}s and was killed"
                        )),
                    }
                }
            };

        // The command ran inside an ephemeral workspace: any files it wrote are
        // invisible on the host and discarded at session end
        // Inject the warning into whichever field the dispatcher surfaces to the
        // model — `output` on success, `error` on failure — so it is never lost.
        if !self.persistent_writes {
            result.output = with_ephemeral_workspace_warning(&result.output).into();
            if let Some(err) = result.error.take() {
                result.error = Some(with_ephemeral_workspace_warning(&err));
            }
        }

        Ok(result)
    }
}

struct DrainHandle {
    task: tokio::task::JoinHandle<()>,
    output: Arc<std::sync::Mutex<DrainOutput>>,
}

#[derive(Clone, Default)]
struct DrainOutput {
    bytes: Vec<u8>,
    truncated: bool,
}

fn spawn_drain<R>(reader: Option<R>, cap: usize) -> DrainHandle
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    let output = Arc::new(std::sync::Mutex::new(DrainOutput::default()));
    let shared = Arc::clone(&output);
    let task = zeroclaw_spawn::spawn!(async move {
        drain_capped_into(reader, cap, shared).await;
    });
    DrainHandle { task, output }
}

async fn finish_drain(mut drain: DrainHandle) -> DrainOutput {
    if tokio::time::timeout(POST_EXIT_DRAIN, &mut drain.task)
        .await
        .is_err()
    {
        drain.task.abort();
        let _ = drain.task.await;
    }

    drain
        .output
        .lock()
        .map(|output| output.clone())
        .unwrap_or_default()
}

async fn abort_drain(drain: DrainHandle) {
    drain.task.abort();
    let _ = drain.task.await;
}

async fn drain_capped_into<R>(
    reader: Option<R>,
    cap: usize,
    output: Arc<std::sync::Mutex<DrainOutput>>,
) where
    R: tokio::io::AsyncRead + Unpin,
{
    use tokio::io::AsyncReadExt;
    let Some(mut reader) = reader else {
        return;
    };
    let mut chunk = [0u8; 8192];
    loop {
        match reader.read(&mut chunk).await {
            Ok(0) => break,
            Ok(n) => {
                let Ok(mut capture) = output.lock() else {
                    break;
                };
                let remaining = cap.saturating_sub(capture.bytes.len());
                if remaining > 0 {
                    let take = n.min(remaining);
                    capture.bytes.extend_from_slice(&chunk[..take]);
                    capture.truncated |= take < n;
                } else {
                    capture.truncated = true;
                }
            }
            Err(_) => break,
        }
    }
}

fn append_truncation_marker(output: &mut String, marker: &str) {
    let mut boundary = MAX_OUTPUT_BYTES.min(output.len());
    while boundary > 0 && !output.is_char_boundary(boundary) {
        boundary -= 1;
    }
    output.truncate(boundary);
    output.push_str(marker);
}

/// Compose the child `PATH` for an Android shell: the platform tool dirs
/// (`/system/bin:/system/xbin`) are prefixed onto the curated PATH, with a
/// TUI-provided PATH winning over the daemon's ambient PATH. Yields the bare
/// platform dirs when the resolved base is empty.
fn android_child_path(tui_path: Option<&str>, ambient_path: &str) -> String {
    let base = tui_path.unwrap_or(ambient_path);
    if base.is_empty() {
        "/system/bin:/system/xbin".to_string()
    } else {
        format!("/system/bin:/system/xbin:{base}")
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn android_child_path_prefixes_platform_dirs_with_tui_path_winning() {
        assert_eq!(
            super::android_child_path(Some("/usr/local/bin"), "/daemon"),
            "/system/bin:/system/xbin:/usr/local/bin"
        );
        assert_eq!(
            super::android_child_path(None, "/daemon"),
            "/system/bin:/system/xbin:/daemon"
        );
        assert_eq!(
            super::android_child_path(None, ""),
            "/system/bin:/system/xbin"
        );
    }

    #[test]
    fn is_android_returns_bool_without_panicking() {
        let _ = zeroclaw_api::platform::is_android();
    }
    use super::*;
    use crate::platform::{DockerRuntime, NativeRuntime, RuntimeAdapter};
    use crate::security::{AutonomyLevel, SecurityPolicy};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use zeroclaw_config::schema::DockerRuntimeConfig;
    use zeroclaw_tools::wrappers::RateLimitedTool;

    #[cfg(unix)]
    struct SwitchingInterpreterRuntime {
        first: PathBuf,
        second: PathBuf,
        use_second: AtomicBool,
    }

    #[cfg(unix)]
    impl RuntimeAdapter for SwitchingInterpreterRuntime {
        fn name(&self) -> &str {
            "switching-test-runtime"
        }

        fn has_filesystem_access(&self) -> bool {
            true
        }

        fn storage_path(&self) -> PathBuf {
            self.first.clone()
        }

        fn supports_long_running(&self) -> bool {
            false
        }

        fn shell_dialect(&self) -> crate::platform::ShellDialect {
            crate::platform::ShellDialect::Posix
        }

        fn build_shell_command(
            &self,
            command: &str,
            workspace_dir: &Path,
        ) -> anyhow::Result<tokio::process::Command> {
            let program = if self.use_second.load(Ordering::SeqCst) {
                &self.second
            } else {
                &self.first
            };
            let mut process = tokio::process::Command::new(program);
            process.arg("-c").arg(command).current_dir(workspace_dir);
            Ok(process)
        }
    }

    #[cfg(unix)]
    struct SwitchingArgumentsRuntime {
        shell: PathBuf,
        builds: AtomicUsize,
        mutable_mount: bool,
    }

    #[cfg(unix)]
    impl RuntimeAdapter for SwitchingArgumentsRuntime {
        fn name(&self) -> &str {
            "switching-arguments-test-runtime"
        }

        fn has_filesystem_access(&self) -> bool {
            true
        }

        fn storage_path(&self) -> PathBuf {
            self.shell.clone()
        }

        fn supports_long_running(&self) -> bool {
            false
        }

        fn shell_dialect(&self) -> crate::platform::ShellDialect {
            crate::platform::ShellDialect::Posix
        }

        fn shell_execution_domain(&self) -> ShellExecutionDomain {
            ShellExecutionDomain::Isolated {
                name: "switching-arguments-test",
                mutable_mount: self.mutable_mount,
            }
        }

        fn build_shell_command(
            &self,
            _command: &str,
            workspace_dir: &Path,
        ) -> anyhow::Result<tokio::process::Command> {
            let invocation = self.builds.fetch_add(1, Ordering::SeqCst);
            let command = if invocation == 0 {
                "printf preliminary"
            } else {
                "printf executed"
            };
            let mut process = tokio::process::Command::new(&self.shell);
            process.arg("-c").arg(command).current_dir(workspace_dir);
            Ok(process)
        }
    }

    #[cfg(unix)]
    fn executable_file(path: &Path) {
        let search_path = std::env::var_os("PATH").unwrap_or_default();
        let source = resolve_program(OsStr::new("true"), Path::new("/"), Some(&search_path), None)
            .expect("test requires a true executable");
        std::fs::copy(source, path).unwrap();
    }

    #[cfg(unix)]
    fn mutate_executable(path: &Path) {
        use std::io::Write;

        std::fs::OpenOptions::new()
            .append(true)
            .open(path)
            .unwrap()
            .write_all(b"changed")
            .unwrap();
    }

    #[cfg(unix)]
    fn switching_shell_fixture() -> (
        tempfile::TempDir,
        Arc<SecurityPolicy>,
        Arc<SwitchingInterpreterRuntime>,
        ShellTool,
    ) {
        let temp = tempfile::TempDir::new().unwrap();
        let first = temp.path().join("first").join("sh");
        let second = temp.path().join("second").join("sh");
        std::fs::create_dir_all(first.parent().unwrap()).unwrap();
        std::fs::create_dir_all(second.parent().unwrap()).unwrap();
        executable_file(&first);
        executable_file(&second);
        let profile = zeroclaw_config::schema::RiskProfileConfig::default();
        let security = Arc::new(SecurityPolicy::from_risk_profile(&profile, temp.path()));
        let runtime = Arc::new(SwitchingInterpreterRuntime {
            first,
            second,
            use_second: AtomicBool::new(false),
        });
        let tool = ShellTool::new(
            Arc::clone(&security),
            Arc::clone(&runtime) as Arc<dyn RuntimeAdapter>,
        );
        (temp, security, runtime, tool)
    }

    #[cfg(unix)]
    struct NamedWrapperSandbox {
        name: &'static str,
        wrapper: Option<PathBuf>,
    }

    #[cfg(unix)]
    struct IsolatedProgramSandbox;

    #[cfg(unix)]
    #[async_trait]
    impl Sandbox for IsolatedProgramSandbox {
        fn wrap_command(&self, _command: &mut std::process::Command) -> std::io::Result<()> {
            Ok(())
        }

        fn wrap_shell_command(
            &self,
            _command: &mut std::process::Command,
            _original_program: &OsStr,
        ) -> std::io::Result<SandboxShellProgram> {
            Ok(SandboxShellProgram::Isolated {
                program: OsString::from("sh"),
                mutable_mount: false,
            })
        }

        fn is_available(&self) -> bool {
            true
        }

        fn name(&self) -> &str {
            "isolated-program-test"
        }

        fn description(&self) -> &str {
            "test isolated shell program"
        }
    }

    #[cfg(unix)]
    #[async_trait]
    impl Sandbox for NamedWrapperSandbox {
        fn wrap_command(&self, command: &mut std::process::Command) -> std::io::Result<()> {
            let Some(wrapper) = &self.wrapper else {
                return Ok(());
            };
            let program = command.get_program().to_os_string();
            let arguments: Vec<OsString> = command.get_args().map(OsStr::to_os_string).collect();
            let cwd = command.get_current_dir().map(Path::to_path_buf);
            let mut wrapped = std::process::Command::new(wrapper);
            wrapped.arg(program).args(arguments);
            if let Some(cwd) = cwd {
                wrapped.current_dir(cwd);
            }
            *command = wrapped;
            Ok(())
        }

        fn is_available(&self) -> bool {
            true
        }

        fn name(&self) -> &str {
            self.name
        }

        fn description(&self) -> &str {
            "test sandbox"
        }
    }

    #[cfg(unix)]
    fn execution_fingerprint(
        tool: &ShellTool,
        source: &str,
    ) -> zeroclaw_api::permission::ActionFingerprint {
        let facts = tool
            .execution_facts_resolver()
            .prepare(source)
            .unwrap()
            .facts;
        zeroclaw_api::permission::ActionFingerprint::compute(&facts)
    }

    #[cfg(unix)]
    #[test]
    fn shell_v1_fingerprint_changes_with_concrete_execution_context() {
        let (temp, security, runtime, base_tool) = switching_shell_fixture();
        let base = execution_fingerprint(&base_tool, "echo hi");

        let env_tool = ShellTool::new(
            Arc::clone(&security),
            Arc::clone(&runtime) as Arc<dyn RuntimeAdapter>,
        )
        .with_tui_env(Some(HashMap::from([(
            "FINGERPRINT_TEST".to_string(),
            "changed".to_string(),
        )])));
        assert_ne!(base, execution_fingerprint(&env_tool, "echo hi"));

        let other_workspace = temp.path().join("other-workspace");
        std::fs::create_dir(&other_workspace).unwrap();
        let profile = zeroclaw_config::schema::RiskProfileConfig::default();
        let other_security = Arc::new(SecurityPolicy::from_risk_profile(
            &profile,
            &other_workspace,
        ));
        let cwd_tool = ShellTool::new(
            other_security,
            Arc::clone(&runtime) as Arc<dyn RuntimeAdapter>,
        );
        assert_ne!(base, execution_fingerprint(&cwd_tool, "echo hi"));

        let wrapper = temp.path().join("sandbox-wrapper");
        executable_file(&wrapper);
        let sandbox_tool = ShellTool::new_with_sandbox(
            Arc::clone(&security),
            Arc::clone(&runtime) as Arc<dyn RuntimeAdapter>,
            Arc::new(NamedWrapperSandbox {
                name: "test-wrapper",
                wrapper: Some(wrapper),
            }),
        );
        assert_ne!(base, execution_fingerprint(&sandbox_tool, "echo hi"));
        assert_ne!(
            base,
            execution_fingerprint(&base_tool, "echo hi >/dev/null")
        );
    }

    #[cfg(unix)]
    #[test]
    fn shell_v1_fingerprint_binds_host_executable_contents() {
        let (temp, security, runtime, interpreter_tool) = switching_shell_fixture();
        let interpreter_facts = interpreter_tool
            .execution_facts_resolver()
            .prepare("echo hi")
            .unwrap()
            .facts;
        assert!(interpreter_facts["interpreter"]["content_sha256"].is_string());
        let interpreter_before =
            zeroclaw_api::permission::ActionFingerprint::compute(&interpreter_facts);
        mutate_executable(&runtime.first);
        let interpreter_after = execution_fingerprint(&interpreter_tool, "echo hi");
        assert_ne!(interpreter_before, interpreter_after);

        let segment = temp.path().join("segment-helper");
        executable_file(&segment);
        let segment_tool = ShellTool::new(Arc::clone(&security), Arc::new(NativeRuntime::new()));
        let segment_command = segment.to_string_lossy();
        let segment_facts = segment_tool
            .execution_facts_resolver()
            .prepare(&segment_command)
            .unwrap()
            .facts;
        assert!(segment_facts["segments"][0]["resolved_executable"]["content_sha256"].is_string());
        let segment_before = zeroclaw_api::permission::ActionFingerprint::compute(&segment_facts);
        mutate_executable(&segment);
        let segment_after = execution_fingerprint(&segment_tool, &segment_command);
        assert_ne!(segment_before, segment_after);

        let wrapper = temp.path().join("sandbox-wrapper");
        executable_file(&wrapper);
        let wrapper_tool = ShellTool::new_with_sandbox(
            security,
            Arc::new(NativeRuntime::new()),
            Arc::new(NamedWrapperSandbox {
                name: "content-test-wrapper",
                wrapper: Some(wrapper.clone()),
            }),
        );
        let wrapper_facts = wrapper_tool
            .execution_facts_resolver()
            .prepare("echo hi")
            .unwrap()
            .facts;
        assert!(wrapper_facts["sandbox"]["launch"]["content_sha256"].is_string());
        let wrapper_before = zeroclaw_api::permission::ActionFingerprint::compute(&wrapper_facts);
        mutate_executable(&wrapper);
        let wrapper_after = execution_fingerprint(&wrapper_tool, "echo hi");
        assert_ne!(wrapper_before, wrapper_after);
    }

    #[cfg(unix)]
    #[test]
    fn shell_v1_treats_invalid_assignment_name_as_the_executable() {
        let workspace = tempfile::TempDir::new().unwrap();
        let bin = workspace.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        let executable = bin.join("tool=x");
        executable_file(&executable);
        let security = Arc::new(SecurityPolicy::from_risk_profile(
            &zeroclaw_config::schema::RiskProfileConfig::default(),
            workspace.path(),
        ));
        let tool = ShellTool::new(Arc::clone(&security), Arc::new(NativeRuntime::new()));
        let resolver = tool.execution_facts_resolver();

        let command = "bin/tool=x echo safe";
        let before = resolver.prepare(command).unwrap().facts;
        assert_eq!(
            before["segments"][0]["resolved_executable"]["path"],
            path_identity(&executable.canonicalize().unwrap())
        );
        mutate_executable(&executable);
        let after = resolver.prepare(command).unwrap().facts;
        assert_ne!(
            zeroclaw_api::permission::ActionFingerprint::compute(&before),
            zeroclaw_api::permission::ActionFingerprint::compute(&after)
        );
    }

    #[cfg(unix)]
    #[test]
    fn shell_v1_rejects_ambiguous_posix_assignment_and_grouping_syntax() {
        let workspace = tempfile::TempDir::new().unwrap();
        let security = Arc::new(SecurityPolicy::from_risk_profile(
            &zeroclaw_config::schema::RiskProfileConfig::default(),
            workspace.path(),
        ));
        let tool = ShellTool::new(security, Arc::new(NativeRuntime::new()));
        let resolver = tool.execution_facts_resolver();

        for command in [
            r"FOO=bar\ baz printf ACTUAL",
            "PATH+=:subdir helper",
            "PATH=~/bin helper",
            "(cd child; helper)",
        ] {
            let error = match resolver.prepare(command) {
                Ok(_) => panic!("ambiguous POSIX syntax must fail closed: {command}"),
                Err(error) => error,
            };
            assert!(
                error.to_string().contains("cannot be reconstructed safely")
                    || error
                        .to_string()
                        .contains("cannot be normalized into a complete execution identity"),
                "{command}: {error}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn shell_v1_rejects_shebang_executables() {
        use std::os::unix::fs::PermissionsExt;

        let workspace = tempfile::TempDir::new().unwrap();
        let script = workspace.path().join("script");
        std::fs::write(&script, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let security = Arc::new(SecurityPolicy::from_risk_profile(
            &zeroclaw_config::schema::RiskProfileConfig::default(),
            workspace.path(),
        ));
        let tool = ShellTool::new(Arc::clone(&security), Arc::new(NativeRuntime::new()));

        let error = match tool.execution_facts_resolver().prepare("./script") {
            Ok(_) => panic!("shebang executable must fail closed"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("unpinned shebang interpreter"));
        let binary = workspace.path().join("binary");
        executable_file(&binary);
        assert!(executable_content_sha256(&binary).is_ok());

        let wrapper = ShellTool::new(
            security,
            Arc::new(NativeRuntime::with_shell(
                script.to_string_lossy().into_owned(),
            )),
        );
        assert!(
            wrapper
                .execution_facts_resolver()
                .prepare("echo strict")
                .is_err()
        );
        assert!(
            wrapper
                .execution_facts_resolver()
                .prepare_static_allow("echo static")
                .is_ok()
        );
    }

    #[test]
    fn shell_v1_limits_isolated_executables_that_can_use_mutable_mounts() {
        use zeroclaw_config::tool_policy::{ToolAction, extract_shell_action};

        let domain = ShellExecutionDomain::Isolated {
            name: "test",
            mutable_mount: true,
        };
        let ToolAction::Shell(action) =
            extract_shell_action("./script", crate::platform::ShellDialect::Posix, None);
        let error = segment_executable_identity(
            &action.segments[0],
            crate::platform::ShellDialect::Posix,
            domain,
            OsStr::new("sh"),
            Path::new("/workspace"),
            None,
            None,
        )
        .unwrap_err();
        assert!(error.to_string().contains("mutable mounted content"));

        let ToolAction::Shell(path_override) =
            extract_shell_action("PATH=. helper", crate::platform::ShellDialect::Posix, None);
        assert!(
            segment_executable_identity(
                &path_override.segments[0],
                crate::platform::ShellDialect::Posix,
                domain,
                OsStr::new("sh"),
                Path::new("/workspace"),
                None,
                None,
            )
            .is_err()
        );

        let ToolAction::Shell(image_command) =
            extract_shell_action("git status", crate::platform::ShellDialect::Posix, None);
        let error = segment_executable_identity(
            &image_command.segments[0],
            crate::platform::ShellDialect::Posix,
            domain,
            OsStr::new("sh"),
            Path::new("/workspace"),
            None,
            None,
        )
        .unwrap_err();
        assert!(error.to_string().contains("mutable mounted content"));
    }

    #[cfg(unix)]
    #[test]
    fn shell_v1_facts_bind_every_required_dimension() {
        let (_temp, _security, _runtime, tool) = switching_shell_fixture();
        let resolver = tool.execution_facts_resolver();
        let facts = resolver.prepare("echo hi >/dev/null").unwrap().facts;
        for field in [
            "interpreter",
            "segments",
            "cwd",
            "environment",
            "redirections",
            "stdin",
            "requested_escalation",
            "sandbox",
            "originating_principal",
        ] {
            assert!(
                facts.get(field).is_some(),
                "missing shell-v1 fingerprint dimension: {field}"
            );
        }
        assert_eq!(
            facts["originating_principal"],
            json!(zeroclaw_api::principal::Principal::shared_operator())
        );
        assert_eq!(facts["requested_escalation"], "none");
        assert_eq!(facts["redirections"]["process"]["stdin"], "null");
        assert!(facts["interpreter"]["resolved_program"].is_object());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn shell_v1_facts_bind_the_final_runtime_argv() {
        let workspace = tempfile::TempDir::new().unwrap();
        let path = std::env::var_os("PATH").unwrap_or_default();
        let shell = resolve_program(OsStr::new("sh"), workspace.path(), Some(&path), None)
            .expect("test requires a POSIX shell");
        let runtime = Arc::new(SwitchingArgumentsRuntime {
            shell,
            builds: AtomicUsize::new(0),
            mutable_mount: false,
        });
        let security = Arc::new(SecurityPolicy::from_risk_profile(
            &zeroclaw_config::schema::RiskProfileConfig::default(),
            workspace.path(),
        ));
        let tool = ShellTool::new_with_sandbox(
            security,
            runtime.clone() as Arc<dyn RuntimeAdapter>,
            Arc::new(IsolatedProgramSandbox),
        )
        .with_tui_env(Some(HashMap::from([(
            "PATH".to_string(),
            path.to_string_lossy().into_owned(),
        )])));

        let mut prepared = tool
            .execution_facts_resolver()
            .prepare("printf requested")
            .unwrap();
        assert_eq!(runtime.builds.load(Ordering::SeqCst), 2);
        assert_eq!(prepared.facts["interpreter"]["execution_domain"], "sandbox");
        assert!(prepared.facts["interpreter"]["resolved_program"].is_null());
        let actual_arguments: Vec<OsString> = prepared
            .command
            .as_std()
            .get_args()
            .map(OsStr::to_os_string)
            .collect();
        assert_eq!(
            prepared.facts["interpreter"]["arguments"],
            json!(
                actual_arguments
                    .iter()
                    .map(|argument| os_identity(argument))
                    .collect::<Vec<_>>()
            )
        );
        let output = prepared.command.output().await.unwrap();
        assert_eq!(String::from_utf8_lossy(&output.stdout), "executed");
    }

    #[cfg(unix)]
    #[test]
    fn shell_v1_uses_declared_inner_shell_for_isolated_runtime() {
        let workspace = tempfile::TempDir::new().unwrap();
        let path = std::env::var_os("PATH").unwrap_or_default();
        let shell = resolve_program(OsStr::new("sh"), workspace.path(), Some(&path), None)
            .expect("test requires a POSIX shell");
        let security = Arc::new(SecurityPolicy::from_risk_profile(
            &zeroclaw_config::schema::RiskProfileConfig::default(),
            workspace.path(),
        ));
        let runtime = Arc::new(SwitchingArgumentsRuntime {
            shell: shell.clone(),
            builds: AtomicUsize::new(0),
            mutable_mount: false,
        });
        let tool = ShellTool::new(security, runtime).with_tui_env(Some(HashMap::from([(
            "PATH".to_string(),
            path.to_string_lossy().into_owned(),
        )])));

        let facts = tool
            .execution_facts_resolver()
            .prepare("echo ok")
            .unwrap()
            .facts;
        assert_eq!(
            facts["interpreter"]["program"],
            os_identity(OsStr::new("sh"))
        );
        assert_eq!(
            facts["interpreter"]["execution_domain"],
            "switching-arguments-test"
        );
        assert_eq!(
            facts["interpreter"]["arguments_source"],
            "sandbox.launch.arguments"
        );
        assert_eq!(
            facts["sandbox"]["launch"]["resolved_program"],
            path_identity(&shell)
        );
    }

    #[cfg(unix)]
    #[test]
    fn static_allow_preserves_mutable_isolated_runtime_compatibility() {
        let workspace = tempfile::TempDir::new().unwrap();
        let path = std::env::var_os("PATH").unwrap_or_default();
        let shell = resolve_program(OsStr::new("sh"), workspace.path(), Some(&path), None)
            .expect("test requires a POSIX shell");
        let security = Arc::new(SecurityPolicy::from_risk_profile(
            &zeroclaw_config::schema::RiskProfileConfig::default(),
            workspace.path(),
        ));
        let runtime = Arc::new(SwitchingArgumentsRuntime {
            shell,
            builds: AtomicUsize::new(0),
            mutable_mount: true,
        });
        let resolver = ShellTool::new(security, runtime)
            .with_tui_env(Some(HashMap::from([(
                "PATH".to_string(),
                path.to_string_lossy().into_owned(),
            )])))
            .execution_facts_resolver();

        let error = match resolver.prepare("git status") {
            Ok(_) => panic!("confirmation-bound execution must reject mutable lookup"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("mutable mounted content"));
        assert!(resolver.prepare_static_allow("git status").is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn shell_v1_resolves_inline_path_wrapper_and_literal_cd_segments() {
        let workspace = tempfile::TempDir::new().unwrap();
        let outer_bin = workspace.path().join("outer-bin");
        let inline_bin = workspace.path().join("inline-bin");
        let subdir = workspace.path().join("subdir");
        std::fs::create_dir_all(&outer_bin).unwrap();
        std::fs::create_dir_all(&inline_bin).unwrap();
        std::fs::create_dir_all(&subdir).unwrap();
        let outer_helper = outer_bin.join("zc-helper");
        let inline_helper = inline_bin.join("zc-helper");
        let relative_helper = subdir.join("helper");
        executable_file(&outer_helper);
        executable_file(&inline_helper);
        executable_file(&relative_helper);

        let security = Arc::new(SecurityPolicy::from_risk_profile(
            &zeroclaw_config::schema::RiskProfileConfig::default(),
            workspace.path(),
        ));
        let path = std::env::join_paths([
            outer_bin.as_path(),
            Path::new("/usr/bin"),
            Path::new("/bin"),
        ])
        .unwrap()
        .to_string_lossy()
        .into_owned();
        let tool = ShellTool::new(security, Arc::new(NativeRuntime::new()))
            .with_tui_env(Some(HashMap::from([("PATH".to_string(), path)])));
        let resolver = tool.execution_facts_resolver();

        let inline = resolver
            .prepare(&format!("PATH={} zc-helper", inline_bin.display()))
            .unwrap()
            .facts;
        assert_eq!(
            inline["segments"][0]["resolved_executable"]["path"],
            path_identity(&inline_helper.canonicalize().unwrap())
        );

        let wrapped = resolver.prepare("command zc-helper").unwrap().facts;
        assert_eq!(wrapped["segments"][0]["executable"], "zc-helper");
        assert_eq!(
            wrapped["segments"][0]["resolved_executable"]["path"],
            path_identity(&outer_helper.canonicalize().unwrap())
        );

        let changed_cwd = resolver.prepare("cd subdir && ./helper").unwrap().facts;
        assert_eq!(
            changed_cwd["segments"][0]["resolved_executable"]["kind"],
            "interpreter_builtin"
        );
        assert_eq!(
            changed_cwd["segments"][1]["resolved_executable"]["path"],
            path_identity(&relative_helper.canonicalize().unwrap())
        );

        let builtin = resolver.prepare("echo ok").unwrap().facts;
        assert_eq!(
            builtin["segments"][0]["resolved_executable"]["kind"],
            "interpreter_builtin"
        );
    }

    #[cfg(unix)]
    #[test]
    fn shell_v1_fails_closed_on_unmodeled_lookup_state_and_launch_wrappers() {
        let workspace = tempfile::TempDir::new().unwrap();
        let bin = workspace.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        executable_file(&bin.join("zc-helper"));
        for decoy in [
            "if",
            "time",
            "xargs",
            "builtin",
            "declare",
            "typeset",
            "local",
            "enable",
            "pushd",
            "popd",
            "let",
            "mapfile",
            "readarray",
            "[[",
            "]]",
            "{",
            "}",
            "zsh",
            "fish",
            "ksh",
            "mksh",
        ] {
            executable_file(&bin.join(decoy));
        }
        let security = Arc::new(SecurityPolicy::from_risk_profile(
            &zeroclaw_config::schema::RiskProfileConfig::default(),
            workspace.path(),
        ));
        let path = std::env::join_paths([bin.as_path(), Path::new("/usr/bin"), Path::new("/bin")])
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let shell = resolve_program(
            OsStr::new("bash"),
            workspace.path(),
            Some(OsStr::new(&path)),
            None,
        )
        .or_else(|_| {
            resolve_program(
                OsStr::new("sh"),
                workspace.path(),
                Some(OsStr::new(&path)),
                None,
            )
        })
        .unwrap();
        let tool = ShellTool::new(
            security,
            Arc::new(NativeRuntime::with_shell(
                shell.to_string_lossy().into_owned(),
            )),
        )
        .with_tui_env(Some(HashMap::from([("PATH".to_string(), path)])));
        let resolver = tool.execution_facts_resolver();

        for command in [
            "echo $(date)",
            "source ./commands.sh",
            "eval echo ok",
            "PATH=/tmp; zc-helper",
            "PATH=/tmp :; zc-helper",
            "PATH=/tmp export FOO=x; zc-helper",
            "PATH+=:/attacker zc-helper",
            "readonly PATH=/attacker; zc-helper",
            "export PATH+=:/attacker; zc-helper",
            "readonly PATH+=:/attacker; zc-helper",
            "export P\\A\\T\\H=/attacker; zc-helper",
            "readonly P\\A\\T\\H=/attacker; zc-helper",
            "unset P\\A\\T\\H; zc-helper",
            "printf -v PATH /attacker; zc-helper",
            "printf -vPATH /attacker; zc-helper",
            "read PATH; zc-helper",
            "getopts x PATH; zc-helper",
            "fc",
            "cd sub\\dir && ./helper",
            "trap /tmp/hook EXIT; echo ok",
            "env PATH=/tmp zc-helper",
            "exec zc-helper",
            "sudo zc-helper",
            "nice zc-helper",
            "nohup zc-helper",
            "timeout 1 zc-helper",
            "xargs zc-helper",
            "xargs",
            "time",
            "if",
            "builtin zc-helper",
            "declare PATH=/attacker; zc-helper",
            "typeset PATH=/attacker; zc-helper",
            "local PATH=/attacker; zc-helper",
            "enable -n echo; echo ok",
            "pushd subdir; zc-helper",
            "popd; zc-helper",
            "let PATH=0; zc-helper",
            "mapfile PATH; zc-helper",
            "readarray PATH; zc-helper",
            "[[ -x zc-helper ]]",
            "{ zc-helper; }",
            "zsh",
            "fish",
            "ksh",
            "mksh",
            "BASH_ENV=/tmp/zeroclaw-bash-env bash",
            "for",
            "while",
            "until",
            "case",
            "select",
            "function",
            "coproc",
            "sh -c 'zc-helper'",
            "bash -lc 'zc-helper'",
            "bash -xc 'zc-helper'",
            "sh ./mutable-script",
            "bash script",
            "bash -l",
        ] {
            assert!(
                resolver.prepare(command).is_err(),
                "unmodeled execution state must fail closed: {command}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn shell_v1_fails_closed_on_conditional_directory_changes() {
        let workspace = tempfile::TempDir::new().unwrap();
        let decoy = workspace.path().join("decoy");
        std::fs::create_dir_all(&decoy).unwrap();
        executable_file(&workspace.path().join("helper"));
        executable_file(&decoy.join("helper"));
        let security = Arc::new(SecurityPolicy::from_risk_profile(
            &zeroclaw_config::schema::RiskProfileConfig::default(),
            workspace.path(),
        ));
        let tool = ShellTool::new(security, Arc::new(NativeRuntime::new()));
        let resolver = tool.execution_facts_resolver();

        for command in ["false && cd decoy; ./helper", "true || cd decoy; ./helper"] {
            assert!(
                resolver.prepare(command).is_err(),
                "conditional cwd state must fail closed: {command}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn shell_v1_fails_closed_on_bash_startup_code_sources() {
        let path = std::env::var_os("PATH").unwrap_or_default();
        let Ok(bash) = resolve_program(OsStr::new("bash"), Path::new("/"), Some(&path), None)
        else {
            return;
        };
        let workspace = tempfile::TempDir::new().unwrap();
        let security = Arc::new(SecurityPolicy::from_risk_profile(
            &zeroclaw_config::schema::RiskProfileConfig::default(),
            workspace.path(),
        ));
        let base_path = path.to_string_lossy().into_owned();

        let bash_env_tool = ShellTool::new(
            Arc::clone(&security),
            Arc::new(NativeRuntime::with_shell(
                bash.to_string_lossy().into_owned(),
            )),
        )
        .with_tui_env(Some(HashMap::from([
            ("PATH".to_string(), base_path.clone()),
            ("BASH_ENV".to_string(), "/tmp/zeroclaw-bash-env".to_string()),
        ])));
        assert!(
            bash_env_tool
                .execution_facts_resolver()
                .prepare("echo ok")
                .is_err()
        );

        let nested_bash_env_tool =
            ShellTool::new(Arc::clone(&security), Arc::new(NativeRuntime::new())).with_tui_env(
                Some(HashMap::from([
                    ("PATH".to_string(), base_path.clone()),
                    ("BASH_ENV".to_string(), "/tmp/zeroclaw-bash-env".to_string()),
                ])),
            );
        assert!(
            nested_bash_env_tool
                .execution_facts_resolver()
                .prepare("bash")
                .is_err()
        );

        let function_tool = ShellTool::new(
            security,
            Arc::new(NativeRuntime::with_shell(
                bash.to_string_lossy().into_owned(),
            )),
        )
        .with_tui_env(Some(HashMap::from([
            ("PATH".to_string(), base_path),
            (
                "BASH_FUNC_zc_helper%%".to_string(),
                "() { echo replaced; }".to_string(),
            ),
        ])));
        assert!(
            function_tool
                .execution_facts_resolver()
                .prepare("echo ok")
                .is_err()
        );

        let cdpath_tool = ShellTool::new(
            Arc::new(SecurityPolicy::from_risk_profile(
                &zeroclaw_config::schema::RiskProfileConfig::default(),
                workspace.path(),
            )),
            Arc::new(NativeRuntime::new()),
        )
        .with_tui_env(Some(HashMap::from([
            (
                "PATH".to_string(),
                std::env::var("PATH").unwrap_or_default(),
            ),
            ("CDPATH".to_string(), "/tmp".to_string()),
        ])));
        assert!(
            cdpath_tool
                .execution_facts_resolver()
                .prepare("cd child && ./helper")
                .is_err()
        );
    }

    #[cfg(unix)]
    #[test]
    fn shell_v1_fails_closed_for_unmodeled_posix_shell_startup() {
        let workspace = tempfile::TempDir::new().unwrap();
        for shell in ["zsh", "fish", "ksh", "custom-shell"] {
            let interpreter = workspace.path().join(shell);
            executable_file(&interpreter);
            let tool = ShellTool::new(
                Arc::new(SecurityPolicy::from_risk_profile(
                    &zeroclaw_config::schema::RiskProfileConfig::default(),
                    workspace.path(),
                )),
                Arc::new(NativeRuntime::with_shell(
                    interpreter.to_string_lossy().into_owned(),
                )),
            );
            assert!(
                tool.execution_facts_resolver().prepare("echo ok").is_err(),
                "{shell} startup configuration is outside the shell-v1 closed model"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn shell_v1_resolves_nested_shell_aliases_before_startup_checks() {
        use std::os::unix::fs::symlink;

        let workspace = tempfile::TempDir::new().unwrap();
        let bin = workspace.path().join("bin");
        let targets = workspace.path().join("targets");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(&targets).unwrap();
        for shell in ["bash", "zsh", "ksh"] {
            let target = targets.join(shell);
            executable_file(&target);
            symlink(&target, bin.join(format!("alias-{shell}"))).unwrap();
        }
        std::fs::hard_link(targets.join("bash"), bin.join("runner")).unwrap();
        let path = std::env::join_paths([bin.as_path(), Path::new("/usr/bin"), Path::new("/bin")])
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let security = Arc::new(SecurityPolicy::from_risk_profile(
            &zeroclaw_config::schema::RiskProfileConfig::default(),
            workspace.path(),
        ));
        for hook in [
            ("BASH_ENV", "/tmp/zeroclaw-bash-env"),
            ("BASH_FUNC_zc_helper%%", "() { echo replaced; }"),
        ] {
            let tool = ShellTool::new(Arc::clone(&security), Arc::new(NativeRuntime::new()))
                .with_tui_env(Some(HashMap::from([
                    ("PATH".to_string(), path.clone()),
                    (hook.0.to_string(), hook.1.to_string()),
                ])));
            for executable in ["alias-bash", "runner"] {
                assert!(
                    tool.execution_facts_resolver().prepare(executable).is_err(),
                    "a shell alias or hard link must not bypass {}: {executable}",
                    hook.0
                );
            }
        }
        let tool = ShellTool::new(security, Arc::new(NativeRuntime::new()))
            .with_tui_env(Some(HashMap::from([("PATH".to_string(), path)])));
        assert!(
            tool.execution_facts_resolver()
                .prepare("BASH_ENV=/tmp/zeroclaw-bash-env runner")
                .is_err(),
            "inline BASH_ENV must fail closed for every external segment"
        );
        for alias in ["alias-zsh", "alias-ksh"] {
            assert!(
                tool.execution_facts_resolver().prepare(alias).is_err(),
                "canonical unmodeled shell alias must fail closed: {alias}"
            );
        }
    }

    #[test]
    fn shell_v1_rejects_multicall_nested_shells() {
        use zeroclaw_config::tool_policy::{ToolAction, extract_shell_action};

        for launcher in ["busybox", "toybox"] {
            let ToolAction::Shell(action) = extract_shell_action(
                &format!("{launcher} sh ./script"),
                crate::platform::ShellDialect::Posix,
                None,
            );
            let error = ensure_nested_shell_startup(
                &action.segments[0],
                Some(Path::new(&format!("/bin/{launcher}"))),
            )
            .unwrap_err();
            assert!(error.to_string().contains("multi-call executable"));
        }
    }

    #[cfg(unix)]
    #[test]
    fn shell_v1_uses_case_sensitive_posix_path_environment() {
        let workspace = tempfile::TempDir::new().unwrap();
        let actual_bin = workspace.path().join("actual-bin");
        let decoy_bin = workspace.path().join("decoy-bin");
        std::fs::create_dir_all(&actual_bin).unwrap();
        std::fs::create_dir_all(&decoy_bin).unwrap();
        let actual = actual_bin.join("zc-case-helper");
        executable_file(&actual);
        executable_file(&decoy_bin.join("zc-case-helper"));
        let path = std::env::join_paths([
            actual_bin.as_path(),
            Path::new("/usr/bin"),
            Path::new("/bin"),
        ])
        .unwrap()
        .to_string_lossy()
        .into_owned();
        let security = Arc::new(SecurityPolicy::from_risk_profile(
            &zeroclaw_config::schema::RiskProfileConfig::default(),
            workspace.path(),
        ));
        let tool = ShellTool::new(security, Arc::new(NativeRuntime::new())).with_tui_env(Some(
            HashMap::from([
                ("PATH".to_string(), path),
                ("Path".to_string(), decoy_bin.to_string_lossy().into_owned()),
            ]),
        ));

        let facts = tool
            .execution_facts_resolver()
            .prepare("zc-case-helper")
            .unwrap()
            .facts;
        assert_eq!(
            facts["segments"][0]["resolved_executable"]["path"],
            path_identity(&actual.canonicalize().unwrap())
        );
    }

    #[cfg(unix)]
    #[test]
    fn shell_v1_uses_dialect_specific_closed_command_classification() {
        use zeroclaw_config::tool_policy::{ToolAction, extract_shell_action};

        let ToolAction::Shell(cmd_action) = extract_shell_action(
            "command helper",
            crate::platform::ShellDialect::WindowsCmd,
            None,
        );
        assert_eq!(cmd_action.segments[0].executable, "command");
        for source in [
            "set PATH=C:\\tools & helper",
            "path C:\\tools",
            "call helper",
            "start helper",
            "if exist file helper",
            "for %i in (*) do helper",
            "echo %PATH%",
            "cmd /C helper",
        ] {
            let ToolAction::Shell(action) =
                extract_shell_action(source, crate::platform::ShellDialect::WindowsCmd, None);
            assert!(
                !action.execution_context_static,
                "dynamic cmd.exe source must fail closed: {source}"
            );
        }

        let ToolAction::Shell(write_output) = extract_shell_action(
            "Write-Output ok",
            crate::platform::ShellDialect::PowerShell,
            None,
        );
        let builtin = segment_executable_identity(
            &write_output.segments[0],
            crate::platform::ShellDialect::PowerShell,
            ShellExecutionDomain::Host,
            OsStr::new("pwsh"),
            Path::new("/"),
            None,
            None,
        )
        .unwrap();
        assert_eq!(builtin["kind"], "interpreter_builtin");

        let ToolAction::Shell(unknown) = extract_shell_action(
            "git status",
            crate::platform::ShellDialect::PowerShell,
            None,
        );
        assert!(
            segment_executable_identity(
                &unknown.segments[0],
                crate::platform::ShellDialect::PowerShell,
                ShellExecutionDomain::Host,
                OsStr::new("pwsh"),
                Path::new("/"),
                std::env::var_os("PATH").as_deref(),
                None,
            )
            .is_err()
        );

        let workspace = tempfile::TempDir::new().unwrap();
        let explicit = workspace.path().join("helper");
        executable_file(&explicit);
        let ToolAction::Shell(explicit_action) = extract_shell_action(
            explicit.to_string_lossy().as_ref(),
            crate::platform::ShellDialect::PowerShell,
            None,
        );
        let identity = segment_executable_identity(
            &explicit_action.segments[0],
            crate::platform::ShellDialect::PowerShell,
            ShellExecutionDomain::Host,
            OsStr::new("pwsh"),
            workspace.path(),
            None,
            None,
        )
        .unwrap();
        assert_eq!(identity["kind"], "host_executable");
    }

    #[cfg(unix)]
    #[test]
    fn execution_revalidation_marks_changed_interpreter_stale() {
        use zeroclaw_api::permission::{ConsumeOutcome, RouteId};

        let (_temp, security, runtime, tool) = switching_shell_fixture();
        let profile = zeroclaw_config::schema::RiskProfileConfig::default();
        let manager = crate::approval::ApprovalManager::from_risk_profile(&profile);
        manager.set_policy_context(Arc::clone(&security), crate::platform::ShellDialect::Posix);
        manager.set_shell_execution_context(tool.execution_facts_resolver());

        let command = "echo hi";
        let approved_facts = manager.shell_fingerprint_facts(command).unwrap();
        let confirmation = manager.mint_confirmation(&approved_facts, RouteId::cli(), 60);
        runtime.use_second.store(true, Ordering::SeqCst);
        let args = json!({
            "command": command,
            "__zeroclaw_confirmation_id": confirmation.confirmation_id.to_string(),
        });
        let (outcome, fingerprint, _expires_at) = manager.authorize_shell_execution(&args);
        assert_eq!(
            outcome,
            crate::approval::ShellAuthorizationOutcome::Confirmation(ConsumeOutcome::Stale)
        );
        assert!(fingerprint.is_none());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn fingerprint_mismatch_is_rejected_before_spawn() {
        let (temp, _security, runtime, tool) = switching_shell_fixture();
        let marker = temp.path().join("spawned");
        mutate_executable(&runtime.first);

        let result = tool
            .execute(json!({
                "command": "echo hi",
                "__zeroclaw_confirmation_consumed": true,
                "__zeroclaw_confirmation_fingerprint": "00".repeat(32),
            }))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(
            result
                .error
                .as_deref()
                .is_some_and(|error| error.contains("fresh approval required"))
        );
        assert!(
            !marker.exists(),
            "mismatched approval must fail before spawn"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn confirmation_expiry_after_consume_is_rejected_before_spawn() {
        use zeroclaw_api::permission::{ConsumeOutcome, RouteId};

        let workspace = tempfile::TempDir::new().unwrap();
        let marker = workspace.path().join("expired-confirmation-spawned");
        let profile = zeroclaw_config::schema::RiskProfileConfig::default();
        let security = Arc::new(SecurityPolicy::from_risk_profile(
            &profile,
            workspace.path(),
        ));
        let tool = ShellTool::new(security.clone(), Arc::new(NativeRuntime::new()));
        let manager = crate::approval::ApprovalManager::from_risk_profile(&profile);
        manager.set_policy_context(security, crate::platform::ShellDialect::Posix);
        manager.set_shell_execution_context(tool.execution_facts_resolver());
        let command = format!("touch {}", marker.display());
        let facts = manager.shell_fingerprint_facts(&command).unwrap();
        let (authorization_args, fingerprint, expires_at) = loop {
            let confirmation = manager.mint_confirmation(&facts, RouteId::cli(), 1);
            let authorization_args = json!({
                "command": command,
                "__zeroclaw_confirmation_id": confirmation.confirmation_id.to_string(),
            });
            let (outcome, fingerprint, expires_at) =
                manager.authorize_shell_execution(&authorization_args);
            match outcome {
                crate::approval::ShellAuthorizationOutcome::Confirmation(
                    ConsumeOutcome::Consumed,
                ) => {
                    break (
                        authorization_args,
                        fingerprint.unwrap(),
                        expires_at.unwrap(),
                    );
                }
                crate::approval::ShellAuthorizationOutcome::Confirmation(
                    ConsumeOutcome::Expired,
                ) => continue,
                other => panic!("fresh confirmation returned {other:?}"),
            }
        };
        while (chrono::Utc::now().timestamp().max(0) as u64) < expires_at {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }

        let result = tool
            .execute(json!({
                "command": authorization_args["command"].clone(),
                "__zeroclaw_confirmation_consumed": true,
                "__zeroclaw_confirmation_fingerprint": fingerprint.as_hex(),
                "__zeroclaw_confirmation_expires_at": expires_at,
            }))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(
            !marker.exists(),
            "expired confirmation must fail before spawn"
        );
    }

    #[tokio::test]
    async fn get_session_id_returns_scoped_session_key() {
        let got = crate::agent::loop_::scope_session_key(Some("gw_abc-123".to_string()), async {
            get_session_id()
        })
        .await;
        assert_eq!(got, Some("gw_abc-123".to_string()));
    }

    #[test]
    fn get_session_id_none_outside_a_scoped_turn() {
        assert_eq!(get_session_id(), None);
    }

    #[tokio::test]
    async fn get_session_id_none_for_empty_session_key() {
        let got =
            crate::agent::loop_::scope_session_key(Some(String::new()), async { get_session_id() })
                .await;
        assert_eq!(got, None);
    }

    fn test_security(autonomy: AutonomyLevel) -> Arc<SecurityPolicy> {
        Arc::new(SecurityPolicy {
            autonomy,
            workspace_dir: std::env::temp_dir(),
            ..SecurityPolicy::default()
        })
    }

    fn test_security_with_allowed_commands(
        autonomy: AutonomyLevel,
        commands: &[&str],
    ) -> Arc<SecurityPolicy> {
        Arc::new(SecurityPolicy {
            autonomy,
            workspace_dir: std::env::temp_dir(),
            allowed_commands: commands
                .iter()
                .map(|command| (*command).to_string())
                .collect(),
            ..SecurityPolicy::default()
        })
    }

    #[cfg(unix)]
    fn unrestricted_shell_test_security() -> Arc<SecurityPolicy> {
        Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Full,
            workspace_dir: std::env::temp_dir(),
            allowed_commands: vec!["*".into()],
            block_high_risk_commands: false,
            ..SecurityPolicy::default()
        })
    }

    fn test_runtime() -> Arc<dyn RuntimeAdapter> {
        Arc::new(NativeRuntime::new())
    }

    #[cfg(windows)]
    fn stdin_reader_command() -> &'static str {
        "more"
    }

    #[cfg(not(windows))]
    fn stdin_reader_command() -> &'static str {
        "cat"
    }

    #[cfg(windows)]
    fn success_with_stderr_command() -> &'static str {
        "echo out && echo warn 1>&2"
    }

    #[cfg(not(windows))]
    fn success_with_stderr_command() -> &'static str {
        "echo out; echo warn >&2"
    }

    #[cfg(windows)]
    fn medium_risk_write_command() -> &'static str {
        "copy NUL zeroclaw_shell_approval_test"
    }

    #[cfg(not(windows))]
    fn medium_risk_write_command() -> &'static str {
        "touch zeroclaw_shell_approval_test"
    }

    fn medium_risk_write_base() -> &'static str {
        medium_risk_write_command()
            .split_whitespace()
            .next()
            .expect("medium-risk test command should have a base command")
    }

    /// The shell tool as assembled in production: `RateLimitedTool<ShellTool>`.
    /// ShellTool owns its own dialect-aware command + forbidden-path validation,
    /// so (like `SkillShellTool`) it is not wrapped in the generic POSIX
    /// `PathGuardedTool`. Tests exercise this exact shape.
    fn wrapped_shell(security: Arc<SecurityPolicy>) -> RateLimitedTool<ShellTool> {
        RateLimitedTool::new(ShellTool::new(security.clone(), test_runtime()), security)
    }

    #[tokio::test]
    async fn wrapped_shell_requires_approval_for_git_diff_output_file() {
        let workspace = tempfile::TempDir::new().unwrap();
        std::fs::write(workspace.path().join("a.txt"), "before\n").unwrap();
        std::fs::write(workspace.path().join("b.txt"), "after\n").unwrap();
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            workspace_dir: workspace.path().to_path_buf(),
            allowed_commands: vec!["git".into()],
            ..SecurityPolicy::default()
        });
        let tool = wrapped_shell(security);

        let result = tool
            .execute(json!({
                "command": "git diff --no-index --output=unapproved.patch a.txt b.txt"
            }))
            .await
            .unwrap();

        assert!(
            !result.success,
            "output-file mutation must require approval"
        );
        assert!(
            result
                .error
                .as_deref()
                .is_some_and(|error| error.contains("requires operator approval")),
            "unexpected policy result: {:?}",
            result.error
        );
        assert!(!workspace.path().join("unapproved.patch").exists());
    }

    /// A forbidden path argument is refused by whichever guard sees it first,
    /// and the two guards word it differently. On a Windows shell dialect the
    /// policy's own scan inside `validate_command_execution_for_shell` runs
    /// before the tool body and reports `Command blocked: forbidden path
    /// argument`; on POSIX dialects that scan is skipped (an operator may
    /// legitimately allow absolute arguments there) and the refusal comes from
    /// the shell tool's workspace scan as `Path blocked by security policy`.
    /// Both are the same verdict, so assert the refusal rather than one
    /// platform's phrasing.
    fn assert_path_argument_blocked(result: &ToolResult, context: &str) {
        assert!(
            !result.success,
            "{context}: the forbidden path argument must be refused, got: {result:?}"
        );
        let error = result.error.as_deref().unwrap_or("");
        assert!(
            error.contains("Path blocked") || error.contains("forbidden path argument"),
            "{context}: expected a path-guard refusal, got: {error:?}"
        );
    }

    #[cfg(unix)]
    fn powershell_test_runtime() -> (Option<tempfile::TempDir>, Arc<dyn RuntimeAdapter>) {
        let temp = tempfile::tempdir().unwrap();
        let powershell = temp.path().join("pwsh");
        executable_file(&powershell);
        let runtime: Arc<dyn RuntimeAdapter> = Arc::new(NativeRuntime::with_shell(
            powershell.to_string_lossy().into_owned(),
        ));
        (Some(temp), runtime)
    }

    #[cfg(windows)]
    fn powershell_test_runtime() -> (Option<tempfile::TempDir>, Arc<dyn RuntimeAdapter>) {
        (
            None,
            Arc::new(NativeRuntime::with_shell("powershell".into())),
        )
    }

    #[test]
    fn shell_tool_name() {
        let tool = ShellTool::new(test_security(AutonomyLevel::Supervised), test_runtime());
        assert_eq!(tool.name(), "shell");
    }

    #[test]
    fn shell_tool_description() {
        let tool = ShellTool::new(test_security(AutonomyLevel::Supervised), test_runtime());
        assert!(!tool.description().is_empty());
    }

    #[test]
    fn shell_tool_schema_has_command() {
        let tool = ShellTool::new(test_security(AutonomyLevel::Supervised), test_runtime());
        let schema = tool.parameters_schema();
        assert!(schema["properties"]["command"].is_object());
        assert!(
            schema["required"]
                .as_array()
                .expect("schema required field should be an array")
                .contains(&json!("command"))
        );
        // The runtime-owned `approved` arg is intentionally absent from the
        // schema: the model must never be told it can self-approve (RFC 7155).
        // `agent::set_runtime_approved_arg` is the only writer on the loop path.
        assert!(schema["properties"].get("approved").is_none());
    }

    #[cfg(all(any(unix, windows), not(target_os = "android")))]
    #[tokio::test]
    async fn rebuilt_shell_tool_adopts_reloaded_runtime_shell_dialect() {
        use crate::platform::{ShellDialect, create_runtime};
        use zeroclaw_config::schema::Config;

        let mut config = Config::default();
        #[cfg(target_os = "windows")]
        {
            config.runtime.shell = Some("cmd".into());
        }
        #[cfg(not(target_os = "windows"))]
        {
            config.runtime.shell = Some("sh".into());
        }

        let security = Arc::new(SecurityPolicy {
            workspace_dir: std::env::temp_dir(),
            ..SecurityPolicy::default()
        });
        let build_shell_tool = |config: &Config| {
            let runtime: Arc<dyn RuntimeAdapter> =
                Arc::from(create_runtime(&config.runtime).expect("runtime should rebuild"));
            ShellTool::new(security.clone(), runtime)
        };

        let before_reload = build_shell_tool(&config);
        #[cfg(target_os = "windows")]
        assert_eq!(
            before_reload.runtime.shell_dialect(),
            ShellDialect::WindowsCmd
        );
        #[cfg(not(target_os = "windows"))]
        assert_eq!(before_reload.runtime.shell_dialect(), ShellDialect::Posix);
        let before_result = before_reload
            .execute(json!({"command": "echo Env:NAME"}))
            .await
            .expect("pre-reload shell should return a tool result");
        assert!(
            before_result.success,
            "the pre-reload non-PowerShell policy should accept the probe: {before_result:?}"
        );

        // A daemon reload re-reads Config and rebuilds the subsystem graph.
        // Use an executable shim named `pwsh` on Unix so the real runtime
        // factory validates the reloaded value without requiring PowerShell to
        // be installed; policy rejection occurs before the shim can spawn.
        #[cfg(unix)]
        let powershell_dir = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        {
            let powershell = powershell_dir.path().join("pwsh");
            executable_file(&powershell);
            config.runtime.shell = Some(powershell.to_string_lossy().into_owned());
        }
        #[cfg(target_os = "windows")]
        {
            config.runtime.shell = Some("powershell".into());
        }

        let after_reload = build_shell_tool(&config);
        assert_eq!(
            after_reload.runtime.shell_dialect(),
            ShellDialect::PowerShell
        );
        let after_result = after_reload
            .execute(json!({"command": "echo Env:NAME", "approved": true}))
            .await
            .expect("reloaded shell should return a policy result");
        assert!(!after_result.success);
        assert!(
            after_result
                .error
                .as_deref()
                .is_some_and(|error| error.contains("not allowed")),
            "the rebuilt PowerShell path must apply provider-aware validation: {after_result:?}"
        );
    }

    #[tokio::test]
    async fn shell_stdin_is_eof_not_the_terminal() {
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            workspace_dir: std::env::temp_dir(),
            allowed_commands: vec![stdin_reader_command().into()],
            ..SecurityPolicy::default()
        });
        let tool = ShellTool::new(security, test_runtime());
        let fut = tool.execute(json!({"command": stdin_reader_command()}));
        let res = tokio::time::timeout(std::time::Duration::from_secs(10), fut).await;
        assert!(
            res.is_ok(),
            "a stdin-reading command hung — stdin is not null and may reach the terminal"
        );
        assert!(
            res.unwrap()
                .expect("stdin reader should return a result")
                .success
        );
    }

    #[tokio::test]
    async fn shell_executes_allowed_command() {
        let tool = ShellTool::new(test_security(AutonomyLevel::Supervised), test_runtime());
        let result = tool
            .execute(json!({"command": "echo hello"}))
            .await
            .expect("echo command execution should succeed");
        assert!(result.success);
        assert!(result.output.trim().contains("hello"));
        assert!(result.error.is_none());
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn shell_executes_windows_nul_redirect_through_cmd_exe() {
        // Native-Windows runtime boundary through the FULLY WRAPPED production
        // shape (`RateLimitedTool<ShellTool>`). `test_runtime()` is
        // `NativeRuntime`, which reports `WindowsCmd`, so ShellTool's own
        // dialect-aware validation accepts a redirect to the `nul` null device
        // and cmd.exe then resolves `nul` to the discard-only device. Because the
        // shell tool now owns its forbidden-path scan (no outer POSIX
        // `PathGuardedTool`), the `\\.\nul` device form is no longer rejected
        // ahead of the tool. Proves the allow decision AND real execution.
        let tool = wrapped_shell(test_security(AutonomyLevel::Supervised));

        // Bare `2>nul`: stderr is discarded, stdout is preserved, command succeeds.
        let result = tool
            .execute(json!({"command": "echo zeroclaw_nul_stdout 2>nul"}))
            .await
            .expect("`2>nul` command should return a result");
        assert!(
            result.success,
            "`2>nul` must be allowed and execute on Windows: {:?}",
            result.error
        );
        assert!(result.output.trim().contains("zeroclaw_nul_stdout"));
        assert!(result.error.is_none());

        // Full `\\.\nul` device form redirecting stdout: nothing is written to a
        // real workspace file, and the command still succeeds.
        let result = tool
            .execute(json!({"command": r"echo zeroclaw_dev >\\.\nul"}))
            .await
            .expect(r"`>\\.\nul` command should return a result");
        assert!(
            result.success,
            r"`>\\.\nul` must be allowed and execute on Windows: {:?}",
            result.error
        );
        assert!(result.error.is_none());
    }

    #[cfg(not(windows))]
    #[tokio::test]
    async fn shell_rejects_nul_redirect_through_posix_production_shape() {
        // The POSIX counterpart, through the same production shape. On a POSIX
        // sink (Unix native, Docker `sh -c`, cron `sh -c`) `nul` is an ordinary
        // relative filename, so a redirect to it — bare `nul` or the `\\.\nul`
        // device form — must stay blocked as an unsafe file redirect. This is the
        // boundary the fix protects: dropping the outer `PathGuardedTool` must not
        // weaken POSIX rejection, because ShellTool's own dialect-aware scan runs.
        let tool = wrapped_shell(test_security(AutonomyLevel::Supervised));
        for cmd in ["echo zeroclaw x >nul", r"echo zeroclaw x >\\.\nul"] {
            let result = tool
                .execute(json!({ "command": cmd }))
                .await
                .expect("command should return a result");
            assert!(
                !result.success,
                "POSIX must reject a redirect to `nul` as an unsafe file target: {cmd}"
            );
        }
    }

    #[tokio::test]
    async fn shell_reports_invalid_docker_workspace_root() {
        let workspace = tempfile::tempdir().expect("workspace tempdir should be created");
        let missing_root = workspace.path().join("missing-root");
        let missing_root_text = missing_root.to_string_lossy().into_owned();
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Full,
            workspace_dir: workspace.path().to_path_buf(),
            allowed_commands: vec!["*".into()],
            block_high_risk_commands: false,
            ..SecurityPolicy::default()
        });
        let runtime = Arc::new(DockerRuntime::new(DockerRuntimeConfig {
            allowed_workspace_roots: vec![missing_root_text.clone()],
            ..DockerRuntimeConfig::default()
        }));
        let tool = ShellTool::new(security, runtime);

        let result = tool
            .execute(json!({"command": "echo hello"}))
            .await
            .expect("invalid Docker root should return a tool result");

        assert!(!result.success);
        let error = result.error.as_deref().unwrap_or("");
        assert!(error.contains("Failed to canonicalize Docker workspace root"));
        assert!(error.contains(missing_root_text.as_str()));
    }

    #[tokio::test]
    async fn shell_blocks_disallowed_command() {
        let tool = ShellTool::new(test_security(AutonomyLevel::Supervised), test_runtime());
        let result = tool
            .execute(json!({"command": "rm -rf /"}))
            .await
            .expect("disallowed command execution should return a result");
        assert!(!result.success);
        let error = result.error.as_deref().unwrap_or("");
        // Which guard refuses first is dialect-dependent: on POSIX hosts the
        // allowlist/high-risk checks fire, while under the Windows shell
        // dialect the forbidden-path scan sees `/` (current-drive root) first
        // and refuses with its own message. All three are the required
        // refusal; none may be weakened into a pass.
        assert!(
            error.contains("not allowed")
                || error.contains("high-risk")
                || error.contains("forbidden path argument"),
            "expected a refusal reason, got: {error:?}"
        );
    }

    #[tokio::test]
    async fn shell_policy_blocks_powershell_native_high_risk_command() {
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Full,
            workspace_dir: std::env::temp_dir(),
            allowed_commands: vec!["*".into()],
            block_high_risk_commands: true,
            ..SecurityPolicy::default()
        });
        let (_powershell_dir, runtime) = powershell_test_runtime();
        let tool = ShellTool::new(security, runtime);

        let result = tool
            .execute(json!({"command": "Remove-Item important.txt"}))
            .await
            .expect("policy rejection should be returned as a tool result");

        assert!(!result.success);
        assert!(
            result
                .error
                .as_deref()
                .is_some_and(|error| error.contains("high-risk")),
            "PowerShell-native operation must be blocked before spawn: {:?}",
            result.error
        );
    }

    #[tokio::test]
    async fn wrapped_shell_blocks_windows_relative_path_for_powershell() {
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Full,
            workspace_dir: std::env::temp_dir(),
            allowed_commands: vec!["cat".into()],
            ..SecurityPolicy::default()
        });
        let (_powershell_dir, runtime) = powershell_test_runtime();
        let tool = RateLimitedTool::new(ShellTool::new(security.clone(), runtime), security);

        let result = tool
            .execute(json!({"command": "cat ..\\secret.txt"}))
            .await
            .expect("path rejection should be returned as a tool result");

        assert!(!result.success);
        assert!(
            result
                .error
                .as_deref()
                .is_some_and(|error| error.contains("forbidden path argument")),
            "Windows-relative path must be blocked before spawn: {:?}",
            result.error
        );
    }

    #[tokio::test]
    async fn wrapped_shell_blocks_powershell_stop_parsing_native_mutation() {
        // `git --% push` would reach native Git as `push` on PowerShell while
        // policy sees only `--%` as the first argument. The dialect-aware
        // validator must reject it before the process is ever built, so the
        // guard holds on this Unix host too.
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Full,
            workspace_dir: std::env::temp_dir(),
            allowed_commands: vec!["git".into()],
            block_high_risk_commands: true,
            ..SecurityPolicy::default()
        });
        let (_powershell_dir, runtime) = powershell_test_runtime();
        let tool = RateLimitedTool::new(ShellTool::new(security.clone(), runtime), security);

        let result = tool
            .execute(json!({"command": "git --% push origin main"}))
            .await
            .expect("policy rejection should be returned as a tool result");

        assert!(!result.success);
        assert!(
            result
                .error
                .as_deref()
                .is_some_and(|error| error.contains("not allowed by security policy")),
            "stop-parsing native mutation must be blocked before spawn: {:?}",
            result.error
        );
    }

    #[tokio::test]
    async fn wrapped_shell_blocks_powershell_mixed_quoted_provider_path() {
        // `cat E'nv:'PATH` binds as the `Env:PATH` provider read on PowerShell,
        // but the interior quote hides the `Env:` prefix from policy's raw-token
        // provider check. The bounded grammar rejects the mixed quoted/unquoted
        // token so it is blocked before the process is ever built.
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Full,
            workspace_dir: std::env::temp_dir(),
            allowed_commands: vec!["cat".into()],
            block_high_risk_commands: true,
            ..SecurityPolicy::default()
        });
        let (_powershell_dir, runtime) = powershell_test_runtime();
        let tool = RateLimitedTool::new(ShellTool::new(security.clone(), runtime), security);

        let result = tool
            .execute(json!({"command": "cat E'nv:'PATH"}))
            .await
            .expect("policy rejection should be returned as a tool result");

        assert!(!result.success);
        assert!(
            result
                .error
                .as_deref()
                .is_some_and(|error| error.contains("not allowed by security policy")),
            "mixed-quoted provider path must be blocked before spawn: {:?}",
            result.error
        );
    }

    #[cfg(target_os = "windows")]
    #[tokio::test]
    async fn powershell_agent_shell_executes_safe_pipeline_and_rejects_dangerous_alias() {
        let workspace = tempfile::TempDir::new().unwrap();
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Full,
            workspace_dir: workspace.path().to_path_buf(),
            allowed_commands: vec!["*".into()],
            block_high_risk_commands: true,
            ..SecurityPolicy::default()
        });
        let runtime: Arc<dyn RuntimeAdapter> =
            Arc::new(NativeRuntime::with_shell("powershell".into()));
        let tool = ShellTool::new(security, runtime);

        let safe = tool
            .execute(json!({
                    "command": "Write-Output \"quoted safe value\" | Select-Object -First 1"
            }))
            .await
            .expect("safe PowerShell pipeline should return a tool result");
        assert!(safe.success, "{:?}", safe.error);
        assert!(safe.output.contains("quoted safe value"), "{}", safe.output);

        let dangerous = tool
            .execute(json!({"command": "ac .\\blocked.txt value", "approved": true}))
            .await
            .expect("dangerous PowerShell alias should return a policy result");
        assert!(!dangerous.success);
        assert!(
            dangerous
                .error
                .as_deref()
                .is_some_and(|error| error.contains("high-risk")),
            "{:?}",
            dangerous.error
        );
        assert!(!workspace.path().join("blocked.txt").exists());
    }

    #[tokio::test]
    async fn shell_uses_runtime_dialect_to_reject_powershell_expression_bypass() {
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            workspace_dir: std::env::temp_dir(),
            allowed_commands: vec!["echo".into()],
            ..SecurityPolicy::default()
        });
        // Policy rejection happens before spawn, so this test does not require
        // pwsh to be installed on the host.
        let (_powershell_dir, runtime) = powershell_test_runtime();
        let tool = ShellTool::new(security, runtime);

        let result = tool
            .execute(json!({
                "command": "echo ([System.IO.File]::Delete('important.txt'))"
            }))
            .await
            .expect("policy rejection should be returned as a tool result");

        assert!(!result.success);
        assert!(
            result
                .error
                .as_deref()
                .is_some_and(|error| error.contains("not allowed")),
            "PowerShell expression must be rejected before spawn: {:?}",
            result.error
        );
    }

    #[tokio::test]
    async fn shell_blocks_readonly() {
        let tool = ShellTool::new(test_security(AutonomyLevel::ReadOnly), test_runtime());
        let result = tool
            .execute(json!({"command": "ls"}))
            .await
            .expect("readonly command execution should return a result");
        assert!(!result.success);
        assert!(
            result
                .error
                .as_ref()
                .expect("error field should be present for blocked command")
                .contains("not allowed")
        );
    }

    #[tokio::test]
    async fn shell_missing_command_param() {
        let tool = ShellTool::new(test_security(AutonomyLevel::Supervised), test_runtime());
        let result = tool.execute(json!({})).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("command"));
    }

    #[tokio::test]
    async fn shell_wrong_type_param() {
        let tool = ShellTool::new(test_security(AutonomyLevel::Supervised), test_runtime());
        let result = tool.execute(json!({"command": 123})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn shell_captures_exit_code() {
        let tool = ShellTool::new(test_security(AutonomyLevel::Supervised), test_runtime());
        let result = tool
            .execute(json!({"command": "ls /nonexistent_dir_xyz"}))
            .await
            .expect("command with nonexistent path should return a result");
        assert!(!result.success);
    }

    // ── Ephemeral-workspace warning────────────────

    #[tokio::test]
    async fn shell_warns_on_ephemeral_workspace() {
        let tool = ShellTool::new(test_security(AutonomyLevel::Supervised), test_runtime())
            .with_persistent_writes(false);
        let result = tool
            .execute(json!({"command": "echo hello"}))
            .await
            .expect("echo command should run");
        assert!(result.success);
        assert!(
            result.output.contains("EPHEMERAL WORKSPACE"),
            "ephemeral warning must be present in output, got: {}",
            result.output
        );
        assert!(
            result.output.contains("mount_workspace"),
            "warning must name the config key to fix it, got: {}",
            result.output
        );
        assert!(
            result.output.contains("hello"),
            "original command output must be preserved, got: {}",
            result.output
        );
    }

    #[tokio::test]
    async fn shell_warns_on_ephemeral_workspace_failure_path() {
        let tool = ShellTool::new(test_security(AutonomyLevel::Supervised), test_runtime())
            .with_persistent_writes(false);
        // Use a bare (non-path-looking) name so the command fails on a missing
        // directory rather than being stopped by the shell tool's own forbidden-
        // path guard. An absolute `/nonexistent...` would be rejected before it
        // ever runs, and this test is about the ephemeral banner on a *runtime*
        // failure, not about path gating.
        let result = tool
            .execute(json!({"command": "ls nonexistent_dir_xyz_4627"}))
            .await
            .expect("command should return a result");
        assert!(!result.success);
        assert!(
            result
                .error
                .as_deref()
                .unwrap_or("")
                .contains("EPHEMERAL WORKSPACE"),
            "ephemeral warning must reach the error field on failures, got: {:?}",
            result.error
        );
    }

    #[tokio::test]
    async fn shell_warns_on_ephemeral_success_with_stderr() {
        let tool = ShellTool::new(test_security(AutonomyLevel::Full), test_runtime())
            .with_persistent_writes(false);
        let result = tool
            .execute(json!({"command": success_with_stderr_command()}))
            .await
            .expect("command should run");
        assert!(
            result.success,
            "command should exit 0, got error: {:?}",
            result.error
        );
        assert!(
            result.output.contains("EPHEMERAL WORKSPACE") && result.output.contains("out"),
            "output must carry banner and preserve stdout, got: {}",
            result.output
        );
        let err = result.error.as_deref().unwrap_or("");
        assert!(
            err.contains("EPHEMERAL WORKSPACE") && err.contains("warn"),
            "error must carry banner and preserve stderr, got: {err:?}"
        );
    }

    #[tokio::test]
    async fn shell_no_warning_when_persistent() {
        let tool = ShellTool::new(test_security(AutonomyLevel::Supervised), test_runtime());
        let result = tool
            .execute(json!({"command": "echo hello"}))
            .await
            .expect("echo command should run");
        assert!(result.success);
        assert!(
            !result.output.contains("EPHEMERAL WORKSPACE"),
            "no ephemeral warning expected on a persistent runtime, got: {}",
            result.output
        );
    }

    #[tokio::test]
    async fn shell_blocks_absolute_path_argument() {
        let tool = wrapped_shell(test_security_with_allowed_commands(
            AutonomyLevel::Supervised,
            &["cat"],
        ));
        let result = tool
            .execute(json!({"command": format!("cat {}", absolute_path_outside_workspace())}))
            .await
            .expect("absolute path argument should be blocked");
        assert_path_argument_blocked(&result, "absolute path argument");
    }

    /// End-to-end regression for the shell workspace-boundary bypass: driving
    /// the REAL wrapped shell tool, a write through an in-workspace symlink
    /// pointing outside must be refused before the command runs, and nothing may
    /// be created at the target. This covers only the direct path-shaped
    /// redirect form the static scan can see; dynamic forms (scripts,
    /// expansion, races) are outside this layer.
    #[cfg(unix)]
    #[tokio::test]
    async fn shell_blocks_symlink_escape_end_to_end() {
        use std::os::unix::fs::symlink;

        let root = std::env::temp_dir().join(format!(
            "zeroclaw_shell_e2e_symlink_escape_{}",
            uuid::Uuid::new_v4()
        ));
        let workspace = root.join("workspace");
        let outside = root.join("outside");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        // `link` inside the workspace points at the outside directory.
        symlink(&outside, workspace.join("link")).unwrap();

        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Full,
            workspace_dir: workspace.clone(),
            allowed_commands: vec!["*".into()],
            block_high_risk_commands: false,
            ..SecurityPolicy::default()
        });
        let tool = wrapped_shell(security);

        let result = tool
            .execute(json!({"command": "echo pwned > link/escape.txt", "approved": true}))
            .await
            .expect("shell tool must return a result");

        assert!(!result.success, "the escaping write must be refused");
        assert!(
            result
                .error
                .as_deref()
                .unwrap_or("")
                .contains("Path blocked"),
            "expected a path-block error, got: {:?}",
            result.error
        );
        assert!(
            !outside.join("escape.txt").exists(),
            "no file may be written outside the workspace through the symlink"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn shell_blocks_option_assignment_path_argument() {
        let tool = wrapped_shell(test_security_with_allowed_commands(
            AutonomyLevel::Supervised,
            &["grep"],
        ));
        let result = tool
            .execute(json!({"command": format!("grep --file={} root ./src", absolute_path_outside_workspace())}))
            .await
            .expect("option-assigned forbidden path should be blocked");
        assert_path_argument_blocked(&result, "option-assigned forbidden path");
    }

    #[tokio::test]
    async fn shell_blocks_short_option_attached_path_argument() {
        let tool = wrapped_shell(test_security_with_allowed_commands(
            AutonomyLevel::Supervised,
            &["grep"],
        ));
        let result = tool
            .execute(json!({"command": format!("grep -f{} root ./src", absolute_path_outside_workspace())}))
            .await
            .expect("short option attached forbidden path should be blocked");
        assert_path_argument_blocked(&result, "short option attached forbidden path");
    }

    #[tokio::test]
    async fn shell_blocks_tilde_user_path_argument() {
        let tool = wrapped_shell(test_security_with_allowed_commands(
            AutonomyLevel::Supervised,
            &["cat"],
        ));
        let result = tool
            .execute(json!({"command": "cat ~root/.ssh/id_rsa"}))
            .await
            .expect("tilde-user path should be blocked");
        assert_path_argument_blocked(&result, "tilde-user path");
    }

    #[tokio::test]
    #[cfg(not(target_os = "windows"))]
    async fn shell_blocks_input_redirection_path_bypass() {
        let tool = ShellTool::new(test_security(AutonomyLevel::Supervised), test_runtime());
        let result = tool
            .execute(json!({"command": "cat </etc/passwd"}))
            .await
            .expect("input redirection bypass should be blocked");
        assert!(!result.success);
        assert!(
            result
                .error
                .as_deref()
                .unwrap_or("")
                .contains("not allowed")
        );
    }

    fn test_security_with_env_cmd() -> Arc<SecurityPolicy> {
        Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            workspace_dir: std::env::temp_dir(),
            allowed_commands: vec![env_print_command().into(), "echo".into()],
            ..SecurityPolicy::default()
        })
    }

    fn test_security_with_env_passthrough(vars: &[&str]) -> Arc<SecurityPolicy> {
        Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            workspace_dir: std::env::temp_dir(),
            allowed_commands: vec![env_print_command().into()],
            shell_env_passthrough: vars.iter().map(|v| (*v).to_string()).collect(),
            ..SecurityPolicy::default()
        })
    }

    #[cfg(target_os = "windows")]
    fn env_print_command() -> &'static str {
        "set"
    }

    #[cfg(not(target_os = "windows"))]
    fn env_print_command() -> &'static str {
        "env"
    }

    #[cfg(target_os = "windows")]
    fn home_env_key() -> &'static str {
        "USERPROFILE"
    }

    #[cfg(not(target_os = "windows"))]
    fn home_env_key() -> &'static str {
        "HOME"
    }

    #[cfg(target_os = "windows")]
    fn absolute_path_outside_workspace() -> &'static str {
        r"C:\Windows\win.ini"
    }

    #[cfg(not(target_os = "windows"))]
    fn absolute_path_outside_workspace() -> &'static str {
        "/etc/passwd"
    }

    fn env_output_contains_key(output: &str, key: &str) -> bool {
        output.lines().any(|line| {
            line.split_once('=')
                .is_some_and(|(name, _)| env_key_eq(name, key))
        })
    }

    fn env_output_contains_assignment(output: &str, key: &str, value: &str) -> bool {
        output.lines().any(|line| {
            line.split_once('=')
                .is_some_and(|(name, actual)| env_key_eq(name, key) && actual == value)
        })
    }

    #[cfg(target_os = "windows")]
    fn env_key_eq(actual: &str, expected: &str) -> bool {
        actual.eq_ignore_ascii_case(expected)
    }

    #[cfg(not(target_os = "windows"))]
    fn env_key_eq(actual: &str, expected: &str) -> bool {
        actual == expected
    }

    /// RAII guard that restores an environment variable to its original state on drop,
    /// ensuring cleanup even if the test panics.
    struct EnvGuard {
        key: &'static str,
        original: Option<String>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let original = std::env::var(key).ok();
            // SAFETY: test-only, single-threaded test runner.
            unsafe { std::env::set_var(key, value) };
            Self { key, original }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.original {
                // SAFETY: test-only, single-threaded test runner.
                Some(val) => unsafe { std::env::set_var(self.key, val) },
                // SAFETY: test-only, single-threaded test runner.
                None => unsafe { std::env::remove_var(self.key) },
            }
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn shell_does_not_leak_api_key() {
        let _g1 = EnvGuard::set("API_KEY", "sk-test-secret-12345");
        let _g2 = EnvGuard::set("ZEROCLAW_API_KEY", "sk-test-secret-67890");

        let tool = ShellTool::new(test_security_with_env_cmd(), test_runtime());
        let result = tool
            .execute(json!({"command": env_print_command()}))
            .await
            .expect("environment print command should succeed");
        assert!(result.success);
        assert!(
            !result.output.contains("sk-test-secret-12345"),
            "API_KEY leaked to shell command output"
        );
        assert!(
            !result.output.contains("sk-test-secret-67890"),
            "ZEROCLAW_API_KEY leaked to shell command output"
        );
    }

    #[tokio::test]
    async fn shell_preserves_path_and_home_for_env_command() {
        let tool = ShellTool::new(test_security_with_env_cmd(), test_runtime());

        let result = tool
            .execute(json!({"command": env_print_command()}))
            .await
            .expect("environment print command should succeed");
        assert!(result.success);
        assert!(
            env_output_contains_key(&result.output, home_env_key()),
            "{} should be available in shell environment",
            home_env_key()
        );
        assert!(
            env_output_contains_key(&result.output, "PATH"),
            "PATH should be available in shell environment"
        );
    }

    #[tokio::test]
    #[cfg(not(target_os = "windows"))]
    async fn shell_blocks_plain_variable_expansion() {
        let tool = ShellTool::new(test_security_with_env_cmd(), test_runtime());
        let result = tool
            .execute(json!({"command": "echo $HOME"}))
            .await
            .expect("plain variable expansion should be blocked");
        assert!(!result.success);
        assert!(
            result
                .error
                .as_deref()
                .unwrap_or("")
                .contains("not allowed")
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn shell_allows_configured_env_passthrough() {
        let _guard = EnvGuard::set("ZEROCLAW_TEST_PASSTHROUGH", "db://unit-test");
        let tool = ShellTool::new(
            test_security_with_env_passthrough(&["ZEROCLAW_TEST_PASSTHROUGH"]),
            test_runtime(),
        );

        let result = tool
            .execute(json!({"command": env_print_command()}))
            .await
            .expect("environment print command should succeed");
        assert!(result.success);
        assert!(env_output_contains_assignment(
            &result.output,
            "ZEROCLAW_TEST_PASSTHROUGH",
            "db://unit-test"
        ));
    }

    #[test]
    fn invalid_shell_env_passthrough_names_are_filtered() {
        let security = SecurityPolicy {
            shell_env_passthrough: vec![
                "VALID_NAME".into(),
                "BAD-NAME".into(),
                "1NOPE".into(),
                "ALSO_VALID".into(),
            ],
            ..SecurityPolicy::default()
        };
        let vars = collect_allowed_shell_env_vars(&security);
        assert!(vars.contains(&"VALID_NAME".to_string()));
        assert!(vars.contains(&"ALSO_VALID".to_string()));
        assert!(!vars.contains(&"BAD-NAME".to_string()));
        assert!(!vars.contains(&"1NOPE".to_string()));
    }

    #[tokio::test]
    async fn shell_requires_approval_for_medium_risk_command() {
        use zeroclaw_api::permission::{ConsumeOutcome, RouteId};

        let workspace = tempfile::TempDir::new().unwrap();
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            allowed_commands: vec![medium_risk_write_base().into()],
            workspace_dir: workspace.path().to_path_buf(),
            ..SecurityPolicy::default()
        });

        let stable_environment = SAFE_SHELL_ENV_VARS
            .iter()
            .filter_map(|name| {
                std::env::var(name)
                    .ok()
                    .map(|value| ((*name).to_string(), value))
            })
            .collect();
        let tool =
            ShellTool::new(security.clone(), test_runtime()).with_tui_env(Some(stable_environment));
        let denied = tool
            .execute(json!({"command": medium_risk_write_command()}))
            .await
            .expect("unapproved command should return a result");
        assert!(!denied.success);
        assert!(
            denied
                .error
                .as_deref()
                .unwrap_or("")
                .contains("operator approval")
        );

        let profile = zeroclaw_config::schema::RiskProfileConfig::default();
        let manager = crate::approval::ApprovalManager::from_risk_profile(&profile);
        manager.set_policy_context(Arc::clone(&security), tool.runtime.shell_dialect());
        manager.set_shell_execution_context(tool.execution_facts_resolver());
        let facts = manager
            .shell_fingerprint_facts(medium_risk_write_command())
            .unwrap();
        assert_eq!(
            facts,
            manager
                .shell_fingerprint_facts(medium_risk_write_command())
                .unwrap(),
            "approval facts must be stable before minting"
        );
        let confirmation = manager.mint_confirmation(&facts, RouteId::cli(), 60);
        let authorization_args = json!({
            "command": medium_risk_write_command(),
            "__zeroclaw_confirmation_id": confirmation.confirmation_id.to_string(),
        });
        let (outcome, fingerprint, expires_at) =
            manager.authorize_shell_execution(&authorization_args);
        assert_eq!(
            outcome,
            crate::approval::ShellAuthorizationOutcome::Confirmation(ConsumeOutcome::Consumed)
        );

        let allowed = tool
            .execute(json!({
                "command": medium_risk_write_command(),
                "__zeroclaw_confirmation_consumed": true,
                "__zeroclaw_confirmation_fingerprint": fingerprint.unwrap().as_hex(),
                "__zeroclaw_confirmation_expires_at": expires_at.unwrap(),
            }))
            .await
            .expect("approved command execution should succeed");
        assert!(allowed.success, "{:?}", allowed.error);
    }

    // ── shell timeout enforcement tests ─────────────────

    #[test]
    fn shell_timeout_can_be_overridden() {
        let tool = ShellTool::new(test_security(AutonomyLevel::Supervised), test_runtime())
            .with_timeout_secs(120);
        assert_eq!(tool.timeout_secs, 120);
    }

    #[test]
    fn shell_output_limit_is_1mb() {
        assert_eq!(
            MAX_OUTPUT_BYTES, 1_048_576,
            "max output must be 1 MB to prevent OOM"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn shell_drains_large_stdout_while_child_runs() {
        let tool =
            ShellTool::new(unrestricted_shell_test_security(), test_runtime()).with_timeout_secs(2);
        let result = tool
            .execute(json!({
                "command": "awk 'BEGIN { for (i = 0; i < 200000; i++) printf \"x\" }'"
            }))
            .await
            .unwrap();

        assert!(
            result.success,
            "large stdout command should not time out: {:?}",
            result.error
        );
        assert_eq!(
            result.output.len(),
            200_000,
            "stdout should be drained while the child is still running"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn shell_marks_stdout_truncated_after_limit() {
        let tool =
            ShellTool::new(unrestricted_shell_test_security(), test_runtime()).with_timeout_secs(2);
        let result = tool
            .execute(json!({
                "command": "awk 'BEGIN { for (i = 0; i < 1048600; i++) printf \"x\" }'"
            }))
            .await
            .unwrap();

        assert!(
            result.success,
            "large stdout command should complete: {:?}",
            result.error
        );
        assert!(
            result.output.ends_with("\n... [output truncated at 1MB]"),
            "stdout should retain the truncation marker after the drain cap"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn shell_marks_stderr_truncated_after_limit() {
        let tool =
            ShellTool::new(unrestricted_shell_test_security(), test_runtime()).with_timeout_secs(2);
        let result = tool
            .execute(json!({
                "command": "awk 'BEGIN { for (i = 0; i < 1048600; i++) printf \"x\" }' 1>&2"
            }))
            .await
            .unwrap();

        assert!(
            result.success,
            "large stderr command should complete: {:?}",
            result.error
        );
        assert!(
            result
                .error
                .as_deref()
                .unwrap_or("")
                .ends_with("\n... [stderr truncated at 1MB]"),
            "stderr should retain the truncation marker after the drain cap"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn shell_keeps_output_when_grandchild_holds_pipe_open() {
        let tool =
            ShellTool::new(unrestricted_shell_test_security(), test_runtime()).with_timeout_secs(2);
        let result = tool
            .execute(json!({"command": "printf done; (sleep 1) &"}))
            .await
            .unwrap();

        assert!(
            result.success,
            "main shell process should complete: {:?}",
            result.error
        );
        assert!(
            result.output.contains("done"),
            "output drained before EOF should be preserved when a grandchild holds the pipe open"
        );
    }

    // ── Non-UTF8 binary output tests ────────────────────

    #[test]
    #[cfg(not(target_os = "windows"))]
    fn decode_output_valid_utf8_roundtrips() {
        let input = "hello 世界 🌍".as_bytes();
        assert_eq!(super::decode_output(input), "hello 世界 🌍");
    }

    #[test]
    fn decode_output_invalid_utf8_uses_replacement_chars() {
        // 0xFF is not valid UTF-8
        let input = b"hello\xFF world";
        let result = super::decode_output(input);
        // Must not panic; non-UTF-8 bytes become replacement characters on non-Windows
        assert!(result.contains("hello"));
        assert!(result.contains("world"));
    }

    #[test]
    fn decode_output_empty_bytes_returns_empty_string() {
        assert_eq!(super::decode_output(b""), "");
    }

    #[test]
    fn windows_code_page_mapping_covers_cjk() {
        use super::windows_code_page_to_encoding;
        assert_eq!(windows_code_page_to_encoding(936), encoding_rs::GBK);
        assert_eq!(windows_code_page_to_encoding(932), encoding_rs::SHIFT_JIS);
        assert_eq!(windows_code_page_to_encoding(949), encoding_rs::EUC_KR);
        assert_eq!(windows_code_page_to_encoding(950), encoding_rs::BIG5);
    }

    #[test]
    fn windows_code_page_mapping_utf8_variants() {
        use super::windows_code_page_to_encoding;
        assert_eq!(windows_code_page_to_encoding(65001), encoding_rs::UTF_8);
        assert_eq!(windows_code_page_to_encoding(20127), encoding_rs::UTF_8);
    }

    #[test]
    fn windows_code_page_mapping_unknown_falls_back_to_utf8() {
        use super::windows_code_page_to_encoding;
        assert_eq!(windows_code_page_to_encoding(99999), encoding_rs::UTF_8);
    }

    #[test]
    fn decode_output_with_cp936_gbk_bytes_transcodes_to_utf8() {
        // GBK encoding of "你好" is [0xC4, 0xE3, 0xBA, 0xC3]
        let gbk_bytes: &[u8] = &[0xC4, 0xE3, 0xBA, 0xC3];
        let decoded = super::decode_output_with_code_page(gbk_bytes, 936);
        assert_eq!(decoded, "你好");
        assert!(!decoded.contains('\u{FFFD}'));
    }

    #[tokio::test]
    async fn shell_blocks_rate_limited() {
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            max_actions_per_hour: 0,
            workspace_dir: std::env::temp_dir(),
            ..SecurityPolicy::default()
        });
        let tool = wrapped_shell(security);
        let result = tool
            .execute(json!({"command": "echo test"}))
            .await
            .expect("rate-limited command should return a result");
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap_or("").contains("Rate limit"));
    }

    #[tokio::test]
    async fn shell_handles_nonexistent_command() {
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Full,
            workspace_dir: std::env::temp_dir(),
            ..SecurityPolicy::default()
        });
        let tool = ShellTool::new(security, test_runtime());
        let result = tool
            .execute(json!({"command": "nonexistent_binary_xyz_12345"}))
            .await
            .unwrap();
        assert!(!result.success);
    }

    #[tokio::test]
    async fn shell_captures_stderr_output() {
        let tool = ShellTool::new(test_security(AutonomyLevel::Full), test_runtime());
        let result = tool
            .execute(json!({"command": "echo error_msg >&2"}))
            .await
            .unwrap();
        assert!(result.error.as_deref().unwrap_or("").contains("error_msg"));
    }

    #[tokio::test]
    async fn shell_record_action_budget_exhaustion() {
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Full,
            max_actions_per_hour: 1,
            workspace_dir: std::env::temp_dir(),
            ..SecurityPolicy::default()
        });
        let tool = wrapped_shell(security);

        let r1 = tool
            .execute(json!({"command": "echo first"}))
            .await
            .unwrap();
        assert!(r1.success);

        let r2 = tool
            .execute(json!({"command": "echo second"}))
            .await
            .unwrap();
        assert!(!r2.success);
        assert!(
            r2.error.as_deref().unwrap_or("").contains("Rate limit")
                || r2.error.as_deref().unwrap_or("").contains("budget")
        );
    }

    // ── Sandbox integration tests ────────────────────────

    #[test]
    fn shell_tool_can_be_constructed_with_sandbox() {
        use crate::security::NoopSandbox;

        let sandbox: Arc<dyn Sandbox> = Arc::new(NoopSandbox);
        let tool = ShellTool::new_with_sandbox(
            test_security(AutonomyLevel::Supervised),
            test_runtime(),
            sandbox,
        );
        assert_eq!(tool.name(), "shell");
    }

    #[test]
    fn noop_sandbox_does_not_modify_command() {
        use crate::security::NoopSandbox;

        let sandbox = NoopSandbox;
        let mut cmd = std::process::Command::new("echo");
        cmd.arg("hello");

        let program_before = cmd.get_program().to_os_string();
        let args_before: Vec<_> = cmd.get_args().map(|a| a.to_os_string()).collect();

        sandbox
            .wrap_command(&mut cmd)
            .expect("wrap_command should succeed");

        assert_eq!(cmd.get_program(), program_before);
        assert_eq!(
            cmd.get_args().map(|a| a.to_os_string()).collect::<Vec<_>>(),
            args_before
        );
    }

    #[tokio::test]
    async fn shell_executes_with_sandbox() {
        use crate::security::NoopSandbox;

        let sandbox: Arc<dyn Sandbox> = Arc::new(NoopSandbox);
        let tool = ShellTool::new_with_sandbox(
            test_security(AutonomyLevel::Supervised),
            test_runtime(),
            sandbox,
        );
        let result = tool
            .execute(json!({"command": "echo sandbox_test"}))
            .await
            .expect("command with sandbox should succeed");
        assert!(result.success);
        assert!(result.output.contains("sandbox_test"));
    }

    // ── TUI env overlay tests ─────────────────────────────────────

    #[tokio::test(flavor = "current_thread")]
    async fn shell_tui_env_is_passed_to_subprocess() {
        // A var that is NOT in SAFE_SHELL_ENV_VARS and NOT in passthrough —
        // it should only appear if tui_env injects it.
        let tool =
            ShellTool::new(test_security_with_env_cmd(), test_runtime()).with_tui_env(Some({
                let mut m = std::collections::HashMap::new();
                m.insert("ZC_TUI_TEST_VAR".to_string(), "tui_injected".to_string());
                m
            }));

        let result = tool
            .execute(json!({"command": env_print_command()}))
            .await
            .expect("environment print command should succeed");

        assert!(result.success);
        assert!(
            env_output_contains_assignment(&result.output, "ZC_TUI_TEST_VAR", "tui_injected"),
            "tui_env var should appear in subprocess env, got:\n{}",
            result.output
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn shell_without_tui_env_does_not_inject_extra_vars() {
        // Without tui_env, a non-safe var must NOT appear.
        let tool = ShellTool::new(test_security_with_env_cmd(), test_runtime());

        let result = tool
            .execute(json!({"command": env_print_command()}))
            .await
            .expect("environment print command should succeed");

        assert!(result.success);
        assert!(
            !result.output.contains("ZC_TUI_TEST_VAR"),
            "non-safe var must not leak without tui_env"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn shell_tui_env_overrides_safe_var() {
        // tui_env wins over the process-level value for a var that is also in SAFE_SHELL_ENV_VARS.
        // This lets the TUI's PATH (e.g. with nix/brew) win over the daemon's PATH.
        let home_key = home_env_key();
        let _guard = EnvGuard::set(home_key, "daemon-home");

        let tool =
            ShellTool::new(test_security_with_env_cmd(), test_runtime()).with_tui_env(Some({
                let mut m = std::collections::HashMap::new();
                m.insert(home_key.to_string(), "tui-home".to_string());
                m
            }));

        let result = tool
            .execute(json!({"command": env_print_command()}))
            .await
            .expect("environment print command should succeed");

        assert!(
            result.success,
            "env should succeed, got output={:?} error={:?}",
            result.output, result.error
        );
        assert!(
            env_output_contains_assignment(&result.output, home_key, "tui-home"),
            "tui_env {home_key} should override daemon {home_key}, got:\n{}",
            result.output
        );
        assert!(
            !env_output_contains_assignment(&result.output, home_key, "daemon-home"),
            "daemon {home_key} must not leak through when tui_env overrides it, got:\n{}",
            result.output
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn shell_tui_env_none_behaves_like_existing() {
        // with_tui_env(None) must be identical to no tui_env at all —
        // only SAFE_SHELL_ENV_VARS + passthrough reach the subprocess.
        let tool = ShellTool::new(test_security_with_env_cmd(), test_runtime()).with_tui_env(None);

        let result = tool
            .execute(json!({"command": env_print_command()}))
            .await
            .expect("environment print command should succeed");

        assert!(result.success);
        assert!(
            !result.output.contains("ZC_TUI_TEST_VAR"),
            "None tui_env must not inject anything extra"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn shell_tui_env_secrets_reach_subprocess_but_not_safe_list() {
        // The whole point: secrets from the TUI env (e.g. SSH_AUTH_SOCK)
        // DO reach the subprocess via tui_env even though they are not
        // in SAFE_SHELL_ENV_VARS.
        let tool =
            ShellTool::new(test_security_with_env_cmd(), test_runtime()).with_tui_env(Some({
                let mut m = std::collections::HashMap::new();
                m.insert("SSH_AUTH_SOCK".to_string(), "/tmp/fake.sock".to_string());
                m
            }));

        // Confirm SSH_AUTH_SOCK is not in the safe list (would be a bug if it were)
        assert!(
            !SAFE_SHELL_ENV_VARS.contains(&"SSH_AUTH_SOCK"),
            "SSH_AUTH_SOCK must not be in SAFE_SHELL_ENV_VARS"
        );

        let result = tool
            .execute(json!({"command": env_print_command()}))
            .await
            .expect("environment print command should succeed");

        assert!(result.success);
        assert!(
            env_output_contains_assignment(&result.output, "SSH_AUTH_SOCK", "/tmp/fake.sock"),
            "SSH_AUTH_SOCK from tui_env must reach subprocess"
        );
    }
}
