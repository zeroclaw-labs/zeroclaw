# Cron precondition gate (pre_hook). Machine-readable run statuses
# (skipped_precondition, precondition_failed, already_in_flight) are wire
# values and deliberately stay out of this catalogue; only the operator-facing
# explanation is localized.
cron-pre-hook-empty-command = pre_hook has an empty command
cron-pre-hook-invalid-timeout = pre_hook timeout_secs must be at least 1
cron-pre-hook-runtime-unsupported = pre_hook is not supported under the {$runtime} runtime: a timeout cannot terminate work started inside it, so the gate's deadline could not be enforced
cron-pre-hook-blocked-read-only = blocked by security policy: autonomy is read-only
cron-pre-hook-blocked-rate-limited = blocked by security policy: rate limit exceeded
cron-pre-hook-blocked-forbidden-path = blocked by security policy: forbidden path argument: {$path}
cron-pre-hook-blocked-budget = blocked by security policy: action budget exhausted
cron-pre-hook-shell-setup-error = pre_hook shell setup error: {$error}
cron-pre-hook-spawn-error = pre_hook spawn error: {$error}
cron-pre-hook-wait-error = pre_hook wait error: {$error}
cron-pre-hook-timed-out = pre_hook timed out after {$seconds}s
cron-pre-hook-skip = pre_hook requested skip (exit {$code})
cron-pre-hook-failed = pre_hook failed (exit {$code})
cron-pre-hook-terminated = pre_hook terminated without an exit code ({$status})
cron-pre-hook-output-truncated = [pre_hook output truncated]
cron-pre-hook-runtime-missing = pre_hook setup error: runtime missing for cron precondition
cron-manual-refused-in-flight = cron job {$id} is already in flight; manual trigger refused
cron-manual-claim-failed = failed to claim cron job {$id} for a manual run: {$error}
cron-owner-ambiguous = cron job {$id} is claimed by {$count} enabled agents ({$owners}); exactly one agent must list it in [agents.<x>].cron_jobs so the job runs under a determinate security policy
