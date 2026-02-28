# Syscall Anomaly Detection

ZeroClaw can monitor syscall-related telemetry emitted by sandboxed command execution
and flag anomalies before they become silent failures.

This feature is designed for daemon runtime paths where `shell` and `process` tools
execute commands repeatedly under policy controls.

## What It Detects

- Unknown syscall names outside an expected baseline profile
- Denied syscall spikes inside a 60-second rolling window
- Syscall-event floods inside a 60-second rolling window
- Denied syscalls in strict mode, even if the syscall is in baseline

The detector consumes command `stderr` and `stdout` lines and parses known signal forms:

- Linux audit style (`syscall=59`)
- Seccomp denial lines (`seccomp denied syscall=openat`)
- SIGSYS / `Bad system call` crash hints

## Configuration

Configure under `[security.syscall_anomaly]`:

```toml
[security.syscall_anomaly]
enabled = true
strict_mode = false
alert_on_unknown_syscall = true
max_denied_events_per_minute = 5
max_total_events_per_minute = 120
max_alerts_per_minute = 30
alert_cooldown_secs = 20
log_path = "syscall-anomalies.log"
baseline_syscalls = [
  "read", "write", "openat", "close", "mmap", "munmap",
  "futex", "clock_gettime", "epoll_wait", "clone", "execve",
  "socket", "connect", "sendto", "recvfrom", "getrandom"
]
```

## Alert and Audit Outputs

When an anomaly is detected:

- A warning log is emitted with target `security::syscall_anomaly`
- A structured JSON line is appended to `log_path`
- A `security_event` audit entry is emitted when security audit logging is enabled

## Tuning Guidance

- Start with `strict_mode = false` to avoid noisy first deployments.
- Expand `baseline_syscalls` for known workloads until unknown alerts stabilize.
- Keep `max_denied_events_per_minute` small for production daemons (for example `3-10`).
- Use higher `max_total_events_per_minute` in high-throughput environments.
- Keep `max_denied_events_per_minute <= max_total_events_per_minute`.
- Keep `max_alerts_per_minute` bounded to prevent alert storms.
- Set `alert_cooldown_secs` to suppress duplicate anomalies during repeated retries.

## Validation

Current validation includes:

- parser extraction tests for audit and seccomp formats
- rolling-buffer incremental scan coverage for long-running process output
- unknown syscall anomaly test
- denied rate threshold test
- config validation tests for thresholds and baseline names
