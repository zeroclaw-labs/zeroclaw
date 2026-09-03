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
//! A hook only runs under a runtime whose work the daemon can actually
//! terminate. Container and remote runtimes are refused up front rather than
//! given a deadline that cannot be enforced.
//!
//! The hook is a config-declared command executed on a timer, so it is gated
//! exactly like the job's own shell command: allowlist, risk level, path guard,
//! autonomy, rate limit, and action budget are all re-checked on every run, and
//! the hook is never validated as `approved`. A blocked hook is a precondition
//! failure, not a skip, so tightening policy can never silently turn a job off.

use std::process::Stdio;

use tokio::io::AsyncReadExt;
use tokio::time::{self, Duration};
use zeroclaw_api::runtime_traits::RuntimeAdapter;
use zeroclaw_config::schema::{Config, CronPreHookDecl};

use crate::i18n::{get_required_cli_string, get_required_cli_string_with_args};
use zeroclaw_config::policy::SecurityPolicy;

/// Exit code a precondition uses to request a clean skip.
pub const PRECONDITION_SKIP_EXIT_CODE: i32 = 10;

/// Run status recorded when a precondition asked for a clean skip.
pub const STATUS_SKIPPED_PRECONDITION: &str = "skipped_precondition";

/// Run status recorded when the precondition itself failed.
pub const STATUS_PRECONDITION_FAILED: &str = "precondition_failed";

/// Upper bound on the hook output carried into run history. The gate exists to
/// avoid work, so its diagnostics stay small even when the hook is chatty.
const MAX_PRE_HOOK_OUTPUT_BYTES: usize = 4096;

/// Operator-facing marker appended when hook output was capped.
///
/// Built at call time rather than held as a `const` because the text is
/// localized; the leading separator stays literal since it is layout, not
/// prose.
fn truncation_marker() -> String {
    format!(
        "\n… {}",
        get_required_cli_string("cron-pre-hook-output-truncated")
    )
}

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
            output: get_required_cli_string("cron-pre-hook-empty-command"),
        };
    }

    if hook.timeout_secs == 0 {
        return PreconditionOutcome::Failed {
            output: get_required_cli_string("cron-pre-hook-invalid-timeout"),
        };
    }

    // Fail closed where a timeout cannot actually stop the work. Under a
    // container runtime the hook is `docker run ... sh -c <command>`, and
    // killing the client leaves the container running past the timeout, after
    // the gate has already reported failure and released the claim. A gate
    // whose deadline is unenforceable is not a gate, so refuse rather than
    // advertise a guarantee that does not hold.
    if !cancellation_is_enforceable(config.runtime.kind) {
        return PreconditionOutcome::Failed {
            output: get_required_cli_string_with_args(
                "cron-pre-hook-runtime-unsupported",
                &[("runtime", config.runtime.kind.as_wire())],
            ),
        };
    }

    if !security.can_act() {
        return PreconditionOutcome::Failed {
            output: get_required_cli_string("cron-pre-hook-blocked-read-only"),
        };
    }

    if security.is_rate_limited() {
        return PreconditionOutcome::Failed {
            output: get_required_cli_string("cron-pre-hook-blocked-rate-limited"),
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
            output: get_required_cli_string_with_args(
                "cron-pre-hook-blocked-forbidden-path",
                &[("path", &path.to_string())],
            ),
        };
    }

    if !security.record_action() {
        return PreconditionOutcome::Failed {
            output: get_required_cli_string("cron-pre-hook-blocked-budget"),
        };
    }

    let mut process = match runtime.build_shell_command(command, &config.data_dir) {
        Ok(process) => process,
        Err(error) => {
            return PreconditionOutcome::Failed {
                output: get_required_cli_string_with_args(
                    "cron-pre-hook-shell-setup-error",
                    &[("error", &error.to_string())],
                ),
            };
        }
    };

    process
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    // The runtime launches the hook as `<shell> -c <command>`, so the work the
    // hook actually does is a child of that shell. Put the shell in its own
    // process group so a timeout can signal the whole tree instead of orphaning
    // the descendants that are doing the work.
    #[cfg(unix)]
    process.process_group(0);

    let mut child = match process.spawn() {
        Ok(child) => child,
        Err(error) => {
            return PreconditionOutcome::Failed {
                output: get_required_cli_string_with_args(
                    "cron-pre-hook-spawn-error",
                    &[("error", &error.to_string())],
                ),
            };
        }
    };

    let child_pid = child.id();
    let mut stdout_pipe = child.stdout.take();
    let mut stderr_pipe = child.stderr.take();
    let timeout = Duration::from_secs(hook.timeout_secs);

    // Read both pipes concurrently with the wait, keeping at most
    // `MAX_PRE_HOOK_OUTPUT_BYTES` per stream in memory. Draining continues past
    // the cap (discarding the excess) so a chatty hook can neither exhaust the
    // daemon's memory nor deadlock on a full pipe.
    let collect = async {
        let (out, err, status) = tokio::join!(
            read_capped(stdout_pipe.as_mut(), MAX_PRE_HOOK_OUTPUT_BYTES),
            read_capped(stderr_pipe.as_mut(), MAX_PRE_HOOK_OUTPUT_BYTES),
            child.wait()
        );
        (out, err, status)
    };

    let ((stdout_bytes, stdout_capped), (stderr_bytes, stderr_capped), status) =
        match time::timeout(timeout, collect).await {
            Ok((out, err, Ok(status))) => (out, err, status),
            Ok((_, _, Err(error))) => {
                return PreconditionOutcome::Failed {
                    output: get_required_cli_string_with_args(
                        "cron-pre-hook-wait-error",
                        &[("error", &error.to_string())],
                    ),
                };
            }
            Err(_) => {
                terminate_process_tree(&mut child, child_pid).await;
                return PreconditionOutcome::Failed {
                    output: get_required_cli_string_with_args(
                        "cron-pre-hook-timed-out",
                        &[("seconds", &hook.timeout_secs.to_string())],
                    ),
                };
            }
        };

    let stdout = String::from_utf8_lossy(&stdout_bytes);
    let stderr = String::from_utf8_lossy(&stderr_bytes);
    let capped = stdout_capped || stderr_capped;

    match status.code() {
        Some(0) => PreconditionOutcome::Proceed,
        Some(PRECONDITION_SKIP_EXIT_CODE) => PreconditionOutcome::Skip {
            output: describe(
                &get_required_cli_string_with_args(
                    "cron-pre-hook-skip",
                    &[("code", &PRECONDITION_SKIP_EXIT_CODE.to_string())],
                ),
                &stdout,
                &stderr,
                capped,
            ),
        },
        Some(code) => PreconditionOutcome::Failed {
            output: describe(
                &get_required_cli_string_with_args(
                    "cron-pre-hook-failed",
                    &[("code", &code.to_string())],
                ),
                &stdout,
                &stderr,
                capped,
            ),
        },
        // No exit code means the hook was terminated by a signal.
        None => PreconditionOutcome::Failed {
            output: describe(
                &get_required_cli_string_with_args(
                    "cron-pre-hook-terminated",
                    &[("status", &status.to_string())],
                ),
                &stdout,
                &stderr,
                capped,
            ),
        },
    }
}

/// Whether a timed-out hook can actually be stopped under this runtime.
///
/// Native execution is cancellable: the hook is spawned into its own process
/// group (unix) or killed as a tree (windows). Container and remote runtimes
/// start work whose lifetime the daemon does not control.
fn cancellation_is_enforceable(kind: zeroclaw_config::schema::RuntimeKind) -> bool {
    use zeroclaw_config::schema::RuntimeKind;
    match kind {
        RuntimeKind::Native => true,
        RuntimeKind::Docker | RuntimeKind::Cloudflare => false,
    }
}

/// Drain `reader` while keeping at most `cap` bytes.
///
/// Returns the retained bytes and whether anything was discarded. Reading
/// continues past the cap on purpose: stopping early would leave the pipe full
/// and block the hook instead of letting it exit.
async fn read_capped<R>(reader: Option<&mut R>, cap: usize) -> (Vec<u8>, bool)
where
    R: tokio::io::AsyncRead + Unpin,
{
    let Some(reader) = reader else {
        return (Vec::new(), false);
    };

    let mut kept = Vec::new();
    let mut chunk = [0u8; 4096];
    let mut discarded = false;

    loop {
        match reader.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(read) => {
                let room = cap.saturating_sub(kept.len());
                if room == 0 {
                    discarded = true;
                    continue;
                }
                let take = room.min(read);
                kept.extend_from_slice(&chunk[..take]);
                if take < read {
                    discarded = true;
                }
            }
        }
    }

    (kept, discarded)
}

/// Kill a timed-out hook and everything it started, then reap it.
///
/// On unix the child was spawned into its own process group, so signalling the
/// group reaches descendants that would otherwise outlive the timeout. Container
/// runtimes are not covered: killing a `docker run` client does not stop the
/// container it started.
async fn terminate_process_tree(child: &mut tokio::process::Child, pid: Option<u32>) {
    #[cfg(unix)]
    if let Some(pid) = pid
        && let Ok(pgid) = libc::pid_t::try_from(pid)
    {
        // SAFETY: `killpg` takes no pointers and the pgid is this child's own
        // group, created by `process_group(0)` above.
        unsafe {
            libc::killpg(pgid, libc::SIGKILL);
        }
    }
    // Windows has no process groups here; `taskkill /T` walks the child tree,
    // which is what `<shell> -c <command>` needs since the real work is a
    // grandchild of the process we hold a handle to.
    #[cfg(windows)]
    if let Some(pid) = pid {
        let _ = tokio::process::Command::new("taskkill")
            .args(["/T", "/F", "/PID", &pid.to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await;
    }
    #[cfg(not(any(unix, windows)))]
    let _ = pid;

    let _ = child.start_kill();
    let _ = child.wait().await;
}

/// Wrap hook output in the same `stdout:`/`stderr:` envelope shell jobs use,
/// bounded so a chatty hook cannot dominate run history.
fn describe(headline: &str, stdout: &str, stderr: &str, capped: bool) -> String {
    let mut body = format!(
        "{headline}\nstdout:\n{}\nstderr:\n{}",
        stdout.trim(),
        stderr.trim()
    );
    if capped {
        body.push_str(&truncation_marker());
    }
    truncate(&body)
}

fn truncate(output: &str) -> String {
    if output.len() <= MAX_PRE_HOOK_OUTPUT_BYTES {
        return output.to_string();
    }

    let marker = truncation_marker();
    let mut cutoff = MAX_PRE_HOOK_OUTPUT_BYTES.saturating_sub(marker.len());
    while cutoff > 0 && !output.is_char_boundary(cutoff) {
        cutoff -= 1;
    }

    let mut truncated = output[..cutoff].to_string();
    truncated.push_str(&marker);
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
        assert!(truncated.ends_with(&truncation_marker()));
        // Round-tripping proves no multi-byte char was cut in half.
        assert_eq!(
            truncated,
            String::from_utf8(truncated.clone().into_bytes()).unwrap()
        );
    }

    /// Keys this module renders with no interpolation.
    const GATE_KEYS_PLAIN: &[&str] = &[
        "cron-pre-hook-empty-command",
        "cron-pre-hook-invalid-timeout",
        "cron-pre-hook-blocked-read-only",
        "cron-pre-hook-blocked-rate-limited",
        "cron-pre-hook-blocked-budget",
        "cron-pre-hook-output-truncated",
    ];

    /// Keys this module renders with arguments, paired with a sample value
    /// that must survive into the rendered string.
    const GATE_KEYS_WITH_ARGS: &[(&str, &str, &str)] = &[
        ("cron-pre-hook-runtime-unsupported", "runtime", "docker"),
        (
            "cron-pre-hook-blocked-forbidden-path",
            "path",
            "/etc/shadow",
        ),
        ("cron-pre-hook-shell-setup-error", "error", "no shell"),
        ("cron-pre-hook-spawn-error", "error", "permission denied"),
        ("cron-pre-hook-wait-error", "error", "interrupted"),
        ("cron-pre-hook-timed-out", "seconds", "45"),
        ("cron-pre-hook-skip", "code", "10"),
        ("cron-pre-hook-failed", "code", "3"),
        ("cron-pre-hook-terminated", "status", "signal: 9"),
    ];

    /// A missing key does not fail the build, it silently degrades the
    /// operator-facing result, so the catalogue contract is asserted here.
    #[test]
    fn every_gate_key_resolves_in_the_catalogue() {
        for key in GATE_KEYS_PLAIN {
            let rendered = get_required_cli_string(key);
            assert!(
                !rendered.contains(key),
                "missing catalogue entry for {key}: got {rendered}"
            );
            assert!(
                !rendered.trim().is_empty(),
                "empty catalogue entry for {key}"
            );
        }
        for (key, arg, sample) in GATE_KEYS_WITH_ARGS {
            let rendered = get_required_cli_string_with_args(key, &[(arg, sample)]);
            assert!(
                !rendered.contains(key),
                "missing catalogue entry for {key}: got {rendered}"
            );
        }
    }

    /// Every argument-bearing key must actually place its argument. A dropped
    /// interpolation is worse than a missing string: the exit code and runtime
    /// name are exactly what an operator acts on.
    #[test]
    fn gate_keys_interpolate_their_arguments() {
        for (key, arg, sample) in GATE_KEYS_WITH_ARGS {
            let rendered = get_required_cli_string_with_args(key, &[(arg, sample)]);
            assert!(
                rendered.contains(sample),
                "{key} dropped its {arg} argument: {rendered}"
            );
        }
    }

    #[test]
    fn wire_statuses_are_not_localized() {
        // The machine-readable statuses are a wire contract shared with the
        // API, tools, and stored history. They must stay stable regardless of
        // locale, so they deliberately have no catalogue entry.
        assert_eq!(STATUS_SKIPPED_PRECONDITION, "skipped_precondition");
        assert_eq!(STATUS_PRECONDITION_FAILED, "precondition_failed");
    }

    #[test]
    fn cancellation_is_only_enforceable_on_the_native_runtime() {
        use zeroclaw_config::schema::RuntimeKind;
        assert!(cancellation_is_enforceable(RuntimeKind::Native));
        // A timeout cannot stop work inside a container or a remote runtime,
        // so the gate must refuse rather than promise a deadline it cannot keep.
        assert!(!cancellation_is_enforceable(RuntimeKind::Docker));
        assert!(!cancellation_is_enforceable(RuntimeKind::Cloudflare));
    }

    #[tokio::test]
    async fn read_capped_keeps_the_cap_and_still_drains_the_rest() {
        // 64 KiB through a 1 KiB cap: the retained buffer stays at the cap and
        // the reader still consumes to EOF rather than stalling the writer.
        let payload = vec![b'x'; 64 * 1024];
        let mut cursor = std::io::Cursor::new(payload);
        let (kept, discarded) = read_capped(Some(&mut cursor), 1024).await;

        assert_eq!(kept.len(), 1024, "memory must stay at the cap");
        assert!(discarded, "the excess must be reported as discarded");
        assert_eq!(
            cursor.position(),
            64 * 1024,
            "the reader must drain to EOF so the writer never blocks"
        );
    }

    #[tokio::test]
    async fn read_capped_keeps_everything_under_the_cap() {
        let mut cursor = std::io::Cursor::new(b"short".to_vec());
        let (kept, discarded) = read_capped(Some(&mut cursor), 1024).await;
        assert_eq!(kept, b"short");
        assert!(!discarded);
    }

    #[tokio::test]
    async fn read_capped_handles_a_missing_pipe() {
        let (kept, discarded) =
            read_capped(Option::<&mut std::io::Cursor<Vec<u8>>>::None, 16).await;
        assert!(kept.is_empty());
        assert!(!discarded);
    }

    #[test]
    fn describe_marks_output_that_was_capped() {
        let marked = describe("headline", "out", "err", true);
        assert!(marked.ends_with(&truncation_marker()));
        let unmarked = describe("headline", "out", "err", false);
        assert!(!unmarked.ends_with(&truncation_marker()));
    }

    #[test]
    fn truncate_leaves_short_output_alone() {
        assert_eq!(
            truncate("pre_hook requested skip"),
            "pre_hook requested skip"
        );
    }
}
