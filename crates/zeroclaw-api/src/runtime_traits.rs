use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// Shell language understood by a runtime's command builder.
///
/// This is part of the execution boundary: security policy must validate the
/// same language that [`RuntimeAdapter::build_shell_command`] will interpret.
///
/// The command-risk policy needs this to apply platform-specific safety rules —
/// notably the null device: a POSIX shell treats `nul` as an ordinary relative
/// filename (so `echo x >nul` would create/truncate a workspace file), while
/// Windows `cmd.exe` resolves it to the discard-only null device. A redirect to
/// `nul` is therefore only safe under [`ShellDialect::WindowsCmd`].
///
/// The dialect follows the *effective execution sink*, not merely the host OS.
/// Docker always runs through `sh -c` and stays [`ShellDialect::Posix`] even on
/// a Windows host. Native cron execution follows the configured runtime dialect.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ShellDialect {
    /// POSIX `sh`/`bash` semantics — Unix native execution and Docker `sh -c`.
    /// The conservative default.
    #[default]
    Posix,
    /// Windows `cmd.exe` semantics — native execution on a Windows host.
    WindowsCmd,
    /// Windows PowerShell or PowerShell 7+.
    PowerShell,
    /// The runtime does not expose shell execution.
    None,
}

/// Tools whose arguments carry a model-authored shell command.
///
/// The dialect guidance is worth its tokens exactly when the model writes a
/// command that a shell will parse. That is not only the `shell` tool: the
/// cron and schedule tools take a `command` argument that runs through the
/// same interpreter and the same dialect validation, so an agent holding only
/// those still needs to be told which language to write.
pub const SHELL_COMMAND_TOOLS: &[&str] = &["shell", "cron_add", "cron_update", "schedule"];

/// Report whether any tool in `tool_names` accepts a model-authored shell
/// command, and therefore whether the shell dialect guidance is worth
/// rendering.
pub fn needs_shell_dialect_guidance<'a>(tool_names: impl IntoIterator<Item = &'a str>) -> bool {
    tool_names
        .into_iter()
        .any(|name| SHELL_COMMAND_TOOLS.contains(&name))
}

/// Deletion advice for POSIX shells, and the fallback for runtimes with no
/// shell at all (which render the Safety section but spawn nothing).
pub const POSIX_DELETION_GUIDANCE: &str =
    "- Prefer `trash` over `rm` (recoverable beats gone forever).\n";

/// What the system prompt tells the model about the runtime's shell: the
/// interpreter's name and the language it speaks.
///
/// Both halves come from the adapter that builds the command, so the name the
/// model reads and the dialect the guidance is written for cannot disagree
/// with each other or with what actually executes. The prompt layer renders
/// this; it never re-derives the dialect by parsing the name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellProfile {
    /// Interpreter name as the operator configured it (`bash`, `zsh`, `pwsh`,
    /// `cmd`), stripped of any directory prefix and `.exe` suffix.
    pub name: String,
    /// Language that interpreter speaks. Never [`ShellDialect::None`] — a
    /// shell-less runtime has no profile at all.
    pub dialect: ShellDialect,
}

impl ShellProfile {
    /// Build the profile for an adapter whose interpreter is fixed by its
    /// dialect, naming it canonically. Returns `None` for
    /// [`ShellDialect::None`] so shell-less runtimes report no shell rather
    /// than naming one they cannot spawn.
    ///
    /// Adapters with a configurable interpreter should construct the profile
    /// directly instead, so the operator's chosen name survives.
    #[must_use]
    pub fn from_dialect(dialect: ShellDialect) -> Option<Self> {
        let name = match dialect {
            ShellDialect::Posix => "sh",
            ShellDialect::WindowsCmd => "cmd",
            ShellDialect::PowerShell => "powershell",
            ShellDialect::None => return None,
        };
        Some(Self {
            name: name.to_string(),
            dialect,
        })
    }

    /// Render the `## Shell` prompt section: the interpreter's name plus the
    /// command forms that dialect actually accepts.
    ///
    /// Naming the shell alone leaves the model to recall the dialect's syntax
    /// from priors, which is where the same session emits `Get-ChildItem` on
    /// one turn and `dir /a` on the next. The verbs listed are the ones a
    /// first turn reaches for (list, read, find, search), chosen because a
    /// mismatch there fails on syntax rather than on policy, and reads as an
    /// ordinary command error the model retries in the same wrong dialect.
    ///
    /// Each line states the form to use rather than enumerating the wrong
    /// ones: naming a command is what steers, and the "not X, not Y" tails
    /// cost tokens on every request to repeat what the correct form already
    /// implies. POSIX gets a name and no list at all, since `ls`/`cat`/`grep`
    /// are already what the model reaches for and the POSIX shells agree on
    /// them. Deletion guidance lives in [`Self::safe_deletion_guidance`],
    /// which the Safety section renders whether or not this one does.
    #[must_use]
    pub fn prompt_section(&self) -> String {
        let name = &self.name;
        match self.dialect {
            // Nothing to correct: the POSIX tool names are the ones the model
            // already reaches for, and `sh`/`bash`/`zsh` agree on them.
            ShellDialect::Posix | ShellDialect::None => {
                format!("## Shell\n\nCommands run through `{name}`.\n")
            }
            ShellDialect::WindowsCmd => format!(
                "## Shell\n\n\
                 Commands run through `{name}` (`cmd.exe`), so write `cmd` builtins rather than \
                 PowerShell cmdlets or POSIX tools:\n\
                 - list: `dir /a`\n\
                 - read: `type file.txt`\n\
                 - find files: `dir /s /b pattern`\n\
                 - search text: `findstr pattern file`\n"
            ),
            ShellDialect::PowerShell => format!(
                "## Shell\n\n\
                 Commands run through `{name}` ({version}), so write cmdlets rather than `cmd` \
                 builtins or POSIX tools:\n\
                 - list: `Get-ChildItem -Force`\n\
                 - read: `Get-Content file.txt`\n\
                 - find files: `Get-ChildItem -Recurse -Filter pattern`\n\
                 - search text: `Select-String -Pattern p -Path f`\n\
                 Spell cmdlets out: `ls`, `cat`, and `rm` are aliases whose parameters differ from \
                 the POSIX tools of the same name.\n",
                version = if name == "pwsh" {
                    "PowerShell 7+, where `&&`/`||` chains and ternaries work"
                } else {
                    "Windows PowerShell 5.1, which has no `&&`/`||` chains or ternaries"
                }
            ),
        }
    }

    /// Return the Safety section's deletion advice for this dialect.
    ///
    /// `trash` is a POSIX-only convenience, so repeating "prefer `trash` over
    /// `rm`" to a PowerShell or `cmd.exe` session names a command that is not
    /// there and leaves the actually-destructive one unaddressed. Each dialect
    /// gets the closest equivalent: PowerShell can preview with `-WhatIf`,
    /// `cmd.exe` has no recoverable form and gets a confirm-first warning.
    #[must_use]
    pub fn safe_deletion_guidance(&self) -> &'static str {
        match self.dialect {
            ShellDialect::Posix | ShellDialect::None => POSIX_DELETION_GUIDANCE,
            ShellDialect::PowerShell => {
                "- `Remove-Item` is not recoverable; preview with `-WhatIf` before deleting.\n"
            }
            ShellDialect::WindowsCmd => {
                "- `del` and `rmdir /s` are not recoverable; confirm the target before deleting.\n"
            }
        }
    }
}

/// Runtime adapter that abstracts platform differences for the agent.
///
/// Implement this trait to port the agent to a new execution environment.
/// The adapter declares platform capabilities (shell access, filesystem,
/// long-running processes) and provides platform-specific implementations
/// for operations like spawning shell commands. The orchestration loop
/// queries these capabilities to adapt its behavior—for example, disabling
/// tool execution on runtimes without shell access.
///
/// Implementations must be `Send + Sync` because the adapter is shared
/// across async tasks on the Tokio runtime.
pub trait RuntimeAdapter: Send + Sync {
    /// Return the human-readable name of this runtime environment.
    ///
    /// Used in logs and diagnostics (e.g., `"native"`, `"docker"`,
    /// `"cloudflare-workers"`).
    fn name(&self) -> &str;

    /// Report whether this runtime supports shell command execution.
    ///
    /// Shell capability is derived from [`Self::shell_dialect`] so adapters
    /// cannot report a shell while omitting the language that policy must
    /// validate (or report a language while disabling shell tools).
    fn has_shell_access(&self) -> bool {
        self.shell_dialect() != ShellDialect::None
    }

    /// Report whether this runtime supports filesystem read/write.
    ///
    /// When `false`, the agent disables file-based tools and falls back to
    /// in-memory storage.
    fn has_filesystem_access(&self) -> bool;

    /// Return the base directory for persistent storage on this runtime.
    ///
    /// Memory backends, logs, and other artifacts are stored under this path.
    /// Implementations should return a platform-appropriate writable directory.
    fn storage_path(&self) -> PathBuf;

    /// Report whether this runtime supports long-running background processes.
    ///
    /// When `true`, the agent may start the gateway server, heartbeat loop,
    /// and other persistent tasks. Serverless runtimes with short execution
    /// limits should return `false`.
    fn supports_long_running(&self) -> bool;

    /// Return the maximum memory budget in bytes for this runtime.
    ///
    /// A value of `0` (the default) indicates no limit. Constrained
    /// environments (embedded, serverless) should return their actual
    /// memory ceiling so the agent can adapt buffer sizes and caching.
    fn memory_budget(&self) -> u64 {
        0
    }

    /// Return the shell language accepted by [`Self::build_shell_command`].
    ///
    /// This is the source of truth for both shell capability and command
    /// policy. Adapters without shell access must return [`ShellDialect::None`].
    ///
    /// An adapter must report the dialect it *actually* runs under, because the
    /// command-risk policy consults this to decide platform-specific safety
    /// (e.g. accepting a redirect to the `nul` null device). Docker executes
    /// via `sh -c` and therefore stays POSIX even on Windows; native cron jobs
    /// follow the configured runtime dialect.
    fn shell_dialect(&self) -> ShellDialect;

    /// Return what to tell the model about this runtime's shell, or `None`
    /// when the runtime has no shell.
    ///
    /// The system prompt renders this so the model writes commands in the
    /// language that will actually interpret them, instead of guessing from
    /// the OS name (which does not distinguish `cmd.exe` from PowerShell on
    /// Windows). Because it is answered by the same adapter that builds the
    /// command, the reported shell cannot drift from the executed one.
    ///
    /// The default derives from [`Self::shell_dialect`], which is correct for
    /// adapters with a fixed interpreter. Adapters that let the operator
    /// choose an interpreter should override this to name the configured one:
    /// `sh`/`bash`/`zsh` differ under POSIX, as do `pwsh` (7+) and
    /// `powershell` (5.1) under PowerShell.
    fn shell_profile(&self) -> Option<ShellProfile> {
        ShellProfile::from_dialect(self.shell_dialect())
    }

    /// Build a shell command process configured for this runtime.
    ///
    /// Constructs a [`tokio::process::Command`] that will execute `command`
    /// with `workspace_dir` as the working directory. Implementations may
    /// prepend sandbox wrappers, set environment variables, or redirect
    /// I/O as appropriate for the platform.
    ///
    /// # Errors
    ///
    /// Returns an error if the runtime does not support shell access or if
    /// the command cannot be constructed (e.g., missing shell binary).
    fn build_shell_command(
        &self,
        command: &str,
        workspace_dir: &Path,
    ) -> anyhow::Result<tokio::process::Command>;

    /// Build a shell command process with runtime-visible environment names.
    ///
    /// `env_keys` contains variable names selected by the caller for
    /// passthrough. Implementations that need explicit forwarding, such as
    /// container runtimes, should pass only these names across their runtime
    /// boundary and rely on the spawned process environment for the values.
    fn build_shell_command_with_env_keys(
        &self,
        command: &str,
        workspace_dir: &Path,
        env_keys: &[&OsStr],
    ) -> anyhow::Result<tokio::process::Command> {
        let _ = env_keys;
        self.build_shell_command(command, workspace_dir)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct DummyRuntime;

    impl RuntimeAdapter for DummyRuntime {
        fn name(&self) -> &str {
            "dummy-runtime"
        }

        fn has_filesystem_access(&self) -> bool {
            true
        }

        fn storage_path(&self) -> PathBuf {
            PathBuf::from("/tmp/dummy-runtime")
        }

        fn supports_long_running(&self) -> bool {
            true
        }

        fn shell_dialect(&self) -> ShellDialect {
            #[cfg(windows)]
            {
                ShellDialect::WindowsCmd
            }
            #[cfg(not(windows))]
            {
                ShellDialect::Posix
            }
        }

        fn build_shell_command(
            &self,
            command: &str,
            workspace_dir: &Path,
        ) -> anyhow::Result<tokio::process::Command> {
            #[cfg(windows)]
            let mut cmd = {
                let mut cmd = tokio::process::Command::new("cmd");
                cmd.args(["/C", "echo", command]);
                cmd
            };

            #[cfg(not(windows))]
            let mut cmd = tokio::process::Command::new("echo");
            #[cfg(not(windows))]
            cmd.arg(command);

            cmd.current_dir(workspace_dir);
            Ok(cmd)
        }
    }

    /// A shell-less adapter, so the `shell_label` default can be checked for
    /// the [`ShellDialect::None`] case without a real serverless runtime.
    struct ShelllessRuntime;

    impl RuntimeAdapter for ShelllessRuntime {
        fn name(&self) -> &str {
            "shell-less-runtime"
        }

        fn has_filesystem_access(&self) -> bool {
            false
        }

        fn storage_path(&self) -> PathBuf {
            PathBuf::from("/tmp/shell-less-runtime")
        }

        fn supports_long_running(&self) -> bool {
            false
        }

        fn shell_dialect(&self) -> ShellDialect {
            ShellDialect::None
        }

        fn build_shell_command(
            &self,
            _command: &str,
            _workspace_dir: &Path,
        ) -> anyhow::Result<tokio::process::Command> {
            anyhow::bail!("shell-less runtime does not support shell commands")
        }
    }

    #[test]
    fn default_memory_budget_is_zero() {
        let runtime = DummyRuntime;
        assert_eq!(runtime.memory_budget(), 0);
    }

    #[test]
    fn dummy_runtime_shell_dialect_matches_compilation_platform() {
        let runtime = DummyRuntime;
        #[cfg(windows)]
        assert_eq!(runtime.shell_dialect(), ShellDialect::WindowsCmd);
        #[cfg(not(windows))]
        assert_eq!(runtime.shell_dialect(), ShellDialect::Posix);
    }

    #[test]
    fn default_shell_profile_follows_the_dialect() {
        // The default naming is what adapters with a fixed interpreter report
        // to the model. Adapters that let the operator pick an interpreter
        // override this to name the configured one.
        let runtime = DummyRuntime;
        let profile = runtime.shell_profile().expect("shell-capable runtime");
        #[cfg(windows)]
        {
            assert_eq!(profile.name, "cmd");
            assert_eq!(profile.dialect, ShellDialect::WindowsCmd);
        }
        #[cfg(not(windows))]
        {
            assert_eq!(profile.name, "sh");
            assert_eq!(profile.dialect, ShellDialect::Posix);
        }
    }

    #[test]
    fn shell_profile_names_every_dialect_it_can_spawn() {
        // Guards the canonical naming the prompt renders for adapters that do
        // not override it.
        for (dialect, expected) in [
            (ShellDialect::Posix, "sh"),
            (ShellDialect::WindowsCmd, "cmd"),
            (ShellDialect::PowerShell, "powershell"),
        ] {
            let profile = ShellProfile::from_dialect(dialect).expect("spawnable dialect");
            assert_eq!(profile.name, expected);
            assert_eq!(profile.dialect, dialect);
        }
    }

    #[test]
    fn shell_less_runtime_reports_no_shell_profile() {
        // `None` means the prompt omits the shell entirely rather than naming
        // a shell the runtime cannot spawn.
        let runtime = ShelllessRuntime;
        assert!(!runtime.has_shell_access());
        assert_eq!(runtime.shell_profile(), None);
        assert_eq!(ShellProfile::from_dialect(ShellDialect::None), None);
    }

    #[test]
    fn posix_prompt_section_names_the_shell_without_a_syntax_table() {
        // POSIX tool names are what the model already reaches for, so the
        // section carries the configured name and nothing else. `bash` must
        // be visible: that is the whole point of naming the variant.
        let profile = ShellProfile {
            name: "bash".to_string(),
            dialect: ShellDialect::Posix,
        };
        let section = profile.prompt_section();
        assert!(section.contains("`bash`"), "{section}");
        assert!(
            !section.contains("Get-ChildItem") && !section.contains("dir /a"),
            "POSIX must not carry a dialect-correction table: {section}"
        );
    }

    #[test]
    fn powershell_prompt_section_steers_off_both_cmd_and_posix() {
        // The failure this guards is a model emitting `dir /a` or `ls` into a
        // PowerShell session, so both wrong dialects must be ruled out.
        let profile = ShellProfile {
            name: "pwsh".to_string(),
            dialect: ShellDialect::PowerShell,
        };
        let section = profile.prompt_section();
        assert!(section.contains("Get-ChildItem -Force"), "{section}");
        assert!(section.contains("Select-String"), "{section}");
        assert!(section.contains("`cmd` builtins"), "{section}");
        assert!(section.contains("POSIX tools"), "{section}");
        // Steering is positive: the correct form carries the instruction, so
        // the wrong ones are ruled out as a class instead of one per line.
        assert!(!section.contains("(not `"), "{section}");
    }

    #[test]
    fn powershell_prompt_section_distinguishes_pwsh_from_windows_powershell() {
        // `&&` chains parse under pwsh 7+ and are a syntax error under 5.1,
        // so the two must not be described identically.
        let seven = ShellProfile {
            name: "pwsh".to_string(),
            dialect: ShellDialect::PowerShell,
        }
        .prompt_section();
        let five = ShellProfile {
            name: "powershell".to_string(),
            dialect: ShellDialect::PowerShell,
        }
        .prompt_section();

        assert!(seven.contains("PowerShell 7+"), "{seven}");
        assert!(seven.contains("ternaries work"), "{seven}");
        assert!(five.contains("Windows PowerShell 5.1"), "{five}");
        assert!(five.contains("no `&&`/`||` chains"), "{five}");
    }

    #[test]
    fn windows_cmd_prompt_section_steers_off_both_powershell_and_posix() {
        let profile =
            ShellProfile::from_dialect(ShellDialect::WindowsCmd).expect("spawnable dialect");
        let section = profile.prompt_section();
        assert!(section.contains("`cmd`"), "{section}");
        assert!(section.contains("dir /a"), "{section}");
        assert!(section.contains("findstr"), "{section}");
        assert!(section.contains("PowerShell cmdlets"), "{section}");
        assert!(section.contains("POSIX tools"), "{section}");
        assert!(!section.contains("(not `"), "{section}");
    }

    #[test]
    fn cron_tools_alone_still_need_the_dialect_guidance() {
        // The cron/schedule tools take a model-authored `command` that runs
        // through the same interpreter, so an agent holding only those is
        // exactly as exposed to a dialect mismatch as one holding `shell`.
        assert!(needs_shell_dialect_guidance(["cron_add"]));
        assert!(needs_shell_dialect_guidance(["cron_update"]));
        assert!(needs_shell_dialect_guidance(["schedule"]));
        assert!(needs_shell_dialect_guidance(["shell"]));
        assert!(needs_shell_dialect_guidance(["file_read", "cron_add"]));
    }

    #[test]
    fn a_tool_surface_that_runs_no_commands_needs_no_dialect_guidance() {
        assert!(!needs_shell_dialect_guidance([]));
        assert!(!needs_shell_dialect_guidance(["file_read", "file_write"]));
        // `cron_list` and `cron_remove` only name existing jobs; neither takes
        // a command for the model to write.
        assert!(!needs_shell_dialect_guidance(["cron_list", "cron_remove"]));
    }

    #[test]
    fn deletion_guidance_names_a_command_the_dialect_actually_has() {
        // `trash` is POSIX-only: recommending it to a PowerShell or cmd
        // session names a command that is not there and leaves the real
        // destructive one unaddressed.
        for (dialect, expected, forbidden) in [
            (ShellDialect::Posix, "trash", "Remove-Item"),
            (ShellDialect::PowerShell, "-WhatIf", "trash"),
            (ShellDialect::WindowsCmd, "rmdir /s", "trash"),
        ] {
            let guidance = ShellProfile::from_dialect(dialect)
                .expect("spawnable dialect")
                .safe_deletion_guidance();
            assert!(guidance.contains(expected), "{dialect:?}: {guidance}");
            assert!(!guidance.contains(forbidden), "{dialect:?}: {guidance}");
        }
    }

    #[test]
    fn runtime_reports_capabilities() {
        let runtime = DummyRuntime;

        assert_eq!(runtime.name(), "dummy-runtime");
        assert!(runtime.has_shell_access());
        assert!(runtime.has_filesystem_access());
        assert!(runtime.supports_long_running());
        #[cfg(windows)]
        assert_eq!(runtime.shell_dialect(), ShellDialect::WindowsCmd);
        #[cfg(not(windows))]
        assert_eq!(runtime.shell_dialect(), ShellDialect::Posix);
        assert_eq!(runtime.storage_path(), PathBuf::from("/tmp/dummy-runtime"));
    }

    #[tokio::test]
    async fn build_shell_command_executes() {
        let runtime = DummyRuntime;
        let mut cmd = runtime
            .build_shell_command("hello-runtime", Path::new("."))
            .unwrap();

        let output = cmd.output().await.unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);

        assert!(output.status.success());
        assert!(stdout.contains("hello-runtime"));
    }

    #[tokio::test]
    async fn default_env_key_builder_delegates_to_shell_command() {
        let runtime = DummyRuntime;
        let mut cmd = runtime
            .build_shell_command_with_env_keys(
                "hello-env-key-runtime",
                Path::new("."),
                &[OsStr::new("ZC_RUNTIME_TOKEN")],
            )
            .unwrap();

        let output = cmd.output().await.unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);

        assert!(output.status.success());
        assert!(stdout.contains("hello-env-key-runtime"));
    }
}
