use super::*;

impl SecurityPolicy {
    /// An unavailable or undetermined sandbox is not an operator opt-out.
    pub fn explicitly_disables_sandbox(&self) -> bool {
        self.sandbox_enabled == Some(false)
            || self
                .sandbox_backend
                .as_deref()
                .is_some_and(|name| name.trim().eq_ignore_ascii_case("none"))
    }

    /// Authorize one fixed native Unix executable, not shell source. A path
    /// must be listed as a path; basename grants cover only bare PATH lookups.
    /// Initial direct execution requires a named grant rather than `*`.
    pub fn validate_literal_argv(&self, program: &str, args: &[String]) -> Result<(), String> {
        let path = Path::new(program);
        let base = path
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or("missing executable")?;
        if self.autonomy == AutonomyLevel::ReadOnly
            || program.is_empty()
            || program.contains('\0')
            || program == "*"
            || (!path.is_absolute() && base != program)
            || !self.allowed_commands.iter().any(|entry| entry == program)
        {
            return Err("Executable not allowed by security policy".into());
        }
        let base = base.to_ascii_lowercase();
        // An interpreter or launcher could treat a literal operand as another
        // program. A shell-backed skill is not a grant to do that with argv.
        if matches!(
            base.as_str(),
            "sh" | "bash"
                | "dash"
                | "ash"
                | "zsh"
                | "ksh"
                | "fish"
                | "csh"
                | "tcsh"
                | "powershell"
                | "pwsh"
                | "cmd"
                | "busybox"
                | "env"
                | "tee"
                | "xargs"
                | "exec"
                | "command"
                | "sudo"
                | "su"
        ) || args.iter().any(|arg| arg.contains('\0'))
        {
            return Err("Direct invocation not permitted for this executable or argument".into());
        }
        let lower = args
            .iter()
            .map(|s| s.to_ascii_lowercase())
            .collect::<Vec<_>>();
        if !self.is_args_safe(&base, &lower, args) {
            return Err("Argument blocked by security policy".into());
        }
        for arg in args {
            let check = |value: &str| {
                self.forbidden_path_candidate(value, ShellDialect::Posix, true)
                    .is_some()
            };
            if check(arg)
                || (arg.starts_with('-')
                    && (arg.split_once('=').is_some_and(|(_, value)| check(value))
                        || attached_short_option_value(arg).is_some_and(check)))
            {
                return Err("Path blocked by security policy".into());
            }
        }
        // Shell syntax is not present. Only executable/argument based risk
        // applies. No implicit approval, including for direct or nested calls.
        let risk = generic_segment_risk(&base, args, "").unwrap_or(CommandRiskLevel::Low);
        self.authorize_command_risk(risk, false, || true)
            .map(|_| ())
    }
}
