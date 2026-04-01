use parking_lot::Mutex;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

// Re-export from zeroclaw-config.
pub use crate::autonomy::AutonomyLevel;

/// Risk score for shell command execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandRiskLevel {
    Low,
    Medium,
    High,
}

/// Classifies whether a tool operation is read-only or side-effecting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolOperation {
    Read,
    Act,
}

/// Sliding-window action tracker for rate limiting.
#[derive(Debug)]
pub struct ActionTracker {
    /// Timestamps of recent actions (kept within the last hour).
    actions: Mutex<Vec<Instant>>,
}

impl Default for ActionTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl ActionTracker {
    pub fn new() -> Self {
        Self {
            actions: Mutex::new(Vec::new()),
        }
    }

    /// Record an action and return the current count within the window.
    pub fn record(&self) -> usize {
        let mut actions = self.actions.lock();
        let cutoff = Instant::now()
            .checked_sub(std::time::Duration::from_secs(3600))
            .unwrap_or_else(Instant::now);
        actions.retain(|t| *t > cutoff);
        actions.push(Instant::now());
        actions.len()
    }

    /// Count of actions in the current window without recording.
    pub fn count(&self) -> usize {
        let mut actions = self.actions.lock();
        let cutoff = Instant::now()
            .checked_sub(std::time::Duration::from_secs(3600))
            .unwrap_or_else(Instant::now);
        actions.retain(|t| *t > cutoff);
        actions.len()
    }
}

impl Clone for ActionTracker {
    fn clone(&self) -> Self {
        let actions = self.actions.lock();
        Self {
            actions: Mutex::new(actions.clone()),
        }
    }
}

/// Per-sender sliding-window rate limiter.
///
/// Each unique sender key (Telegram thread ID, Discord channel, etc.) gets
/// its own independent [`ActionTracker`] bucket. When no sender is in scope
/// (cron jobs, CLI), the [`GLOBAL_KEY`] bucket is used.
///
/// Note: sender buckets accumulate for the daemon lifetime with no eviction.
/// This is acceptable for bounded sets of chat IDs; in high-cardinality deployments,
/// consider periodic cleanup.
#[derive(Debug)]
pub struct PerSenderTracker {
    buckets: parking_lot::Mutex<HashMap<String, ActionTracker>>,
}

impl PerSenderTracker {
    /// Bucket key used when no per-sender context is available (cron, CLI).
    pub const GLOBAL_KEY: &'static str = "__global__";

    /// Create an empty tracker with no sender buckets.
    pub fn new() -> Self {
        Self {
            buckets: parking_lot::Mutex::new(HashMap::new()),
        }
    }

    /// Resolve the current sender key from the task-local, falling back to GLOBAL_KEY.
    fn current_key() -> String {
        zeroclaw_api::TOOL_LOOP_THREAD_ID
            .try_with(|v| v.clone())
            .ok()
            .flatten()
            .unwrap_or_else(|| Self::GLOBAL_KEY.to_string())
    }

    /// Record one action for the current sender. Returns `true` if allowed
    /// (count after recording <= max), `false` if budget exhausted.
    pub fn record_for_current(&self, max: u32) -> bool {
        let key = Self::current_key();
        self.record_within(&key, max)
    }

    /// Record one action for `key`. Allows the action when count == max (≤ max);
    /// blocks and returns false when count > max.
    pub fn record_within(&self, key: &str, max: u32) -> bool {
        let mut buckets = self.buckets.lock();
        let tracker = buckets.entry(key.to_string()).or_default();
        let count = tracker.record();
        count <= max as usize
    }

    /// Check if the current sender is at or over the limit (without recording).
    pub fn is_limited_for_current(&self, max: u32) -> bool {
        let key = Self::current_key();
        self.is_exhausted(&key, max)
    }

    /// Check if `key` is at or over `max` (without recording).
    /// Does NOT insert a bucket for unseen keys.
    /// A max of 0 is always exhausted (zero budget means no actions allowed).
    /// Returns true when count has reached or exceeded max. Note: acquires write lock
    /// because ActionTracker::count prunes stale entries internally. Also note: returns
    /// true one count earlier than record_within would block.
    pub fn is_exhausted(&self, key: &str, max: u32) -> bool {
        if max == 0 {
            return true;
        }
        let mut buckets = self.buckets.lock();
        match buckets.get_mut(key) {
            Some(tracker) => tracker.count() >= max as usize,
            None => false,
        }
    }
}

impl Clone for PerSenderTracker {
    fn clone(&self) -> Self {
        let buckets = self.buckets.lock();
        Self {
            buckets: parking_lot::Mutex::new(buckets.clone()),
        }
    }
}

impl Default for PerSenderTracker {
    fn default() -> Self {
        Self::new()
    }
}

/// Security policy enforced on all tool executions
#[derive(Debug, Clone)]
pub struct SecurityPolicy {
    pub autonomy: AutonomyLevel,
    pub workspace_dir: PathBuf,
    pub workspace_only: bool,
    pub allowed_commands: Vec<String>,
    pub forbidden_paths: Vec<String>,
    pub allowed_roots: Vec<PathBuf>,
    pub max_actions_per_hour: u32,
    pub max_cost_per_day_cents: u32,
    pub require_approval_for_medium_risk: bool,
    pub block_high_risk_commands: bool,
    pub shell_env_passthrough: Vec<String>,
    pub shell_timeout_secs: u64,
    pub tracker: PerSenderTracker,
}

/// Default allowed commands for Unix platforms.
#[cfg(not(target_os = "windows"))]
fn default_allowed_commands() -> Vec<String> {
    #[allow(unused_mut)]
    let mut cmds = vec![
        "git".into(),
        "npm".into(),
        "cargo".into(),
        "ls".into(),
        "cat".into(),
        "grep".into(),
        "find".into(),
        "echo".into(),
        "pwd".into(),
        "wc".into(),
        "head".into(),
        "tail".into(),
        "date".into(),
        "df".into(),
        "du".into(),
        "uname".into(),
        "uptime".into(),
        "hostname".into(),
        "python".into(),
        "python3".into(),
        "pip".into(),
        "node".into(),
    ];
    // `free` is Linux-only; it does not exist on macOS or other BSDs.
    #[cfg(target_os = "linux")]
    cmds.push("free".into());
    cmds
}

/// Default allowed commands for Windows platforms.
///
/// Includes both native Windows commands and their Unix equivalents
/// (available via Git for Windows, WSL, etc.).
#[cfg(target_os = "windows")]
fn default_allowed_commands() -> Vec<String> {
    vec![
        // Cross-platform tools
        "git".into(),
        "npm".into(),
        "cargo".into(),
        "echo".into(),
        // Windows-native equivalents
        "dir".into(),
        "type".into(),
        "findstr".into(),
        "where".into(),
        "more".into(),
        "date".into(),
        // Unix commands (available via Git for Windows / MSYS2)
        "ls".into(),
        "cat".into(),
        "grep".into(),
        "find".into(),
        "pwd".into(),
        "wc".into(),
        "head".into(),
        "tail".into(),
        "df".into(),
        "du".into(),
        "uname".into(),
        "uptime".into(),
        "hostname".into(),
        "python".into(),
        "python3".into(),
        "pip".into(),
        "node".into(),
    ]
}

/// Default forbidden paths for Unix platforms.
#[cfg(not(target_os = "windows"))]
fn default_forbidden_paths() -> Vec<String> {
    vec![
        "/etc".into(),
        "/root".into(),
        "/home".into(),
        "/usr".into(),
        "/bin".into(),
        "/sbin".into(),
        "/lib".into(),
        "/opt".into(),
        "/boot".into(),
        "/dev".into(),
        "/proc".into(),
        "/sys".into(),
        "/var".into(),
        "/tmp".into(),
        "~/.ssh".into(),
        "~/.gnupg".into(),
        "~/.aws".into(),
        "~/.config".into(),
    ]
}

/// Default forbidden paths for Windows platforms.
#[cfg(target_os = "windows")]
fn default_forbidden_paths() -> Vec<String> {
    vec![
        "C:\\Windows".into(),
        "C:\\Windows\\System32".into(),
        "C:\\Program Files".into(),
        "C:\\Program Files (x86)".into(),
        "C:\\ProgramData".into(),
        "~/.ssh".into(),
        "~/.gnupg".into(),
        "~/.aws".into(),
        "~/.config".into(),
    ]
}

impl Default for SecurityPolicy {
    fn default() -> Self {
        Self {
            autonomy: AutonomyLevel::Supervised,
            workspace_dir: PathBuf::from("."),
            workspace_only: true,
            allowed_commands: default_allowed_commands(),
            forbidden_paths: default_forbidden_paths(),
            allowed_roots: Vec::new(),
            max_actions_per_hour: 20,
            max_cost_per_day_cents: 500,
            require_approval_for_medium_risk: true,
            block_high_risk_commands: true,
            shell_env_passthrough: vec![],
            shell_timeout_secs: 60,
            tracker: PerSenderTracker::new(),
        }
    }
}

fn home_dir() -> Option<PathBuf> {
    #[cfg(not(target_os = "windows"))]
    {
        std::env::var_os("HOME").map(PathBuf::from)
    }
    #[cfg(target_os = "windows")]
    {
        std::env::var_os("USERPROFILE")
            .or_else(|| std::env::var_os("HOME"))
            .map(PathBuf::from)
    }
}

fn expand_user_path(path: &str) -> PathBuf {
    if path == "~"
        && let Some(home) = home_dir()
    {
        return home;
    }

    if let Some(stripped) = path.strip_prefix("~/")
        && let Some(home) = home_dir()
    {
        return home.join(stripped);
    }

    PathBuf::from(path)
}

fn rootless_path(path: &Path) -> Option<PathBuf> {
    let mut relative = PathBuf::new();

    for component in path.components() {
        match component {
            std::path::Component::Prefix(_)
            | std::path::Component::RootDir
            | std::path::Component::CurDir => {}
            std::path::Component::ParentDir => return None,
            std::path::Component::Normal(part) => relative.push(part),
        }
    }

    if relative.as_os_str().is_empty() {
        None
    } else {
        Some(relative)
    }
}

// ── Shell Command Parsing Utilities ───────────────────────────────────────
// These helpers implement a minimal quote-aware shell lexer. They exist
// because security validation must reason about the *structure* of a
// command (separators, operators, quoting) rather than treating it as a
// flat string — otherwise an attacker could hide dangerous sub-commands
// inside quoted arguments or chained operators.
/// Skip leading environment variable assignments (e.g. `FOO=bar cmd args`).
/// Returns the remainder starting at the first non-assignment word.
fn skip_env_assignments(s: &str) -> &str {
    let mut rest = s;
    loop {
        let Some(word) = rest.split_whitespace().next() else {
            return rest;
        };
        // Environment assignment: contains '=' and starts with a letter or underscore
        if word.contains('=')
            && word
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        {
            // Advance past this word
            rest = rest[word.len()..].trim_start();
        } else {
            return rest;
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QuoteState {
    None,
    Single,
    Double,
}

/// Split a shell command into sub-commands by unquoted separators.
///
/// Separators:
/// - `;` and newline
/// - `|`
/// - `&&`, `||`
///
/// Characters inside single or double quotes are treated as literals, so
/// `sqlite3 db "SELECT 1; SELECT 2;"` remains a single segment.
fn split_unquoted_segments(command: &str) -> Vec<String> {
    let mut segments = Vec::new();
    let mut current = String::new();
    let mut quote = QuoteState::None;
    let mut escaped = false;
    let mut chars = command.chars().peekable();

    let push_segment = |segments: &mut Vec<String>, current: &mut String| {
        let trimmed = current.trim();
        if !trimmed.is_empty() {
            segments.push(trimmed.to_string());
        }
        current.clear();
    };

    while let Some(ch) = chars.next() {
        match quote {
            QuoteState::Single => {
                if ch == '\'' {
                    quote = QuoteState::None;
                }
                current.push(ch);
            }
            QuoteState::Double => {
                if escaped {
                    escaped = false;
                    current.push(ch);
                    continue;
                }
                if ch == '\\' {
                    escaped = true;
                    current.push(ch);
                    continue;
                }
                if ch == '"' {
                    quote = QuoteState::None;
                }
                current.push(ch);
            }
            QuoteState::None => {
                if escaped {
                    escaped = false;
                    current.push(ch);
                    continue;
                }
                if ch == '\\' {
                    escaped = true;
                    current.push(ch);
                    continue;
                }

                match ch {
                    '\'' => {
                        quote = QuoteState::Single;
                        current.push(ch);
                    }
                    '"' => {
                        quote = QuoteState::Double;
                        current.push(ch);
                    }
                    ';' | '\n' => push_segment(&mut segments, &mut current),
                    '|' => {
                        if chars.next_if_eq(&'|').is_some() {
                            // Consume full `||`; both characters are separators.
                        }
                        push_segment(&mut segments, &mut current);
                    }
                    '&' => {
                        if chars.next_if_eq(&'&').is_some() {
                            // `&&` is a separator; single `&` is handled separately.
                            push_segment(&mut segments, &mut current);
                        } else {
                            current.push(ch);
                        }
                    }
                    _ => current.push(ch),
                }
            }
        }
    }

    let trimmed = current.trim();
    if !trimmed.is_empty() {
        segments.push(trimmed.to_string());
    }

    segments
}

/// Detect a single unquoted `&` operator (background/chain). `&&` is allowed.
///
/// We treat any standalone `&` as unsafe in policy validation because it can
/// chain hidden sub-commands and escape foreground timeout expectations.
fn contains_unquoted_single_ampersand(command: &str) -> bool {
    let mut quote = QuoteState::None;
    let mut escaped = false;
    let mut chars = command.chars().peekable();

    while let Some(ch) = chars.next() {
        match quote {
            QuoteState::Single => {
                if ch == '\'' {
                    quote = QuoteState::None;
                }
            }
            QuoteState::Double => {
                if escaped {
                    escaped = false;
                    continue;
                }
                if ch == '\\' {
                    escaped = true;
                    continue;
                }
                if ch == '"' {
                    quote = QuoteState::None;
                }
            }
            QuoteState::None => {
                if escaped {
                    escaped = false;
                    continue;
                }
                if ch == '\\' {
                    escaped = true;
                    continue;
                }
                match ch {
                    '\'' => quote = QuoteState::Single,
                    '"' => quote = QuoteState::Double,
                    '&' => {
                        if chars.next_if_eq(&'&').is_none() {
                            return true;
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    false
}

/// Detect an unquoted character in a shell command.
fn contains_unquoted_char(command: &str, target: char) -> bool {
    let mut quote = QuoteState::None;
    let mut escaped = false;

    for ch in command.chars() {
        match quote {
            QuoteState::Single => {
                if ch == '\'' {
                    quote = QuoteState::None;
                }
            }
            QuoteState::Double => {
                if escaped {
                    escaped = false;
                    continue;
                }
                if ch == '\\' {
                    escaped = true;
                    continue;
                }
                if ch == '"' {
                    quote = QuoteState::None;
                }
            }
            QuoteState::None => {
                if escaped {
                    escaped = false;
                    continue;
                }
                if ch == '\\' {
                    escaped = true;
                    continue;
                }
                match ch {
                    '\'' => quote = QuoteState::Single,
                    '"' => quote = QuoteState::Double,
                    _ if ch == target => return true,
                    _ => {}
                }
            }
        }
    }

    false
}

/// Returns true if `command` contains an unquoted `>` that is NOT a safe
/// stderr form (`2>/dev/null`, `2>&1`).
fn contains_unsafe_output_redirect(command: &str) -> bool {
    // Strip safe redirect-to-null and fd-merge patterns, then check for remaining `>`.
    let safe = command
        .replace("2>/dev/null", "")
        .replace(">/dev/null", "")
        .replace("1>/dev/null", "")
        .replace("2>&1", "")
        .replace("1>&2", "");
    contains_unquoted_char(&safe, '>')
}

/// Returns true if `command` contains an unquoted `<` that is NOT a heredoc (`<<`).
fn contains_unquoted_input_redirect(command: &str) -> bool {
    // Strip here-strings (`<<<`) first, then heredocs (`<<`), then check for remaining `<`.
    let safe = command.replace("<<<", "").replace("<<", "");
    contains_unquoted_char(&safe, '<')
}

/// Detect unquoted shell variable expansions like `$HOME`, `$1`, `$?`.
///
/// Escaped dollars (`\$`) are ignored. Variables inside single quotes are
/// treated as literals and therefore ignored.
fn contains_unquoted_shell_variable_expansion(command: &str) -> bool {
    let mut quote = QuoteState::None;
    let mut escaped = false;
    let chars: Vec<char> = command.chars().collect();

    for i in 0..chars.len() {
        let ch = chars[i];

        match quote {
            QuoteState::Single => {
                if ch == '\'' {
                    quote = QuoteState::None;
                }
                continue;
            }
            QuoteState::Double => {
                if escaped {
                    escaped = false;
                    continue;
                }
                if ch == '\\' {
                    escaped = true;
                    continue;
                }
                if ch == '"' {
                    quote = QuoteState::None;
                    continue;
                }
            }
            QuoteState::None => {
                if escaped {
                    escaped = false;
                    continue;
                }
                if ch == '\\' {
                    escaped = true;
                    continue;
                }
                if ch == '\'' {
                    quote = QuoteState::Single;
                    continue;
                }
                if ch == '"' {
                    quote = QuoteState::Double;
                    continue;
                }
            }
        }

        if ch != '$' {
            continue;
        }

        let Some(next) = chars.get(i + 1).copied() else {
            continue;
        };
        if next.is_ascii_alphanumeric()
            || matches!(
                next,
                '_' | '{' | '(' | '#' | '?' | '!' | '$' | '*' | '@' | '-'
            )
        {
            return true;
        }
    }

    false
}

fn strip_wrapping_quotes(token: &str) -> &str {
    token.trim_matches(|c| c == '"' || c == '\'')
}

fn looks_like_path(candidate: &str) -> bool {
    candidate.starts_with('/')
        || candidate.starts_with("./")
        || candidate.starts_with("../")
        || candidate.starts_with('~')
        || candidate == "."
        || candidate == ".."
        || candidate.contains('/')
        // Windows path patterns: drive letters (C:\, D:\) and UNC paths (\\server\share)
        || (cfg!(target_os = "windows")
            && (candidate
                .get(1..3)
                .is_some_and(|s| s == ":\\" || s == ":/")
                || candidate.starts_with("\\\\")))
}

fn attached_short_option_value(token: &str) -> Option<&str> {
    // Examples:
    // -f/etc/passwd   -> /etc/passwd
    // -C../outside    -> ../outside
    // -I./include     -> ./include
    let body = token.strip_prefix('-')?;
    if body.starts_with('-') || body.len() < 2 {
        return None;
    }
    let value = body[1..].trim_start_matches('=').trim();
    if value.is_empty() { None } else { Some(value) }
}

fn redirection_target(token: &str) -> Option<&str> {
    let marker_idx = token.find(['<', '>'])?;
    let mut rest = &token[marker_idx + 1..];
    rest = rest.trim_start_matches(['<', '>']);
    rest = rest.trim_start_matches('&');
    rest = rest.trim_start_matches(|c: char| c.is_ascii_digit());
    let trimmed = rest.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

/// Extract the basename from a command path, handling both Unix (`/`) and
/// Windows (`\`) separators so that `C:\Git\bin\git.exe` resolves to `git.exe`.
fn command_basename(raw: &str) -> &str {
    let after_fwd = raw.rsplit('/').next().unwrap_or(raw);
    after_fwd.rsplit('\\').next().unwrap_or(after_fwd)
}

/// Strip common Windows executable suffixes (.exe, .cmd, .bat) for uniform
/// matching against allowlists and risk tables. On non-Windows platforms this
/// is a no-op that returns the input unchanged.
fn strip_windows_exe_suffix(name: &str) -> &str {
    if cfg!(target_os = "windows") {
        name.strip_suffix(".exe")
            .or_else(|| name.strip_suffix(".cmd"))
            .or_else(|| name.strip_suffix(".bat"))
            .unwrap_or(name)
    } else {
        name
    }
}

fn is_allowlist_entry_match(allowed: &str, executable: &str, executable_base: &str) -> bool {
    let allowed = strip_wrapping_quotes(allowed).trim();
    if allowed.is_empty() {
        return false;
    }

    // Explicit wildcard support for "allow any command name/path".
    if allowed == "*" {
        return true;
    }

    // Path-like allowlist entries must match the executable token exactly
    // after "~" expansion.
    if looks_like_path(allowed) {
        let allowed_path = expand_user_path(allowed);
        let executable_path = expand_user_path(executable);
        return executable_path == allowed_path;
    }

    // Command-name entries continue to match by basename.
    // On Windows, also match when the executable has a .exe/.cmd/.bat suffix
    // that the allowlist entry omits (e.g., allowlist "git" matches "git.exe").
    if allowed == executable_base {
        return true;
    }

    #[cfg(target_os = "windows")]
    {
        let base_lower = executable_base.to_ascii_lowercase();
        let allowed_lower = allowed.to_ascii_lowercase();
        for ext in &[".exe", ".cmd", ".bat"] {
            if base_lower == format!("{allowed_lower}{ext}") {
                return true;
            }
            if allowed_lower == format!("{base_lower}{ext}") {
                return true;
            }
        }
    }

    false
}

impl SecurityPolicy {
    // ── Risk Classification ──────────────────────────────────────────────
    // Risk is assessed per-segment (split on shell operators), and the
    // highest risk across all segments wins. This prevents bypasses like
    // `ls && rm -rf /` from being classified as Low just because `ls` is safe.

    /// Classify command risk. Any high-risk segment marks the whole command high.
    pub fn command_risk_level(&self, command: &str) -> CommandRiskLevel {
        let mut saw_medium = false;

        for segment in split_unquoted_segments(command) {
            let cmd_part = skip_env_assignments(&segment);
            let mut words = cmd_part.split_whitespace();
            let Some(base_raw) = words.next() else {
                continue;
            };

            let base_owned = command_basename(base_raw).to_ascii_lowercase();
            let base = strip_windows_exe_suffix(&base_owned);

            let args: Vec<String> = words.map(|w| w.to_ascii_lowercase()).collect();
            let joined_segment = cmd_part.to_ascii_lowercase();

            // High-risk commands (Unix and Windows)
            if matches!(
                base,
                "rm" | "mkfs"
                    | "dd"
                    | "shutdown"
                    | "reboot"
                    | "halt"
                    | "poweroff"
                    | "sudo"
                    | "su"
                    | "chown"
                    | "chmod"
                    | "useradd"
                    | "userdel"
                    | "usermod"
                    | "passwd"
                    | "mount"
                    | "umount"
                    | "iptables"
                    | "ufw"
                    | "firewall-cmd"
                    | "curl"
                    | "wget"
                    | "nc"
                    | "ncat"
                    | "netcat"
                    | "scp"
                    | "ssh"
                    | "ftp"
                    | "telnet"
                    // Windows-specific high-risk commands
                    | "del"
                    | "rmdir"
                    | "format"
                    | "reg"
                    | "net"
                    | "runas"
                    | "icacls"
                    | "takeown"
                    | "powershell"
                    | "pwsh"
                    | "wmic"
                    | "sc"
                    | "netsh"
            ) {
                return CommandRiskLevel::High;
            }

            if joined_segment.contains("rm -rf /")
                || joined_segment.contains("rm -fr /")
                || joined_segment.contains(":(){:|:&};:")
                // Windows destructive patterns
                || joined_segment.contains("del /s /q")
                || joined_segment.contains("rmdir /s /q")
                || joined_segment.contains("format c:")
            {
                return CommandRiskLevel::High;
            }

            // Medium-risk commands (state-changing, but not inherently destructive)
            let medium = match base {
                "git" => args.first().is_some_and(|verb| {
                    matches!(
                        verb.as_str(),
                        "commit"
                            | "push"
                            | "reset"
                            | "clean"
                            | "rebase"
                            | "merge"
                            | "cherry-pick"
                            | "revert"
                            | "branch"
                            | "checkout"
                            | "switch"
                            | "tag"
                    )
                }),
                "npm" | "pnpm" | "yarn" => args.first().is_some_and(|verb| {
                    matches!(
                        verb.as_str(),
                        "install" | "add" | "remove" | "uninstall" | "update" | "publish"
                    )
                }),
                "cargo" => args.first().is_some_and(|verb| {
                    matches!(
                        verb.as_str(),
                        "add" | "remove" | "install" | "clean" | "publish"
                    )
                }),
                "touch" | "mkdir" | "mv" | "cp" | "ln"
                // Windows medium-risk equivalents
                | "copy" | "xcopy" | "robocopy" | "move" | "ren" | "rename" | "mklink" => true,
                _ => false,
            };

            saw_medium |= medium;
        }

        if saw_medium {
            CommandRiskLevel::Medium
        } else {
            CommandRiskLevel::Low
        }
    }

    // ── Command Execution Policy Gate ──────────────────────────────────────
    // Validation follows a strict precedence order:
    //   1. Allowlist check (is the base command permitted at all?)
    //   2. Risk classification (high / medium / low)
    //   3. Policy flags (block_high_risk_commands, require_approval_for_medium_risk)
    //      — explicit allowlist entries exempt a command from the high-risk block,
    //        but the wildcard "*" does NOT grant an exemption.
    //   4. Autonomy level × approval status (supervised requires explicit approval)
    // This ordering ensures deny-by-default: unknown commands are rejected
    // before any risk or autonomy logic runs.

    /// Validate full command execution policy (allowlist + risk gate).
    pub fn validate_command_execution(
        &self,
        command: &str,
        approved: bool,
    ) -> Result<CommandRiskLevel, String> {
        if !self.is_command_allowed(command) {
            return Err(format!("Command not allowed by security policy: {command}"));
        }

        let risk = self.command_risk_level(command);

        // When the operator has set `allowed_commands = ["*"]` AND explicitly
        // disabled `block_high_risk_commands`, they have opted out of all
        // command-level restrictions.  Short-circuit: skip the risk and
        // autonomy gates entirely.  See #4485.
        let has_wildcard = self.allowed_commands.iter().any(|c| c.trim() == "*");
        if has_wildcard && !self.block_high_risk_commands {
            return Ok(risk);
        }

        if risk == CommandRiskLevel::High {
            if self.block_high_risk_commands && !self.is_command_explicitly_allowed(command) {
                return Err("Command blocked: high-risk command is disallowed by policy".into());
            }
            if self.autonomy == AutonomyLevel::Supervised && !approved {
                return Err(
                    "Command requires explicit approval (approved=true): high-risk operation"
                        .into(),
                );
            }
        }

        if risk == CommandRiskLevel::Medium
            && self.autonomy == AutonomyLevel::Supervised
            && self.require_approval_for_medium_risk
            && !approved
        {
            return Err(
                "Command requires explicit approval (approved=true): medium-risk operation".into(),
            );
        }

        Ok(risk)
    }

    /// Check whether **every** segment of a command is explicitly listed in
    /// `allowed_commands` — i.e., matched by a concrete entry rather than by
    /// the wildcard `"*"`.
    ///
    /// This is used to exempt explicitly-allowlisted high-risk commands from
    /// the `block_high_risk_commands` gate. The wildcard entry intentionally
    /// does **not** qualify as an explicit allowlist match, so that operators
    /// who set `allowed_commands = ["*"]` still get the high-risk safety net.
    fn is_command_explicitly_allowed(&self, command: &str) -> bool {
        let segments = split_unquoted_segments(command);
        for segment in &segments {
            let cmd_part = skip_env_assignments(segment);
            let mut words = cmd_part.split_whitespace();
            let raw_executable = strip_wrapping_quotes(words.next().unwrap_or("")).trim();
            let executable = if let Some(idx) = raw_executable.find(['<', '>']) {
                &raw_executable[..idx]
            } else {
                raw_executable
            };
            let base_cmd_owned = command_basename(executable).to_ascii_lowercase();
            let base_cmd = strip_windows_exe_suffix(&base_cmd_owned);

            if base_cmd.is_empty() {
                continue;
            }

            let explicitly_listed = self.allowed_commands.iter().any(|allowed| {
                let allowed = strip_wrapping_quotes(allowed).trim();
                // Skip wildcard — it does not count as an explicit entry.
                if allowed.is_empty() || allowed == "*" {
                    return false;
                }
                is_allowlist_entry_match(allowed, executable, base_cmd)
            });

            if !explicitly_listed {
                return false;
            }
        }

        // At least one real command must be present.
        segments.iter().any(|s| {
            let s = skip_env_assignments(s.trim());
            s.split_whitespace().next().is_some_and(|w| !w.is_empty())
        })
    }

    // ── Layered Command Allowlist ──────────────────────────────────────────
    // Defence-in-depth: five independent gates run in order before the
    // per-segment allowlist check. Each gate targets a specific bypass
    // technique. If any gate rejects, the whole command is blocked.

    /// Check if a shell command is allowed.
    ///
    /// Validates the **entire** command string, not just the first word:
    /// - Blocks subshell operators (`` ` ``, `$(`) that hide arbitrary execution
    /// - Splits on command separators (`|`, `&&`, `||`, `;`, newlines) and
    ///   validates each sub-command against the allowlist
    /// - Blocks single `&` background chaining (`&&` remains supported)
    /// - Blocks shell redirections (`<`, `>`, `>>`) that can bypass path policy
    /// - Blocks dangerous arguments (e.g. `find -exec`, `git config`)
    pub fn is_command_allowed(&self, command: &str) -> bool {
        if self.autonomy == AutonomyLevel::ReadOnly {
            return false;
        }

        // Block subshell/expansion operators — these allow hiding arbitrary
        // commands inside an allowed command (e.g. `echo $(rm -rf /)`) and
        // bypassing path checks through variable indirection. The helper below
        // ignores escapes and literals inside single quotes, so `$(` or `${`
        // literals are permitted there.
        if command.contains('`')
            || contains_unquoted_shell_variable_expansion(command)
            || command.contains("<(")
            || command.contains(">(")
        {
            return false;
        }

        // Block shell redirections that target files. Allow safe forms:
        //   - `2>/dev/null`, `>/dev/null`, `1>/dev/null` (output suppression)
        //   - `2>&1`, `1>&2` (fd merging)
        //   - `<<` heredocs, `<<<` here-strings (input literals)
        if contains_unsafe_output_redirect(command) {
            return false;
        }
        if contains_unquoted_input_redirect(command) {
            return false;
        }

        // Block `tee` — it can write to arbitrary files, bypassing the
        // redirect check above (e.g. `echo secret | tee /etc/crontab`)
        if command
            .split_whitespace()
            .any(|w| w == "tee" || w.ends_with("/tee"))
        {
            return false;
        }

        // Block background command chaining (`&`), which can hide extra
        // sub-commands and outlive timeout expectations. Keep `&&` allowed.
        // Strip `2>&1` first so its `&` isn't flagged as background chaining.
        let ampersand_check = command.replace("2>&1", "");
        if contains_unquoted_single_ampersand(&ampersand_check) {
            return false;
        }

        // Split on unquoted command separators and validate each sub-command.
        let segments = split_unquoted_segments(command);
        for segment in &segments {
            // Strip leading env var assignments (e.g. FOO=bar cmd)
            let cmd_part = skip_env_assignments(segment);

            let mut words = cmd_part.split_whitespace();
            let raw_executable = strip_wrapping_quotes(words.next().unwrap_or("")).trim();
            // Strip inline redirections from the executable token, e.g.
            // `cat</dev/null` -> `cat`, so the allowlist check sees the real
            // command name rather than the redirect target path.
            let executable = if let Some(idx) = raw_executable.find(['<', '>']) {
                &raw_executable[..idx]
            } else {
                raw_executable
            };
            let base_cmd_owned = command_basename(executable).to_ascii_lowercase();
            let base_cmd = strip_windows_exe_suffix(&base_cmd_owned);

            if base_cmd.is_empty() {
                continue;
            }

            if !self
                .allowed_commands
                .iter()
                .any(|allowed| is_allowlist_entry_match(allowed, executable, base_cmd))
            {
                return false;
            }

            // Validate arguments for the command
            let args: Vec<String> = words.map(|w| w.to_ascii_lowercase()).collect();
            if !self.is_args_safe(base_cmd, &args) {
                return false;
            }
        }

        // At least one command must be present
        segments.iter().any(|s| {
            let s = skip_env_assignments(s.trim());
            s.split_whitespace().next().is_some_and(|w| !w.is_empty())
        })
    }

    /// Check for dangerous arguments that allow sub-command execution.
    fn is_args_safe(&self, base: &str, args: &[String]) -> bool {
        let base = base.to_ascii_lowercase();
        match base.as_str() {
            "find" => {
                // find -exec and find -ok allow arbitrary command execution
                !args.iter().any(|arg| arg == "-exec" || arg == "-ok")
            }
            "git" => {
                // git config, alias, and -c can be used to set dangerous options
                // (e.g. git config core.editor "rm -rf /")
                !args.iter().any(|arg| {
                    arg == "config"
                        || arg.starts_with("config.")
                        || arg == "alias"
                        || arg.starts_with("alias.")
                        || arg == "-c"
                })
            }
            _ => true,
        }
    }

    /// Return the first path-like argument blocked by path policy.
    ///
    /// This is best-effort token parsing for shell commands and is intended
    /// as a safety gate before command execution.
    pub fn forbidden_path_argument(&self, command: &str) -> Option<String> {
        let forbidden_candidate = |raw: &str| {
            let candidate = strip_wrapping_quotes(raw).trim();
            if candidate.is_empty() || candidate.contains("://") {
                return None;
            }
            if looks_like_path(candidate) && !self.is_path_allowed(candidate) {
                Some(candidate.to_string())
            } else {
                None
            }
        };

        for segment in split_unquoted_segments(command) {
            let cmd_part = skip_env_assignments(&segment);
            let mut words = cmd_part.split_whitespace();
            let Some(executable) = words.next() else {
                continue;
            };

            // Cover inline forms like `cat</etc/passwd`.
            if let Some(target) = redirection_target(strip_wrapping_quotes(executable))
                && let Some(blocked) = forbidden_candidate(target)
            {
                return Some(blocked);
            }

            for token in words {
                let candidate = strip_wrapping_quotes(token).trim();
                if candidate.is_empty() || candidate.contains("://") {
                    continue;
                }

                if let Some(target) = redirection_target(candidate)
                    && let Some(blocked) = forbidden_candidate(target)
                {
                    return Some(blocked);
                }

                // Handle option assignment forms like `--file=/etc/passwd`.
                if candidate.starts_with('-') {
                    if let Some((_, value)) = candidate.split_once('=')
                        && let Some(blocked) = forbidden_candidate(value)
                    {
                        return Some(blocked);
                    }
                    if let Some(value) = attached_short_option_value(candidate)
                        && let Some(blocked) = forbidden_candidate(value)
                    {
                        return Some(blocked);
                    }
                    continue;
                }

                if let Some(blocked) = forbidden_candidate(candidate) {
                    return Some(blocked);
                }
            }
        }

        None
    }

    // ── Path Validation ────────────────────────────────────────────────
    // Layered checks: null-byte injection → component-level traversal →
    // URL-encoded traversal → tilde expansion → absolute-path block →
    // forbidden-prefix match. Each layer addresses a distinct escape
    // technique; together they enforce workspace confinement.

    /// Check if a file path is allowed (no path traversal, within workspace)
    pub fn is_path_allowed(&self, path: &str) -> bool {
        // Block null bytes (can truncate paths in C-backed syscalls)
        if path.contains('\0') {
            return false;
        }

        // Block path traversal: check for ".." as a path component
        if Path::new(path)
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            return false;
        }

        // Block URL-encoded traversal attempts (e.g. ..%2f)
        let lower = path.to_lowercase();
        if lower.contains("..%2f") || lower.contains("%2f..") {
            return false;
        }

        // Reject "~user" forms because the shell expands them at runtime and
        // they can escape workspace policy.
        if path.starts_with('~') && path != "~" && !path.starts_with("~/") {
            return false;
        }

        // Expand "~" for consistent matching with forbidden paths and allowlists.
        let expanded_path = expand_user_path(path);

        // When workspace_only is set and the path is absolute, only allow it
        // if it falls within the workspace directory or an explicit allowed
        // root.  The workspace/allowed-root check runs BEFORE the forbidden
        // prefix list so that workspace paths under broad defaults like
        // "/home" are not rejected.  This mirrors the priority order in
        // `is_resolved_path_allowed`.  See #2880.
        if expanded_path.is_absolute() {
            let in_workspace = expanded_path.starts_with(&self.workspace_dir);
            let in_allowed_root = self
                .allowed_roots
                .iter()
                .any(|root| expanded_path.starts_with(root));

            if in_workspace || in_allowed_root {
                return true;
            }

            // Absolute path outside workspace/allowed roots — block when
            // workspace_only, or fall through to forbidden-prefix check.
            if self.workspace_only {
                return false;
            }
        }

        // Block forbidden paths using path-component-aware matching
        for forbidden in &self.forbidden_paths {
            let forbidden_path = expand_user_path(forbidden);
            if expanded_path.starts_with(forbidden_path) {
                return false;
            }
        }

        true
    }

    /// Validate that a resolved path is inside the workspace or an allowed root.
    /// Call this AFTER joining `workspace_dir` + relative path and canonicalizing.
    pub fn is_resolved_path_allowed(&self, resolved: &Path) -> bool {
        // Prefer canonical workspace root so `/a/../b` style config paths don't
        // cause false positives or negatives.
        let workspace_root = self
            .workspace_dir
            .canonicalize()
            .unwrap_or_else(|_| self.workspace_dir.clone());
        if resolved.starts_with(&workspace_root) {
            return true;
        }

        // Check extra allowed roots (e.g. shared skills directories) before
        // forbidden checks so explicit allowlists can coexist with broad
        // default forbidden roots such as `/home` and `/tmp`.
        for root in &self.allowed_roots {
            let canonical = root.canonicalize().unwrap_or_else(|_| root.clone());
            if resolved.starts_with(&canonical) {
                return true;
            }
        }

        // For paths outside workspace/allowlist, block forbidden roots to
        // prevent symlink escapes and sensitive directory access.
        for forbidden in &self.forbidden_paths {
            let forbidden_path = expand_user_path(forbidden);
            if resolved.starts_with(&forbidden_path) {
                return false;
            }
        }

        // When workspace_only is disabled the user explicitly opted out of
        // workspace confinement after forbidden-path checks are applied.
        if !self.workspace_only {
            return true;
        }

        false
    }

    fn runtime_config_dir(&self) -> Option<PathBuf> {
        let parent = self.workspace_dir.parent()?;
        Some(
            parent
                .canonicalize()
                .unwrap_or_else(|_| parent.to_path_buf()),
        )
    }

    pub fn is_runtime_config_path(&self, resolved: &Path) -> bool {
        let Some(config_dir) = self.runtime_config_dir() else {
            return false;
        };
        if !resolved.starts_with(&config_dir) {
            return false;
        }
        if resolved.parent() != Some(config_dir.as_path()) {
            return false;
        }

        let Some(file_name) = resolved.file_name().and_then(|value| value.to_str()) else {
            return false;
        };

        file_name == "config.toml"
            || file_name == "config.toml.bak"
            || file_name == "active_workspace.toml"
            || file_name.starts_with(".config.toml.tmp-")
            || file_name.starts_with(".active_workspace.toml.tmp-")
    }

    pub fn runtime_config_violation_message(&self, resolved: &Path) -> String {
        format!(
            "Refusing to modify ZeroClaw runtime config/state file: {}. Use dedicated config tools or edit it manually outside the agent loop.",
            resolved.display()
        )
    }

    pub fn resolved_path_violation_message(&self, resolved: &Path) -> String {
        let guidance = if self.allowed_roots.is_empty() {
            "Add the directory to [autonomy].allowed_roots (for example: allowed_roots = [\"/absolute/path\"]), or move the file into the workspace."
        } else {
            "Add a matching parent directory to [autonomy].allowed_roots, or move the file into the workspace."
        };

        format!(
            "Resolved path escapes workspace allowlist: {}. {}",
            resolved.display(),
            guidance
        )
    }

    /// Check if autonomy level permits any action at all
    pub fn can_act(&self) -> bool {
        self.autonomy != AutonomyLevel::ReadOnly
    }

    // ── Tool Operation Gating ──────────────────────────────────────────────
    // Read operations bypass autonomy and rate checks because they have
    // no side effects. Act operations must pass both the autonomy gate
    // (not read-only) and the sliding-window rate limiter.

    /// Enforce policy for a tool operation.
    ///
    /// Read operations are always allowed by autonomy/rate gates.
    /// Act operations require non-readonly autonomy and available action budget.
    pub fn enforce_tool_operation(
        &self,
        operation: ToolOperation,
        operation_name: &str,
    ) -> Result<(), String> {
        match operation {
            ToolOperation::Read => Ok(()),
            ToolOperation::Act => {
                if !self.can_act() {
                    return Err(format!(
                        "Security policy: read-only mode, cannot perform '{operation_name}'"
                    ));
                }

                if !self.record_action() {
                    return Err("Rate limit exceeded: action budget exhausted".to_string());
                }

                Ok(())
            }
        }
    }

    /// Record an action for the current sender and check if rate-limited.
    /// Returns `true` if allowed, `false` if budget exhausted.
    pub fn record_action(&self) -> bool {
        self.tracker.record_for_current(self.max_actions_per_hour)
    }

    /// Check if the current sender would be rate-limited without recording.
    pub fn is_rate_limited(&self) -> bool {
        self.tracker
            .is_limited_for_current(self.max_actions_per_hour)
    }

    /// Resolve a user-provided path for tool use.
    ///
    /// Expands `~` prefixes and resolves relative paths against the workspace
    /// directory. This should be called **after** `is_path_allowed` to obtain
    /// the filesystem path that the tool actually operates on.
    pub fn resolve_tool_path(&self, path: &str) -> PathBuf {
        let expanded = expand_user_path(path);
        if expanded.is_absolute() {
            expanded
        } else if let Some(workspace_hint) = rootless_path(&self.workspace_dir) {
            if let Ok(stripped) = expanded.strip_prefix(&workspace_hint) {
                if stripped.as_os_str().is_empty() {
                    self.workspace_dir.clone()
                } else {
                    self.workspace_dir.join(stripped)
                }
            } else {
                self.workspace_dir.join(expanded)
            }
        } else {
            self.workspace_dir.join(expanded)
        }
    }

    /// Check whether the given raw path (before canonicalization) falls under
    /// an `allowed_roots` entry. Tilde expansion is applied to the path
    /// before comparison. This is useful for tool-level pre-checks that want
    /// to allow absolute paths that are explicitly permitted by policy.
    pub fn is_under_allowed_root(&self, path: &str) -> bool {
        let expanded = expand_user_path(path);
        if !expanded.is_absolute() {
            return false;
        }
        self.allowed_roots.iter().any(|root| {
            let canonical = root.canonicalize().unwrap_or_else(|_| root.clone());
            expanded.starts_with(&canonical) || expanded.starts_with(root)
        })
    }

    /// Build from config sections
    pub fn from_config(
        autonomy_config: &crate::schema::AutonomyConfig,
        workspace_dir: &Path,
    ) -> Self {
        // When autonomy is Full, disable workspace_only so the agent can
        // access paths outside the workspace.  Forbidden-path checks still
        // apply, preventing access to sensitive system directories.
        // See issue #5463.
        let effective_workspace_only = if autonomy_config.level == AutonomyLevel::Full {
            false
        } else {
            autonomy_config.workspace_only
        };

        Self {
            autonomy: autonomy_config.level,
            workspace_dir: workspace_dir.to_path_buf(),
            workspace_only: effective_workspace_only,
            allowed_commands: autonomy_config.allowed_commands.clone(),
            forbidden_paths: autonomy_config.forbidden_paths.clone(),
            allowed_roots: autonomy_config
                .allowed_roots
                .iter()
                .map(|root| {
                    let expanded = expand_user_path(root);
                    if expanded.is_absolute() {
                        expanded
                    } else {
                        workspace_dir.join(expanded)
                    }
                })
                .collect(),
            max_actions_per_hour: autonomy_config.max_actions_per_hour,
            max_cost_per_day_cents: autonomy_config.max_cost_per_day_cents,
            require_approval_for_medium_risk: autonomy_config.require_approval_for_medium_risk,
            block_high_risk_commands: autonomy_config.block_high_risk_commands,
            shell_env_passthrough: autonomy_config.shell_env_passthrough.clone(),
            shell_timeout_secs: autonomy_config.shell_timeout_secs,
            tracker: PerSenderTracker::new(),
        }
    }

    /// Render a human-readable summary of the active security constraints
    /// suitable for injection into the LLM system prompt.
    ///
    /// Giving the LLM visibility into these constraints prevents it from
    /// wasting tokens on commands / paths that will be rejected at runtime.
    /// See issue #2404.
    pub fn prompt_summary(&self) -> String {
        use std::fmt::Write;

        let mut out = String::new();

        // Autonomy level
        let _ = writeln!(out, "**Autonomy level**: {:?}", self.autonomy);

        // Workspace constraint
        if self.workspace_only {
            let _ = writeln!(
                out,
                "**Workspace boundary**: file operations are restricted to `{}`.",
                self.workspace_dir.display()
            );
        }

        // Allowed roots
        if !self.allowed_roots.is_empty() {
            let roots: Vec<String> = self
                .allowed_roots
                .iter()
                .map(|p| format!("`{}`", p.display()))
                .collect();
            let _ = writeln!(out, "**Additional allowed paths**: {}", roots.join(", "));
        }

        // Allowed commands
        if !self.allowed_commands.is_empty() {
            let cmds: Vec<String> = self
                .allowed_commands
                .iter()
                .map(|c| format!("`{c}`"))
                .collect();
            let _ = writeln!(
                out,
                "**Allowed shell commands**: {}. \
                 You may execute these commands freely.",
                cmds.join(", ")
            );
        }

        // Forbidden paths
        if !self.forbidden_paths.is_empty() {
            let paths: Vec<String> = self
                .forbidden_paths
                .iter()
                .map(|p| format!("`{p}`"))
                .collect();
            let _ = writeln!(
                out,
                "**Forbidden paths**: {}. \
                 Avoid accessing these paths.",
                paths.join(", ")
            );
        }

        // Risk controls
        if self.block_high_risk_commands {
            let _ = writeln!(
                out,
                "Exercise caution with destructive commands (rm, kill, reboot, etc.)."
            );
        }
        if self.require_approval_for_medium_risk {
            let _ = writeln!(
                out,
                "**Medium-risk commands** require user approval before execution."
            );
        }

        // Rate limit
        let _ = writeln!(
            out,
            "**Rate limit**: max {} actions per hour per chat (each conversation has its own independent budget).",
            self.max_actions_per_hour
        );

        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_policy() -> SecurityPolicy {
        SecurityPolicy::default()
    }

    fn readonly_policy() -> SecurityPolicy {
        SecurityPolicy {
            autonomy: AutonomyLevel::ReadOnly,
            ..SecurityPolicy::default()
        }
    }

    fn full_policy() -> SecurityPolicy {
        SecurityPolicy {
            autonomy: AutonomyLevel::Full,
            ..SecurityPolicy::default()
        }
    }

    // ── AutonomyLevel ────────────────────────────────────────

    #[test]
    fn autonomy_default_is_supervised() {
        assert_eq!(AutonomyLevel::default(), AutonomyLevel::Supervised);
    }

    #[test]
    fn autonomy_serde_roundtrip() {
        let json = serde_json::to_string(&AutonomyLevel::Full).unwrap();
        assert_eq!(json, "\"full\"");
        let parsed: AutonomyLevel = serde_json::from_str("\"readonly\"").unwrap();
        assert_eq!(parsed, AutonomyLevel::ReadOnly);
        let parsed2: AutonomyLevel = serde_json::from_str("\"supervised\"").unwrap();
        assert_eq!(parsed2, AutonomyLevel::Supervised);
    }

    #[test]
    fn can_act_readonly_false() {
        assert!(!readonly_policy().can_act());
    }

    #[test]
    fn can_act_supervised_true() {
        assert!(default_policy().can_act());
    }

    #[test]
    fn can_act_full_true() {
        assert!(full_policy().can_act());
    }

    #[test]
    fn enforce_tool_operation_read_allowed_in_readonly_mode() {
        let p = readonly_policy();
        assert!(
            p.enforce_tool_operation(ToolOperation::Read, "memory_recall")
                .is_ok()
        );
    }

    #[test]
    fn enforce_tool_operation_act_blocked_in_readonly_mode() {
        let p = readonly_policy();
        let err = p
            .enforce_tool_operation(ToolOperation::Act, "memory_store")
            .unwrap_err();
        assert!(err.contains("read-only mode"));
    }

    #[test]
    fn enforce_tool_operation_act_uses_rate_budget() {
        let p = SecurityPolicy {
            max_actions_per_hour: 0,
            ..default_policy()
        };
        let err = p
            .enforce_tool_operation(ToolOperation::Act, "memory_store")
            .unwrap_err();
        assert!(err.contains("Rate limit exceeded"));
    }

    // ── is_command_allowed ───────────────────────────────────

    #[test]
    fn allowed_commands_basic() {
        let p = default_policy();
        assert!(p.is_command_allowed("ls"));
        assert!(p.is_command_allowed("git status"));
        assert!(p.is_command_allowed("cargo build --release"));
        assert!(p.is_command_allowed("cat file.txt"));
        assert!(p.is_command_allowed("grep -r pattern ."));
        assert!(p.is_command_allowed("date"));
    }

    #[test]
    fn blocked_commands_basic() {
        let p = default_policy();
        assert!(!p.is_command_allowed("rm -rf /"));
        assert!(!p.is_command_allowed("sudo apt install"));
        assert!(!p.is_command_allowed("curl http://evil.com"));
        assert!(!p.is_command_allowed("wget http://evil.com"));
        assert!(!p.is_command_allowed("ruby exploit.rb"));
        assert!(!p.is_command_allowed("perl malicious.pl"));
    }

    #[test]
    fn readonly_blocks_all_commands() {
        let p = readonly_policy();
        assert!(!p.is_command_allowed("ls"));
        assert!(!p.is_command_allowed("cat file.txt"));
        assert!(!p.is_command_allowed("echo hello"));
    }

    #[test]
    fn full_autonomy_still_uses_allowlist() {
        let p = full_policy();
        assert!(p.is_command_allowed("ls"));
        assert!(!p.is_command_allowed("rm -rf /"));
    }

    #[test]
    fn command_with_absolute_path_extracts_basename() {
        let p = default_policy();
        assert!(p.is_command_allowed("/usr/bin/git status"));
        assert!(p.is_command_allowed("/bin/ls -la"));
    }

    #[test]
    fn allowlist_supports_explicit_executable_paths() {
        let p = SecurityPolicy {
            allowed_commands: vec!["/usr/bin/antigravity".into()],
            ..SecurityPolicy::default()
        };

        assert!(p.is_command_allowed("/usr/bin/antigravity"));
        assert!(!p.is_command_allowed("antigravity"));
    }

    #[test]
    fn allowlist_supports_wildcard_entry() {
        let p = SecurityPolicy {
            allowed_commands: vec!["*".into()],
            ..SecurityPolicy::default()
        };

        assert!(p.is_command_allowed("python3 --version"));
        assert!(p.is_command_allowed("/usr/bin/antigravity"));

        // Wildcard still respects risk gates in validate_command_execution.
        let blocked = p.validate_command_execution("rm -rf /tmp/test", true);
        assert!(blocked.is_err());
        assert!(blocked.unwrap_err().contains("high-risk"));
    }

    #[test]
    fn empty_command_blocked() {
        let p = default_policy();
        assert!(!p.is_command_allowed(""));
        assert!(!p.is_command_allowed("   "));
    }

    #[test]
    fn command_with_pipes_validates_all_segments() {
        let p = default_policy();
        // Both sides of the pipe are in the allowlist
        assert!(p.is_command_allowed("ls | grep foo"));
        assert!(p.is_command_allowed("cat file.txt | wc -l"));
        // Second command not in allowlist — blocked
        assert!(!p.is_command_allowed("ls | curl http://evil.com"));
        assert!(!p.is_command_allowed("echo hello | ruby -"));
    }

    #[test]
    fn custom_allowlist() {
        let p = SecurityPolicy {
            allowed_commands: vec!["docker".into(), "kubectl".into()],
            ..SecurityPolicy::default()
        };
        assert!(p.is_command_allowed("docker ps"));
        assert!(p.is_command_allowed("kubectl get pods"));
        assert!(!p.is_command_allowed("ls"));
        assert!(!p.is_command_allowed("git status"));
    }

    #[test]
    fn empty_allowlist_blocks_everything() {
        let p = SecurityPolicy {
            allowed_commands: vec![],
            ..SecurityPolicy::default()
        };
        assert!(!p.is_command_allowed("ls"));
        assert!(!p.is_command_allowed("echo hello"));
    }

    #[test]
    fn command_risk_low_for_read_commands() {
        let p = default_policy();
        assert_eq!(p.command_risk_level("git status"), CommandRiskLevel::Low);
        assert_eq!(p.command_risk_level("ls -la"), CommandRiskLevel::Low);
    }

    #[test]
    fn command_risk_medium_for_mutating_commands() {
        let p = SecurityPolicy {
            allowed_commands: vec!["git".into(), "touch".into()],
            ..SecurityPolicy::default()
        };
        assert_eq!(
            p.command_risk_level("git reset --hard HEAD~1"),
            CommandRiskLevel::Medium
        );
        assert_eq!(
            p.command_risk_level("touch file.txt"),
            CommandRiskLevel::Medium
        );
    }

    #[test]
    fn command_risk_high_for_dangerous_commands() {
        let p = SecurityPolicy {
            allowed_commands: vec!["rm".into()],
            ..SecurityPolicy::default()
        };
        assert_eq!(
            p.command_risk_level("rm -rf /tmp/test"),
            CommandRiskLevel::High
        );
    }

    #[test]
    fn validate_command_requires_approval_for_medium_risk() {
        let p = SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            require_approval_for_medium_risk: true,
            allowed_commands: vec!["touch".into()],
            ..SecurityPolicy::default()
        };

        let denied = p.validate_command_execution("touch test.txt", false);
        assert!(denied.is_err());
        assert!(denied.unwrap_err().contains("requires explicit approval"),);

        let allowed = p.validate_command_execution("touch test.txt", true);
        assert_eq!(allowed.unwrap(), CommandRiskLevel::Medium);
    }

    #[test]
    fn validate_command_blocks_high_risk_via_wildcard() {
        // Wildcard allows the command through is_command_allowed, but
        // block_high_risk_commands still rejects it because "*" does not
        // count as an explicit allowlist entry.
        let p = SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            allowed_commands: vec!["*".into()],
            ..SecurityPolicy::default()
        };

        let result = p.validate_command_execution("rm -rf /tmp/test", true);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("high-risk"));
    }

    #[test]
    fn validate_command_allows_explicitly_listed_high_risk() {
        // When a high-risk command is explicitly in allowed_commands, the
        // block_high_risk_commands gate is bypassed — the operator has made
        // a deliberate decision to permit it.
        let p = SecurityPolicy {
            autonomy: AutonomyLevel::Full,
            allowed_commands: vec!["curl".into()],
            block_high_risk_commands: true,
            ..SecurityPolicy::default()
        };

        let result = p.validate_command_execution("curl https://api.example.com/data", true);
        assert_eq!(result.unwrap(), CommandRiskLevel::High);
    }

    #[test]
    fn validate_command_allows_wget_when_explicitly_listed() {
        let p = SecurityPolicy {
            autonomy: AutonomyLevel::Full,
            allowed_commands: vec!["wget".into()],
            block_high_risk_commands: true,
            ..SecurityPolicy::default()
        };

        let result =
            p.validate_command_execution("wget https://releases.example.com/v1.tar.gz", true);
        assert_eq!(result.unwrap(), CommandRiskLevel::High);
    }

    #[test]
    fn validate_command_blocks_non_listed_high_risk_when_another_is_allowed() {
        // Allowing curl explicitly should not exempt wget.
        let p = SecurityPolicy {
            autonomy: AutonomyLevel::Full,
            allowed_commands: vec!["curl".into()],
            block_high_risk_commands: true,
            ..SecurityPolicy::default()
        };

        let result = p.validate_command_execution("wget https://evil.com", true);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not allowed"));
    }

    #[test]
    fn validate_command_explicit_rm_bypasses_high_risk_block() {
        // Operator explicitly listed "rm" — they accept the risk.
        let p = SecurityPolicy {
            autonomy: AutonomyLevel::Full,
            allowed_commands: vec!["rm".into()],
            block_high_risk_commands: true,
            ..SecurityPolicy::default()
        };

        let result = p.validate_command_execution("rm -rf /tmp/test", true);
        assert_eq!(result.unwrap(), CommandRiskLevel::High);
    }

    #[test]
    fn validate_command_high_risk_still_needs_approval_in_supervised() {
        // Even when explicitly allowed, supervised mode still requires
        // approval for high-risk commands (the approval gate is separate
        // from the block gate).
        let p = SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            allowed_commands: vec!["curl".into()],
            block_high_risk_commands: true,
            ..SecurityPolicy::default()
        };

        let denied = p.validate_command_execution("curl https://api.example.com", false);
        assert!(denied.is_err());
        assert!(denied.unwrap_err().contains("requires explicit approval"));

        let allowed = p.validate_command_execution("curl https://api.example.com", true);
        assert_eq!(allowed.unwrap(), CommandRiskLevel::High);
    }

    #[test]
    fn validate_command_pipe_needs_all_segments_explicitly_allowed() {
        // When a pipeline contains a high-risk command, every segment
        // must be explicitly allowed for the exemption to apply.
        let p = SecurityPolicy {
            autonomy: AutonomyLevel::Full,
            allowed_commands: vec!["curl".into(), "grep".into()],
            block_high_risk_commands: true,
            ..SecurityPolicy::default()
        };

        let result = p.validate_command_execution("curl https://api.example.com | grep data", true);
        assert_eq!(result.unwrap(), CommandRiskLevel::High);
    }

    #[test]
    fn validate_command_full_mode_skips_medium_risk_approval_gate() {
        let p = SecurityPolicy {
            autonomy: AutonomyLevel::Full,
            require_approval_for_medium_risk: true,
            allowed_commands: vec!["touch".into()],
            ..SecurityPolicy::default()
        };

        let result = p.validate_command_execution("touch test.txt", false);
        assert_eq!(result.unwrap(), CommandRiskLevel::Medium);
    }

    #[test]
    fn validate_command_rejects_background_chain_bypass() {
        let p = default_policy();
        let result = p.validate_command_execution("ls & python3 -c 'print(1)'", false);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not allowed"));
    }

    // ── is_path_allowed ─────────────────────────────────────

    #[test]
    fn relative_paths_allowed() {
        let p = default_policy();
        assert!(p.is_path_allowed("file.txt"));
        assert!(p.is_path_allowed("src/main.rs"));
        assert!(p.is_path_allowed("deep/nested/dir/file.txt"));
    }

    #[test]
    fn path_traversal_blocked() {
        let p = default_policy();
        assert!(!p.is_path_allowed("../etc/passwd"));
        assert!(!p.is_path_allowed("../../root/.ssh/id_rsa"));
        assert!(!p.is_path_allowed("foo/../../../etc/shadow"));
        assert!(!p.is_path_allowed(".."));
    }

    #[test]
    fn absolute_paths_blocked_when_workspace_only() {
        let p = default_policy();
        assert!(!p.is_path_allowed("/etc/passwd"));
        assert!(!p.is_path_allowed("/root/.ssh/id_rsa"));
        assert!(!p.is_path_allowed("/tmp/file.txt"));
    }

    #[test]
    fn absolute_path_inside_workspace_allowed_when_workspace_only() {
        let p = SecurityPolicy {
            workspace_dir: PathBuf::from("/home/user/.zeroclaw/workspace"),
            workspace_only: true,
            ..SecurityPolicy::default()
        };
        // Absolute path inside workspace should be allowed
        assert!(p.is_path_allowed("/home/user/.zeroclaw/workspace/images/example.png"));
        assert!(p.is_path_allowed("/home/user/.zeroclaw/workspace/file.txt"));
        // Absolute path outside workspace should still be blocked
        assert!(!p.is_path_allowed("/home/user/other/file.txt"));
        assert!(!p.is_path_allowed("/tmp/file.txt"));
    }

    #[test]
    fn absolute_path_in_allowed_root_permitted_when_workspace_only() {
        let p = SecurityPolicy {
            workspace_dir: PathBuf::from("/home/user/.zeroclaw/workspace"),
            workspace_only: true,
            allowed_roots: vec![PathBuf::from("/home/user/.zeroclaw/shared")],
            ..SecurityPolicy::default()
        };
        // Path in allowed root should be permitted
        assert!(p.is_path_allowed("/home/user/.zeroclaw/shared/data.txt"));
        // Path in workspace should still be permitted
        assert!(p.is_path_allowed("/home/user/.zeroclaw/workspace/file.txt"));
        // Path outside both should still be blocked
        assert!(!p.is_path_allowed("/home/user/other/file.txt"));
    }

    #[test]
    fn absolute_paths_allowed_when_not_workspace_only() {
        let p = SecurityPolicy {
            workspace_only: false,
            forbidden_paths: vec![],
            ..SecurityPolicy::default()
        };
        assert!(p.is_path_allowed("/tmp/file.txt"));
    }

    #[test]
    fn forbidden_paths_blocked() {
        let p = SecurityPolicy {
            workspace_only: false,
            ..SecurityPolicy::default()
        };
        assert!(!p.is_path_allowed("/etc/passwd"));
        assert!(!p.is_path_allowed("/root/.bashrc"));
        assert!(!p.is_path_allowed("~/.ssh/id_rsa"));
        assert!(!p.is_path_allowed("~/.gnupg/pubring.kbx"));
    }

    #[test]
    fn empty_path_allowed() {
        let p = default_policy();
        assert!(p.is_path_allowed(""));
    }

    #[test]
    fn dotfile_in_workspace_allowed() {
        let p = default_policy();
        assert!(p.is_path_allowed(".gitignore"));
        assert!(p.is_path_allowed(".env"));
    }

    // ── from_config ─────────────────────────────────────────

    #[test]
    fn from_config_maps_all_fields() {
        let autonomy_config = crate::schema::AutonomyConfig {
            level: AutonomyLevel::Full,
            workspace_only: false,
            allowed_commands: vec!["docker".into()],
            forbidden_paths: vec!["/secret".into()],
            max_actions_per_hour: 100,
            max_cost_per_day_cents: 1000,
            require_approval_for_medium_risk: false,
            block_high_risk_commands: false,
            shell_env_passthrough: vec!["DATABASE_URL".into()],
            ..crate::schema::AutonomyConfig::default()
        };
        let workspace = PathBuf::from("/tmp/test-workspace");
        let policy = SecurityPolicy::from_config(&autonomy_config, &workspace);

        assert_eq!(policy.autonomy, AutonomyLevel::Full);
        assert!(!policy.workspace_only);
        assert_eq!(policy.allowed_commands, vec!["docker"]);
        assert_eq!(policy.forbidden_paths, vec!["/secret"]);
        assert_eq!(policy.max_actions_per_hour, 100);
        assert_eq!(policy.max_cost_per_day_cents, 1000);
        assert!(!policy.require_approval_for_medium_risk);
        assert!(!policy.block_high_risk_commands);
        assert_eq!(policy.shell_env_passthrough, vec!["DATABASE_URL"]);
        assert_eq!(policy.workspace_dir, PathBuf::from("/tmp/test-workspace"));
    }

    #[test]
    fn from_config_full_autonomy_overrides_workspace_only() {
        // Issue #5463: Full autonomy should disable workspace_only even if the
        // config default keeps it true.
        let autonomy_config = crate::schema::AutonomyConfig {
            level: AutonomyLevel::Full,
            ..crate::schema::AutonomyConfig::default()
        };
        let workspace = PathBuf::from("/tmp/test-workspace");
        let policy = SecurityPolicy::from_config(&autonomy_config, &workspace);

        assert_eq!(policy.autonomy, AutonomyLevel::Full);
        assert!(
            !policy.workspace_only,
            "Full autonomy must override workspace_only to false"
        );
    }

    #[test]
    fn from_config_supervised_preserves_workspace_only() {
        let autonomy_config = crate::schema::AutonomyConfig {
            level: AutonomyLevel::Supervised,
            ..crate::schema::AutonomyConfig::default()
        };
        let workspace = PathBuf::from("/tmp/test-workspace");
        let policy = SecurityPolicy::from_config(&autonomy_config, &workspace);

        assert!(
            policy.workspace_only,
            "Supervised autonomy must preserve workspace_only default (true)"
        );
    }

    #[test]
    fn from_config_normalizes_allowed_roots() {
        let autonomy_config = crate::schema::AutonomyConfig {
            allowed_roots: vec!["~/Desktop".into(), "shared-data".into()],
            ..crate::schema::AutonomyConfig::default()
        };
        let workspace = PathBuf::from("/tmp/test-workspace");
        let policy = SecurityPolicy::from_config(&autonomy_config, &workspace);

        let expected_home_root = if let Some(home) = std::env::var_os("HOME") {
            PathBuf::from(home).join("Desktop")
        } else {
            PathBuf::from("~/Desktop")
        };

        assert_eq!(policy.allowed_roots[0], expected_home_root);
        assert_eq!(policy.allowed_roots[1], workspace.join("shared-data"));
    }

    #[test]
    fn resolved_path_violation_message_includes_allowed_roots_guidance() {
        let p = default_policy();
        let msg = p.resolved_path_violation_message(Path::new("/tmp/outside.txt"));
        assert!(msg.contains("escapes workspace"));
        assert!(msg.contains("allowed_roots"));
    }

    // ── Default policy ──────────────────────────────────────

    #[test]
    fn default_policy_has_sane_values() {
        let p = SecurityPolicy::default();
        assert_eq!(p.autonomy, AutonomyLevel::Supervised);
        assert!(p.workspace_only);
        assert!(!p.allowed_commands.is_empty());
        assert!(!p.forbidden_paths.is_empty());
        assert!(p.max_actions_per_hour > 0);
        assert!(p.max_cost_per_day_cents > 0);
        assert!(p.require_approval_for_medium_risk);
        assert!(p.block_high_risk_commands);
        assert!(p.shell_env_passthrough.is_empty());
    }

    // ── ActionTracker / rate limiting ───────────────────────

    #[test]
    fn action_tracker_starts_at_zero() {
        let tracker = ActionTracker::new();
        assert_eq!(tracker.count(), 0);
    }

    #[test]
    fn action_tracker_records_actions() {
        let tracker = ActionTracker::new();
        assert_eq!(tracker.record(), 1);
        assert_eq!(tracker.record(), 2);
        assert_eq!(tracker.record(), 3);
        assert_eq!(tracker.count(), 3);
    }

    #[test]
    fn record_action_allows_within_limit() {
        let p = SecurityPolicy {
            max_actions_per_hour: 5,
            ..SecurityPolicy::default()
        };
        for _ in 0..5 {
            assert!(p.record_action(), "should allow actions within limit");
        }
    }

    #[test]
    fn record_action_blocks_over_limit() {
        let p = SecurityPolicy {
            max_actions_per_hour: 3,
            ..SecurityPolicy::default()
        };
        assert!(p.record_action()); // 1
        assert!(p.record_action()); // 2
        assert!(p.record_action()); // 3
        assert!(!p.record_action()); // 4 — over limit
    }

    #[test]
    fn is_rate_limited_reflects_count() {
        let p = SecurityPolicy {
            max_actions_per_hour: 2,
            ..SecurityPolicy::default()
        };
        assert!(!p.is_rate_limited());
        p.record_action();
        assert!(!p.is_rate_limited());
        p.record_action();
        assert!(p.is_rate_limited());
    }

    #[test]
    fn action_tracker_clone_is_independent() {
        let tracker = ActionTracker::new();
        tracker.record();
        tracker.record();
        let cloned = tracker.clone();
        assert_eq!(cloned.count(), 2);
        tracker.record();
        assert_eq!(tracker.count(), 3);
        assert_eq!(cloned.count(), 2); // clone is independent
    }

    // ── Edge cases: command injection ────────────────────────

    #[test]
    fn command_injection_semicolon_blocked() {
        let p = default_policy();
        // First word is "ls;" (with semicolon) — doesn't match "ls" in allowlist.
        // This is a safe default: chained commands are blocked.
        assert!(!p.is_command_allowed("ls; rm -rf /"));
    }

    #[test]
    fn command_injection_semicolon_no_space() {
        let p = default_policy();
        assert!(!p.is_command_allowed("ls;rm -rf /"));
    }

    #[test]
    fn quoted_semicolons_do_not_split_sqlite_command() {
        let p = SecurityPolicy {
            allowed_commands: vec!["sqlite3".into()],
            ..SecurityPolicy::default()
        };
        assert!(p.is_command_allowed(
            "sqlite3 /tmp/test.db \"CREATE TABLE t(id INT); INSERT INTO t VALUES(1); SELECT * FROM t;\""
        ));
        assert_eq!(
            p.command_risk_level(
                "sqlite3 /tmp/test.db \"CREATE TABLE t(id INT); INSERT INTO t VALUES(1); SELECT * FROM t;\""
            ),
            CommandRiskLevel::Low
        );
    }

    #[test]
    fn unquoted_semicolon_after_quoted_sql_still_splits_commands() {
        let p = SecurityPolicy {
            allowed_commands: vec!["sqlite3".into()],
            ..SecurityPolicy::default()
        };
        assert!(!p.is_command_allowed("sqlite3 /tmp/test.db \"SELECT 1;\"; rm -rf /"));
    }

    #[test]
    fn command_injection_backtick_blocked() {
        let p = default_policy();
        assert!(!p.is_command_allowed("echo `whoami`"));
        assert!(!p.is_command_allowed("echo `rm -rf /`"));
    }

    #[test]
    fn command_injection_dollar_paren_blocked() {
        let p = default_policy();
        assert!(!p.is_command_allowed("echo $(cat /etc/passwd)"));
        assert!(!p.is_command_allowed("echo $(rm -rf /)"));
    }

    #[test]
    fn command_injection_dollar_paren_literal_inside_single_quotes_allowed() {
        let p = default_policy();
        assert!(p.is_command_allowed("echo '$(cat /etc/passwd)'"));
    }

    #[test]
    fn command_injection_dollar_brace_literal_inside_single_quotes_allowed() {
        let p = default_policy();
        assert!(p.is_command_allowed("echo '${HOME}'"));
    }

    #[test]
    fn command_injection_dollar_brace_unquoted_blocked() {
        let p = default_policy();
        assert!(!p.is_command_allowed("echo ${HOME}"));
    }

    #[test]
    fn command_with_env_var_prefix() {
        let p = default_policy();
        // "FOO=bar" is the first word — not in allowlist
        assert!(!p.is_command_allowed("FOO=bar rm -rf /"));
    }

    #[test]
    fn command_newline_injection_blocked() {
        let p = default_policy();
        // Newline splits into two commands; "rm" is not in allowlist
        assert!(!p.is_command_allowed("ls\nrm -rf /"));
        // Both allowed — OK
        assert!(p.is_command_allowed("ls\necho hello"));
    }

    #[test]
    fn command_injection_and_chain_blocked() {
        let p = default_policy();
        assert!(!p.is_command_allowed("ls && rm -rf /"));
        assert!(!p.is_command_allowed("echo ok && curl http://evil.com"));
        // Both allowed — OK
        assert!(p.is_command_allowed("ls && echo done"));
    }

    #[test]
    fn command_injection_or_chain_blocked() {
        let p = default_policy();
        assert!(!p.is_command_allowed("ls || rm -rf /"));
        // Both allowed — OK
        assert!(p.is_command_allowed("ls || echo fallback"));
    }

    #[test]
    fn command_injection_background_chain_blocked() {
        let p = default_policy();
        assert!(!p.is_command_allowed("ls & rm -rf /"));
        assert!(!p.is_command_allowed("ls&rm -rf /"));
        assert!(!p.is_command_allowed("echo ok & python3 -c 'print(1)'"));
    }

    #[test]
    fn command_injection_redirect_blocked() {
        let p = default_policy();
        assert!(!p.is_command_allowed("echo secret > /etc/crontab"));
        assert!(!p.is_command_allowed("ls >> /tmp/exfil.txt"));
        assert!(!p.is_command_allowed("cat < /etc/passwd"));
        assert!(!p.is_command_allowed("echo secret > output.txt"));
    }

    #[test]
    fn heredoc_and_stderr_redirects_allowed() {
        let p = default_policy();
        // Heredoc marker on a single line — << should not trigger < blocker
        assert!(p.is_command_allowed("cat << 'EOF'"));
        assert!(p.is_command_allowed("cat <<EOF"));
        assert!(p.is_command_allowed("cat <<< 'hello'"));
        // Stderr suppression
        assert!(p.is_command_allowed("python3 script.py 2>/dev/null"));
        assert!(p.is_command_allowed("ls 2>&1"));
        assert!(p.is_command_allowed("grep foo bar 2>/dev/null"));
        assert!(p.is_command_allowed("ls >/dev/null"));
        assert!(p.is_command_allowed("cmd 1>/dev/null"));
        assert!(p.is_command_allowed("cmd 1>&2"));
        // Actual file redirects still blocked
        assert!(!p.is_command_allowed("cat < /etc/passwd"));
        assert!(!p.is_command_allowed("echo secret > output.txt"));
    }

    #[test]
    fn redirect_helper_unit_tests() {
        assert!(!contains_unquoted_input_redirect("cat << 'EOF'"));
        assert!(!contains_unquoted_input_redirect("cat <<< 'hello'"));
        assert!(contains_unquoted_input_redirect("cat < /etc/passwd"));
        assert!(!contains_unquoted_input_redirect("echo 'a<b'"));
        assert!(!contains_unsafe_output_redirect("cmd 2>/dev/null"));
        assert!(!contains_unsafe_output_redirect("cmd >/dev/null"));
        assert!(!contains_unsafe_output_redirect("cmd 1>/dev/null"));
        assert!(!contains_unsafe_output_redirect("cmd 2>&1"));
        assert!(!contains_unsafe_output_redirect("cmd 1>&2"));
        assert!(contains_unsafe_output_redirect("echo hi > file.txt"));
        assert!(!contains_unsafe_output_redirect("echo 'a>b'"));
    }

    #[test]
    fn quoted_ampersand_and_redirect_literals_are_not_treated_as_operators() {
        let p = default_policy();
        assert!(p.is_command_allowed("echo \"A&B\""));
        assert!(p.is_command_allowed("echo \"A>B\""));
        assert!(p.is_command_allowed("echo \"A<B\""));
    }

    #[test]
    fn command_argument_injection_blocked() {
        let p = default_policy();
        // find -exec is a common bypass
        assert!(!p.is_command_allowed("find . -exec rm -rf {} +"));
        assert!(!p.is_command_allowed("find / -ok cat {} \\;"));
        // git config/alias can execute commands
        assert!(!p.is_command_allowed("git config core.editor \"rm -rf /\""));
        assert!(!p.is_command_allowed("git alias.st status"));
        assert!(!p.is_command_allowed("git -c core.editor=calc.exe commit"));
        // Legitimate commands should still work
        assert!(p.is_command_allowed("find . -name '*.txt'"));
        assert!(p.is_command_allowed("git status"));
        assert!(p.is_command_allowed("git add ."));
    }

    #[test]
    fn command_injection_dollar_brace_blocked() {
        let p = default_policy();
        assert!(!p.is_command_allowed("echo ${IFS}cat${IFS}/etc/passwd"));
    }

    #[test]
    fn command_injection_plain_dollar_var_blocked() {
        let p = default_policy();
        assert!(!p.is_command_allowed("cat $HOME/.ssh/id_rsa"));
        assert!(!p.is_command_allowed("cat $SECRET_FILE"));
    }

    #[test]
    fn command_injection_tee_blocked() {
        let p = default_policy();
        assert!(!p.is_command_allowed("echo secret | tee /etc/crontab"));
        assert!(!p.is_command_allowed("ls | /usr/bin/tee outfile"));
        assert!(!p.is_command_allowed("tee file.txt"));
    }

    #[test]
    fn command_injection_process_substitution_blocked() {
        let p = default_policy();
        assert!(!p.is_command_allowed("cat <(echo pwned)"));
        assert!(!p.is_command_allowed("ls >(cat /etc/passwd)"));
    }

    #[test]
    fn command_env_var_prefix_with_allowed_cmd() {
        let p = default_policy();
        // env assignment + allowed command — OK
        assert!(p.is_command_allowed("FOO=bar ls"));
        assert!(p.is_command_allowed("LANG=C grep pattern file"));
        // env assignment + disallowed command — blocked
        assert!(!p.is_command_allowed("FOO=bar rm -rf /"));
    }

    #[test]
    fn forbidden_path_argument_detects_absolute_path() {
        let p = default_policy();
        assert_eq!(
            p.forbidden_path_argument("cat /etc/passwd"),
            Some("/etc/passwd".into())
        );
    }

    #[test]
    fn forbidden_path_argument_detects_parent_dir_reference() {
        let p = default_policy();
        assert_eq!(
            p.forbidden_path_argument("cat ../secret.txt"),
            Some("../secret.txt".into())
        );
        assert_eq!(
            p.forbidden_path_argument("find .. -name '*.rs'"),
            Some("..".into())
        );
    }

    #[test]
    fn forbidden_path_argument_allows_workspace_relative_paths() {
        let p = default_policy();
        assert_eq!(p.forbidden_path_argument("cat src/main.rs"), None);
        assert_eq!(p.forbidden_path_argument("grep -r todo ./src"), None);
    }

    #[test]
    fn forbidden_path_argument_detects_option_assignment_paths() {
        let p = default_policy();
        assert_eq!(
            p.forbidden_path_argument("grep --file=/etc/passwd root ./src"),
            Some("/etc/passwd".into())
        );
        assert_eq!(
            p.forbidden_path_argument("cat --input=../secret.txt"),
            Some("../secret.txt".into())
        );
    }

    #[test]
    fn forbidden_path_argument_allows_safe_option_assignment_paths() {
        let p = default_policy();
        assert_eq!(
            p.forbidden_path_argument("grep --file=./patterns.txt root ./src"),
            None
        );
    }

    #[test]
    fn forbidden_path_argument_detects_short_option_attached_paths() {
        let p = default_policy();
        assert_eq!(
            p.forbidden_path_argument("grep -f/etc/passwd root ./src"),
            Some("/etc/passwd".into())
        );
        assert_eq!(
            p.forbidden_path_argument("git -C../outside status"),
            Some("../outside".into())
        );
    }

    #[test]
    fn forbidden_path_argument_allows_safe_short_option_attached_paths() {
        let p = default_policy();
        assert_eq!(
            p.forbidden_path_argument("grep -f./patterns.txt root ./src"),
            None
        );
        assert_eq!(p.forbidden_path_argument("git -C./repo status"), None);
    }

    #[test]
    fn forbidden_path_argument_detects_tilde_user_paths() {
        let p = default_policy();
        assert_eq!(
            p.forbidden_path_argument("cat ~root/.ssh/id_rsa"),
            Some("~root/.ssh/id_rsa".into())
        );
        assert_eq!(
            p.forbidden_path_argument("ls ~nobody"),
            Some("~nobody".into())
        );
    }

    #[test]
    fn forbidden_path_argument_detects_input_redirection_paths() {
        let p = default_policy();
        assert_eq!(
            p.forbidden_path_argument("cat </etc/passwd"),
            Some("/etc/passwd".into())
        );
        assert_eq!(
            p.forbidden_path_argument("cat</etc/passwd"),
            Some("/etc/passwd".into())
        );
    }

    // ── Edge cases: path traversal ──────────────────────────

    #[test]
    fn path_traversal_encoded_dots() {
        let p = default_policy();
        // Literal ".." in path — always blocked
        assert!(!p.is_path_allowed("foo/..%2f..%2fetc/passwd"));
    }

    #[test]
    fn path_traversal_double_dot_in_filename() {
        let p = default_policy();
        // ".." in a filename (not a path component) is allowed
        assert!(p.is_path_allowed("my..file.txt"));
        // But actual traversal components are still blocked
        assert!(!p.is_path_allowed("../etc/passwd"));
        assert!(!p.is_path_allowed("foo/../etc/passwd"));
    }

    #[test]
    fn path_with_null_byte_blocked() {
        let p = default_policy();
        assert!(!p.is_path_allowed("file\0.txt"));
    }

    #[test]
    fn path_symlink_style_absolute() {
        let p = default_policy();
        assert!(!p.is_path_allowed("/proc/self/root/etc/passwd"));
    }

    #[test]
    fn path_home_tilde_ssh() {
        let p = SecurityPolicy {
            workspace_only: false,
            ..SecurityPolicy::default()
        };
        assert!(!p.is_path_allowed("~/.ssh/id_rsa"));
        assert!(!p.is_path_allowed("~/.gnupg/secring.gpg"));
        assert!(!p.is_path_allowed("~root/.ssh/id_rsa"));
        assert!(!p.is_path_allowed("~nobody"));
    }

    #[test]
    fn path_var_run_blocked() {
        let p = SecurityPolicy {
            workspace_only: false,
            ..SecurityPolicy::default()
        };
        assert!(!p.is_path_allowed("/var/run/docker.sock"));
    }

    // ── Edge cases: rate limiter boundary ────────────────────

    #[test]
    fn rate_limit_exactly_at_boundary() {
        let p = SecurityPolicy {
            max_actions_per_hour: 1,
            ..SecurityPolicy::default()
        };
        assert!(p.record_action()); // 1 — exactly at limit
        assert!(!p.record_action()); // 2 — over
        assert!(!p.record_action()); // 3 — still over
    }

    #[test]
    fn rate_limit_zero_blocks_everything() {
        let p = SecurityPolicy {
            max_actions_per_hour: 0,
            ..SecurityPolicy::default()
        };
        assert!(!p.record_action());
    }

    #[test]
    fn rate_limit_high_allows_many() {
        let p = SecurityPolicy {
            max_actions_per_hour: 10000,
            ..SecurityPolicy::default()
        };
        for _ in 0..100 {
            assert!(p.record_action());
        }
    }

    // ── Edge cases: autonomy + command combos ────────────────

    #[test]
    fn readonly_blocks_even_safe_commands() {
        let p = SecurityPolicy {
            autonomy: AutonomyLevel::ReadOnly,
            allowed_commands: vec!["ls".into(), "cat".into()],
            ..SecurityPolicy::default()
        };
        assert!(!p.is_command_allowed("ls"));
        assert!(!p.is_command_allowed("cat"));
        assert!(!p.can_act());
    }

    #[test]
    fn supervised_allows_listed_commands() {
        let p = SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            allowed_commands: vec!["git".into()],
            ..SecurityPolicy::default()
        };
        assert!(p.is_command_allowed("git status"));
        assert!(!p.is_command_allowed("docker ps"));
    }

    #[test]
    fn full_autonomy_still_respects_forbidden_paths() {
        let p = SecurityPolicy {
            autonomy: AutonomyLevel::Full,
            workspace_only: false,
            ..SecurityPolicy::default()
        };
        assert!(!p.is_path_allowed("/etc/shadow"));
        assert!(!p.is_path_allowed("/root/.bashrc"));
    }

    #[test]
    fn workspace_only_false_allows_resolved_outside_workspace() {
        let workspace = std::env::temp_dir().join("zeroclaw_test_ws_only_false");
        let _ = std::fs::create_dir_all(&workspace);
        let canonical_workspace = workspace
            .canonicalize()
            .unwrap_or_else(|_| workspace.clone());

        let p = SecurityPolicy {
            workspace_dir: canonical_workspace.clone(),
            workspace_only: false,
            forbidden_paths: vec!["/etc".into(), "/var".into()],
            ..SecurityPolicy::default()
        };

        // Path outside workspace should be allowed when workspace_only=false
        let outside = std::env::var_os("HOME")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/home"))
            .join("zeroclaw_outside_ws");
        assert!(
            p.is_resolved_path_allowed(&outside),
            "workspace_only=false must allow resolved paths outside workspace"
        );

        // Forbidden paths must still be blocked even with workspace_only=false
        assert!(
            !p.is_resolved_path_allowed(Path::new("/etc/passwd")),
            "forbidden paths must be blocked even when workspace_only=false"
        );
        assert!(
            !p.is_resolved_path_allowed(Path::new("/var/run/docker.sock")),
            "forbidden /var must be blocked even when workspace_only=false"
        );

        let _ = std::fs::remove_dir_all(&workspace);
    }

    #[test]
    fn workspace_only_true_blocks_resolved_outside_workspace() {
        let workspace = std::env::temp_dir().join("zeroclaw_test_ws_only_true");
        let _ = std::fs::create_dir_all(&workspace);
        let canonical_workspace = workspace
            .canonicalize()
            .unwrap_or_else(|_| workspace.clone());

        let p = SecurityPolicy {
            workspace_dir: canonical_workspace.clone(),
            workspace_only: true,
            ..SecurityPolicy::default()
        };

        // Path inside workspace — allowed
        let inside = canonical_workspace.join("subdir");
        assert!(
            p.is_resolved_path_allowed(&inside),
            "path inside workspace must be allowed"
        );

        // Path outside workspace — blocked
        let outside = std::env::temp_dir()
            .canonicalize()
            .unwrap_or_else(|_| std::env::temp_dir())
            .join("zeroclaw_outside_ws_true");
        assert!(
            !p.is_resolved_path_allowed(&outside),
            "workspace_only=true must block resolved paths outside workspace"
        );

        let _ = std::fs::remove_dir_all(&workspace);
    }

    // ── Edge cases: from_config preserves tracker ────────────

    #[test]
    fn from_config_creates_fresh_tracker() {
        let autonomy_config = crate::schema::AutonomyConfig {
            level: AutonomyLevel::Full,
            workspace_only: false,
            allowed_commands: vec![],
            forbidden_paths: vec![],
            max_actions_per_hour: 10,
            max_cost_per_day_cents: 100,
            require_approval_for_medium_risk: true,
            block_high_risk_commands: true,
            ..crate::schema::AutonomyConfig::default()
        };
        let workspace = PathBuf::from("/tmp/test");
        let policy = SecurityPolicy::from_config(&autonomy_config, &workspace);
        assert!(!policy.is_rate_limited());
    }

    // ══════════════════════════════════════════════════════════
    // SECURITY CHECKLIST TESTS
    // Checklist: gateway not public, pairing required,
    //            filesystem scoped (no /), access via tunnel
    // ══════════════════════════════════════════════════════════

    // ── Checklist #3: Filesystem scoped (no /) ──────────────

    #[test]
    fn checklist_root_path_blocked() {
        let p = default_policy();
        assert!(!p.is_path_allowed("/"));
        assert!(!p.is_path_allowed("/anything"));
    }

    #[test]
    fn checklist_all_system_dirs_blocked() {
        let p = SecurityPolicy {
            workspace_only: false,
            ..SecurityPolicy::default()
        };
        for dir in [
            "/etc", "/root", "/home", "/usr", "/bin", "/sbin", "/lib", "/opt", "/boot", "/dev",
            "/proc", "/sys", "/var", "/tmp",
        ] {
            assert!(
                !p.is_path_allowed(dir),
                "System dir should be blocked: {dir}"
            );
            assert!(
                !p.is_path_allowed(&format!("{dir}/subpath")),
                "Subpath of system dir should be blocked: {dir}/subpath"
            );
        }
    }

    #[test]
    fn checklist_sensitive_dotfiles_blocked() {
        let p = SecurityPolicy {
            workspace_only: false,
            ..SecurityPolicy::default()
        };
        for path in [
            "~/.ssh/id_rsa",
            "~/.gnupg/secring.gpg",
            "~/.aws/credentials",
            "~/.config/secrets",
        ] {
            assert!(
                !p.is_path_allowed(path),
                "Sensitive dotfile should be blocked: {path}"
            );
        }
    }

    #[test]
    fn checklist_null_byte_injection_blocked() {
        let p = default_policy();
        assert!(!p.is_path_allowed("safe\0/../../../etc/passwd"));
        assert!(!p.is_path_allowed("\0"));
        assert!(!p.is_path_allowed("file\0"));
    }

    #[test]
    fn checklist_workspace_only_blocks_absolute_outside_workspace() {
        let p = SecurityPolicy {
            workspace_only: true,
            ..SecurityPolicy::default()
        };
        assert!(!p.is_path_allowed("/any/absolute/path"));
        assert!(p.is_path_allowed("relative/path.txt"));
    }

    #[test]
    fn checklist_resolved_path_must_be_in_workspace() {
        let p = SecurityPolicy {
            workspace_dir: PathBuf::from("/home/user/project"),
            ..SecurityPolicy::default()
        };
        // Inside workspace — allowed
        assert!(p.is_resolved_path_allowed(Path::new("/home/user/project/src/main.rs")));
        // Outside workspace — blocked (symlink escape)
        assert!(!p.is_resolved_path_allowed(Path::new("/etc/passwd")));
        assert!(!p.is_resolved_path_allowed(Path::new("/home/user/other_project/file")));
        // Root — blocked
        assert!(!p.is_resolved_path_allowed(Path::new("/")));
    }

    #[test]
    fn checklist_default_policy_is_workspace_only() {
        let p = SecurityPolicy::default();
        assert!(
            p.workspace_only,
            "Default policy must be workspace_only=true"
        );
    }

    #[test]
    fn checklist_default_forbidden_paths_comprehensive() {
        let p = SecurityPolicy::default();
        // Must contain all critical system dirs
        for dir in ["/etc", "/root", "/proc", "/sys", "/dev", "/var", "/tmp"] {
            assert!(
                p.forbidden_paths.iter().any(|f| f == dir),
                "Default forbidden_paths must include {dir}"
            );
        }
        // Must contain sensitive dotfiles
        for dot in ["~/.ssh", "~/.gnupg", "~/.aws"] {
            assert!(
                p.forbidden_paths.iter().any(|f| f == dot),
                "Default forbidden_paths must include {dot}"
            );
        }
    }

    // ── §1.2 Path resolution / symlink bypass tests ──────────

    #[test]
    fn resolved_path_blocks_outside_workspace() {
        let workspace = std::env::temp_dir().join("zeroclaw_test_resolved_path");
        let _ = std::fs::create_dir_all(&workspace);

        // Use the canonicalized workspace so starts_with checks match
        let canonical_workspace = workspace
            .canonicalize()
            .unwrap_or_else(|_| workspace.clone());

        let policy = SecurityPolicy {
            workspace_dir: canonical_workspace.clone(),
            ..SecurityPolicy::default()
        };

        // A resolved path inside the workspace should be allowed
        let inside = canonical_workspace.join("subdir").join("file.txt");
        assert!(
            policy.is_resolved_path_allowed(&inside),
            "path inside workspace should be allowed"
        );

        // A resolved path outside the workspace should be blocked
        let canonical_temp = std::env::temp_dir()
            .canonicalize()
            .unwrap_or_else(|_| std::env::temp_dir());
        let outside = canonical_temp.join("outside_workspace_zeroclaw");
        assert!(
            !policy.is_resolved_path_allowed(&outside),
            "path outside workspace must be blocked"
        );

        let _ = std::fs::remove_dir_all(&workspace);
    }

    #[test]
    fn resolved_path_blocks_root_escape() {
        let policy = SecurityPolicy {
            workspace_dir: PathBuf::from("/home/zeroclaw_user/project"),
            ..SecurityPolicy::default()
        };

        assert!(
            !policy.is_resolved_path_allowed(Path::new("/etc/passwd")),
            "resolved path to /etc/passwd must be blocked"
        );
        assert!(
            !policy.is_resolved_path_allowed(Path::new("/root/.bashrc")),
            "resolved path to /root/.bashrc must be blocked"
        );
    }

    #[cfg(unix)]
    #[test]
    fn resolved_path_blocks_symlink_escape() {
        use std::os::unix::fs::symlink;

        let root = std::env::temp_dir().join("zeroclaw_test_symlink_escape");
        let workspace = root.join("workspace");
        let outside = root.join("outside_target");

        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&outside).unwrap();

        // Create a symlink inside workspace pointing outside
        let link_path = workspace.join("escape_link");
        symlink(&outside, &link_path).unwrap();

        let policy = SecurityPolicy {
            workspace_dir: workspace.clone(),
            ..SecurityPolicy::default()
        };

        // The resolved symlink target should be outside workspace
        let resolved = link_path.canonicalize().unwrap();
        assert!(
            !policy.is_resolved_path_allowed(&resolved),
            "symlink-resolved path outside workspace must be blocked"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn allowed_roots_permits_paths_outside_workspace() {
        use std::os::unix::fs::symlink;

        let root = std::env::temp_dir().join("zeroclaw_test_allowed_roots");
        let workspace = root.join("workspace");
        let extra = root.join("extra_root");
        let extra_file = extra.join("data.txt");

        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&extra).unwrap();
        std::fs::write(&extra_file, "test").unwrap();

        // Symlink inside workspace pointing to extra root
        let link_path = workspace.join("link_to_extra");
        symlink(&extra, &link_path).unwrap();

        let resolved = link_path.join("data.txt").canonicalize().unwrap();

        // Without allowed_roots — blocked (symlink escape)
        let policy_without = SecurityPolicy {
            workspace_dir: workspace.clone(),
            allowed_roots: vec![],
            ..SecurityPolicy::default()
        };
        assert!(
            !policy_without.is_resolved_path_allowed(&resolved),
            "without allowed_roots, symlink target must be blocked"
        );

        // With allowed_roots — permitted
        let policy_with = SecurityPolicy {
            workspace_dir: workspace.clone(),
            allowed_roots: vec![extra.clone()],
            ..SecurityPolicy::default()
        };
        assert!(
            policy_with.is_resolved_path_allowed(&resolved),
            "with allowed_roots containing the target, symlink must be allowed"
        );

        // Unrelated path still blocked
        let unrelated = root.join("unrelated");
        std::fs::create_dir_all(&unrelated).unwrap();
        assert!(
            !policy_with.is_resolved_path_allowed(&unrelated.canonicalize().unwrap()),
            "paths outside workspace and allowed_roots must still be blocked"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn is_path_allowed_blocks_null_bytes() {
        let policy = default_policy();
        assert!(
            !policy.is_path_allowed("file\0.txt"),
            "paths with null bytes must be blocked"
        );
    }

    #[test]
    fn is_path_allowed_blocks_url_encoded_traversal() {
        let policy = default_policy();
        assert!(
            !policy.is_path_allowed("..%2fetc%2fpasswd"),
            "URL-encoded path traversal must be blocked"
        );
        assert!(
            !policy.is_path_allowed("subdir%2f..%2f..%2fetc"),
            "URL-encoded parent dir traversal must be blocked"
        );
    }

    #[test]
    fn resolve_tool_path_expands_tilde() {
        let p = SecurityPolicy {
            workspace_dir: PathBuf::from("/workspace"),
            ..SecurityPolicy::default()
        };
        let resolved = p.resolve_tool_path("~/Documents/file.txt");
        // Should expand ~ to home dir, not join with workspace
        assert!(resolved.is_absolute());
        assert!(!resolved.starts_with("/workspace"));
        assert!(resolved.to_string_lossy().ends_with("Documents/file.txt"));
    }

    #[test]
    fn resolve_tool_path_keeps_absolute() {
        let p = SecurityPolicy {
            workspace_dir: PathBuf::from("/workspace"),
            ..SecurityPolicy::default()
        };
        let resolved = p.resolve_tool_path("/some/absolute/path");
        assert_eq!(resolved, PathBuf::from("/some/absolute/path"));
    }

    #[test]
    fn resolve_tool_path_joins_relative() {
        let p = SecurityPolicy {
            workspace_dir: PathBuf::from("/workspace"),
            ..SecurityPolicy::default()
        };
        let resolved = p.resolve_tool_path("relative/path.txt");
        assert_eq!(resolved, PathBuf::from("/workspace/relative/path.txt"));
    }

    #[test]
    fn resolve_tool_path_normalizes_workspace_prefixed_relative_paths() {
        let p = SecurityPolicy {
            workspace_dir: PathBuf::from("/zeroclaw-data/workspace"),
            ..SecurityPolicy::default()
        };
        let resolved = p.resolve_tool_path("zeroclaw-data/workspace/scripts/daily.py");
        assert_eq!(
            resolved,
            PathBuf::from("/zeroclaw-data/workspace/scripts/daily.py")
        );
    }

    #[test]
    fn is_under_allowed_root_matches_allowed_roots() {
        let p = SecurityPolicy {
            workspace_dir: PathBuf::from("/workspace"),
            workspace_only: true,
            allowed_roots: vec![PathBuf::from("/projects"), PathBuf::from("/data")],
            ..SecurityPolicy::default()
        };
        assert!(p.is_under_allowed_root("/projects/myapp/src/main.rs"));
        assert!(p.is_under_allowed_root("/data/file.csv"));
        assert!(!p.is_under_allowed_root("/etc/passwd"));
        assert!(!p.is_under_allowed_root("relative/path"));
    }

    #[test]
    fn is_under_allowed_root_returns_false_for_empty_roots() {
        let p = SecurityPolicy {
            workspace_dir: PathBuf::from("/workspace"),
            workspace_only: true,
            allowed_roots: vec![],
            ..SecurityPolicy::default()
        };
        assert!(!p.is_under_allowed_root("/any/path"));
    }

    #[test]
    fn runtime_config_paths_are_protected() {
        let workspace = PathBuf::from("/tmp/zeroclaw-profile/workspace");
        let policy = SecurityPolicy {
            workspace_dir: workspace.clone(),
            ..SecurityPolicy::default()
        };
        let config_dir = workspace.parent().unwrap();

        assert!(policy.is_runtime_config_path(&config_dir.join("config.toml")));
        assert!(policy.is_runtime_config_path(&config_dir.join("config.toml.bak")));
        assert!(policy.is_runtime_config_path(&config_dir.join(".config.toml.tmp-1234")));
        assert!(policy.is_runtime_config_path(&config_dir.join("active_workspace.toml")));
        assert!(policy.is_runtime_config_path(&config_dir.join(".active_workspace.toml.tmp-1234")));
    }

    #[test]
    fn workspace_files_are_not_runtime_config_paths() {
        let workspace = PathBuf::from("/tmp/zeroclaw-profile/workspace");
        let policy = SecurityPolicy {
            workspace_dir: workspace.clone(),
            ..SecurityPolicy::default()
        };
        let nested_dir = workspace.join("notes");

        assert!(!policy.is_runtime_config_path(&workspace.join("notes.txt")));
        assert!(!policy.is_runtime_config_path(&nested_dir.join("config.toml")));
    }

    // ── prompt_summary ──────────────────────────────────────

    #[test]
    fn prompt_summary_includes_autonomy_level() {
        let p = default_policy();
        let summary = p.prompt_summary();
        assert!(
            summary.contains("Supervised"),
            "should mention autonomy level"
        );
    }

    #[test]
    fn prompt_summary_includes_workspace_boundary_when_workspace_only() {
        let p = SecurityPolicy {
            workspace_dir: PathBuf::from("/home/user/project"),
            workspace_only: true,
            ..SecurityPolicy::default()
        };
        let summary = p.prompt_summary();
        assert!(
            summary.contains("Workspace boundary"),
            "should mention workspace boundary"
        );
        assert!(
            summary.contains("/home/user/project"),
            "should mention workspace path"
        );
    }

    #[test]
    fn prompt_summary_omits_workspace_boundary_when_not_workspace_only() {
        let p = SecurityPolicy {
            workspace_only: false,
            ..SecurityPolicy::default()
        };
        let summary = p.prompt_summary();
        assert!(
            !summary.contains("Workspace boundary"),
            "should not mention workspace boundary"
        );
    }

    #[test]
    fn prompt_summary_includes_allowed_commands() {
        let p = SecurityPolicy {
            allowed_commands: vec!["git".into(), "ls".into()],
            ..SecurityPolicy::default()
        };
        let summary = p.prompt_summary();
        assert!(summary.contains("`git`"), "should list allowed commands");
        assert!(summary.contains("`ls`"), "should list allowed commands");
        assert!(
            summary.contains("You may execute these commands freely"),
            "should mention allowed commands positively"
        );
    }

    #[test]
    fn prompt_summary_includes_forbidden_paths() {
        let p = SecurityPolicy {
            workspace_only: false,
            forbidden_paths: vec!["/etc".into(), "~/.ssh".into()],
            ..SecurityPolicy::default()
        };
        let summary = p.prompt_summary();
        assert!(summary.contains("`/etc`"), "should list forbidden paths");
        assert!(summary.contains("`~/.ssh`"), "should list forbidden paths");
    }

    #[test]
    fn prompt_summary_includes_rate_limit() {
        let p = SecurityPolicy {
            max_actions_per_hour: 42,
            ..SecurityPolicy::default()
        };
        let summary = p.prompt_summary();
        assert!(summary.contains("42"), "should mention rate limit");
        assert!(
            summary.contains("actions per hour"),
            "should explain rate limit"
        );
    }

    #[test]
    fn prompt_summary_includes_risk_controls() {
        let p = SecurityPolicy {
            block_high_risk_commands: true,
            require_approval_for_medium_risk: true,
            ..SecurityPolicy::default()
        };
        let summary = p.prompt_summary();
        assert!(
            summary.contains("Exercise caution with destructive commands"),
            "should mention high-risk caution"
        );
        assert!(
            summary.contains("Medium-risk commands"),
            "should mention medium-risk approval"
        );
    }

    #[test]
    fn prompt_summary_includes_allowed_roots() {
        let p = SecurityPolicy {
            allowed_roots: vec![PathBuf::from("/shared/data"), PathBuf::from("/opt/tools")],
            ..SecurityPolicy::default()
        };
        let summary = p.prompt_summary();
        assert!(
            summary.contains("`/shared/data`"),
            "should list allowed roots"
        );
        assert!(
            summary.contains("`/opt/tools`"),
            "should list allowed roots"
        );
    }

    #[test]
    fn wildcard_with_block_high_risk_false_allows_everything() {
        let p = SecurityPolicy {
            allowed_commands: vec!["*".into()],
            block_high_risk_commands: false,
            workspace_only: false,
            ..SecurityPolicy::default()
        };
        assert!(
            p.validate_command_execution("rm -rf /tmp/test", true)
                .is_ok()
        );
        assert!(p.validate_command_execution("nohup firefox", true).is_ok());
        assert!(
            p.validate_command_execution("ls /usr/bin/firefox", true)
                .is_ok()
        );
    }

    #[test]
    fn wildcard_with_block_high_risk_true_still_blocks() {
        // Ensure the existing safety net is preserved: wildcard + block_high_risk_commands=true
        // should still block high-risk commands.
        let p = SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            allowed_commands: vec!["*".into()],
            block_high_risk_commands: true,
            ..SecurityPolicy::default()
        };
        let result = p.validate_command_execution("rm -rf /tmp/test", true);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("high-risk"));
    }

    #[test]
    fn per_sender_tracker_isolates_counts() {
        let t = PerSenderTracker::new();
        // sender A hits limit=2 on 3rd call
        assert!(t.record_within("chat_a", 2)); // count=1 ≤ 2 → ok
        assert!(t.record_within("chat_a", 2)); // count=2 ≤ 2 → ok
        assert!(!t.record_within("chat_a", 2)); // count=3 > 2 → blocked
        // sender B is unaffected — its bucket is empty
        assert!(t.record_within("chat_b", 2)); // count=1 ≤ 2 → ok
        assert!(t.record_within("chat_b", 2)); // count=2 ≤ 2 → ok
        assert!(!t.record_within("chat_b", 2)); // count=3 > 2 → blocked
    }

    #[test]
    fn per_sender_tracker_global_key_fallback() {
        let t = PerSenderTracker::new();
        assert!(!t.is_exhausted(PerSenderTracker::GLOBAL_KEY, 1));
        t.record_within(PerSenderTracker::GLOBAL_KEY, u32::MAX);
        // after 1 action, count=1 ≥ 1 → exhausted at max=1
        assert!(t.is_exhausted(PerSenderTracker::GLOBAL_KEY, 1));
    }

    #[test]
    fn per_sender_tracker_is_exhausted_reads_without_spurious_insert() {
        let t = PerSenderTracker::new();
        // Key "ghost" has never been recorded — should not be exhausted at max=1
        assert!(!t.is_exhausted("ghost", 1));
    }
}
