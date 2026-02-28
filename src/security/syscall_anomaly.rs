//! Syscall anomaly detection for daemon shell/process execution.
//!
//! This detector consumes command output streams (stdout/stderr), extracts
//! syscall-related telemetry hints (seccomp/audit lines), and raises alerts
//! when the observed pattern deviates from the configured baseline.

use crate::config::{AuditConfig, SyscallAnomalyConfig};
use crate::security::audit::{AuditEvent, AuditEventType, AuditLogger};
use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

const RATE_WINDOW: Duration = Duration::from_secs(60);
const MAX_ALERT_SAMPLE_CHARS: usize = 240;

/// Alert category emitted by syscall anomaly detection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyscallAnomalyKind {
    UnknownSyscall,
    DeniedSyscall,
    DeniedRateExceeded,
    EventRateExceeded,
}

/// Structured anomaly alert entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyscallAnomalyAlert {
    pub timestamp: DateTime<Utc>,
    pub kind: SyscallAnomalyKind,
    pub command: String,
    pub syscall: Option<String>,
    pub denied_events_last_minute: u32,
    pub total_events_last_minute: u32,
    pub sample: String,
}

#[derive(Debug, Clone)]
struct ParsedSyscallSignal {
    syscall: Option<String>,
    denied: bool,
    raw_line: String,
}

#[derive(Debug, Clone)]
struct ObservedEvent {
    at: Instant,
    denied: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct AlertKey {
    kind: SyscallAnomalyKind,
    syscall: Option<String>,
    command_id: String,
}

#[derive(Debug, Default)]
struct DetectorState {
    events: VecDeque<ObservedEvent>,
    alert_timestamps: VecDeque<Instant>,
    last_alert_by_key: HashMap<AlertKey, Instant>,
}

/// Stateful detector that emits runtime syscall anomalies.
pub struct SyscallAnomalyDetector {
    config: SyscallAnomalyConfig,
    baseline: HashSet<String>,
    state: Mutex<DetectorState>,
    anomaly_log_path: PathBuf,
    audit_logger: Option<AuditLogger>,
}

impl SyscallAnomalyDetector {
    /// Build a detector from runtime config.
    pub fn new(
        config: SyscallAnomalyConfig,
        zeroclaw_dir: impl AsRef<Path>,
        audit_config: AuditConfig,
    ) -> Self {
        let baseline = normalize_baseline(&config.baseline_syscalls);
        let anomaly_log_path = resolve_log_path(zeroclaw_dir.as_ref(), config.log_path.as_str());
        let audit_logger = AuditLogger::new(audit_config, zeroclaw_dir.as_ref().to_path_buf()).ok();

        Self {
            config,
            baseline,
            state: Mutex::new(DetectorState::default()),
            anomaly_log_path,
            audit_logger,
        }
    }

    /// Inspect command output and emit anomalies (if any).
    ///
    /// Returns emitted alerts primarily for tests and diagnostics.
    pub fn inspect_command_output(
        &self,
        command: &str,
        stdout: &str,
        stderr: &str,
        exit_code: Option<i32>,
    ) -> Vec<SyscallAnomalyAlert> {
        if !self.config.enabled {
            return Vec::new();
        }

        let signals = extract_signals(stderr, stdout);
        if signals.is_empty() {
            return Vec::new();
        }

        let mut alerts: Vec<SyscallAnomalyAlert> = Vec::new();
        let now = Instant::now();
        let timestamp = Utc::now();

        let mut state = self.state.lock();
        prune_old_events(&mut state.events, now);

        for signal in &signals {
            state.events.push_back(ObservedEvent {
                at: now,
                denied: signal.denied,
            });
            prune_old_events(&mut state.events, now);

            let denied_count = count_denied(&state.events);
            let total_count = u32::try_from(state.events.len()).unwrap_or(u32::MAX);

            if let Some(syscall) = signal.syscall.as_deref() {
                let normalized = normalize_syscall_name(syscall);
                let unknown = !self.baseline.contains(&normalized);
                if self.config.alert_on_unknown_syscall && unknown {
                    alerts.push(SyscallAnomalyAlert {
                        timestamp,
                        kind: SyscallAnomalyKind::UnknownSyscall,
                        command: command.to_string(),
                        syscall: Some(normalized),
                        denied_events_last_minute: denied_count,
                        total_events_last_minute: total_count,
                        sample: truncate_sample(&signal.raw_line),
                    });
                }
            }

            if self.config.strict_mode && signal.denied {
                alerts.push(SyscallAnomalyAlert {
                    timestamp,
                    kind: SyscallAnomalyKind::DeniedSyscall,
                    command: command.to_string(),
                    syscall: signal
                        .syscall
                        .as_deref()
                        .map(normalize_syscall_name)
                        .filter(|name| !name.is_empty()),
                    denied_events_last_minute: denied_count,
                    total_events_last_minute: total_count,
                    sample: truncate_sample(&signal.raw_line),
                });
            }
        }

        let denied_count = count_denied(&state.events);
        let total_count = u32::try_from(state.events.len()).unwrap_or(u32::MAX);
        if denied_count > self.config.max_denied_events_per_minute {
            let sample = signals
                .iter()
                .find(|signal| signal.denied)
                .map_or_else(String::new, |signal| truncate_sample(&signal.raw_line));
            alerts.push(SyscallAnomalyAlert {
                timestamp,
                kind: SyscallAnomalyKind::DeniedRateExceeded,
                command: command.to_string(),
                syscall: None,
                denied_events_last_minute: denied_count,
                total_events_last_minute: total_count,
                sample,
            });
        }
        if total_count > self.config.max_total_events_per_minute {
            let sample = signals
                .first()
                .map_or_else(String::new, |signal| truncate_sample(&signal.raw_line));
            alerts.push(SyscallAnomalyAlert {
                timestamp,
                kind: SyscallAnomalyKind::EventRateExceeded,
                command: command.to_string(),
                syscall: None,
                denied_events_last_minute: denied_count,
                total_events_last_minute: total_count,
                sample,
            });
        }
        // Deduplicate per command inspection call to avoid repeated spam.
        let mut seen = HashSet::new();
        let mut emit_queue = Vec::new();
        for alert in alerts {
            let key = (alert.kind, alert.syscall.clone(), alert.sample.clone());
            if !seen.insert(key) {
                continue;
            }
            if should_emit_alert(&mut state, &self.config, &alert, now) {
                emit_queue.push(alert);
            }
        }
        drop(state);

        for alert in &emit_queue {
            self.emit_alert(alert, exit_code);
        }

        emit_queue
    }

    fn emit_alert(&self, alert: &SyscallAnomalyAlert, exit_code: Option<i32>) {
        tracing::warn!(
            target: "security::syscall_anomaly",
            kind = ?alert.kind,
            command = %alert.command,
            syscall = %alert.syscall.as_deref().unwrap_or("-"),
            denied_last_min = alert.denied_events_last_minute,
            total_last_min = alert.total_events_last_minute,
            sample = %alert.sample,
            "syscall anomaly detected"
        );

        if let Err(error) = self.append_log_line(alert) {
            tracing::debug!("failed to append syscall anomaly log: {error}");
        }

        if let Some(logger) = &self.audit_logger {
            let mut event = AuditEvent::new(AuditEventType::SecurityEvent)
                .with_actor("daemon".to_string(), None, None)
                .with_action(alert.command.clone(), "high".to_string(), true, false)
                .with_result(false, exit_code, 0, Some(alert.sample.clone()));
            event.security.policy_violation = true;
            let _ = logger.log(&event);
        }
    }

    fn append_log_line(&self, alert: &SyscallAnomalyAlert) -> anyhow::Result<()> {
        if let Some(parent) = self.anomaly_log_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let line = serde_json::to_string(alert)?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.anomaly_log_path)?;
        writeln!(file, "{line}")?;
        file.sync_all()?;
        Ok(())
    }
}

fn normalize_baseline(raw: &[String]) -> HashSet<String> {
    raw.iter()
        .map(|name| normalize_syscall_name(name))
        .filter(|name| !name.is_empty())
        .collect()
}

fn normalize_syscall_name(name: &str) -> String {
    name.trim().to_ascii_lowercase()
}

fn resolve_log_path(base_dir: &Path, configured_path: &str) -> PathBuf {
    let trimmed = configured_path.trim();
    let path = Path::new(trimmed);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base_dir.join(path)
    }
}

fn truncate_sample(raw: &str) -> String {
    if raw.len() <= MAX_ALERT_SAMPLE_CHARS {
        return raw.to_string();
    }
    let idx = crate::util::floor_utf8_char_boundary(raw, MAX_ALERT_SAMPLE_CHARS);
    format!("{}...", &raw[..idx])
}

fn count_denied(events: &VecDeque<ObservedEvent>) -> u32 {
    let count = events.iter().filter(|event| event.denied).count();
    u32::try_from(count).unwrap_or(u32::MAX)
}

fn prune_old_events(events: &mut VecDeque<ObservedEvent>, now: Instant) {
    while let Some(event) = events.front() {
        if now.duration_since(event.at) <= RATE_WINDOW {
            break;
        }
        let _ = events.pop_front();
    }
}

fn prune_old_alert_timestamps(timestamps: &mut VecDeque<Instant>, now: Instant) {
    while let Some(at) = timestamps.front() {
        if now.duration_since(*at) <= RATE_WINDOW {
            break;
        }
        let _ = timestamps.pop_front();
    }
}

fn command_identity(command: &str) -> String {
    let token = command.split_whitespace().next().unwrap_or("-");
    let lowered = token.to_ascii_lowercase();
    if lowered.len() <= 64 {
        lowered
    } else {
        let boundary = crate::util::floor_utf8_char_boundary(&lowered, 64);
        lowered[..boundary].to_string()
    }
}

fn should_emit_alert(
    state: &mut DetectorState,
    config: &SyscallAnomalyConfig,
    alert: &SyscallAnomalyAlert,
    now: Instant,
) -> bool {
    prune_old_alert_timestamps(&mut state.alert_timestamps, now);
    state
        .last_alert_by_key
        .retain(|_, at| now.duration_since(*at) <= RATE_WINDOW);

    if state.alert_timestamps.len() >= config.max_alerts_per_minute as usize {
        return false;
    }

    let key = AlertKey {
        kind: alert.kind,
        syscall: alert.syscall.clone(),
        command_id: command_identity(&alert.command),
    };

    if let Some(last_at) = state.last_alert_by_key.get(&key) {
        let cooldown = Duration::from_secs(config.alert_cooldown_secs);
        if now.duration_since(*last_at) < cooldown {
            return false;
        }
    }

    state.alert_timestamps.push_back(now);
    state.last_alert_by_key.insert(key, now);
    true
}

fn extract_signals(stderr: &str, stdout: &str) -> Vec<ParsedSyscallSignal> {
    stderr
        .lines()
        .chain(stdout.lines())
        .filter_map(parse_syscall_signal)
        .collect()
}

fn parse_syscall_signal(line: &str) -> Option<ParsedSyscallSignal> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }

    let lower = trimmed.to_ascii_lowercase();
    let looks_relevant = lower.contains("syscall")
        || lower.contains("seccomp")
        || lower.contains("sigsys")
        || lower.contains("bad system call")
        || lower.contains("audit(");
    if !looks_relevant {
        return None;
    }

    let denied = lower.contains("denied")
        || lower.contains("blocked")
        || lower.contains("forbidden")
        || lower.contains("bad system call")
        || lower.contains("sigsys")
        || lower.contains("operation not permitted")
        || lower.contains(" eperm")
        || lower.contains("killed");

    let syscall = extract_syscall_name(trimmed);

    if syscall.is_none() && !denied && !lower.contains("seccomp") {
        return None;
    }

    Some(ParsedSyscallSignal {
        syscall,
        denied,
        raw_line: trimmed.to_string(),
    })
}

fn extract_syscall_name(line: &str) -> Option<String> {
    let captures = syscall_field_re().captures(line)?;
    let raw = captures.get(1)?.as_str().trim().trim_matches('"');
    if raw.is_empty() {
        return None;
    }

    if let Some(symbolic) = normalize_symbolic_syscall(raw) {
        return Some(symbolic);
    }

    if let Some(syscall_nr) = parse_syscall_number(raw) {
        if let Some(mapped) = map_linux_x86_64_syscall(syscall_nr) {
            return Some(mapped.to_string());
        }
        return Some(format!("syscall#{syscall_nr}"));
    }

    Some(normalize_syscall_name(raw))
}

fn syscall_field_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r#"(?i)\b(?:syscall(?:_name)?|system\s+call)\s*(?:nr|number)?\s*(?:=|:)?\s*([A-Za-z0-9_x]+)"#,
        )
            .expect("syscall regex must compile")
    })
}

fn normalize_symbolic_syscall(raw: &str) -> Option<String> {
    if let Some(stripped) = raw.strip_prefix("__NR_") {
        return Some(stripped.to_ascii_lowercase());
    }
    if let Some(stripped) = raw.strip_prefix("__nr_") {
        return Some(stripped.to_ascii_lowercase());
    }
    if let Some(stripped) = raw.strip_prefix("SYS_") {
        return Some(stripped.to_ascii_lowercase());
    }
    None
}

fn parse_syscall_number(raw: &str) -> Option<i64> {
    let lower = raw.to_ascii_lowercase();
    if lower.starts_with("0x") {
        i64::from_str_radix(lower.trim_start_matches("0x"), 16).ok()
    } else if lower.chars().all(|ch| ch.is_ascii_digit()) {
        lower.parse::<i64>().ok()
    } else {
        None
    }
}

fn map_linux_x86_64_syscall(number: i64) -> Option<&'static str> {
    match number {
        0 => Some("read"),
        1 => Some("write"),
        2 => Some("open"),
        3 => Some("close"),
        9 => Some("mmap"),
        10 => Some("mprotect"),
        11 => Some("munmap"),
        12 => Some("brk"),
        16 => Some("ioctl"),
        32 => Some("dup"),
        33 => Some("dup2"),
        39 => Some("getpid"),
        41 => Some("socket"),
        42 => Some("connect"),
        43 => Some("accept"),
        44 => Some("sendto"),
        45 => Some("recvfrom"),
        47 => Some("recvmsg"),
        50 => Some("listen"),
        51 => Some("getsockname"),
        52 => Some("getpeername"),
        54 => Some("setsockopt"),
        55 => Some("getsockopt"),
        56 => Some("clone"),
        57 => Some("fork"),
        59 => Some("execve"),
        60 => Some("exit"),
        61 => Some("wait4"),
        72 => Some("fcntl"),
        202 => Some("futex"),
        218 => Some("set_tid_address"),
        231 => Some("exit_group"),
        257 => Some("openat"),
        262 => Some("newfstatat"),
        273 => Some("set_robust_list"),
        291 => Some("epoll_create1"),
        318 => Some("getrandom"),
        332 => Some("statx"),
        435 => Some("clone3"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn detector_with(config: SyscallAnomalyConfig) -> SyscallAnomalyDetector {
        let tmp = tempfile::tempdir().expect("tempdir");
        let audit = AuditConfig {
            enabled: false,
            ..AuditConfig::default()
        };
        SyscallAnomalyDetector::new(config, tmp.path(), audit)
    }

    #[test]
    fn parse_syscall_signal_extracts_numeric_audit_syscall() {
        let line = r#"audit: type=1326 audit(1.234:66): auid=0 uid=0 gid=0 arch=c000003e syscall=59 compat=0"#;
        let signal = parse_syscall_signal(line).expect("signal should parse");
        assert_eq!(signal.syscall.as_deref(), Some("execve"));
        assert!(!signal.denied);
    }

    #[test]
    fn parse_syscall_signal_marks_denied_from_seccomp_line() {
        let line = "seccomp: denied syscall=openat by profile strict";
        let signal = parse_syscall_signal(line).expect("signal should parse");
        assert_eq!(signal.syscall.as_deref(), Some("openat"));
        assert!(signal.denied);
    }

    #[test]
    fn parse_syscall_signal_extracts_symbolic_name() {
        let line = "seccomp denied syscall=__NR_openat profile=default";
        let signal = parse_syscall_signal(line).expect("signal should parse");
        assert_eq!(signal.syscall.as_deref(), Some("openat"));
        assert!(signal.denied);
    }

    #[test]
    fn parse_syscall_signal_extracts_space_separated_number() {
        let line = "seccomp blocked system call nr 59 from child";
        let signal = parse_syscall_signal(line).expect("signal should parse");
        assert_eq!(signal.syscall.as_deref(), Some("execve"));
        assert!(signal.denied);
    }

    #[test]
    fn parse_syscall_signal_extracts_hex_syscall_number() {
        let line = "audit: type=1326 syscall=0x3b seccomp denied";
        let signal = parse_syscall_signal(line).expect("signal should parse");
        assert_eq!(signal.syscall.as_deref(), Some("execve"));
        assert!(signal.denied);
    }

    #[test]
    fn detector_alerts_on_unknown_syscall() {
        let config = SyscallAnomalyConfig {
            baseline_syscalls: vec!["read".into(), "write".into()],
            ..SyscallAnomalyConfig::default()
        };
        let detector = detector_with(config);
        let alerts = detector.inspect_command_output(
            "echo hi",
            "",
            "audit: type=1326 syscall=openat denied",
            Some(1),
        );
        assert!(alerts
            .iter()
            .any(|alert| alert.kind == SyscallAnomalyKind::UnknownSyscall));
    }

    #[test]
    fn detector_alerts_on_denied_rate_spike() {
        let config = SyscallAnomalyConfig {
            strict_mode: false,
            max_denied_events_per_minute: 1,
            baseline_syscalls: vec!["openat".into()],
            ..SyscallAnomalyConfig::default()
        };
        let detector = detector_with(config);
        let alerts = detector.inspect_command_output(
            "echo hi",
            "",
            "seccomp denied syscall=openat\nseccomp denied syscall=openat",
            Some(1),
        );
        assert!(alerts
            .iter()
            .any(|alert| alert.kind == SyscallAnomalyKind::DeniedRateExceeded));
    }

    #[test]
    fn detector_respects_disabled_mode() {
        let config = SyscallAnomalyConfig {
            enabled: false,
            ..SyscallAnomalyConfig::default()
        };
        let detector = detector_with(config);
        let alerts = detector.inspect_command_output(
            "echo hi",
            "",
            "seccomp denied syscall=openat",
            Some(1),
        );
        assert!(alerts.is_empty());
    }

    #[test]
    fn detector_applies_alert_cooldown() {
        let config = SyscallAnomalyConfig {
            max_denied_events_per_minute: 1,
            max_alerts_per_minute: 100,
            alert_cooldown_secs: 120,
            baseline_syscalls: vec!["openat".into()],
            ..SyscallAnomalyConfig::default()
        };
        let detector = detector_with(config);

        let first = detector.inspect_command_output(
            "echo hi",
            "",
            "seccomp denied syscall=openat\nseccomp denied syscall=openat",
            Some(1),
        );
        assert!(first
            .iter()
            .any(|alert| alert.kind == SyscallAnomalyKind::DeniedRateExceeded));

        let second = detector.inspect_command_output(
            "echo hi",
            "",
            "seccomp denied syscall=openat\nseccomp denied syscall=openat",
            Some(1),
        );
        assert!(
            !second
                .iter()
                .any(|alert| alert.kind == SyscallAnomalyKind::DeniedRateExceeded),
            "cooldown should suppress repeated identical rate alerts"
        );
    }

    #[test]
    fn detector_limits_alerts_per_minute() {
        let config = SyscallAnomalyConfig {
            max_alerts_per_minute: 1,
            alert_cooldown_secs: 1,
            baseline_syscalls: vec!["read".into(), "write".into()],
            ..SyscallAnomalyConfig::default()
        };
        let detector = detector_with(config);
        let alerts = detector.inspect_command_output(
            "echo hi",
            "",
            "seccomp denied syscall=openat\nseccomp denied syscall=clone3",
            Some(1),
        );
        assert_eq!(alerts.len(), 1, "alert budget should cap emitted alerts");
    }

    #[test]
    fn default_baseline_covers_common_mapped_syscalls() {
        let baseline = normalize_baseline(&SyscallAnomalyConfig::default().baseline_syscalls);
        let mapped_common = [43_i64, 50, 57, 72, 218, 273];
        for syscall_nr in mapped_common {
            let name = map_linux_x86_64_syscall(syscall_nr).expect("mapping should exist");
            assert!(
                baseline.contains(name),
                "default baseline should include mapped syscall {name}"
            );
        }
    }
}
