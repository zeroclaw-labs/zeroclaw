//! Deterministic precondition gate for cron jobs.
//!
//! A job may declare a `pre_hook` in config. The hook is a cheap local command
//! the daemon runs immediately before the job body, on both scheduled and
//! manual runs, and its exit code decides what happens next:
//!
//! | Exit | Meaning |
//! | --- | --- |
//! | `0` | preconditions met — run the job |
//! | `10` | preconditions not met — record a clean skip, never start the body |
//! | anything else, a signal, or a timeout | the gate itself failed — record a precondition failure |
//!
//! The hook is a config-declared command executed on a timer, so it is gated
//! exactly like the job's own shell command: allowlist, risk level, path guard,
//! autonomy, rate limit, and action budget are all re-checked on every run, and
//! the hook is never validated as `approved`. A blocked hook is a precondition
//! failure, not a skip, so tightening policy can never silently turn a job off.

use std::process::Stdio;

use tokio::time::{self, Duration};
use zeroclaw_api::runtime_traits::RuntimeAdapter;
use zeroclaw_config::schema::{Config, CronPreHookDecl};

use crate::security::SecurityPolicy;

/// Exit code a precondition uses to request a clean skip.
pub const PRECONDITION_SKIP_EXIT_CODE: i32 = 10;

/// Run status recorded when a precondition asked for a clean skip.
pub const STATUS_SKIPPED_PRECONDITION: &str = "skipped_precondition";

/// Run status recorded when the precondition itself failed.
pub const STATUS_PRECONDITION_FAILED: &str = "precondition_failed";

/// Upper bound on the hook output carried into run history. The gate exists to
/// avoid work, so its diagnostics stay small even when the hook is chatty.
const MAX_PRE_HOOK_OUTPUT_BYTES: usize = 4096;

const TRUNCATION_MARKER: &str = "\n… [pre_hook output truncated]";

/// What a precondition decided about the run that follows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreconditionOutcome {
    /// Exit `0`: preconditions met, run the job body.
    Proceed,
    /// Exit `10`: preconditions not met. Not a failure.
    Skip { output: String },
    /// The gate could not authorize the run: any other exit, a signal, a
    /// timeout, a spawn error, or a security-policy denial.
    Failed { output: String },
}

/// Resolve the precondition gate declared for `job_id`.
///
/// Config is the only source: a pre-hook is not stored on the cron row and is
/// not reachable from the `cron_add`/`cron_update` tools, so only an operator
/// editing `config.toml` can introduce one. Imperative jobs have no gate.
pub(crate) fn declared_for<'a>(
    config: &'a Config,
    job_source: &str,
    job_id: &str,
) -> Option<&'a CronPreHookDecl> {
    if job_source != "declarative" {
        return None;
    }
    config.cron.get(job_id)?.pre_hook.as_ref()
}

/// Run a job's precondition gate and classify the result.
///
/// Never panics and never propagates: every failure mode maps onto
/// [`PreconditionOutcome::Failed`] so the caller records a precondition failure
/// rather than starting the job body.
pub(crate) async fn evaluate(
    config: &Config,
    runtime: &dyn RuntimeAdapter,
    security: &SecurityPolicy,
    hook: &CronPreHookDecl,
) -> PreconditionOutcome {
    let command = hook.command.trim();
    if command.is_empty() {
        return PreconditionOutcome::Failed {
            output: "pre_hook has an empty command".to_string(),
        };
    }

    if hook.timeout_secs == 0 {
        return PreconditionOutcome::Failed {
            output: "pre_hook timeout_secs must be at least 1".to_string(),
        };
    }

    if !security.can_act() {
        return PreconditionOutcome::Failed {
            output: "blocked by security policy: autonomy is read-only".to_string(),
        };
    }

    if security.is_rate_limited() {
        return PreconditionOutcome::Failed {
            output: "blocked by security policy: rate limit exceeded".to_string(),
        };
    }

    // The gate is validated with `approved = false` on every path, including a
    // manually approved run: an operator approving a job body must not also
    // approve a command the gate would otherwise be refused.
    if let Err(error) =
        super::validate_shell_command_with_security(runtime, security, command, false)
    {
        return PreconditionOutcome::Failed {
            output: error.to_string(),
        };
    }

    if let Some(path) = security.forbidden_path_argument(command) {
        return PreconditionOutcome::Failed {
            output: format!("blocked by security policy: forbidden path argument: {path}"),
        };
    }

    if !security.record_action() {
        return PreconditionOutcome::Failed {
            output: "blocked by security policy: action budget exhausted".to_string(),
        };
    }

    let mut process = match runtime.build_shell_command(command, &config.data_dir) {
        Ok(process) => process,
        Err(error) => {
            return PreconditionOutcome::Failed {
                output: format!("pre_hook shell setup error: {error}"),
            };
        }
    };

    process
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let child = match process.spawn() {
        Ok(child) => child,
        Err(error) => {
            return PreconditionOutcome::Failed {
                output: format!("pre_hook spawn error: {error}"),
            };
        }
    };

    let timeout = Duration::from_secs(hook.timeout_secs);
    let output = match time::timeout(timeout, child.wait_with_output()).await {
        Ok(Ok(output)) => output,
        Ok(Err(error)) => {
            return PreconditionOutcome::Failed {
                output: format!("pre_hook spawn error: {error}"),
            };
        }
        // Dropping the timed-out future kills the child (`kill_on_drop`).
        Err(_) => {
            return PreconditionOutcome::Failed {
                output: format!("pre_hook timed out after {}s", hook.timeout_secs),
            };
        }
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    match output.status.code() {
        Some(0) => PreconditionOutcome::Proceed,
        Some(PRECONDITION_SKIP_EXIT_CODE) => PreconditionOutcome::Skip {
            output: describe(
                &format!("pre_hook requested skip (exit {PRECONDITION_SKIP_EXIT_CODE})"),
                &stdout,
                &stderr,
            ),
        },
        Some(code) => PreconditionOutcome::Failed {
            output: describe(&format!("pre_hook failed (exit {code})"), &stdout, &stderr),
        },
        // No exit code means the hook was terminated by a signal.
        None => PreconditionOutcome::Failed {
            output: describe(
                &format!(
                    "pre_hook terminated without an exit code ({})",
                    output.status
                ),
                &stdout,
                &stderr,
            ),
        },
    }
}

/// Wrap hook output in the same `stdout:`/`stderr:` envelope shell jobs use,
/// bounded so a chatty hook cannot dominate run history.
fn describe(headline: &str, stdout: &str, stderr: &str) -> String {
    let body = format!(
        "{headline}\nstdout:\n{}\nstderr:\n{}",
        stdout.trim(),
        stderr.trim()
    );
    truncate(&body)
}

fn truncate(output: &str) -> String {
    if output.len() <= MAX_PRE_HOOK_OUTPUT_BYTES {
        return output.to_string();
    }

    let mut cutoff = MAX_PRE_HOOK_OUTPUT_BYTES.saturating_sub(TRUNCATION_MARKER.len());
    while cutoff > 0 && !output.is_char_boundary(cutoff) {
        cutoff -= 1;
    }

    let mut truncated = output[..cutoff].to_string();
    truncated.push_str(TRUNCATION_MARKER);
    truncated
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeroclaw_config::schema::{CronJobDecl, CronPreHookDecl};

    fn config_with_hook(job_id: &str, hook: Option<CronPreHookDecl>) -> Config {
        let mut config = Config::default();
        config.cron.insert(
            job_id.to_string(),
            CronJobDecl {
                pre_hook: hook,
                ..CronJobDecl::default()
            },
        );
        config
    }

    #[test]
    fn declared_for_reads_the_gate_from_config() {
        let config = config_with_hook(
            "nightly",
            Some(CronPreHookDecl {
                command: "check.sh".into(),
                timeout_secs: 5,
            }),
        );

        let hook = declared_for(&config, "declarative", "nightly").expect("gate should resolve");
        assert_eq!(hook.command, "check.sh");
        assert_eq!(hook.timeout_secs, 5);
    }

    #[test]
    fn declared_for_ignores_imperative_jobs() {
        let config = config_with_hook(
            "nightly",
            Some(CronPreHookDecl {
                command: "check.sh".into(),
                timeout_secs: 5,
            }),
        );

        // A gate is declarable only in config. An imperative row that happens
        // to share an id with a config entry must not inherit its gate.
        assert!(declared_for(&config, "imperative", "nightly").is_none());
    }

    #[test]
    fn declared_for_returns_none_without_a_declared_gate() {
        let config = config_with_hook("nightly", None);
        assert!(declared_for(&config, "declarative", "nightly").is_none());
        assert!(declared_for(&config, "declarative", "unknown-job").is_none());
    }

    #[test]
    fn pre_hook_timeout_default_is_thirty_seconds() {
        // The documented default lives in the schema; assert the two agree.
        assert_eq!(CronPreHookDecl::default().timeout_secs, 30);
    }

    #[test]
    fn truncate_bounds_output_on_a_char_boundary() {
        let long = "é".repeat(MAX_PRE_HOOK_OUTPUT_BYTES);
        let truncated = truncate(&long);

        assert!(truncated.len() <= MAX_PRE_HOOK_OUTPUT_BYTES);
        assert!(truncated.ends_with(TRUNCATION_MARKER));
        // Round-tripping proves no multi-byte char was cut in half.
        assert_eq!(
            truncated,
            String::from_utf8(truncated.clone().into_bytes()).unwrap()
        );
    }

    #[test]
    fn truncate_leaves_short_output_alone() {
        assert_eq!(
            truncate("pre_hook requested skip"),
            "pre_hook requested skip"
        );
    }
}
