use anyhow::Context as _;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use zeroclaw_api::runtime_traits::ShellDialect;

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
    state: Mutex<ActionTrackerState>,
}

#[derive(Debug)]
struct ActionTrackerState {
    /// Timestamps of successful actions (kept within the last hour).
    committed: Vec<Instant>,
    /// Calls admitted by a wrapper but not yet completed.
    in_flight: usize,
}

const ACTION_WINDOW: Duration = Duration::from_secs(3600);

fn retain_actions_after(actions: &mut Vec<Instant>, cutoff: Option<Instant>) {
    if let Some(cutoff) = cutoff {
        actions.retain(|timestamp| *timestamp > cutoff);
    }
}

impl Default for ActionTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl ActionTracker {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(ActionTrackerState {
                committed: Vec::new(),
                in_flight: 0,
            }),
        }
    }

    /// Record an action and return the current count within the window.
    pub fn record(&self) -> usize {
        let mut state = self.state.lock();
        let now = Instant::now();
        retain_actions_after(&mut state.committed, now.checked_sub(ACTION_WINDOW));
        state.committed.push(now);
        state.committed.len()
    }

    /// Count of actions in the current window without recording.
    pub fn count(&self) -> usize {
        let mut state = self.state.lock();
        retain_actions_after(
            &mut state.committed,
            Instant::now().checked_sub(ACTION_WINDOW),
        );
        state.committed.len()
    }

    fn used(&self) -> usize {
        let mut state = self.state.lock();
        retain_actions_after(
            &mut state.committed,
            Instant::now().checked_sub(ACTION_WINDOW),
        );
        state.committed.len() + state.in_flight
    }

    fn try_record(&self, max: u32) -> bool {
        if max == 0 {
            return false;
        }

        let mut state = self.state.lock();
        let now = Instant::now();
        retain_actions_after(&mut state.committed, now.checked_sub(ACTION_WINDOW));
        if state.committed.len() + state.in_flight >= max as usize {
            return false;
        }
        state.committed.push(now);
        true
    }

    fn reserve(&self, max: u32) -> bool {
        if max == 0 {
            return false;
        }

        let mut state = self.state.lock();
        retain_actions_after(
            &mut state.committed,
            Instant::now().checked_sub(ACTION_WINDOW),
        );
        if state.committed.len() + state.in_flight >= max as usize {
            return false;
        }
        state.in_flight += 1;
        true
    }

    fn commit(&self) -> bool {
        let mut state = self.state.lock();
        if state.in_flight == 0 {
            return false;
        }
        state.in_flight -= 1;
        let now = Instant::now();
        retain_actions_after(&mut state.committed, now.checked_sub(ACTION_WINDOW));
        state.committed.push(now);
        true
    }

    fn release(&self) -> (bool, bool) {
        let mut state = self.state.lock();
        if state.in_flight == 0 {
            return (false, false);
        }
        state.in_flight -= 1;
        retain_actions_after(
            &mut state.committed,
            Instant::now().checked_sub(ACTION_WINDOW),
        );
        let is_empty = state.committed.is_empty() && state.in_flight == 0;
        (true, is_empty)
    }
}

impl Clone for ActionTracker {
    fn clone(&self) -> Self {
        let state = self.state.lock();
        Self {
            state: Mutex::new(ActionTrackerState {
                committed: state.committed.clone(),
                in_flight: 0,
            }),
        }
    }
}

/// Per-sender sliding-window rate limiter. The bucket map is Arc-shared
/// so cloned policies (SubAgents) consume from the same budgets.
#[derive(Debug)]
pub struct PerSenderTracker {
    buckets: std::sync::Arc<parking_lot::Mutex<HashMap<String, ActionTracker>>>,
}

impl PerSenderTracker {
    /// Bucket key used when no per-sender context is available (cron, CLI).
    pub const GLOBAL_KEY: &'static str = "__global__";

    /// Create an empty tracker with no sender buckets.
    pub fn new() -> Self {
        Self {
            buckets: std::sync::Arc::new(parking_lot::Mutex::new(HashMap::new())),
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

    /// Record one action for `key` when a slot remains.
    /// Rejected attempts are not recorded.
    pub fn record_within(&self, key: &str, max: u32) -> bool {
        if max == 0 {
            return false;
        }
        let mut buckets = self.buckets.lock();
        let tracker = buckets.entry(key.to_string()).or_default();
        tracker.try_record(max)
    }

    /// Atomically reserve one action slot for the current sender.
    pub fn reserve_for_current(&self, max: u32) -> Option<ActionReservation> {
        let key = Self::current_key();
        self.reserve_within(key, max)
    }

    fn reserve_within(&self, key: String, max: u32) -> Option<ActionReservation> {
        if max == 0 {
            return None;
        }
        {
            let mut buckets = self.buckets.lock();
            let admitted = match buckets.get(&key) {
                Some(tracker) => tracker.reserve(max),
                None => {
                    let tracker = ActionTracker::new();
                    let admitted = tracker.reserve(max);
                    buckets.insert(key.clone(), tracker);
                    admitted
                }
            };
            if !admitted {
                return None;
            }
        }
        Some(ActionReservation {
            tracker: self.clone(),
            key,
            finished: false,
        })
    }

    fn commit_reservation(&self, key: &str) -> bool {
        let buckets = self.buckets.lock();
        buckets.get(key).is_some_and(ActionTracker::commit)
    }

    fn release_reservation(&self, key: &str) -> bool {
        let mut buckets = self.buckets.lock();
        let Some(tracker) = buckets.get(key) else {
            return false;
        };
        let (released, remove_bucket) = tracker.release();
        if remove_bucket {
            buckets.remove(key);
        }
        released
    }

    /// Check if the current sender is at or over the limit (without recording).
    pub fn is_limited_for_current(&self, max: u32) -> bool {
        let key = Self::current_key();
        self.is_exhausted(&key, max)
    }

    pub fn is_exhausted(&self, key: &str, max: u32) -> bool {
        let mut buckets = self.buckets.lock();
        let used = buckets.get(key).map_or(0, ActionTracker::used);
        if used == 0 {
            buckets.remove(key);
        }
        max == 0 || used >= max as usize
    }
}

/// One sender-scoped in-flight action slot.
///
/// Dropping an uncommitted reservation releases only this invocation's slot.
#[must_use = "dropping the reservation releases the action slot"]
pub struct ActionReservation {
    tracker: PerSenderTracker,
    key: String,
    finished: bool,
}

impl ActionReservation {
    /// Convert this in-flight slot into one committed action.
    pub fn commit(mut self) {
        let committed = self.tracker.commit_reservation(&self.key);
        assert!(committed, "owned action reservation must still exist");
        self.finished = true;
    }
}

impl Drop for ActionReservation {
    fn drop(&mut self) {
        if !self.finished {
            let _ = self.tracker.release_reservation(&self.key);
        }
    }
}

impl Clone for PerSenderTracker {
    fn clone(&self) -> Self {
        Self {
            buckets: std::sync::Arc::clone(&self.buckets),
        }
    }
}

impl Default for PerSenderTracker {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone)]
pub struct SecurityPolicy {
    pub autonomy: AutonomyLevel,
    /// Name of the risk profile this policy was built from. Used to gate
    /// delegation: a Delegate may only target an agent sharing the caller's
    /// risk profile. Empty when constructed outside the profile path.
    pub risk_profile_name: String,
    /// Whether and to which agents this profile may delegate.
    pub delegation_policy: crate::autonomy::DelegationPolicy,
    pub workspace_dir: PathBuf,
    pub config_path: Option<PathBuf>,
    pub data_dir: Option<PathBuf>,
    pub workspace_only: bool,
    pub allowed_commands: Vec<String>,
    pub forbidden_paths: Vec<String>,
    /// Directories the agent can read AND write under. Includes
    /// `RiskProfileConfig.allowed_roots` plus any cross-agent
    /// `AccessMode::ReadWrite` grants resolved from
    /// `agent.workspace.access` at policy construction time.
    pub allowed_roots: Vec<PathBuf>,
    /// Directories the agent can read but NOT write under. Populated
    /// from cross-agent `AccessMode::Read` grants at policy
    /// construction time. Empty when no read-only cross-agent access
    /// is configured.
    pub allowed_roots_read_only: Vec<PathBuf>,
    /// Directories the agent can write but NOT read under. Populated
    /// from cross-agent `AccessMode::Write` grants; read-side tools
    /// ignore this list.
    pub allowed_roots_write_only: Vec<PathBuf>,
    pub max_actions_per_hour: u32,
    pub max_cost_per_day_cents: u32,
    pub require_approval_for_medium_risk: bool,
    pub block_high_risk_commands: bool,
    pub shell_env_passthrough: Vec<String>,
    pub shell_timeout_secs: u64,
    /// Tool name allowlist. `None` is unrestricted (default for agents
    /// without an explicit `risk_profile.allowed_tools` setting).
    /// `Some(vec![])` denies every tool. `Some(list)` admits only the
    /// listed names. Enforced at the agent loop's tool-dispatch site.
    pub allowed_tools: Option<Vec<String>>,
    /// Tool name denylist. Subtracts from the allowed set (whether the
    /// allowed set comes from `allowed_tools` or from the unrestricted
    /// default). `None` and `Some(vec![])` both mean "exclude nothing".
    pub excluded_tools: Option<Vec<String>>,
    /// Tools that never require approval in this profile. Mirrors
    /// `RiskProfileConfig.auto_approve`.
    pub auto_approve: Vec<String>,
    /// Tools that always require approval in this profile. Mirrors
    /// `RiskProfileConfig.always_ask`.
    pub always_ask: Vec<String>,
    /// Whether the sandbox is enabled for this profile. `None`
    /// inherits the global default at the call site.
    pub sandbox_enabled: Option<bool>,
    /// Sandbox backend identifier (e.g. `"firejail"`, `"landlock"`).
    /// `None` inherits the global default.
    pub sandbox_backend: Option<String>,
    /// Extra arguments forwarded to firejail when `sandbox_backend`
    /// resolves to `"firejail"`.
    pub firejail_args: Vec<String>,
    pub tracker: PerSenderTracker,
}

impl SecurityPolicy {
    /// True when `name` is admissible under the current policy.
    /// `allowed_tools = None` is unrestricted; `Some(list)` is the
    /// allowlist. `excluded_tools` always subtracts.
    pub fn is_tool_allowed(&self, name: &str) -> bool {
        let allowed = self
            .allowed_tools
            .as_ref()
            .is_none_or(|list| list.iter().any(|t| t == name));
        allowed && !self.is_tool_excluded(name)
    }

    pub fn is_tool_excluded(&self, name: &str) -> bool {
        self.excluded_tools
            .as_ref()
            .is_some_and(|list| list.iter().any(|t| t == name))
    }
}

/// Default allowed commands for Unix platforms.
#[cfg(not(target_os = "windows"))]
pub(crate) fn default_allowed_commands() -> Vec<String> {
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
/// Includes both native Windows commands and their Unix equivalents
/// (available via Git for Windows, WSL, etc.).
#[cfg(target_os = "windows")]
pub(crate) fn default_allowed_commands() -> Vec<String> {
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
pub(crate) fn default_forbidden_paths() -> Vec<String> {
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
pub(crate) fn default_forbidden_paths() -> Vec<String> {
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

fn roots_contain(roots: &[PathBuf], expanded: &Path) -> bool {
    roots.iter().any(|root| {
        let canonical = root.canonicalize().unwrap_or_else(|_| root.clone());
        expanded.starts_with(&canonical) || expanded.starts_with(root)
    })
}

fn path_contains(parent: &Path, child: &Path) -> bool {
    let canonical_parent = parent
        .canonicalize()
        .unwrap_or_else(|_| parent.to_path_buf());
    let canonical_child = child.canonicalize().unwrap_or_else(|_| child.to_path_buf());
    canonical_child.starts_with(&canonical_parent) || child.starts_with(parent)
}

/// Specific kind of escalation violation returned by
/// [`SecurityPolicy::ensure_no_escalation_beyond`]. Each variant names
/// the field that violated subset semantics so the SubAgent spawn path
/// can produce a precise error to the caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EscalationViolation {
    /// Child raises `autonomy` above the parent (e.g. parent
    /// `Supervised`, child `Full`). The autonomy level gates the
    /// entire `can_act` and approval flow, so silent escalation here
    /// would bypass every other guard.
    AutonomyAboveParent {
        child: AutonomyLevel,
        parent: AutonomyLevel,
    },
    /// `child.allowed_roots` contains a path the parent cannot rw.
    ReadWriteRootNotInParent { path: PathBuf },
    /// `child.allowed_roots_read_only` contains a path the parent
    /// cannot read at all (not in parent rw or read-only lists).
    ReadOnlyRootNotInParent { path: PathBuf },
    /// `child.allowed_roots_write_only` contains a path the parent
    /// cannot write at all (not in parent rw or write-only lists).
    WriteOnlyRootNotInParent { path: PathBuf },
    /// `child.allowed_commands` contains a shell command the parent
    /// has no allowance for.
    CommandNotInParent { command: String },
    /// Parent enforces workspace_only but the child override tries to
    /// turn it off.
    WorkspaceOnlyDisabledByChild,
    /// Child drops a forbidden_paths entry the parent enforces. Subset
    /// semantics on forbidden lists run the opposite direction from
    /// allowlists: parent ⊆ child, so the child can ADD entries but
    /// never DROP them.
    ForbiddenPathDroppedByChild { path: String },
    /// Child raises `shell_env_passthrough` to leak env vars the
    /// parent declined to forward.
    ShellEnvPassthroughExpanded { variable: String },
    /// Child override raises `max_actions_per_hour` above the
    /// parent's ceiling.
    MaxActionsExceeded { child: u32, parent: u32 },
    /// Child override raises `max_cost_per_day_cents` above the
    /// parent's ceiling.
    MaxCostExceeded { child: u32, parent: u32 },
    /// Child override raises `shell_timeout_secs` above the parent's
    /// ceiling. The shell budget is a runaway-process guard; raising
    /// it on the child side defeats the parent's intent.
    ShellTimeoutExceeded { child: u64, parent: u64 },
    /// Child flips `block_high_risk_commands` from `true` (parent) to
    /// `false`, opening the high-risk command surface the parent
    /// closed.
    BlockHighRiskCommandsDisabledByChild,
    /// Child flips `require_approval_for_medium_risk` from `true`
    /// (parent) to `false`, bypassing the human-in-the-loop step the
    /// parent required.
    RequireApprovalDisabledByChild,
}

impl std::fmt::Display for EscalationViolation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AutonomyAboveParent { child, parent } => {
                write!(f, "subagent autonomy={child:?} exceeds parent's {parent:?}")
            }
            Self::ReadWriteRootNotInParent { path } => write!(
                f,
                "subagent allowed_roots entry {path:?} is not contained within any of the parent's allowed_roots entries"
            ),
            Self::ReadOnlyRootNotInParent { path } => write!(
                f,
                "subagent allowed_roots_read_only entry {path:?} is not contained within the parent's allowed_roots or allowed_roots_read_only"
            ),
            Self::WriteOnlyRootNotInParent { path } => write!(
                f,
                "subagent allowed_roots_write_only entry {path:?} is not contained within the parent's allowed_roots or allowed_roots_write_only"
            ),
            Self::CommandNotInParent { command } => write!(
                f,
                "subagent allowed_commands entry {command:?} is not present on the parent's allowed_commands"
            ),
            Self::WorkspaceOnlyDisabledByChild => write!(
                f,
                "subagent attempts to disable workspace_only but the parent enforces it"
            ),
            Self::ForbiddenPathDroppedByChild { path } => write!(
                f,
                "subagent drops forbidden_paths entry {path:?} that the parent enforces"
            ),
            Self::ShellEnvPassthroughExpanded { variable } => write!(
                f,
                "subagent shell_env_passthrough entry {variable:?} is not present on the parent's list"
            ),
            Self::MaxActionsExceeded { child, parent } => write!(
                f,
                "subagent max_actions_per_hour={child} exceeds parent's {parent}"
            ),
            Self::MaxCostExceeded { child, parent } => write!(
                f,
                "subagent max_cost_per_day_cents={child} exceeds parent's {parent}"
            ),
            Self::ShellTimeoutExceeded { child, parent } => write!(
                f,
                "subagent shell_timeout_secs={child} exceeds parent's {parent}"
            ),
            Self::BlockHighRiskCommandsDisabledByChild => write!(
                f,
                "subagent attempts to set block_high_risk_commands=false but the parent enforces it"
            ),
            Self::RequireApprovalDisabledByChild => write!(
                f,
                "subagent attempts to set require_approval_for_medium_risk=false but the parent enforces it"
            ),
        }
    }
}

impl std::error::Error for EscalationViolation {}

impl Default for SecurityPolicy {
    fn default() -> Self {
        Self {
            autonomy: AutonomyLevel::Supervised,
            risk_profile_name: String::new(),
            delegation_policy: crate::autonomy::DelegationPolicy::default(),
            workspace_dir: PathBuf::from("."),
            config_path: None,
            data_dir: None,
            workspace_only: true,
            allowed_commands: default_allowed_commands(),
            forbidden_paths: default_forbidden_paths(),
            allowed_roots: Vec::new(),
            allowed_roots_read_only: Vec::new(),
            allowed_roots_write_only: Vec::new(),
            max_actions_per_hour: 20,
            max_cost_per_day_cents: 500,
            require_approval_for_medium_risk: true,
            block_high_risk_commands: true,
            shell_env_passthrough: vec![],
            shell_timeout_secs: 60,
            allowed_tools: None,
            excluded_tools: None,
            auto_approve: vec![],
            always_ask: vec![],
            sandbox_enabled: None,
            sandbox_backend: None,
            firejail_args: vec![],
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

/// Resolve `path` to its real target, following symlinks component by component.
///
/// Unlike [`Path::canonicalize`] this does NOT require the target to exist, so a
/// path whose leaf is about to be created (`touch link/new.txt`) still resolves,
/// while a symlinked component is followed to its target even when that target
/// does not exist yet (a *dangling* symlink `link -> /outside/new` — the write
/// would still land outside, so it must be resolved and blocked, exactly as
/// `file_write` blocks writing through a symlink leaf). Lexical `.`/`..` are
/// applied without touching the filesystem. Symlink chains are bounded to guard
/// against cycles: exhausting the hop budget returns `None` ("could not
/// resolve"), and callers fail closed by BLOCKING the path - a crafted cycle
/// never falls back to the literal input. An unreadable link is kept as a
/// literal component while resolution continues on its ancestors.
fn resolve_symlinked_path(path: &Path) -> Option<PathBuf> {
    let mut suffix: Vec<std::ffi::OsString> = Vec::new();
    let mut current = path.to_path_buf();
    // Bounds the number of `..`-style retries plus symlink hops so a symlink
    // cycle cannot spin forever. `None` on exhaustion means "could not resolve";
    // the caller FAILS CLOSED (blocks), so a crafted deep/cyclic symlink chain
    // cannot exhaust the budget and fall back to an allowed in-workspace path.
    let mut budget: u32 = 64;
    loop {
        // Canonicalizing the deepest EXISTING prefix normalizes it the same way
        // `is_resolved_path_allowed` normalizes the workspace root (e.g. `/tmp`
        // vs its real location), so ordinary in-workspace paths stay in-workspace.
        if let Ok(resolved) = current.canonicalize() {
            let mut result = resolved;
            for component in suffix.iter().rev() {
                result.push(component);
            }
            return Some(result);
        }
        // A *dangling* symlink cannot be canonicalized (its target does not
        // exist), yet a write through it still lands at the target. Follow it
        // explicitly so the resolved path reflects where the write would go.
        // Only symlink hops consume the budget (a symlink cycle is the only way
        // to loop forever); stripping non-existent trailing components is bounded
        // by the finite path depth, so a deeply-nested create is NOT false-blocked.
        if current
            .symlink_metadata()
            .is_ok_and(|m| m.file_type().is_symlink())
            && let Ok(target) = std::fs::read_link(&current)
        {
            if budget == 0 {
                return None;
            }
            budget -= 1;
            current = if target.is_absolute() {
                target
            } else {
                current
                    .parent()
                    .map(|parent| parent.join(&target))
                    .unwrap_or(target)
            };
            continue;
        }
        // Otherwise strip the trailing (non-existent) component and retry on the
        // parent. An absolute input terminates at the filesystem root, which
        // always canonicalizes; running out of components should be unreachable
        // for the absolute inputs the caller passes, so treat it as unresolvable
        // and fail closed rather than trusting the literal path.
        match (current.file_name(), current.parent()) {
            (Some(name), Some(parent)) if !parent.as_os_str().is_empty() => {
                suffix.push(name.to_os_string());
                current = parent.to_path_buf();
            }
            _ => return None,
        }
    }
}

fn is_null_device(path: &Path) -> bool {
    #[cfg(not(target_os = "windows"))]
    {
        path == Path::new("/dev/null")
    }
    #[cfg(target_os = "windows")]
    {
        let s = path.to_string_lossy();
        let lower = s.to_ascii_lowercase();
        lower == "nul" || lower == r"\\.\nul"
    }
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

struct NormalizedRootlessPath {
    drive: Option<u8>,
    text: String,
}

fn normalized_rootless_path_text(path: &Path) -> Option<NormalizedRootlessPath> {
    let mut text = path.to_string_lossy().replace('\\', "/");

    if let Some(rest) = text.strip_prefix("//?/UNC/") {
        text = rest.to_string();
    } else if let Some(rest) = text.strip_prefix("//?/") {
        text = rest.to_string();
    }

    let mut drive = None;
    let bytes = text.as_bytes();
    if bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic() {
        drive = Some(bytes[0].to_ascii_lowercase());
        text = text[2..].to_string();
    }

    let parts: Vec<&str> = text
        .trim_start_matches('/')
        .split('/')
        .filter(|part| !part.is_empty() && *part != ".")
        .collect();

    if parts.is_empty() || parts.contains(&"..") {
        None
    } else {
        Some(NormalizedRootlessPath {
            drive,
            text: parts.join("/"),
        })
    }
}

fn workspace_prefixed_relative_suffix(path: &Path, workspace_dir: &Path) -> Option<PathBuf> {
    let path_text = normalized_rootless_path_text(path)?;
    let workspace_text = normalized_rootless_path_text(workspace_dir)?;

    if path_text.drive.is_some() && path_text.drive != workspace_text.drive {
        return None;
    }

    if path_text.text == workspace_text.text {
        return Some(PathBuf::new());
    }

    let prefix = format!("{}/", workspace_text.text);
    path_text
        .text
        .strip_prefix(&prefix)
        .map(|suffix| PathBuf::from(suffix.replace('/', std::path::MAIN_SEPARATOR_STR)))
}

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

fn split_unquoted_segments(command: &str) -> Vec<String> {
    let mut segments = Vec::new();
    let mut current = String::new();
    let mut quote = QuoteState::None;
    let mut escaped = false;
    // Heredoc state: Some(delim) while inside a heredoc body.
    let mut heredoc_delimiter: Option<String> = None;
    // Accumulates the current line while inside a heredoc body, for terminator detection.
    let mut heredoc_line_buf = String::new();
    // True while reading the delimiter word that follows `<<`.
    let mut reading_heredoc_word = false;
    let mut heredoc_word_buf = String::new();
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
                    if heredoc_delimiter.is_some() {
                        heredoc_line_buf.push(ch);
                    } else {
                        current.push(ch);
                    }
                    continue;
                }
                if ch == '\\' {
                    escaped = true;
                    if heredoc_delimiter.is_some() {
                        heredoc_line_buf.push(ch);
                    } else {
                        current.push(ch);
                    }
                    continue;
                }

                // Reading the delimiter word that follows `<<`.
                if reading_heredoc_word {
                    if ch == '\n' {
                        // Finalise the delimiter and enter the heredoc body.
                        let raw = heredoc_word_buf.trim().trim_start_matches('-');
                        let delim = raw
                            .trim_matches(|c| c == '\'' || c == '"' || c == '\\')
                            .to_string();
                        if !delim.is_empty() {
                            heredoc_delimiter = Some(delim);
                        }
                        heredoc_word_buf.clear();
                        reading_heredoc_word = false;
                        // The newline after `<<WORD` belongs to the same segment.
                        current.push(ch);
                    } else {
                        heredoc_word_buf.push(ch);
                        current.push(ch);
                    }
                    continue;
                }

                if let Some(delim) = heredoc_delimiter.as_deref() {
                    if ch == '\n' {
                        if heredoc_line_buf.trim() == delim {
                            // Terminator line reached — end of heredoc body.
                            heredoc_delimiter = None;
                            heredoc_line_buf.clear();
                            push_segment(&mut segments, &mut current);
                        } else {
                            heredoc_line_buf.clear();
                        }
                    } else {
                        heredoc_line_buf.push(ch);
                    }
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
                    '<' => {
                        current.push(ch);
                        // Detect `<<` (heredoc) but not `<<<` (here-string).
                        if chars.peek() == Some(&'<') {
                            let second = chars.next().unwrap();
                            current.push(second);
                            if chars.peek() != Some(&'<') {
                                reading_heredoc_word = true;
                            }
                            // `<<<` falls through with no heredoc tracking.
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
/// Strip fd-merge redirect patterns (`N>&M`, `N<&M`, `>&N`, `<&N`, `N>&-`, etc.)
/// so their `&` doesn't get flagged as a background operator.
fn strip_fd_merge_redirects(command: &str) -> String {
    use std::sync::OnceLock;
    // Matches patterns like: 2>&1, 1>&2, >&2, <&0, 2<&-, >&-
    static FD_MERGE_RE: OnceLock<regex::Regex> = OnceLock::new();
    let re = FD_MERGE_RE.get_or_init(|| {
        regex::Regex::new(r"\d*[><]&[\d-]").expect("FD_MERGE_RE regex must compile")
    });
    re.replace_all(command, "").to_string()
}

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
                    // This must consume the second '&' so `&&` is not later
                    // re-read as a lone trailing '&'.
                    '&' if chars.next_if_eq(&'&').is_none() => {
                        return true;
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
fn contains_unsafe_output_redirect_for_shell(command: &str, dialect: ShellDialect) -> bool {
    // Strip safe redirect-to-dev patterns (with word boundary enforcement),
    // then fd-merge patterns, then check for remaining `>`.
    use regex::Regex;
    use std::sync::OnceLock;

    static SAFE_OUTPUT_RE: OnceLock<Regex> = OnceLock::new();
    let re = SAFE_OUTPUT_RE.get_or_init(|| {
        Regex::new(&format!(
            r"\d*>[ ]?/dev/({})(\s|[;&|)]|$)",
            safe_device_redirect_names_pattern()
        ))
        .unwrap()
    });

    let safe = re.replace_all(command, "$2").to_string();
    // Windows null device: strip `>nul`, `1>nul`, `2>nul`, `2>NUL`, and the
    // `\\.\nul` device form (case-insensitive) — the platform equivalent of the
    // `/dev/null` forms stripped above. A trailing non-boundary char (e.g.
    // `>nul.txt`, `>null`) is left intact so only the bare device matches.
    //
    // Gated on the effective shell: only Windows `cmd.exe` resolves `nul` to
    // the discard-only null device. Under a POSIX shell (Unix native or Docker
    // `sh -c`) `nul` is an ordinary relative filename, so
    // `echo x >nul` would create/truncate a workspace file — it must stay
    // flagged as an unsafe file redirect.
    let safe = if matches!(dialect, ShellDialect::WindowsCmd) {
        static SAFE_NUL_OUTPUT_RE: OnceLock<Regex> = OnceLock::new();
        let nul_re = SAFE_NUL_OUTPUT_RE.get_or_init(|| {
            Regex::new(r"(?i)\d*>[ ]?(?:\\\\\.\\)?nul(\s|[;&|)]|$)")
                .expect("SAFE_NUL_OUTPUT_RE regex must compile")
        });
        nul_re.replace_all(&safe, "$1").to_string()
    } else {
        safe
    };
    // Also strip fd-merge redirects (2>&1, 1>&2, >&N, etc.)
    let safe = strip_fd_merge_redirects(&safe);
    contains_unquoted_char(&safe, '>')
}

/// POSIX-dialect convenience wrapper for tests — the conservative default that
/// production reaches when it has no effective shell context. Production shell
/// tools call the `_for_shell` form with the runtime's actual dialect.
#[cfg(test)]
fn contains_unsafe_output_redirect(command: &str) -> bool {
    contains_unsafe_output_redirect_for_shell(command, ShellDialect::Posix)
}

/// Returns true if `command` contains an unquoted `<` that is NOT a heredoc (`<<`)
/// or a safe input redirect from `/dev/*`.
fn contains_unquoted_input_redirect(command: &str) -> bool {
    // Strip here-strings (`<<<`) first, then heredocs (`<<`), then safe /dev/* sources
    // with word boundary enforcement.
    use regex::Regex;
    use std::sync::OnceLock;

    static SAFE_INPUT_RE: OnceLock<Regex> = OnceLock::new();
    let re = SAFE_INPUT_RE.get_or_init(|| {
        Regex::new(r"<[ ]?/dev/(null|zero)(\s|[;&|)]|$)").expect("SAFE_INPUT_RE regex must compile")
    });

    let safe = command.replace("<<<", "").replace("<<", "");
    let safe = re.replace_all(&safe, "$2").to_string();
    // Also strip fd-merge redirects (<&0, <&-, etc.) so they don't leave a bare `<`
    let safe = strip_fd_merge_redirects(&safe);
    contains_unquoted_char(&safe, '<')
}

/// Detect unquoted shell variable expansions like `$HOME`, `$1`, `$?`.
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
        || candidate == "~"
        || candidate.starts_with("~/")
        || (candidate.starts_with('~') && candidate.contains('/'))
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

fn shell_uses_windows_path_syntax(dialect: ShellDialect) -> bool {
    matches!(dialect, ShellDialect::WindowsCmd | ShellDialect::PowerShell)
}

fn has_windows_drive_prefix(candidate: &str) -> bool {
    let bytes = candidate.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

fn is_windows_drive_relative(candidate: &str) -> bool {
    has_windows_drive_prefix(candidate)
        && !candidate
            .as_bytes()
            .get(2)
            .is_some_and(|separator| matches!(*separator, b'/' | b'\\'))
}

fn looks_like_path_for_shell(candidate: &str, dialect: ShellDialect) -> bool {
    looks_like_path(candidate)
        || (shell_uses_windows_path_syntax(dialect)
            && (candidate.contains('\\') || has_windows_drive_prefix(candidate)))
}

fn shell_path_tokens_equal(left: &str, right: &str, dialect: ShellDialect) -> bool {
    if shell_uses_windows_path_syntax(dialect) {
        // Backslash acceptance is a dialect concern (PowerShell/cmd accept `\`
        // on every host), but case sensitivity is a *host* concern. Normalize
        // separators for both sides, then compare with the host filesystem's
        // case rules so cross-platform `pwsh` on a case-sensitive host does not
        // treat `/tmp/Safe/tool` and `/tmp/safe/tool` as the same executable.
        host_path_tokens_equal(&left.replace('\\', "/"), &right.replace('\\', "/"))
    } else {
        expand_user_path(left) == expand_user_path(right)
    }
}

/// Compare two path tokens with the host filesystem's case semantics after `~`
/// expansion: case-insensitive on Windows, exact on case-sensitive Unix. An
/// explicit path is a trust anchor, so folding case on Unix would authorize a
/// differently cased — and potentially different — file.
fn host_path_tokens_equal(left: &str, right: &str) -> bool {
    let left = expand_user_path(left);
    let right = expand_user_path(right);
    #[cfg(target_os = "windows")]
    {
        left.as_os_str().eq_ignore_ascii_case(right.as_os_str())
    }
    #[cfg(not(target_os = "windows"))]
    {
        left == right
    }
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
    let mut chars = body.chars();
    chars.next();
    let value = chars.as_str().trim_start_matches('=').trim();
    if value.is_empty() { None } else { Some(value) }
}

enum RedirectionArgument<'a> {
    Target { prefix: &'a str, target: &'a str },
    NeedsNextToken { prefix: &'a str },
    FdOnly { prefix: &'a str },
    None,
}

fn parse_redirection_argument(token: &str) -> RedirectionArgument<'_> {
    let Some(marker_idx) = token.find(['<', '>']) else {
        return RedirectionArgument::None;
    };
    let prefix = token[..marker_idx].trim();
    let mut rest = &token[marker_idx + 1..];
    rest = rest.trim_start_matches(['<', '>']);
    if let Some(after_amp) = rest.strip_prefix('&') {
        let remaining = after_amp.trim_start_matches(|c: char| c.is_ascii_digit() || c == '-');
        if remaining.is_empty() {
            return RedirectionArgument::FdOnly { prefix };
        }
    }
    rest = rest.trim_start_matches('&');
    rest = rest.trim_start_matches(|c: char| c.is_ascii_digit());
    let trimmed = rest.trim();
    if trimmed.is_empty() {
        RedirectionArgument::NeedsNextToken { prefix }
    } else {
        RedirectionArgument::Target {
            prefix,
            target: trimmed,
        }
    }
}

const SAFE_DEVICE_REDIRECT_TARGETS: [&str; 4] =
    ["/dev/null", "/dev/stdout", "/dev/stderr", "/dev/zero"];

fn safe_device_redirect_names_pattern() -> String {
    SAFE_DEVICE_REDIRECT_TARGETS
        .iter()
        .map(|target| target.trim_start_matches("/dev/"))
        .collect::<Vec<_>>()
        .join("|")
}

fn is_safe_device_redirect_target(target: &str, dialect: ShellDialect) -> bool {
    let target = strip_wrapping_quotes(target).trim();
    if SAFE_DEVICE_REDIRECT_TARGETS.contains(&target) {
        return true;
    }
    // Windows null device: `nul`/`NUL` (case-insensitive) and the full `\\.\nul`
    // device form. Only under a native Windows `cmd.exe` shell does `nul` always
    // resolve to the discard-only null device. Under a POSIX shell (Unix native
    // or Docker `sh -c`) `nul` is an ordinary relative filename, so it
    // must not be treated as a safe device.
    matches!(dialect, ShellDialect::WindowsCmd)
        && (target.eq_ignore_ascii_case("nul") || target.eq_ignore_ascii_case(r"\\.\nul"))
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

/// Compare two bare command names using the same semantics everywhere a
/// command allowlist is interpreted. Path-like entries are handled by their
/// callers and deliberately do not pass through this case-folding rule.
fn command_names_equivalent(left: &str, right: &str) -> bool {
    let left_lower = left.to_ascii_lowercase();
    let right_lower = right.to_ascii_lowercase();
    if left_lower == right_lower {
        return true;
    }

    // On Windows, an omitted executable suffix does not distinguish command
    // names (for example, `git` and `git.exe` grant the same command access).
    #[cfg(target_os = "windows")]
    {
        for ext in &[".exe", ".cmd", ".bat"] {
            if right_lower == format!("{left_lower}{ext}") {
                return true;
            }
            if left_lower == format!("{right_lower}{ext}") {
                return true;
            }
        }
    }

    false
}

fn command_allowlist_entries_equivalent(left: &str, right: &str) -> bool {
    let left = strip_wrapping_quotes(left).trim();
    let right = strip_wrapping_quotes(right).trim();
    if left.is_empty() || right.is_empty() {
        return false;
    }

    // Preserve exact equality for paths. Case-folding a path would widen the
    // policy on case-sensitive filesystems.
    if looks_like_path(left) || looks_like_path(right) {
        return left == right;
    }

    command_names_equivalent(left, right)
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

    // Command-name entries continue to match by basename, case-insensitively.
    // Callers lowercase the basename before it reaches here, so folding only
    // one side would leave an entry written as `Git` or `Docker` unable to
    // match anything.
    command_names_equivalent(allowed, executable_base)
}

/// Decide whether a completed PowerShell token must be rejected by the bounded
/// grammar. Two shapes are rejected because the token the later provider, path,
/// allowlist, and risk checks would inspect differs from the argument
/// PowerShell actually binds:
///
///   * Mixed quoted and unquoted fragments in one token (`E'nv:'PATH`,
///     `C':'\win.ini`). PowerShell concatenates them before binding, so the
///     raw token with embedded quote delimiters can hide a provider path,
///     drive prefix, or `..` traversal.
///   * The bare stop-parsing token `--%` (also matched via the collapsed body
///     for forms whose quote delimiters were removed).
///
/// Fully bare and fully quoted tokens are accepted and parsed normally.
fn reject_powershell_token(has_bare: bool, has_quoted: bool, body: &str) -> bool {
    has_bare && (has_quoted || body == "--%")
}

/// Split a PowerShell command into simple pipeline stages.
///
/// PowerShell is an expression language, not just a command launcher. The
/// generic POSIX-oriented splitter cannot safely reason about constructs such
/// as `(...)`, script blocks, type literals, call operators, or backtick
/// escapes. This parser intentionally accepts only a bounded command grammar:
/// bare command invocations, quoted/plain arguments, simple variable reads,
/// and pipelines. Everything else fails closed.
fn split_powershell_pipeline_syntax(command: &str) -> Option<Vec<String>> {
    let mut segments = Vec::new();
    let mut current = String::new();
    let mut quote = QuoteState::None;
    let mut chars = command.chars().peekable();
    // Track each whitespace-delimited token so the bounded grammar can reject
    // two PowerShell constructs that would otherwise let policy inspect a token
    // different from the one PowerShell binds:
    //
    //   * The stop-parsing token `--%`, which makes PowerShell pass the rest of
    //     the line to a native command verbatim (`git --% push` reaches Git as
    //     `push` while policy only sees `--%`).
    //   * Mixed quoted/unquoted fragments in a single argument. PowerShell
    //     concatenates adjacent fragments before binding, so `E'nv:'PATH` binds
    //     as `Env:PATH` and `C':'\win.ini` as `C:\win.ini`. The later provider,
    //     path, allowlist, and risk checks see the raw token with quote
    //     delimiters still embedded, so a mixed token can hide a provider path,
    //     drive prefix, or `..` traversal from them.
    //
    // `token_body` accumulates the token with quote *delimiters* removed (so
    // `-"-"%` collapses to `--%`). `token_has_bare` records an unquoted
    // character and `token_has_quoted` records a quote delimiter; a token with
    // both is a mixed construction and is rejected. Fully bare and fully quoted
    // tokens are left for normal parsing.
    let mut token_body = String::new();
    let mut token_has_bare = false;
    let mut token_has_quoted = false;

    while let Some(ch) = chars.next() {
        // Backtick changes PowerShell parsing in every quoting mode. Supporting
        // it would require a complete lexer, so the bounded grammar rejects it.
        if ch == '`' || ch == '\0' {
            return None;
        }

        match quote {
            QuoteState::Single => {
                if ch == '\'' {
                    if chars.peek() == Some(&'\'') {
                        current.push(ch);
                        chars.next();
                        current.push(ch);
                        token_body.push(ch);
                        continue;
                    }
                    quote = QuoteState::None;
                    current.push(ch);
                } else {
                    token_body.push(ch);
                    current.push(ch);
                }
            }
            QuoteState::Double => {
                if ch == '"' {
                    if chars.peek() == Some(&'"') {
                        current.push(ch);
                        chars.next();
                        current.push(ch);
                        token_body.push(ch);
                        continue;
                    }
                    quote = QuoteState::None;
                    current.push(ch);
                } else {
                    token_body.push(ch);
                    current.push(ch);
                }
            }
            QuoteState::None => match ch {
                '\'' => {
                    quote = QuoteState::Single;
                    token_has_quoted = true;
                    current.push(ch);
                }
                '"' => {
                    quote = QuoteState::Double;
                    token_has_quoted = true;
                    current.push(ch);
                }
                ' ' | '\t' => {
                    if reject_powershell_token(token_has_bare, token_has_quoted, &token_body) {
                        return None;
                    }
                    token_body.clear();
                    token_has_bare = false;
                    token_has_quoted = false;
                    current.push(ch);
                }
                '|' => {
                    // `||` is a control-flow operator, not a pipeline.
                    if chars.peek() == Some(&'|') {
                        return None;
                    }
                    if reject_powershell_token(token_has_bare, token_has_quoted, &token_body) {
                        return None;
                    }
                    token_body.clear();
                    token_has_bare = false;
                    token_has_quoted = false;
                    let segment = current.trim();
                    if segment.is_empty() {
                        return None;
                    }
                    segments.push(segment.to_string());
                    current.clear();
                }
                // These characters introduce expressions, statements,
                // redirection, splatting, or alternate invocation forms.
                ';' | '\r' | '\n' | '&' | '(' | ')' | '{' | '}' | '[' | ']' | '<' | '>' | '@' => {
                    return None;
                }
                _ => {
                    token_body.push(ch);
                    token_has_bare = true;
                    current.push(ch);
                }
            },
        }
    }

    if quote != QuoteState::None {
        return None;
    }

    if reject_powershell_token(token_has_bare, token_has_quoted, &token_body) {
        return None;
    }

    let segment = current.trim();
    if segment.is_empty() {
        return None;
    }
    segments.push(segment.to_string());

    Some(segments)
}

fn split_simple_powershell_pipeline(command: &str) -> Option<Vec<String>> {
    let segments = split_powershell_pipeline_syntax(command)?;
    powershell_variables_are_simple(command).then_some(segments)
}

/// Accept only `$Name` and `$Name.Property` reads outside single-quoted
/// literals. Subexpressions, braced variables, scoped variables, and special
/// variables are rejected because they change parsing or hide executable text.
fn powershell_variables_are_simple(command: &str) -> bool {
    let chars: Vec<char> = command.chars().collect();
    let mut quote = QuoteState::None;
    let mut i = 0;

    while i < chars.len() {
        let ch = chars[i];
        match quote {
            QuoteState::Single => {
                if ch == '\'' {
                    if chars.get(i + 1) == Some(&'\'') {
                        i += 2;
                        continue;
                    }
                    quote = QuoteState::None;
                }
                i += 1;
                continue;
            }
            QuoteState::Double => {
                if ch == '"' {
                    if chars.get(i + 1) == Some(&'"') {
                        i += 2;
                        continue;
                    }
                    quote = QuoteState::None;
                    i += 1;
                    continue;
                }
            }
            QuoteState::None => {
                if ch == '\'' {
                    quote = QuoteState::Single;
                    i += 1;
                    continue;
                }
                if ch == '"' {
                    quote = QuoteState::Double;
                    i += 1;
                    continue;
                }
            }
        }

        if ch != '$' {
            i += 1;
            continue;
        }

        i += 1;
        if i >= chars.len() || !(chars[i].is_ascii_alphabetic() || chars[i] == '_') {
            return false;
        }
        i += 1;
        while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
            i += 1;
        }
        if chars.get(i) == Some(&':') {
            return false;
        }
        while chars.get(i) == Some(&'.') {
            i += 1;
            if i >= chars.len() || !(chars[i].is_ascii_alphabetic() || chars[i] == '_') {
                return false;
            }
            i += 1;
            while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
        }
    }

    true
}

fn is_powershell_allowlist_entry_match(
    allowed: &str,
    executable: &str,
    executable_base: &str,
) -> bool {
    if allowed.trim() == "*" {
        return true;
    }

    let allowed = strip_wrapping_quotes(allowed).trim();
    if looks_like_path_for_shell(allowed, ShellDialect::PowerShell) {
        // An explicit path is the trust anchor, so it must match with the host
        // filesystem's case semantics: case-insensitive on Windows, exact on
        // case-sensitive Unix. Folding case on every host would let an
        // allowlist entry `/tmp/Safe/tool` authorize the distinct executable
        // `/tmp/safe/tool`.
        return shell_path_tokens_equal(allowed, executable, ShellDialect::PowerShell);
    }

    let allowed = strip_powershell_executable_suffix(allowed);
    allowed.eq_ignore_ascii_case(executable_base)
}

fn strip_powershell_executable_suffix(name: &str) -> &str {
    match name.rsplit_once('.') {
        Some((stem, extension)) if extension.eq_ignore_ascii_case("exe") => stem,
        _ => name,
    }
}

fn is_powershell_batch_file(name: &str) -> bool {
    name.rsplit_once('.').is_some_and(|(_, extension)| {
        extension.eq_ignore_ascii_case("cmd") || extension.eq_ignore_ascii_case("bat")
    })
}

fn is_powershell_provider_argument(argument: &str) -> bool {
    let argument = strip_wrapping_quotes(argument).to_ascii_lowercase();
    argument.contains("::")
        || [
            "alias:",
            "cert:",
            "env:",
            "function:",
            "hkcu:",
            "hklm:",
            "variable:",
            "wsman:",
        ]
        .iter()
        .any(|provider| argument.starts_with(provider))
}

fn is_known_read_only_powershell_command(base: &str) -> bool {
    matches!(
        base,
        "write-output"
            | "echo"
            | "get-date"
            | "get-childitem"
            | "gci"
            | "dir"
            | "ls"
            | "get-location"
            | "gl"
            | "pwd"
            | "get-content"
            | "gc"
            | "type"
            | "cat"
            | "test-path"
            | "resolve-path"
            | "measure-object"
            | "select-object"
            | "sort-object"
            | "compare-object"
            | "format-list"
            | "format-table"
            | "format-wide"
            | "format-custom"
    )
}

fn powershell_named_risk(base: &str) -> Option<CommandRiskLevel> {
    if is_known_read_only_powershell_command(base) {
        return Some(CommandRiskLevel::Low);
    }

    if matches!(
        base,
        "remove-item"
            | "clear-content"
            | "set-content"
            | "add-content"
            | "out-file"
            | "start-process"
            | "stop-process"
            | "invoke-webrequest"
            | "invoke-restmethod"
            | "invoke-expression"
            | "invoke-command"
            | "ac"
            | "clc"
            | "del"
            | "erase"
            | "rd"
            | "ri"
            | "rm"
            | "rmdir"
            | "sc"
            | "saps"
            | "start"
            | "kill"
            | "spps"
            | "curl"
            | "wget"
            | "iwr"
            | "irm"
            | "iex"
            | "icm"
    ) {
        return Some(CommandRiskLevel::High);
    }

    if matches!(
        base,
        "new-item"
            | "copy-item"
            | "move-item"
            | "rename-item"
            | "ni"
            | "copy"
            | "cp"
            | "cpi"
            | "move"
            | "mv"
            | "mi"
            | "ren"
            | "rni"
            | "mkdir"
            | "md"
    ) {
        return Some(CommandRiskLevel::Medium);
    }

    None
}

fn generic_segment_risk(
    base: &str,
    args: &[String],
    joined_segment: &str,
) -> Option<CommandRiskLevel> {
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
            // Windows-specific high-risk commands retained from the existing
            // cross-platform compatibility policy.
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
        return Some(CommandRiskLevel::High);
    }

    if joined_segment.contains("rm -rf /")
        || joined_segment.contains("rm -fr /")
        || joined_segment.contains(":(){:|:&};:")
        || joined_segment.contains("del /s /q")
        || joined_segment.contains("rmdir /s /q")
        || joined_segment.contains("format c:")
    {
        return Some(CommandRiskLevel::High);
    }

    match base {
        "git" => Some(
            if args.first().is_some_and(|verb| {
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
            }) {
                CommandRiskLevel::Medium
            } else {
                CommandRiskLevel::Low
            },
        ),
        "npm" | "pnpm" | "yarn" => Some(
            if args.first().is_some_and(|verb| {
                matches!(
                    verb.as_str(),
                    "install" | "add" | "remove" | "uninstall" | "update" | "publish"
                )
            }) {
                CommandRiskLevel::Medium
            } else {
                CommandRiskLevel::Low
            },
        ),
        "cargo" => Some(
            if args.first().is_some_and(|verb| {
                matches!(
                    verb.as_str(),
                    "add" | "remove" | "install" | "clean" | "publish"
                )
            }) {
                CommandRiskLevel::Medium
            } else {
                CommandRiskLevel::Low
            },
        ),
        "touch" | "mkdir" | "mv" | "cp" | "ln" | "copy" | "xcopy" | "robocopy" | "move" | "ren"
        | "rename" | "mklink" => Some(CommandRiskLevel::Medium),
        _ => None,
    }
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

            match generic_segment_risk(base, &args, &joined_segment) {
                Some(CommandRiskLevel::High) => return CommandRiskLevel::High,
                Some(CommandRiskLevel::Medium) => saw_medium = true,
                Some(CommandRiskLevel::Low) | None => {}
            }
        }

        if saw_medium {
            CommandRiskLevel::Medium
        } else {
            CommandRiskLevel::Low
        }
    }

    /// Classify command risk using the language the runtime will execute.
    pub fn command_risk_level_for_shell(
        &self,
        command: &str,
        dialect: ShellDialect,
    ) -> CommandRiskLevel {
        match dialect {
            ShellDialect::Posix | ShellDialect::WindowsCmd => {
                return self.command_risk_level(command);
            }
            ShellDialect::None => return CommandRiskLevel::High,
            ShellDialect::PowerShell => {}
        }

        let Some(segments) = split_simple_powershell_pipeline(command) else {
            return CommandRiskLevel::High;
        };
        let mut saw_medium = false;
        let has_pipeline = segments.len() > 1;

        for segment in segments {
            let mut words = segment.split_whitespace();
            let Some(base_raw) = words.next() else {
                return CommandRiskLevel::High;
            };
            let base_owned = command_basename(base_raw).to_ascii_lowercase();
            if is_powershell_batch_file(&base_owned) {
                return CommandRiskLevel::High;
            }
            let base = strip_powershell_executable_suffix(&base_owned);
            let arguments: Vec<&str> = words.collect();
            let arguments_lower: Vec<String> = arguments
                .iter()
                .map(|argument| argument.to_ascii_lowercase())
                .collect();
            if arguments
                .iter()
                .any(|argument| is_powershell_provider_argument(argument))
                || (segment.contains('$')
                    && (has_pipeline || !matches!(base, "write-output" | "echo")))
            {
                return CommandRiskLevel::High;
            }

            if base_owned.ends_with(".ps1")
                || base_owned.ends_with(".psm1")
                || base_owned.ends_with(".psd1")
                || matches!(
                    base,
                    "." | "cmd"
                        | "command"
                        | "powershell"
                        | "pwsh"
                        | "sh"
                        | "bash"
                        | "zsh"
                        | "fish"
                        | "wsl"
                )
            {
                return CommandRiskLevel::High;
            }

            match powershell_named_risk(base) {
                Some(CommandRiskLevel::High) => return CommandRiskLevel::High,
                Some(CommandRiskLevel::Medium) => {
                    saw_medium = true;
                    continue;
                }
                Some(CommandRiskLevel::Low) => continue,
                None => {}
            }

            match generic_segment_risk(base, &arguments_lower, &segment.to_ascii_lowercase()) {
                Some(CommandRiskLevel::High) => return CommandRiskLevel::High,
                Some(CommandRiskLevel::Medium) => saw_medium = true,
                Some(CommandRiskLevel::Low) => {}
                // PowerShell resolves bare names through aliases, functions,
                // cmdlets, scripts, and applications. If none of the known
                // command families above recognizes the name, treating it as
                // low risk would let a mutable alias or function hide behind
                // the wildcard allowlist.
                None => return CommandRiskLevel::High,
            }
        }

        if saw_medium {
            CommandRiskLevel::Medium
        } else {
            CommandRiskLevel::Low
        }
    }

    /// Validate full command execution policy (allowlist + risk gate).
    ///
    /// Uses the conservative POSIX shell dialect. Shell tools that know the
    /// runtime's effective shell should call
    /// [`validate_command_execution_for_shell`](Self::validate_command_execution_for_shell)
    /// so the Windows `nul` null device is only accepted under `cmd.exe`.
    pub fn validate_command_execution(
        &self,
        command: &str,
        approved: bool,
    ) -> Result<CommandRiskLevel, String> {
        self.validate_command_execution_for_shell(command, approved, ShellDialect::Posix)
    }

    /// Validate a command against the policy and the runtime's shell language.
    ///
    /// The dialect decides platform-specific redirect safety (e.g. the Windows
    /// `nul` null device is discard-only under `cmd.exe` but an ordinary file
    /// under a POSIX shell).
    pub fn validate_command_execution_for_shell(
        &self,
        command: &str,
        approved: bool,
        dialect: ShellDialect,
    ) -> Result<CommandRiskLevel, String> {
        if dialect == ShellDialect::None {
            return Err("Command blocked: configured runtime has no shell access".into());
        }

        if !self.is_command_allowed_for_shell(command, dialect) {
            return Err(format!("Command not allowed by security policy: {command}"));
        }

        let risk = self.command_risk_level_for_shell(command, dialect);

        if risk == CommandRiskLevel::High {
            if self.block_high_risk_commands
                && !self.is_command_explicitly_allowed_for_shell(command, dialect)
            {
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

        // Path confinement here is specific to Windows shell dialects, whose
        // relative forms (`..\x`, `C:x`) the host-default PathGuardedTool
        // scanner cannot recognize. POSIX path policy is already enforced by
        // that wrapper; running it again here would reject legitimate absolute
        // arguments an operator explicitly allowed (e.g. `rm -rf /tmp/x`).
        if shell_uses_windows_path_syntax(dialect)
            && let Some(path) = self.forbidden_path_argument_for_shell(command, dialect)
        {
            return Err(format!("Command blocked: forbidden path argument: {path}"));
        }

        Ok(risk)
    }

    fn is_command_explicitly_allowed_for_shell(
        &self,
        command: &str,
        dialect: ShellDialect,
    ) -> bool {
        match dialect {
            ShellDialect::PowerShell => {
                let Some(segments) = split_powershell_pipeline_syntax(command) else {
                    return false;
                };
                segments.iter().all(|segment| {
                    let raw_executable =
                        strip_wrapping_quotes(segment.split_whitespace().next().unwrap_or(""))
                            .trim();
                    let base_owned = command_basename(raw_executable).to_ascii_lowercase();
                    let base = strip_powershell_executable_suffix(&base_owned);
                    !base.is_empty()
                        && !is_powershell_batch_file(&base_owned)
                        && self.allowed_commands.iter().any(|allowed| {
                            allowed.trim() != "*"
                                && is_powershell_allowlist_entry_match(
                                    allowed,
                                    raw_executable,
                                    base,
                                )
                        })
                })
            }
            ShellDialect::Posix | ShellDialect::WindowsCmd => {
                self.is_command_explicitly_allowed(command)
            }
            ShellDialect::None => false,
        }
    }

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

    pub fn is_command_allowed(&self, command: &str) -> bool {
        self.is_command_allowed_for_shell(command, ShellDialect::Posix)
    }

    /// Check the command allowlist using the runtime's actual shell language.
    ///
    /// Allowlist + shell-safety check against a specific shell dialect. The
    /// dialect selects the command grammar (PowerShell vs POSIX-like) and gates
    /// platform-specific redirect safety (the Windows `nul` null device is only
    /// discard-safe under `cmd.exe`).
    pub fn is_command_allowed_for_shell(&self, command: &str, dialect: ShellDialect) -> bool {
        match dialect {
            ShellDialect::PowerShell => self.is_simple_powershell_command_allowed(command),
            ShellDialect::Posix | ShellDialect::WindowsCmd => {
                self.is_posix_like_command_allowed(command, dialect)
            }
            ShellDialect::None => false,
        }
    }

    fn is_simple_powershell_command_allowed(&self, command: &str) -> bool {
        if self.autonomy == AutonomyLevel::ReadOnly {
            return false;
        }

        let has_wildcard = self.allowed_commands.iter().any(|c| c.trim() == "*");
        // Preserve the existing trusted-environment escape hatch shared with
        // POSIX/cmd policy: wildcard plus disabled high-risk blocking opts out
        // of command-level syntax restrictions. In every other configuration,
        // apply the complete bounded PowerShell grammar before a named command
        // can qualify for an allowlist or high-risk exemption.
        if has_wildcard && !self.block_high_risk_commands {
            return true;
        }

        let Some(segments) = split_simple_powershell_pipeline(command) else {
            return false;
        };

        for segment in &segments {
            let mut words = segment.split_whitespace();
            let raw_executable = strip_wrapping_quotes(words.next().unwrap_or("")).trim();
            if raw_executable.is_empty()
                || raw_executable.starts_with('$')
                || raw_executable.starts_with(['\'', '"'])
            {
                return false;
            }

            let base_owned = command_basename(raw_executable).to_ascii_lowercase();
            if is_powershell_batch_file(&base_owned) {
                return false;
            }
            let base = strip_powershell_executable_suffix(&base_owned);
            if !self
                .allowed_commands
                .iter()
                .any(|allowed| is_powershell_allowlist_entry_match(allowed, raw_executable, base))
            {
                return false;
            }

            let args_cased: Vec<String> = words.map(str::to_string).collect();
            if args_cased
                .iter()
                .any(|argument| is_powershell_provider_argument(argument))
            {
                return false;
            }
            let args: Vec<String> = args_cased
                .iter()
                .map(|word| word.to_ascii_lowercase())
                .collect();
            if !self.is_args_safe(base, &args, &args_cased) {
                return false;
            }
        }

        true
    }

    fn is_posix_like_command_allowed(&self, command: &str, dialect: ShellDialect) -> bool {
        if self.autonomy == AutonomyLevel::ReadOnly {
            return false;
        }

        // When the operator has explicitly opted out of all command-level
        // restrictions (wildcard + no high-risk blocking), skip the
        // subshell/expansion guard entirely. This allows backticks,
        // $(), heredocs, etc. in trusted environments.
        let has_wildcard = self.allowed_commands.iter().any(|c| c.trim() == "*");
        if has_wildcard && !self.block_high_risk_commands {
            return true;
        }

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
        if contains_unsafe_output_redirect_for_shell(command, dialect) {
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
        // Strip fd-merge redirects (N>&M, N<&M) first so their `&` isn't
        // flagged as background chaining.
        let ampersand_check = strip_fd_merge_redirects(command);
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

            // Validate arguments for the command.
            // Both case-preserved and lowercased argument lists are provided:
            //   - `args_cased` for case-sensitive comparisons (e.g. git -C vs -c)
            //   - `args` (lowercased) for case-insensitive matches (e.g. subcommand names)
            let args_cased: Vec<String> = words.map(|w| w.to_string()).collect();
            let args: Vec<String> = args_cased.iter().map(|w| w.to_ascii_lowercase()).collect();
            if !self.is_args_safe(base_cmd, &args, &args_cased) {
                return false;
            }
        }

        // At least one command must be present
        segments.iter().any(|s| {
            let s = skip_env_assignments(s.trim());
            s.split_whitespace().next().is_some_and(|w| !w.is_empty())
        })
    }

    fn is_args_safe(&self, base: &str, args: &[String], args_cased: &[String]) -> bool {
        let base = base.to_ascii_lowercase();
        match base.as_str() {
            "find" => {
                // find -exec and find -ok allow arbitrary command execution
                !args.iter().any(|arg| arg == "-exec" || arg == "-ok")
            }
            "git" => {
                !args_cased.iter().any(|arg| arg == "-c")
                    && !args.iter().any(|arg| {
                        arg == "config"
                            || arg.starts_with("config.")
                            || arg == "alias"
                            || arg.starts_with("alias.")
                    })
            }
            "python" | "python3" => !args
                .iter()
                .any(|arg| arg.starts_with("-c") || arg.starts_with("-m")),
            "node" => {
                // -e/--eval evaluates argument as JavaScript
                // -p/--print same as --eval but prints the result
                // starts_with covers glued form: node -e'code' (one whitespace token)
                // Ref: https://nodejs.org/api/cli.html
                !args.iter().any(|arg| {
                    arg.starts_with("-e")
                        || arg.starts_with("--eval")
                        || arg.starts_with("-p")
                        || arg.starts_with("--print")
                })
            }
            "pip" | "pip3" => {
                // install/download fetch external packages; setup.py runs arbitrary code
                // Ref: https://blog.phylum.io/python-package-installation-attacks/
                !args.iter().any(|arg| arg == "install" || arg == "download")
            }
            "npm" => {
                // exec can fetch+run remote packages (npx behavior)
                // install fetches external packages; lifecycle scripts run arbitrary code
                // Ref: https://cheatsheetseries.owasp.org/cheatsheets/NPM_Security_Cheat_Sheet.html
                !args.iter().any(|arg| {
                    arg == "exec" || arg == "install" || arg == "i" || arg == "add" || arg == "ci"
                })
            }
            "cargo" => {
                // install fetches+builds external crate; build.rs executes arbitrary code
                // Ref: https://shnatsel.medium.com/do-not-run-any-cargo-commands-on-untrusted-projects
                !args.iter().any(|arg| arg == "install")
            }
            _ => true,
        }
    }

    /// Scan `command` for forbidden path arguments against a specific shell
    /// dialect. The dialect gates which redirect targets count as safe devices
    /// (the Windows `nul` null device is only a safe device under `cmd.exe`) and
    /// which relative forms are recognized as paths — Windows-relative paths are
    /// understood for both cmd.exe and PowerShell, including cross-platform
    /// PowerShell runtimes.
    pub fn forbidden_path_argument_for_shell(
        &self,
        command: &str,
        dialect: ShellDialect,
    ) -> Option<String> {
        self.forbidden_path_argument_impl(command, dialect, false)
    }

    /// Like [`SecurityPolicy::forbidden_workspace_path_argument`], but against
    /// a specific shell dialect: it uses the effective shell's path syntax and
    /// classifies platform-specific safe redirect devices by the shell that will
    /// execute the command, resolving relative candidates through symlinks
    /// before the workspace-boundary check.
    pub fn forbidden_workspace_path_argument_for_shell(
        &self,
        command: &str,
        dialect: ShellDialect,
    ) -> Option<String> {
        self.forbidden_path_argument_impl(command, dialect, true)
    }

    fn forbidden_path_argument_impl(
        &self,
        command: &str,
        dialect: ShellDialect,
        resolve_workspace: bool,
    ) -> Option<String> {
        let forbidden_candidate = |raw: &str| {
            let candidate = strip_wrapping_quotes(raw).trim();
            if candidate.is_empty() || candidate.contains("://") {
                return None;
            }
            if !looks_like_path_for_shell(candidate, dialect) {
                return None;
            }
            // String-level policy: absolute paths outside the workspace, `..`
            // traversal, `~user` forms, and forbidden prefixes.
            if !self.is_path_allowed_for_shell(candidate, dialect) {
                return Some(candidate.to_string());
            }
            // A workspace-relative argument can still escape the boundary via a
            // symlink whose target is outside the workspace: the string check
            // above passes because the literal path has no `..` and is not
            // absolute, yet the real target is elsewhere. The file tools already
            // block this by canonicalizing before checking; mirror that here for
            // the path-shaped argument forms this static scan can see (a full
            // boundary needs the execution-time sandbox). Resolve the deepest
            // existing ancestor (the leaf may be about to be created, e.g.
            // `touch link/new.txt`) and re-check the resolved target with the
            // symlink-aware policy.
            // For a command that runs IN the workspace, also resolve symlinks and
            // re-check: a workspace-relative arg can point outside via an
            // in-workspace symlink, which the string check above misses. A
            // command argument may be read OR written, so accept the resolved
            // target if it is allowed for EITHER (mirrors the string
            // `is_path_allowed`, which honors both read-only and write-only
            // roots). Fail closed otherwise: a target outside every allowed root
            // is blocked, and so is one that cannot be resolved at all (a symlink
            // cycle exhausting the hop budget) - `None` means "unresolvable", not
            // "allowed" - so a crafted chain cannot dodge the check.
            if resolve_workspace {
                match self.resolve_command_path_argument(candidate, dialect) {
                    Some(resolved)
                        if self.is_resolved_path_allowed(&resolved)
                            || self.is_resolved_path_readable(&resolved) => {}
                    _ => return Some(candidate.to_string()),
                }
            }
            None
        };
        let forbidden_non_redirect_candidate = |raw: &str| {
            let candidate = strip_wrapping_quotes(raw).trim();
            if candidate.is_empty() || candidate.contains("://") {
                return None;
            }
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
                return None;
            }
            forbidden_candidate(candidate)
        };
        let executable_has_explicit_path_allowlist = |raw: &str| {
            let executable = strip_wrapping_quotes(raw).trim();
            self.allowed_commands.iter().any(|allowed| {
                let allowed = strip_wrapping_quotes(allowed).trim();
                allowed != "*"
                    && looks_like_path_for_shell(allowed, dialect)
                    && shell_path_tokens_equal(allowed, executable, dialect)
            })
        };

        for segment in split_unquoted_segments(command) {
            let cmd_part = skip_env_assignments(&segment);
            let mut words = cmd_part.split_whitespace();
            let Some(executable) = words.next() else {
                continue;
            };

            let executable_candidate = strip_wrapping_quotes(executable).trim();
            let executable_without_redirect = executable_candidate
                .find(['<', '>'])
                .map_or(executable_candidate, |index| &executable_candidate[..index]);
            if !executable_has_explicit_path_allowlist(executable_without_redirect)
                && let Some(blocked) = forbidden_non_redirect_candidate(executable_without_redirect)
            {
                return Some(blocked);
            }

            let executable_redirect = parse_redirection_argument(strip_wrapping_quotes(executable));
            let mut next_is_redirect_target = false;
            // Cover inline forms like `cat</etc/passwd`.
            match executable_redirect {
                RedirectionArgument::Target { target, .. } => {
                    if !is_safe_device_redirect_target(target, dialect)
                        && let Some(blocked) = forbidden_candidate(target)
                    {
                        return Some(blocked);
                    }
                }
                RedirectionArgument::NeedsNextToken { .. } => {
                    next_is_redirect_target = true;
                }
                RedirectionArgument::FdOnly { .. } | RedirectionArgument::None => {}
            }

            for token in words {
                let candidate = strip_wrapping_quotes(token).trim();
                if candidate.is_empty() {
                    continue;
                }

                if next_is_redirect_target {
                    next_is_redirect_target = false;
                    if is_safe_device_redirect_target(candidate, dialect) {
                        continue;
                    }
                    if let Some(blocked) = forbidden_candidate(candidate) {
                        return Some(blocked);
                    }
                    continue;
                }

                if candidate.contains("://") {
                    continue;
                }

                match parse_redirection_argument(candidate) {
                    RedirectionArgument::Target { prefix, target } => {
                        if let Some(blocked) = forbidden_non_redirect_candidate(prefix) {
                            return Some(blocked);
                        }
                        if is_safe_device_redirect_target(target, dialect) {
                            continue;
                        }
                        if let Some(blocked) = forbidden_candidate(target) {
                            return Some(blocked);
                        }
                    }
                    RedirectionArgument::NeedsNextToken { prefix } => {
                        if let Some(blocked) = forbidden_non_redirect_candidate(prefix) {
                            return Some(blocked);
                        }
                        next_is_redirect_target = true;
                        continue;
                    }
                    RedirectionArgument::FdOnly { prefix } => {
                        if let Some(blocked) = forbidden_non_redirect_candidate(prefix) {
                            return Some(blocked);
                        }
                        continue;
                    }
                    RedirectionArgument::None => {}
                }

                // Handle option assignment forms like `--file=/etc/passwd`.
                if let Some(blocked) = forbidden_non_redirect_candidate(candidate) {
                    return Some(blocked);
                }
                if candidate.starts_with('-') {
                    continue;
                }
            }
        }

        None
    }

    /// Return the first path-like executable or argument blocked by path
    /// policy using the host platform's default shell syntax.
    ///
    /// String-level command path guard: flags a path argument that is absolute
    /// and outside the workspace, uses `..` traversal, a `~user` form, or a
    /// forbidden prefix. Best-effort token parsing, intended as a safety gate
    /// before command execution. Does NOT resolve symlinks, so it is safe for
    /// callers whose working directory is NOT the workspace (e.g. cron jobs run
    /// in `data_dir`). Shell/skill tools, which run IN the workspace, should use
    /// [`SecurityPolicy::forbidden_workspace_path_argument`], which additionally
    /// follows in-workspace symlinks to block escapes.
    pub fn forbidden_path_argument(&self, command: &str) -> Option<String> {
        #[cfg(target_os = "windows")]
        let dialect = ShellDialect::WindowsCmd;
        #[cfg(not(target_os = "windows"))]
        let dialect = ShellDialect::Posix;

        self.forbidden_path_argument_for_shell(command, dialect)
    }

    /// Like [`SecurityPolicy::forbidden_path_argument`] but for a command that
    /// runs IN the workspace: each workspace-relative path argument is also
    /// resolved (following symlinks, including dangling ones) and re-checked
    /// against the workspace boundary with the host platform's default shell
    /// syntax, catching an in-workspace symlink that points outside for the
    /// argument forms this static scan can see.
    ///
    /// This is best-effort, defense-in-depth hardening over a token-scanned
    /// command line - NOT a complete workspace boundary, and NOT equivalent to
    /// the file tools, which resolve an operation-aware target at the call site.
    /// It flags a *path-shaped* argument (one with a separator, e.g. `link/x`, a
    /// redirect target, or an absolute / `..` form) that escapes via an
    /// in-workspace symlink. It does NOT, and cannot from a static parse, cover:
    /// a *bare* argument with no separator (`cat somelink`) that is a symlink; a
    /// path computed at run time via variable expansion or command substitution
    /// (`$VAR`, `$(...)`), `eval`, or a write done inside an executed script
    /// (`sh ./x.sh`, where only the script path is scanned); a quoted path
    /// holding whitespace (`"link dir/out"`), which the whitespace tokenizer
    /// fragments; read-vs-write direction (an argument may be read or written, so
    /// a resolved target allowed for EITHER passes, unlike the operation-aware
    /// file tools); or non-Unix relative forms (a `link\file` path on Windows). A
    /// shell command is Turing-complete; complete containment is the execution
    /// boundary (the OS sandbox and the broader granular sandbox-policy work),
    /// not this preflight.
    pub fn forbidden_workspace_path_argument(&self, command: &str) -> Option<String> {
        #[cfg(target_os = "windows")]
        let dialect = ShellDialect::WindowsCmd;
        #[cfg(not(target_os = "windows"))]
        let dialect = ShellDialect::Posix;

        self.forbidden_workspace_path_argument_for_shell(command, dialect)
    }

    fn is_path_allowed_for_shell(&self, path: &str, dialect: ShellDialect) -> bool {
        if !shell_uses_windows_path_syntax(dialect) {
            return self.is_path_allowed(path);
        }

        // `C:relative` resolves against a per-drive current directory on
        // Windows rather than the configured workspace. There is no stable
        // workspace-relative interpretation, so fail closed on every host.
        if is_windows_drive_relative(path) {
            return false;
        }

        // PowerShell accepts backslashes as path separators on every host.
        // Normalize only for policy evaluation; the original command remains
        // unchanged for process construction.
        let normalized = path.replace('\\', "/");

        // A drive-qualified path cannot name the Unix workspace. This matters
        // for cross-platform PowerShell, where host-native `Path` parsing would
        // otherwise treat `C:/outside` as an ordinary relative path.
        #[cfg(not(target_os = "windows"))]
        if self.workspace_only && has_windows_drive_prefix(&normalized) {
            return false;
        }

        // On Windows, a leading slash without a drive is rooted on the current
        // drive. It is not workspace-relative even though `Path::is_absolute`
        // intentionally reports false for this form.
        #[cfg(target_os = "windows")]
        if self.workspace_only && normalized.starts_with('/') && !normalized.starts_with("//") {
            return false;
        }

        self.is_path_allowed(&normalized)
    }

    /// Resolve a shell command path argument to the canonical target used for
    /// workspace-boundary checks. Relative arguments are taken relative to the
    /// workspace directory; `~` is expanded. Because a command may be about to
    /// CREATE the leaf (e.g. `touch dir/new.txt`), symlinks are resolved on the
    /// deepest existing ancestor and any non-existent trailing components are
    /// re-appended, so an in-workspace symlink pointing outside is followed to
    /// its real target. Relative arguments are joined onto the workspace
    /// directory BEFORE resolving. Returns `None` when no trustworthy target
    /// exists: a null byte in the input, or an unresolvable path (a symlink
    /// cycle exhausting the resolver's hop budget). Callers MUST treat `None`
    /// as a block (fail closed), never as "nothing to re-check" - see
    /// `forbidden_path_argument_impl`.
    fn resolve_command_path_argument(
        &self,
        candidate: &str,
        dialect: ShellDialect,
    ) -> Option<PathBuf> {
        if candidate.contains('\0') {
            return None;
        }
        let normalized;
        let candidate = if shell_uses_windows_path_syntax(dialect) {
            normalized = candidate.replace('\\', "/");
            normalized.as_str()
        } else {
            candidate
        };
        let expanded = expand_user_path(candidate);
        let joined = if expanded.is_absolute() {
            expanded
        } else {
            self.workspace_dir.join(expanded)
        };
        resolve_symlinked_path(&joined)
    }

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

        // The null device is always permitted regardless of workspace or
        // forbidden-path config; the rest of /dev remains blocked as usual.
        if is_null_device(&expanded_path) {
            return true;
        }

        if expanded_path.is_absolute() {
            let in_workspace = expanded_path.starts_with(&self.workspace_dir);
            let in_allowed_root = self
                .allowed_roots
                .iter()
                .any(|root| expanded_path.starts_with(root));
            let in_read_only_root = self
                .allowed_roots_read_only
                .iter()
                .any(|root| expanded_path.starts_with(root));
            let in_write_only_root = self
                .allowed_roots_write_only
                .iter()
                .any(|root| expanded_path.starts_with(root));

            if in_workspace || in_allowed_root || in_read_only_root || in_write_only_root {
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

    pub fn is_resolved_path_readable(&self, resolved: &Path) -> bool {
        // Universal POSIX device files: any operator running on Linux,
        // macOS, or BSD expects these to be readable. Adding them to
        // the per-agent config would be friction without security
        // benefit (they have no agent-relevant content).
        const POSIX_DEVICE_READS: &[&str] =
            &["/dev/null", "/dev/zero", "/dev/random", "/dev/urandom"];
        for device in POSIX_DEVICE_READS {
            if resolved == Path::new(device) {
                return true;
            }
        }

        // Workspace + read-write allowlist + read-only allowlist.
        // Inlined rather than delegating to `is_resolved_path_allowed`
        // so the write-only allowlist is intentionally NOT in scope
        // here.
        let workspace_root = self
            .workspace_dir
            .canonicalize()
            .unwrap_or_else(|_| self.workspace_dir.clone());
        if resolved.starts_with(&workspace_root) {
            return true;
        }
        for root in &self.allowed_roots {
            let canonical = root.canonicalize().unwrap_or_else(|_| root.clone());
            if resolved.starts_with(&canonical) {
                return true;
            }
        }
        for root in &self.allowed_roots_read_only {
            let canonical = root.canonicalize().unwrap_or_else(|_| root.clone());
            if resolved.starts_with(&canonical) {
                return true;
            }
        }
        for root in &self.allowed_roots_write_only {
            let canonical = root.canonicalize().unwrap_or_else(|_| root.clone());
            if resolved.starts_with(&canonical) {
                return false;
            }
        }

        // Forbidden paths gate after the explicit allowlists so the
        // allowlists can coexist with broad default forbidden roots
        // such as `/home` and `/tmp`.
        for forbidden in &self.forbidden_paths {
            let forbidden_path = expand_user_path(forbidden);
            if resolved.starts_with(&forbidden_path) {
                return false;
            }
        }
        if !self.workspace_only {
            return true;
        }
        false
    }

    /// Return the canonical allowlisted root directory that authorizes reading
    /// `resolved`: the workspace first, then read-write roots, then read-only
    /// roots. Callers bind a directory-handle-scoped open (cap-std beneath/
    /// no-follow) to this boundary instead of re-walking a pathname that could be
    /// swapped between the readability check and the open. Returns `None` when no
    /// bounded allowlist root contains the path (e.g. a fully permissive,
    /// non-`workspace_only` policy, or a device path) — there is then no
    /// confinement boundary to bind to. Assumes `resolved` is already canonical
    /// and has passed [`Self::is_resolved_path_readable`].
    pub fn approved_read_root(&self, resolved: &Path) -> Option<PathBuf> {
        let workspace_root = self
            .workspace_dir
            .canonicalize()
            .unwrap_or_else(|_| self.workspace_dir.clone());
        if resolved.starts_with(&workspace_root) {
            return Some(workspace_root);
        }
        for root in self
            .allowed_roots
            .iter()
            .chain(self.allowed_roots_read_only.iter())
        {
            let canonical = root.canonicalize().unwrap_or_else(|_| root.clone());
            if resolved.starts_with(&canonical) {
                return Some(canonical);
            }
        }
        None
    }

    pub fn is_resolved_path_allowed(&self, resolved: &Path) -> bool {
        if is_null_device(resolved) {
            return true;
        }

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

        // Write-only cross-agent grants land here. The bot can write
        // under these paths but `is_resolved_path_readable` does not
        // see them — `AccessMode::Write` is one-way by design.
        for root in &self.allowed_roots_write_only {
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

    fn runtime_config_dirs(&self) -> Vec<PathBuf> {
        let canon = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
        let mut dirs: Vec<PathBuf> = Vec::new();
        if let Some(parent) = self.config_path.as_deref().and_then(Path::parent) {
            dirs.push(canon(parent));
        }
        if let Some(parent) = self.workspace_dir.parent() {
            let dir = canon(parent);
            if !dirs.contains(&dir) {
                dirs.push(dir);
            }
        }
        if let Some(data_dir) = self.data_dir.as_deref() {
            let dir = canon(data_dir);
            if !dirs.contains(&dir) {
                dirs.push(dir);
            }
        }
        dirs
    }

    pub fn is_runtime_config_path(&self, resolved: &Path) -> bool {
        let Some(file_name) = resolved.file_name().and_then(|value| value.to_str()) else {
            return false;
        };
        let is_protected_name = file_name == "config.toml"
            || file_name == "config.toml.bak"
            || file_name.starts_with(".config.toml.tmp-")
            || file_name == "estop-state.json"
            || file_name == "otp-secret"
            || file_name == "webauthn_credentials.json";
        if !is_protected_name {
            return false;
        }
        let Some(parent) = resolved.parent() else {
            return false;
        };
        self.runtime_config_dirs()
            .iter()
            .any(|dir| parent == dir.as_path())
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

    /// Check whether autonomy permits a tool operation without recording it.
    pub fn authorize_tool_operation(
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
                Ok(())
            }
        }
    }

    /// Enforce policy and record an action for callers without a rate-limit wrapper.
    pub fn enforce_tool_operation(
        &self,
        operation: ToolOperation,
        operation_name: &str,
    ) -> Result<(), String> {
        self.authorize_tool_operation(operation, operation_name)?;
        if operation == ToolOperation::Act && !self.record_action() {
            return Err("Rate limit exceeded: action budget exhausted".to_string());
        }
        Ok(())
    }

    /// Atomically reserve one action slot for a production-wrapped invocation.
    pub fn reserve_action(&self) -> Option<ActionReservation> {
        self.tracker.reserve_for_current(self.max_actions_per_hour)
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
            } else if let Some(stripped) =
                workspace_prefixed_relative_suffix(&expanded, &self.workspace_dir)
            {
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

    pub fn is_under_allowed_root(&self, path: &str) -> bool {
        let expanded = expand_user_path(path);
        if !expanded.is_absolute() {
            return false;
        }
        roots_contain(&self.allowed_roots, &expanded)
            || roots_contain(&self.allowed_roots_write_only, &expanded)
    }

    #[must_use]
    pub fn is_under_read_only_allowed_root(&self, path: &str) -> bool {
        let expanded = expand_user_path(path);
        if !expanded.is_absolute() {
            return false;
        }
        roots_contain(&self.allowed_roots_read_only, &expanded)
    }

    /// Union of all three root tiers; directionality is enforced later
    /// by the resolved-path checks.
    #[must_use]
    pub fn is_under_any_allowed_root(&self, path: &str) -> bool {
        self.is_under_allowed_root(path) || self.is_under_read_only_allowed_root(path)
    }

    pub fn ensure_no_escalation_beyond(
        &self,
        parent: &SecurityPolicy,
    ) -> Result<(), EscalationViolation> {
        // Autonomy: child must not exceed parent. ReadOnly < Supervised
        // < Full per the AutonomyLevel ordering.
        if self.autonomy > parent.autonomy {
            return Err(EscalationViolation::AutonomyAboveParent {
                child: self.autonomy,
                parent: parent.autonomy,
            });
        }

        for root in &self.allowed_roots {
            if !parent.allowed_roots.iter().any(|p| path_contains(p, root)) {
                return Err(EscalationViolation::ReadWriteRootNotInParent { path: root.clone() });
            }
        }
        for root in &self.allowed_roots_read_only {
            let in_parent_rw = parent.allowed_roots.iter().any(|p| path_contains(p, root));
            let in_parent_ro = parent
                .allowed_roots_read_only
                .iter()
                .any(|p| path_contains(p, root));
            if !in_parent_rw && !in_parent_ro {
                return Err(EscalationViolation::ReadOnlyRootNotInParent { path: root.clone() });
            }
        }
        for root in &self.allowed_roots_write_only {
            let in_parent_rw = parent.allowed_roots.iter().any(|p| path_contains(p, root));
            let in_parent_wo = parent
                .allowed_roots_write_only
                .iter()
                .any(|p| path_contains(p, root));
            if !in_parent_rw && !in_parent_wo {
                return Err(EscalationViolation::WriteOnlyRootNotInParent { path: root.clone() });
            }
        }
        for cmd in &self.allowed_commands {
            if !parent
                .allowed_commands
                .iter()
                .any(|p| command_allowlist_entries_equivalent(p, cmd))
            {
                return Err(EscalationViolation::CommandNotInParent {
                    command: cmd.clone(),
                });
            }
        }
        if parent.workspace_only && !self.workspace_only {
            return Err(EscalationViolation::WorkspaceOnlyDisabledByChild);
        }

        // Forbidden paths run the OPPOSITE direction from allowlists:
        // the parent's forbidden set must be a subset of the child's,
        // i.e. the child cannot drop a parent's forbidden entry.
        for parent_forbidden in &parent.forbidden_paths {
            if !self.forbidden_paths.iter().any(|c| c == parent_forbidden) {
                return Err(EscalationViolation::ForbiddenPathDroppedByChild {
                    path: parent_forbidden.clone(),
                });
            }
        }

        // shell_env_passthrough is a leak surface: every child entry
        // must already be on the parent's list.
        for var in &self.shell_env_passthrough {
            if !parent.shell_env_passthrough.iter().any(|p| p == var) {
                return Err(EscalationViolation::ShellEnvPassthroughExpanded {
                    variable: var.clone(),
                });
            }
        }

        if self.max_actions_per_hour > parent.max_actions_per_hour {
            return Err(EscalationViolation::MaxActionsExceeded {
                child: self.max_actions_per_hour,
                parent: parent.max_actions_per_hour,
            });
        }
        if self.max_cost_per_day_cents > parent.max_cost_per_day_cents {
            return Err(EscalationViolation::MaxCostExceeded {
                child: self.max_cost_per_day_cents,
                parent: parent.max_cost_per_day_cents,
            });
        }
        if self.shell_timeout_secs > parent.shell_timeout_secs {
            return Err(EscalationViolation::ShellTimeoutExceeded {
                child: self.shell_timeout_secs,
                parent: parent.shell_timeout_secs,
            });
        }
        if parent.block_high_risk_commands && !self.block_high_risk_commands {
            return Err(EscalationViolation::BlockHighRiskCommandsDisabledByChild);
        }
        if parent.require_approval_for_medium_risk && !self.require_approval_for_medium_risk {
            return Err(EscalationViolation::RequireApprovalDisabledByChild);
        }

        Ok(())
    }

    pub fn from_risk_profile(
        risk_profile: &crate::schema::RiskProfileConfig,
        workspace_dir: &Path,
    ) -> Self {
        Self::from_profiles(risk_profile, None, workspace_dir)
    }

    pub fn from_profiles(
        risk_profile: &crate::schema::RiskProfileConfig,
        runtime_profile: Option<&crate::schema::RuntimeProfileConfig>,
        workspace_dir: &Path,
    ) -> Self {
        // When autonomy is Full, disable workspace_only so the agent can
        // access paths outside the workspace. Forbidden-path checks still
        // apply, preventing access to sensitive system directories.
        let effective_workspace_only = if risk_profile.level == AutonomyLevel::Full {
            false
        } else {
            risk_profile.workspace_only
        };

        let runtime_default = crate::schema::RuntimeProfileConfig::default();
        let runtime = runtime_profile.unwrap_or(&runtime_default);

        Self {
            autonomy: risk_profile.level,
            risk_profile_name: String::new(),
            delegation_policy: risk_profile.delegation_policy.clone(),
            workspace_dir: workspace_dir.to_path_buf(),
            // Set by `for_agent` once the install root is known; the
            // profile-only constructor has no config path.
            config_path: None,
            // Set by `for_agent` once the data dir is known; the
            // profile-only constructor has no data dir.
            data_dir: None,
            workspace_only: effective_workspace_only,
            allowed_commands: risk_profile.allowed_commands.clone(),
            forbidden_paths: risk_profile.forbidden_paths.clone(),
            allowed_roots: risk_profile
                .allowed_roots
                .iter()
                .filter(|root| {
                    let t = root.trim();
                    !t.is_empty() && t != crate::traits::UNSET_DISPLAY && t != "*"
                })
                .map(|root| {
                    let expanded = expand_user_path(root);
                    if expanded.is_absolute() {
                        expanded
                    } else {
                        workspace_dir.join(expanded)
                    }
                })
                .collect(),
            allowed_roots_read_only: Vec::new(),
            allowed_roots_write_only: Vec::new(),
            max_actions_per_hour: runtime.max_actions_per_hour,
            max_cost_per_day_cents: runtime.max_cost_per_day_cents,
            require_approval_for_medium_risk: risk_profile.require_approval_for_medium_risk,
            block_high_risk_commands: risk_profile.block_high_risk_commands,
            shell_env_passthrough: risk_profile.shell_env_passthrough.clone(),
            shell_timeout_secs: runtime.shell_timeout_secs,
            allowed_tools: if risk_profile.allowed_tools.is_empty() {
                None
            } else {
                Some(risk_profile.allowed_tools.clone())
            },
            excluded_tools: if risk_profile.excluded_tools.is_empty() {
                None
            } else {
                Some(risk_profile.excluded_tools.clone())
            },
            auto_approve: risk_profile.auto_approve.clone(),
            always_ask: risk_profile.always_ask.clone(),
            sandbox_enabled: risk_profile.sandbox_enabled,
            sandbox_backend: risk_profile.sandbox_backend.clone(),
            firejail_args: risk_profile.firejail_args.clone(),
            tracker: PerSenderTracker::new(),
        }
    }

    pub fn for_agent(config: &crate::schema::Config, agent_alias: &str) -> anyhow::Result<Self> {
        let risk_profile = config.risk_profile_for_agent(agent_alias).ok_or_else(|| {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"agent_alias": agent_alias})),
                "SecurityPolicy::for_agent: agent has no resolvable risk_profile"
            );
            anyhow::Error::msg(format!(
                "agents.{agent_alias} has no resolvable risk_profile (load-time validation should have caught this)"
            ))
        })?;
        let runtime_profile = config.runtime_profile_for_agent(agent_alias);
        // Per-agent workspace becomes the SecurityPolicy boundary so
        // file_read/write/edit and the shell tool jail to the agent's
        // own dir, not the install-wide legacy path.
        let agent_workspace = config.agent_workspace_dir(agent_alias);
        // The per-agent workspace is the shell tool's spawn cwd and the file-tool
        // jail root. Create it here so every path that builds a per-agent policy
        // (agent loop, gateway, channels) has the directory present. A missing cwd
        // makes the shell tool's process spawn fail with ENOENT on a fresh agent.
        std::fs::create_dir_all(&agent_workspace).with_context(|| {
            format!(
                "SecurityPolicy::for_agent: failed to create agent workspace dir {}",
                agent_workspace.display()
            )
        })?;
        let mut policy = Self::from_profiles(risk_profile, runtime_profile, &agent_workspace);
        if let Some(agent_cfg) = config.agents.get(agent_alias) {
            policy.risk_profile_name = agent_cfg.risk_profile.trim().to_string();
        }
        policy.config_path = Some(config.config_path.clone());
        // Runtime data dir: same predicate extends there so state files
        // that the gateway constructs with `&config.data_dir`
        // (`webauthn_credentials.json` for WebAuthnManager) are protected
        // from agent overwrites when `data_dir` overlaps an allowed root.
        policy.data_dir = Some(config.data_dir.clone());

        policy
            .allowed_roots_read_only
            .push(config.shared_workspace_dir().join("skills"));

        if let Some(agent_cfg) = config.agents.get(agent_alias) {
            for (sibling_alias, mode) in &agent_cfg.workspace.access {
                let sibling_dir = config.agent_workspace_dir(sibling_alias.as_str());
                match mode {
                    crate::multi_agent::AccessMode::Read => {
                        policy.allowed_roots_read_only.push(sibling_dir);
                    }
                    crate::multi_agent::AccessMode::Write => {
                        policy.allowed_roots_write_only.push(sibling_dir);
                    }
                    crate::multi_agent::AccessMode::ReadWrite => {
                        policy.allowed_roots.push(sibling_dir);
                    }
                }
            }

            // The escape-hatch flag retains its all-paths semantics —
            // agents that genuinely need to read or write outside any
            // per-agent scope opt in here. Defaults to false.
            if agent_cfg.workspace.unrestricted_filesystem {
                policy.workspace_only = false;
            }
        }

        Ok(policy)
    }

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

    // Platform-specific test paths: Unix uses `/…` paths, Windows uses
    // `C:\…` paths so that `Path::is_absolute()` returns the correct
    // value on each platform.

    #[cfg(not(target_os = "windows"))]
    fn tp_ws() -> PathBuf {
        PathBuf::from("/home/user/.zeroclaw/workspace")
    }
    #[cfg(target_os = "windows")]
    fn tp_ws() -> PathBuf {
        PathBuf::from("C:\\Users\\user\\.zeroclaw\\workspace")
    }

    #[cfg(not(target_os = "windows"))]
    fn tp_ws_shared() -> PathBuf {
        PathBuf::from("/home/user/.zeroclaw/shared")
    }
    #[cfg(target_os = "windows")]
    fn tp_ws_shared() -> PathBuf {
        PathBuf::from("C:\\Users\\user\\.zeroclaw\\shared")
    }

    #[cfg(not(target_os = "windows"))]
    fn tp_outside1() -> &'static str {
        "/home/user/other/file.txt"
    }
    #[cfg(target_os = "windows")]
    fn tp_outside1() -> &'static str {
        "C:\\Users\\user\\other\\file.txt"
    }

    #[cfg(not(target_os = "windows"))]
    fn tp_outside2() -> &'static str {
        "/tmp/file.txt"
    }
    #[cfg(target_os = "windows")]
    fn tp_outside2() -> &'static str {
        "C:\\Users\\Public\\file.txt"
    }

    #[cfg(not(target_os = "windows"))]
    fn tp_sys() -> &'static str {
        "/etc"
    }
    #[cfg(target_os = "windows")]
    fn tp_sys() -> &'static str {
        "C:\\Windows\\System32"
    }

    #[cfg(not(target_os = "windows"))]
    fn tp_sys_sub(sub: &str) -> String {
        format!("/{sub}")
    }
    #[cfg(target_os = "windows")]
    fn tp_sys_sub(sub: &str) -> String {
        format!("C:\\Windows\\{}", sub.replace('/', "\\"))
    }

    #[cfg(not(target_os = "windows"))]
    fn tp_proj() -> PathBuf {
        PathBuf::from("/projects")
    }
    #[cfg(target_os = "windows")]
    fn tp_proj() -> PathBuf {
        PathBuf::from("C:\\projects")
    }

    #[cfg(not(target_os = "windows"))]
    fn tp_data() -> PathBuf {
        PathBuf::from("/data")
    }
    #[cfg(target_os = "windows")]
    fn tp_data() -> PathBuf {
        PathBuf::from("C:\\data")
    }

    #[cfg(not(target_os = "windows"))]
    fn tp_rw() -> PathBuf {
        PathBuf::from("/rw-data")
    }
    #[cfg(target_os = "windows")]
    fn tp_rw() -> PathBuf {
        PathBuf::from("C:\\rw-data")
    }

    #[cfg(not(target_os = "windows"))]
    fn tp_ro() -> PathBuf {
        PathBuf::from("/ro-shared")
    }
    #[cfg(target_os = "windows")]
    fn tp_ro() -> PathBuf {
        PathBuf::from("C:\\ro-shared")
    }

    #[test]
    fn is_tool_allowed_none_is_unrestricted() {
        let p = SecurityPolicy {
            allowed_tools: None,
            excluded_tools: None,
            ..SecurityPolicy::default()
        };
        assert!(p.is_tool_allowed("shell"));
        assert!(p.is_tool_allowed("spawn_subagent"));
        assert!(p.is_tool_allowed("anything_else"));
    }

    #[test]
    fn is_tool_allowed_some_empty_denies_all() {
        let p = SecurityPolicy {
            allowed_tools: Some(vec![]),
            ..SecurityPolicy::default()
        };
        assert!(!p.is_tool_allowed("shell"));
        assert!(!p.is_tool_allowed("spawn_subagent"));
    }

    #[test]
    fn is_tool_allowed_allowlist_admits_only_listed() {
        let p = SecurityPolicy {
            allowed_tools: Some(vec!["shell".into(), "memory_recall".into()]),
            ..SecurityPolicy::default()
        };
        assert!(p.is_tool_allowed("shell"));
        assert!(p.is_tool_allowed("memory_recall"));
        assert!(!p.is_tool_allowed("spawn_subagent"));
        assert!(!p.is_tool_allowed("file_write"));
    }

    #[test]
    fn is_tool_allowed_excluded_overrides_allowlist() {
        let p = SecurityPolicy {
            allowed_tools: Some(vec!["shell".into(), "spawn_subagent".into()]),
            excluded_tools: Some(vec!["spawn_subagent".into()]),
            ..SecurityPolicy::default()
        };
        assert!(p.is_tool_allowed("shell"));
        assert!(
            !p.is_tool_allowed("spawn_subagent"),
            "excluded_tools must subtract from allowlist"
        );
    }

    #[test]
    fn is_tool_allowed_excluded_alone_subtracts_from_unrestricted() {
        let p = SecurityPolicy {
            allowed_tools: None,
            excluded_tools: Some(vec!["spawn_subagent".into()]),
            ..SecurityPolicy::default()
        };
        assert!(p.is_tool_allowed("shell"));
        assert!(!p.is_tool_allowed("spawn_subagent"));
    }

    #[test]
    fn is_tool_excluded_reflects_denylist_independent_of_allowlist() {
        // No denylist → nothing excluded, regardless of allowlist.
        let none = SecurityPolicy {
            allowed_tools: Some(vec!["shell".into()]),
            excluded_tools: None,
            ..SecurityPolicy::default()
        };
        assert!(!none.is_tool_excluded("shell"));
        assert!(
            !none.is_tool_excluded("deploy__run"),
            "a tool omitted from the allowlist is not 'excluded' — the denylist is separate"
        );

        // Denylist subtracts by name, independent of the allowlist.
        let denied = SecurityPolicy {
            allowed_tools: None,
            excluded_tools: Some(vec!["deploy__status".into()]),
            ..SecurityPolicy::default()
        };
        assert!(denied.is_tool_excluded("deploy__status"));
        assert!(!denied.is_tool_excluded("deploy__run"));
    }

    #[test]
    fn from_profiles_propagates_every_risk_profile_field() {
        use crate::schema::RiskProfileConfig;
        use std::path::Path;

        let rp = RiskProfileConfig {
            level: AutonomyLevel::ReadOnly,
            workspace_only: true,
            allowed_commands: vec!["only_this".into()],
            forbidden_paths: vec!["/secret".into()],
            require_approval_for_medium_risk: false,
            block_high_risk_commands: false,
            shell_env_passthrough: vec!["EDITOR".into(), "PAGER".into()],
            auto_approve: vec!["memory_recall".into()],
            always_ask: vec!["shell".into()],
            allowed_roots: vec!["/tmp/extra".into()],
            delegation_policy: crate::autonomy::DelegationPolicy::default(),
            approval_route: None,
            allowed_tools: vec!["shell".into(), "memory_recall".into()],
            excluded_tools: vec!["spawn_subagent".into()],
            sandbox_enabled: Some(true),
            sandbox_backend: Some("firejail".into()),
            firejail_args: vec!["--net=none".into()],
        };

        let policy = SecurityPolicy::from_profiles(&rp, None, Path::new("/ws"));

        assert_eq!(policy.autonomy, AutonomyLevel::ReadOnly, "level → autonomy");
        assert!(policy.workspace_only, "workspace_only");
        assert_eq!(policy.allowed_commands, vec!["only_this".to_string()]);
        assert_eq!(policy.forbidden_paths, vec!["/secret".to_string()]);
        assert!(!policy.require_approval_for_medium_risk);
        assert!(!policy.block_high_risk_commands);
        assert_eq!(
            policy.shell_env_passthrough,
            vec!["EDITOR".to_string(), "PAGER".to_string()]
        );
        assert_eq!(
            policy.auto_approve,
            vec!["memory_recall".to_string()],
            "auto_approve must reach the policy"
        );
        assert_eq!(
            policy.always_ask,
            vec!["shell".to_string()],
            "always_ask must reach the policy"
        );
        assert!(
            policy.allowed_roots.iter().any(|p| p.ends_with("extra")),
            "allowed_roots expansion must reach the policy"
        );
        assert_eq!(
            policy.allowed_tools.as_deref(),
            Some(&["shell".to_string(), "memory_recall".to_string()][..]),
            "allowed_tools must reach the policy"
        );
        assert_eq!(
            policy.excluded_tools.as_deref(),
            Some(&["spawn_subagent".to_string()][..]),
            "excluded_tools must reach the policy"
        );
        assert_eq!(policy.sandbox_enabled, Some(true), "sandbox_enabled");
        assert_eq!(
            policy.sandbox_backend.as_deref(),
            Some("firejail"),
            "sandbox_backend"
        );
        assert_eq!(
            policy.firejail_args,
            vec!["--net=none".to_string()],
            "firejail_args"
        );
    }

    #[test]
    fn from_profiles_full_autonomy_drops_workspace_only() {
        use crate::schema::RiskProfileConfig;
        use std::path::Path;

        let rp = RiskProfileConfig {
            level: AutonomyLevel::Full,
            workspace_only: true,
            ..RiskProfileConfig::default()
        };

        let policy = SecurityPolicy::from_profiles(&rp, None, Path::new("/ws"));
        assert!(
            !policy.workspace_only,
            "Full autonomy must drop workspace_only even when the profile sets it true"
        );
    }

    #[test]
    fn from_profiles_empty_allowed_tools_means_unrestricted_not_deny_all() {
        use crate::schema::RiskProfileConfig;
        use std::path::Path;

        let risk = RiskProfileConfig {
            allowed_tools: Vec::new(),
            ..RiskProfileConfig::default()
        };

        let policy = SecurityPolicy::from_profiles(&risk, None, Path::new("/ws"));

        assert!(
            policy.allowed_tools.is_none(),
            "RiskProfileConfig cannot distinguish an omitted allowed_tools field \
             from allowed_tools = []; both map to no authorization constraint"
        );
        assert!(
            policy.is_tool_allowed("filesystem__write_file"),
            "empty risk-profile allowed_tools is unrestricted, not deny-all"
        );
    }

    #[test]
    fn from_profiles_with_runtime_profile_propagates_budget_caps() {
        use crate::schema::RuntimeProfileConfig;
        use std::path::Path;

        let risk = crate::schema::RiskProfileConfig {
            level: AutonomyLevel::Supervised,
            ..crate::schema::RiskProfileConfig::default()
        };
        let runtime = RuntimeProfileConfig {
            max_actions_per_hour: 99,
            max_cost_per_day_cents: 1234,
            shell_timeout_secs: 300,
            ..RuntimeProfileConfig::default()
        };

        let policy = SecurityPolicy::from_profiles(&risk, Some(&runtime), Path::new("/ws"));

        assert_eq!(policy.max_actions_per_hour, 99);
        assert_eq!(policy.max_cost_per_day_cents, 1234);
        assert_eq!(policy.shell_timeout_secs, 300);
    }

    #[test]
    fn from_profiles_without_runtime_profile_uses_defaults() {
        use std::path::Path;

        let risk = crate::schema::RiskProfileConfig {
            level: AutonomyLevel::Supervised,
            ..crate::schema::RiskProfileConfig::default()
        };

        let policy = SecurityPolicy::from_profiles(&risk, None, Path::new("/ws"));

        assert_eq!(policy.max_actions_per_hour, 20);
        assert_eq!(policy.max_cost_per_day_cents, 500);
        assert_eq!(policy.shell_timeout_secs, 60);
    }

    fn unix_forbidden_path_policy() -> SecurityPolicy {
        SecurityPolicy {
            workspace_dir: PathBuf::from("/workspace"),
            forbidden_paths: vec!["/dev".into(), "/etc".into()],
            ..SecurityPolicy::default()
        }
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

    #[test]
    fn authorize_tool_operation_does_not_consume_rate_budget() {
        let p = SecurityPolicy {
            max_actions_per_hour: 1,
            ..default_policy()
        };

        assert!(
            p.authorize_tool_operation(ToolOperation::Act, "coding_agent")
                .is_ok()
        );
        assert!(
            p.authorize_tool_operation(ToolOperation::Act, "coding_agent")
                .is_ok()
        );
        assert!(p.record_action(), "authorization must leave the slot free");
        assert!(!p.record_action(), "the committed action must consume it");
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
    fn mixed_case_bare_allowlist_entry_matches_on_every_platform() {
        // Callers lowercase the executable basename before the allowlist
        // comparison, so an entry written with any uppercase could never match
        // until both sides were folded.
        let p = SecurityPolicy {
            allowed_commands: vec!["Git".into(), "DOCKER".into()],
            ..SecurityPolicy::default()
        };
        assert!(p.is_command_allowed("git status"));
        assert!(p.is_command_allowed("docker ps"));
        // The invocation may also be capitalized; the basename is folded too.
        assert!(p.is_command_allowed("GIT status"));
        // Entries that are genuinely absent are still refused.
        assert!(!p.is_command_allowed("kubectl get pods"));
    }

    #[test]
    fn mixed_case_allowlist_entry_does_not_widen_path_matching() {
        // Path-like entries stay exact rather than using command-name folding.
        let p = SecurityPolicy {
            allowed_commands: vec!["/usr/bin/Antigravity".into()],
            ..SecurityPolicy::default()
        };
        assert!(p.is_command_allowed("/usr/bin/Antigravity"));
        assert!(!p.is_command_allowed("/usr/bin/antigravity"));
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
    fn command_risk_classifies_powershell_commands() {
        let p = default_policy();
        for command in [
            "Remove-Item important.txt",
            "Set-Content output.txt value",
            "Start-Process calc.exe",
            "Invoke-WebRequest https://example.com",
            "ri important.txt",
            "iwr https://example.com",
            "ac output.txt value",
            "clc output.txt",
            "wsl.exe --exec rm important.txt",
        ] {
            assert_eq!(
                p.command_risk_level_for_shell(command, ShellDialect::PowerShell),
                CommandRiskLevel::High,
                "PowerShell-native command should be high risk: {command}"
            );
        }

        for command in [
            "New-Item output.txt",
            "Copy-Item from.txt to.txt",
            "ni output.txt",
        ] {
            assert_eq!(
                p.command_risk_level_for_shell(command, ShellDialect::PowerShell),
                CommandRiskLevel::Medium,
                "PowerShell-native mutation should be medium risk: {command}"
            );
        }

        for command in ["Write-Output safe", "Get-Date", "Get-ChildItem"] {
            assert_eq!(
                p.command_risk_level_for_shell(command, ShellDialect::PowerShell),
                CommandRiskLevel::Low,
                "read-only PowerShell command should stay low risk: {command}"
            );
        }
    }

    #[test]
    fn powershell_only_names_do_not_change_other_shell_dialects() {
        let p = default_policy();

        for command in [
            "ri file.txt",
            "iwr example.test",
            "ni file.txt",
            "md output",
            "start app",
        ] {
            assert_eq!(
                p.command_risk_level_for_shell(command, ShellDialect::Posix),
                CommandRiskLevel::Low,
                "PowerShell risk names must not leak into POSIX: {command}"
            );
            assert_eq!(
                p.command_risk_level_for_shell(command, ShellDialect::WindowsCmd),
                CommandRiskLevel::Low,
                "PowerShell risk names must not leak into cmd.exe: {command}"
            );
        }
    }

    #[test]
    fn validate_command_blocks_powershell_native_command_via_wildcard() {
        let p = SecurityPolicy {
            autonomy: AutonomyLevel::Full,
            allowed_commands: vec!["*".into()],
            block_high_risk_commands: true,
            ..SecurityPolicy::default()
        };

        let error = p
            .validate_command_execution_for_shell(
                "Remove-Item important.txt",
                true,
                ShellDialect::PowerShell,
            )
            .expect_err("PowerShell-native high-risk commands must be blocked");
        assert!(error.contains("high-risk"));
    }

    #[test]
    fn validate_command_allows_low_risk_powershell_command_via_wildcard() {
        let p = SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            allowed_commands: vec!["*".into()],
            block_high_risk_commands: true,
            ..SecurityPolicy::default()
        };

        let risk = p
            .validate_command_execution_for_shell(
                "Write-Output safe",
                false,
                ShellDialect::PowerShell,
            )
            .expect("low-risk PowerShell commands should not require approval");
        assert_eq!(risk, CommandRiskLevel::Low);
    }

    #[test]
    fn powershell_stop_parsing_token_is_rejected_by_grammar() {
        // `--%` is PowerShell's stop-parsing token: it strips itself and passes
        // the remaining arguments to a native command verbatim. Without special
        // handling the policy would classify `git --% push` by its first token
        // `--%` (low risk) while Git actually receives `push`. The bounded
        // grammar must reject the unquoted operator so it never reaches the
        // named risk classifier.
        assert_eq!(
            split_powershell_pipeline_syntax("git --% push origin main"),
            None
        );
        assert_eq!(
            split_powershell_pipeline_syntax("git --% reset --hard"),
            None
        );
        assert_eq!(split_simple_powershell_pipeline("git --% push"), None);
        // Also rejected when it is the trailing token or inside a pipeline stage.
        assert_eq!(split_powershell_pipeline_syntax("echo hi | git --%"), None);

        // A quoted `--%` is an ordinary literal argument, not the operator, so
        // it must NOT trip the guard (avoid over-blocking legitimate strings).
        assert!(split_powershell_pipeline_syntax("echo \"--%\"").is_some());
        assert!(split_powershell_pipeline_syntax("echo '--%'").is_some());
        // A token that merely contains `--%` as a substring is not the operator.
        assert!(split_powershell_pipeline_syntax("echo --%tail").is_some());

        // Mixed quoting must not launder the operator: PowerShell assembles
        // adjacent quoted and unquoted fragments into one token, so `-"-"%`,
        // `"-"-%`, and `--"%"` all reach the parser as the stop-parsing `--%`
        // while a naive "any quote makes it a literal" check would let them
        // through. The guard collapses quote delimiters and still rejects them
        // because at least one character of the token is bare.
        assert_eq!(
            split_powershell_pipeline_syntax("git -\"-\"% push origin main"),
            None
        );
        assert_eq!(
            split_powershell_pipeline_syntax("git \"-\"-% reset --hard"),
            None
        );
        assert_eq!(split_powershell_pipeline_syntax("git --\"%\" push"), None);
        assert_eq!(split_powershell_pipeline_syntax("git -'-'% push"), None);
        // A fully quoted `--%` stays a literal argument (no bare character).
        assert!(split_powershell_pipeline_syntax("echo \"--%\"").is_some());
        // But `x"--%"` is a mixed bare+quoted token (PowerShell binds it as
        // `x--%`), so it is rejected by the mixed-quoting guard.
        assert_eq!(split_powershell_pipeline_syntax("echo x\"--%\""), None);
    }

    #[test]
    fn validate_command_blocks_powershell_stop_parsing_git_mutation() {
        // Default Windows-style policy: `git` is on the default allowlist and
        // the policy is supervised with medium-risk approval enabled. Before the
        // fix, `git --% push` was classified Low and ran without approval.
        let p = default_policy();

        for command in [
            "git --% push origin main",
            "git --% reset --hard",
            "git --% clean -fdx",
        ] {
            let error = p
                .validate_command_execution_for_shell(command, false, ShellDialect::PowerShell)
                .expect_err("stop-parsing native mutation must not be silently allowed");
            assert!(
                error.contains("not allowed by security policy"),
                "unexpected acceptance for {command}: {error}"
            );
        }

        // Approval must not launder the stop-parsing token either: the command
        // is rejected outright rather than downgraded to an approved mutation.
        let approved = p.validate_command_execution_for_shell(
            "git --% push origin main",
            true,
            ShellDialect::PowerShell,
        );
        assert!(
            approved.is_err(),
            "approval must not bypass the grammar guard"
        );

        // The equivalent parsed command is still governed normally: an ordinary
        // `git push` remains a recognized medium-risk mutation.
        assert_eq!(
            p.command_risk_level_for_shell("git push origin main", ShellDialect::PowerShell),
            CommandRiskLevel::Medium,
        );
    }

    #[test]
    fn powershell_grammar_rejects_mixed_quoted_tokens() {
        // PowerShell concatenates adjacent quoted and unquoted fragments before
        // binding an argument, so a token that mixes bare and quoted characters
        // reaches the native command as a different string than policy's later
        // provider, path, allowlist, and risk checks inspect. The bounded
        // grammar rejects such tokens so those checks never see a laundered
        // provider path, drive prefix, or `..` traversal.
        for command in [
            // `Env:`/provider access hidden by an interior quote.
            "cat E'nv:'PATH",
            "cat \"env\":path",
            // Drive prefix split by a quote: binds as `C:\Windows\win.ini`.
            "cat C':'\\Windows\\win.ini",
            // `..` traversal split across a quote boundary.
            "cat .'.'\\secret.txt",
            "cat \"..\"\\secret.txt",
        ] {
            assert_eq!(
                split_powershell_pipeline_syntax(command),
                None,
                "mixed quoted/unquoted token must be rejected: {command}"
            );
        }

        // Fully bare and fully quoted tokens remain valid — only the *mix* is
        // rejected, so ordinary quoted arguments are not over-blocked.
        assert!(split_powershell_pipeline_syntax("cat env:path").is_some());
        assert!(split_powershell_pipeline_syntax("cat \"env:path\"").is_some());
        assert!(split_powershell_pipeline_syntax("cat 'env:path'").is_some());
        assert!(split_powershell_pipeline_syntax("cat \"my file.txt\"").is_some());
    }

    #[test]
    fn validate_blocks_powershell_provider_hidden_by_mixed_quoting() {
        // End-to-end through the dialect-aware validator: a default-allowlisted
        // read command must not launder an `Env:` provider read past policy by
        // splitting the provider prefix with a quote.
        let p = default_policy();
        let denied = p.validate_command_execution_for_shell(
            "cat E'nv:'PATH",
            true,
            ShellDialect::PowerShell,
        );
        assert!(
            denied.is_err(),
            "quote-hidden provider path must be rejected, got {denied:?}"
        );

        // The unobscured provider form is still recognized and blocked too, so
        // the guard is not the only thing standing between policy and `Env:`.
        assert!(
            p.command_risk_level_for_shell("cat env:path", ShellDialect::PowerShell)
                == CommandRiskLevel::High
        );
    }

    #[test]
    fn powershell_stop_parsing_token_still_follows_trusted_optout() {
        // The wildcard + disabled high-risk-blocking escape hatch intentionally
        // skips the bounded grammar, exactly as it does for backticks and
        // subexpressions. `--%` is not special-cased against that contract.
        let trusted = SecurityPolicy {
            autonomy: AutonomyLevel::Full,
            allowed_commands: vec!["*".into()],
            block_high_risk_commands: false,
            ..SecurityPolicy::default()
        };
        assert!(
            trusted
                .validate_command_execution_for_shell(
                    "git --% push origin main",
                    true,
                    ShellDialect::PowerShell,
                )
                .is_ok(),
            "trusted opt-out must keep bypassing the grammar for --% too"
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
        assert!(!p.is_path_allowed(&tp_sys_sub("etc/passwd")));
        assert!(!p.is_path_allowed(&tp_sys_sub("root/.ssh/id_rsa")));
        assert!(!p.is_path_allowed(tp_outside2()));
    }

    #[test]
    fn absolute_path_inside_workspace_allowed_when_workspace_only() {
        let ws = tp_ws();
        let p = SecurityPolicy {
            workspace_dir: ws.clone(),
            workspace_only: true,
            ..SecurityPolicy::default()
        };
        assert!(p.is_path_allowed(&format!("{}/images/example.png", ws.display())));
        assert!(p.is_path_allowed(&format!("{}/file.txt", ws.display())));
        assert!(!p.is_path_allowed(tp_outside1()));
        assert!(!p.is_path_allowed(tp_outside2()));
    }

    #[test]
    fn absolute_path_in_allowed_root_permitted_when_workspace_only() {
        let ws = tp_ws();
        let shared = tp_ws_shared();
        let p = SecurityPolicy {
            workspace_dir: ws.clone(),
            workspace_only: true,
            allowed_roots: vec![shared.clone()],
            ..SecurityPolicy::default()
        };
        assert!(p.is_path_allowed(&format!("{}/data.txt", shared.display())));
        assert!(p.is_path_allowed(&format!("{}/file.txt", ws.display())));
        assert!(!p.is_path_allowed(tp_outside1()));
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
        assert!(!p.is_path_allowed(&tp_sys_sub("etc/passwd")));
        assert!(!p.is_path_allowed(&tp_sys_sub("root/.bashrc")));
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
        let risk = crate::schema::RiskProfileConfig {
            level: AutonomyLevel::Full,
            workspace_only: false,
            allowed_commands: vec!["docker".into()],
            forbidden_paths: vec!["/secret".into()],
            require_approval_for_medium_risk: false,
            block_high_risk_commands: false,
            shell_env_passthrough: vec!["DATABASE_URL".into()],
            ..crate::schema::RiskProfileConfig::default()
        };
        let runtime = crate::schema::RuntimeProfileConfig {
            max_actions_per_hour: 100,
            max_cost_per_day_cents: 1000,
            ..crate::schema::RuntimeProfileConfig::default()
        };
        let workspace = PathBuf::from("/tmp/test-workspace");
        let policy = SecurityPolicy::from_profiles(&risk, Some(&runtime), &workspace);

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
        //: Full autonomy should disable workspace_only even if the
        // config default keeps it true.
        let autonomy_config = crate::schema::RiskProfileConfig {
            level: AutonomyLevel::Full,
            ..crate::schema::RiskProfileConfig::default()
        };
        let workspace = PathBuf::from("/tmp/test-workspace");
        let policy = SecurityPolicy::from_risk_profile(&autonomy_config, &workspace);

        assert_eq!(policy.autonomy, AutonomyLevel::Full);
        assert!(
            !policy.workspace_only,
            "Full autonomy must override workspace_only to false"
        );
    }

    #[test]
    fn from_config_supervised_preserves_workspace_only() {
        let autonomy_config = crate::schema::RiskProfileConfig {
            level: AutonomyLevel::Supervised,
            ..crate::schema::RiskProfileConfig::default()
        };
        let workspace = PathBuf::from("/tmp/test-workspace");
        let policy = SecurityPolicy::from_risk_profile(&autonomy_config, &workspace);

        assert!(
            policy.workspace_only,
            "Supervised autonomy must preserve workspace_only default (true)"
        );
    }

    #[test]
    fn from_config_normalizes_allowed_roots() {
        let autonomy_config = crate::schema::RiskProfileConfig {
            allowed_roots: vec!["~/Desktop".into(), "shared-data".into()],
            ..crate::schema::RiskProfileConfig::default()
        };
        let workspace = tp_ws();
        let policy = SecurityPolicy::from_risk_profile(&autonomy_config, &workspace);

        let expected_home_root = if let Some(home) = home_dir() {
            home.join("Desktop")
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
    fn action_tracker_retains_actions_when_cutoff_is_unavailable() {
        let mut actions = vec![Instant::now(), Instant::now()];

        retain_actions_after(&mut actions, None);

        assert_eq!(actions.len(), 2);
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

    #[test]
    fn action_tracker_clone_does_not_copy_in_flight_usage() {
        let tracker = ActionTracker::new();
        assert!(tracker.reserve(1));

        let cloned = tracker.clone();

        assert_eq!(cloned.used(), 0);
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
        // Path-prefix bypass: /dev/null followed by extra path component
        assert!(!p.is_command_allowed("echo secret>/dev/nullextra"));
        assert!(!p.is_command_allowed("echo secret > /dev/null/../../etc/passwd"));
        assert!(!p.is_command_allowed("echo secret>/dev/stderrfoo"));
        // Word→non-word boundary bypasses
        assert!(!p.is_command_allowed("ls 2>/dev/stderr.log"));
        assert!(!p.is_command_allowed("cat>/dev/zero/path"));
        assert!(!p.is_command_allowed("echo>/dev/stdout.bak"));
    }

    // ── Interpreter argument injection ────────────────────

    #[test]
    fn interpreter_inline_eval_blocked() {
        let p = default_policy();
        // python: -c executes code string, -m runs arbitrary module
        assert!(!p.is_command_allowed("python3 -c 'import os; os.system(\"id\")'"));
        assert!(!p.is_command_allowed("python -c '__import__(\"os\").system(\"id\")'"));
        assert!(!p.is_command_allowed("python3 -m http.server"));
        assert!(!p.is_command_allowed("python3 -m pip install evil"));
        // Broad -m block: these are intentional collateral
        assert!(!p.is_command_allowed("python3 -m pytest"));
        assert!(!p.is_command_allowed("python3 -m mypy src/"));
        assert!(!p.is_command_allowed("python3 -m venv .venv"));
        // Glued form: -mhttp.server is one token
        assert!(!p.is_command_allowed("python3 -mhttp.server"));
        // node: -e/--eval evaluates JS, -p/--print evaluates and prints
        assert!(!p.is_command_allowed("node -e 'require(\"child_process\").execSync(\"id\")'"));
        assert!(!p.is_command_allowed("node --eval 'process.exit(1)'"));
        assert!(!p.is_command_allowed("node --eval=process.exit(1)"));
        assert!(!p.is_command_allowed("node -p '1+1'"));
        assert!(!p.is_command_allowed("node --print 'process.env'"));
        assert!(!p.is_command_allowed("node --print=process.env"));
        // Glued form bypass: -c'code' is one whitespace token
        assert!(!p.is_command_allowed("python3 -c'import os'"));
        assert!(!p.is_command_allowed("node -e'process.exit()'"));
        // Flag with other args before it
        assert!(!p.is_command_allowed("python3 -W ignore -c 'import os'"));
    }

    #[test]
    fn package_manager_install_blocked() {
        let p = default_policy();
        // pip: install/download fetch external packages and run setup.py
        assert!(!p.is_command_allowed("pip install evil-package"));
        assert!(!p.is_command_allowed("pip3 install evil-package"));
        assert!(!p.is_command_allowed("pip download evil-package"));
        // npm: exec fetches remote, install runs lifecycle scripts
        assert!(!p.is_command_allowed("npm exec -- malicious-pkg"));
        assert!(!p.is_command_allowed("npm install malicious-pkg"));
        assert!(!p.is_command_allowed("npm i malicious-pkg"));
        assert!(!p.is_command_allowed("npm add malicious-pkg"));
        assert!(!p.is_command_allowed("npm ci"));
        // cargo: install fetches+builds external crate (build.rs runs arbitrary code)
        assert!(!p.is_command_allowed("cargo install malicious-crate"));
    }

    #[test]
    fn safe_interpreter_usage_allowed() {
        let p = default_policy();
        // Running local files is safe — user trusts their workspace
        assert!(p.is_command_allowed("python3 script.py"));
        assert!(p.is_command_allowed("node app.js"));
        // Read-only / local workspace operations
        assert!(p.is_command_allowed("pip list"));
        assert!(p.is_command_allowed("pip freeze"));
        assert!(p.is_command_allowed("pip show requests"));
        assert!(p.is_command_allowed("npm test"));
        assert!(p.is_command_allowed("npm list"));
        assert!(p.is_command_allowed("cargo build"));
        assert!(p.is_command_allowed("cargo test"));
        assert!(p.is_command_allowed("cargo run"));
    }

    #[test]
    fn safe_redirect_to_dev_null_allowed() {
        let p = default_policy();
        assert!(p.is_command_allowed("echo secret > /dev/null"));
        assert!(p.is_command_allowed("ls 2> /dev/null"));
        assert!(p.is_command_allowed("find . 2>&1 > /dev/null"));
        assert!(p.is_command_allowed("cat</dev/null"));
    }

    #[test]
    fn windows_nul_redirect_allowed_only_under_cmd_exe() {
        // The Windows null device is a safe discard-only redirect target — but
        // ONLY under a native Windows `cmd.exe` shell. Under a POSIX shell (Unix
        // native or Docker `sh -c`) `nul` is an ordinary relative
        // filename, so `>nul` must stay blocked to prevent a workspace-file write.
        use ShellDialect::{Posix, WindowsCmd};
        let p = SecurityPolicy {
            allowed_commands: vec!["git".into(), "echo".into()],
            ..SecurityPolicy::default()
        };

        for cmd in [
            "git -C E:/repo ls-tree -r --name-only HEAD path 2>nul",
            "echo x >nul",
            "echo x 1>NUL",
            "echo x 2>Nul",
            "echo x > nul",     // target as a separate token
            r"echo x >\\.\nul", // full device form
        ] {
            // cmd.exe: nul resolves to the discard-only null device.
            assert!(
                p.is_command_allowed_for_shell(cmd, WindowsCmd),
                "cmd.exe must allow the nul null device: {cmd}"
            );
            // POSIX shell: the SAME command must be blocked (nul is a real file).
            assert!(
                !p.is_command_allowed_for_shell(cmd, Posix),
                "POSIX shell must block a redirect to `nul` (an ordinary file): {cmd}"
            );
        }

        // The default (dialect-less) entry point is POSIX/fail-closed, so it
        // blocks `nul`; configured runtime consumers pass their actual dialect.
        assert!(!p.is_command_allowed("echo x >nul"));

        // The redirect gate itself: nul is stripped as safe only under cmd.exe.
        assert!(!contains_unsafe_output_redirect_for_shell(
            "git status 2>nul",
            WindowsCmd
        ));
        assert!(contains_unsafe_output_redirect_for_shell(
            "git status 2>nul",
            Posix
        ));
        assert!(!contains_unsafe_output_redirect_for_shell(
            r"echo x >\\.\nul",
            WindowsCmd
        ));
        assert!(contains_unsafe_output_redirect_for_shell(
            r"echo x >\\.\nul",
            Posix
        ));

        // /dev/null stays safe under BOTH dialects; a real file and a non-bare
        // `nul`-prefixed name stay blocked under both.
        assert!(p.is_command_allowed_for_shell("git status 2>/dev/null", Posix));
        assert!(p.is_command_allowed_for_shell("git status 2>/dev/null", WindowsCmd));
        assert!(!p.is_command_allowed_for_shell("echo secret 2>out.txt", WindowsCmd));
        assert!(!p.is_command_allowed_for_shell("echo secret >nul.txt", WindowsCmd));
        assert!(contains_unsafe_output_redirect_for_shell(
            "echo secret >nul.txt",
            WindowsCmd
        ));
    }

    #[test]
    fn safe_redirect_to_dev_stdout_allowed() {
        let p = default_policy();
        assert!(p.is_command_allowed("echo hello > /dev/stdout"));
        assert!(p.is_command_allowed("cat /dev/zero > /dev/stdout"));
    }

    #[test]
    fn safe_redirect_to_dev_stderr_allowed() {
        let p = default_policy();
        assert!(p.is_command_allowed("echo error > /dev/stderr"));
        assert!(p.is_command_allowed("ls 1> /dev/stderr"));
    }

    #[test]
    fn safe_redirect_to_dev_zero_allowed() {
        let p = default_policy();
        assert!(p.is_command_allowed("cat /dev/zero > /dev/null"));
    }

    #[test]
    fn safe_file_descriptor_redirect_allowed() {
        let p = default_policy();
        assert!(p.is_command_allowed("find . 2>&1"));
        assert!(p.is_command_allowed("echo hello 1>&2"));
        assert!(p.is_command_allowed("ls 2>&1 > /dev/null"));
        // Bare fd redirects (implicit fd number)
        assert!(p.is_command_allowed("echo error >&2"));
        assert!(p.is_command_allowed("cat <&0"));
        assert!(p.is_command_allowed("echo >&-"));
        assert!(p.is_command_allowed("echo 3>&-"));
    }

    #[test]
    fn heredoc_and_herestring_allowed() {
        let p = default_policy();
        assert!(p.is_command_allowed("cat << 'EOF'"));
        assert!(p.is_command_allowed("cat <<EOF"));
        assert!(p.is_command_allowed("cat <<< 'hello'"));
        // Input redirects from files still blocked
        assert!(!p.is_command_allowed("cat < /etc/passwd"));
        // Output redirects to files still blocked
        assert!(!p.is_command_allowed("echo secret > output.txt"));
    }

    #[test]
    fn multiline_heredoc_allowed() {
        let p = default_policy();
        // Multiline heredoc body must not be split into separate segments that
        // fail the allowlist check on the body lines.
        assert!(p.is_command_allowed("cat <<EOF\nhello world\nEOF"));
        assert!(p.is_command_allowed("cat <<'EOF'\nhello world\nEOF"));
        assert!(p.is_command_allowed("cat << EOF\nhello world\nEOF"));
        // Quoted delimiter variant
        assert!(p.is_command_allowed("cat <<\"EOF\"\nhello world\nEOF"));
        // Heredoc followed by an allowed command is still two valid segments
        assert!(p.is_command_allowed("cat <<EOF\nhello\nEOF\necho done"));
        // Heredoc followed by a disallowed command must be blocked
        assert!(!p.is_command_allowed("cat <<EOF\nhello\nEOF\nrm -rf /"));
        // Unterminated heredoc — entire input stays as one segment (safe: cat is allowed).
        assert!(p.is_command_allowed("cat <<EOF\nhello world"));
    }

    #[test]
    fn redirect_helper_unit_tests() {
        assert!(!contains_unquoted_input_redirect("cat << 'EOF'"));
        assert!(!contains_unquoted_input_redirect("cat <<< 'hello'"));
        assert!(contains_unquoted_input_redirect("cat < /etc/passwd"));
        assert!(!contains_unquoted_input_redirect("echo 'a<b'"));
        assert!(!contains_unquoted_input_redirect("cat</dev/null"));
        // Input redirect word→non-word bypass (same fix as output redirects)
        assert!(contains_unquoted_input_redirect("cat</dev/null.secret"));
        assert!(contains_unquoted_input_redirect(
            "cat </dev/zero/etc/passwd"
        ));
        assert!(!contains_unsafe_output_redirect("cmd 2>/dev/null"));
        assert!(!contains_unsafe_output_redirect("cmd >/dev/null"));
        assert!(!contains_unsafe_output_redirect("cmd 1>/dev/null"));
        assert!(!contains_unsafe_output_redirect("cmd 2>&1"));
        assert!(!contains_unsafe_output_redirect("cmd 1>&2"));
        assert!(!contains_unsafe_output_redirect("echo > /dev/stdout"));
        assert!(!contains_unsafe_output_redirect("echo > /dev/stderr"));
        assert!(!contains_unsafe_output_redirect("echo > /dev/zero"));
        assert!(contains_unsafe_output_redirect("echo hi > file.txt"));
        assert!(!contains_unsafe_output_redirect("echo 'a>b'"));
        // Word→non-word boundary bypasses: dot, slash, or other non-operator chars
        // after a safe device name must NOT strip the redirect
        assert!(contains_unsafe_output_redirect("ls 2>/dev/stderr.log"));
        assert!(contains_unsafe_output_redirect("cat>/dev/zero/path"));
        assert!(contains_unsafe_output_redirect("echo>/dev/stdout.bak"));
    }

    #[test]
    fn quoted_ampersand_and_redirect_literals_are_not_treated_as_operators() {
        let p = default_policy();
        assert!(p.is_command_allowed("echo \"A&B\""));
        assert!(p.is_command_allowed("echo \"A>B\""));
        assert!(p.is_command_allowed("echo \"A<B\""));
    }

    #[test]
    fn git_dash_c_uppercase_is_allowed() {
        // git -C (change directory) must not be
        // conflated with git -c (set config override) after arg lowercasing.
        let p = default_policy();
        assert!(
            p.is_command_allowed("git -C /home/user/repo status --short"),
            "git -C is benign and should be allowed"
        );
        assert!(
            p.is_command_allowed("git -C /home/user/repo log --oneline -1"),
            "git -C with log should be allowed"
        );
        // git -c (lowercase) is still blocked — config override injection
        assert!(
            !p.is_command_allowed("git -c core.editor=\"rm -rf /\" commit"),
            "git -c must remain blocked"
        );
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
        let p = unix_forbidden_path_policy();
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
    fn powershell_path_guard_blocks_windows_relative_and_executable_paths() {
        let p = SecurityPolicy {
            autonomy: AutonomyLevel::Full,
            workspace_dir: tp_ws(),
            allowed_commands: vec!["cat".into(), "git".into()],
            ..SecurityPolicy::default()
        };

        for (command, blocked_path) in [
            ("cat ..\\secret.txt", "..\\secret.txt"),
            ("cat .\\..\\secret.txt", ".\\..\\secret.txt"),
            ("cat ~\\.ssh\\id_rsa", "~\\.ssh\\id_rsa"),
            ("cat C:secret.txt", "C:secret.txt"),
            ("..\\git.exe status", "..\\git.exe"),
        ] {
            assert_eq!(
                p.forbidden_path_argument_for_shell(command, ShellDialect::PowerShell),
                Some(blocked_path.to_string()),
                "PowerShell path should be blocked: {command}"
            );

            let error = p
                .validate_command_execution_for_shell(command, true, ShellDialect::PowerShell)
                .expect_err("named allowlists must not bypass PowerShell path confinement");
            assert!(
                error.contains("forbidden path argument"),
                "unexpected rejection for {command}: {error}"
            );
        }

        assert_eq!(
            p.forbidden_path_argument_for_shell("cat .\\src\\main.rs", ShellDialect::PowerShell),
            None
        );
        assert_eq!(
            p.forbidden_path_argument_for_shell(".\\git.exe status", ShellDialect::PowerShell),
            None
        );

        let explicit_path_policy = SecurityPolicy {
            autonomy: AutonomyLevel::Full,
            workspace_dir: tp_ws(),
            allowed_commands: vec!["/usr/bin/antigravity".into()],
            ..SecurityPolicy::default()
        };
        assert!(
            explicit_path_policy
                .validate_command_execution_for_shell(
                    "/usr/bin/antigravity",
                    true,
                    ShellDialect::PowerShell,
                )
                .is_ok(),
            "an exact executable-path allowlist must retain its existing meaning"
        );
    }

    #[test]
    fn powershell_allowlist_accepts_case_insensitive_exe_suffix() {
        assert!(is_powershell_allowlist_entry_match(
            "git.EXE", "git.exe", "git"
        ));
    }

    #[test]
    fn powershell_allowlist_accepts_windows_relative_path_spelling() {
        assert!(is_powershell_allowlist_entry_match(
            r".\tools\git.exe",
            "./tools/git.exe",
            "git"
        ));
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn powershell_allowlist_path_is_case_sensitive_on_unix() {
        // The explicit path is the trust anchor. On a case-sensitive host,
        // `/tmp/Safe/tool` and `/tmp/safe/tool` can be different executables,
        // so a differently cased request must not inherit the allowlist grant.
        let p = SecurityPolicy {
            autonomy: AutonomyLevel::Full,
            workspace_dir: tp_ws(),
            allowed_commands: vec!["/tmp/Safe/tool".into()],
            block_high_risk_commands: true,
            ..SecurityPolicy::default()
        };

        // Exact case remains authorized.
        assert!(
            p.validate_command_execution_for_shell(
                "/tmp/Safe/tool",
                true,
                ShellDialect::PowerShell,
            )
            .is_ok(),
            "exact-case explicit path must stay authorized"
        );

        // Differently cased executable must NOT match the trusted entry.
        assert!(
            p.validate_command_execution_for_shell(
                "/tmp/safe/tool",
                true,
                ShellDialect::PowerShell,
            )
            .is_err(),
            "case-distinct executable must not inherit the allowlist grant on a case-sensitive host"
        );

        // The workspace-exemption path (forbidden_path_argument) must apply the
        // same case rule: the differently cased executable is not exempted by
        // the trusted entry, so it is subject to path confinement.
        assert!(
            !is_powershell_allowlist_entry_match("/tmp/Safe/tool", "/tmp/safe/tool", "tool"),
            "path allowlist match must be case-sensitive on Unix"
        );
        assert!(
            is_powershell_allowlist_entry_match("/tmp/Safe/tool", "/tmp/Safe/tool", "tool"),
            "exact-case path allowlist match must still succeed"
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
        let p = unix_forbidden_path_policy();
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
        let p = unix_forbidden_path_policy();
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
        // Bare `~user` with no path component is not a forbidden path argument:
        // narrowed to avoid false-positives on non-path `~`-prefixed tokens
        // (e.g. `~20`). A `~user/...` form with a slash still blocks (above).
        assert_eq!(p.forbidden_path_argument("ls ~nobody"), None);
    }

    #[test]
    fn forbidden_path_argument_ignores_tilde_non_path_and_heredoc_body() {
        let p = unix_forbidden_path_policy();

        // Tilde-then-non-slash tokens are not home paths: `~20`, `~589`, `~foo`.
        assert_eq!(
            p.forbidden_path_argument("echo \"about ~20 percent\""),
            None
        );
        assert_eq!(
            p.forbidden_path_argument("python3 -c \"print('about ~589 lines')\""),
            None
        );
        assert_eq!(
            p.forbidden_path_argument("printf 'roughly ~foo here\\n'"),
            None
        );

        // Heredoc body content is stdin data, never an argv path argument.
        let heredoc =
            "cat <<'EOF' > ./out.txt\nthis line has ~20 percent and /etc/passwd mentioned\nEOF";
        assert_eq!(p.forbidden_path_argument(heredoc), None);

        // Security preserved: real forbidden home/absolute path arguments still block.
        assert_eq!(
            p.forbidden_path_argument("cat ~/.ssh/id_rsa"),
            Some("~/.ssh/id_rsa".into())
        );
        assert_eq!(
            p.forbidden_path_argument("cat ~root/.ssh/id_rsa"),
            Some("~root/.ssh/id_rsa".into())
        );
        assert_eq!(
            p.forbidden_path_argument("cat /etc/shadow"),
            Some("/etc/shadow".into())
        );
        // A forbidden path used as a real argument on the heredoc opener line
        // must still block, even when a heredoc body follows.
        assert_eq!(
            p.forbidden_path_argument("cat /etc/passwd <<'EOF'\nbody ~20\nEOF"),
            Some("/etc/passwd".into())
        );
    }

    #[test]
    fn forbidden_path_argument_blocks_path_after_quoted_heredoc_like_text() {
        let p = unix_forbidden_path_policy();

        assert_eq!(
            p.forbidden_path_argument("printf \"<<EOF\nbody\nEOF\" /etc/shadow"),
            Some("/etc/shadow".into())
        );

        // Single-quoted variant of the same shape.
        assert_eq!(
            p.forbidden_path_argument("printf '<<EOF\nbody\nEOF' /etc/passwd"),
            Some("/etc/passwd".into())
        );
    }

    #[test]
    fn forbidden_path_argument_detects_input_redirection_paths() {
        let p = unix_forbidden_path_policy();
        assert_eq!(
            p.forbidden_path_argument("cat </etc/passwd"),
            Some("/etc/passwd".into())
        );
        assert_eq!(
            p.forbidden_path_argument("cat</etc/passwd"),
            Some("/etc/passwd".into())
        );
    }

    #[test]
    fn forbidden_path_argument_allows_safe_device_redirect_targets() {
        let p = unix_forbidden_path_policy();
        assert_eq!(p.forbidden_path_argument("ls missing 2>/dev/null"), None);
        assert_eq!(p.forbidden_path_argument("ls missing 2> /dev/null"), None);
        assert_eq!(p.forbidden_path_argument("echo hi >/dev/stdout"), None);
        assert_eq!(p.forbidden_path_argument("echo hi > /dev/stdout"), None);
        assert_eq!(p.forbidden_path_argument("echo err 1>/dev/stderr"), None);
        assert_eq!(p.forbidden_path_argument("echo err 1> /dev/stderr"), None);
        assert_eq!(p.forbidden_path_argument("cat </dev/zero"), None);
        assert_eq!(p.forbidden_path_argument("cat < /dev/zero"), None);
        #[cfg(not(target_os = "windows"))]
        assert_eq!(p.forbidden_path_argument("cat /dev/null"), None);
        assert_eq!(p.forbidden_path_argument("cat ./safe.txt>/dev/null"), None);
        assert_eq!(p.forbidden_path_argument("cat> /dev/null"), None);
        assert_eq!(p.forbidden_path_argument("cat ./safe.txt>&2"), None);
    }

    #[test]
    fn forbidden_path_argument_blocks_unsafe_redirect_targets() {
        let p = unix_forbidden_path_policy();
        assert_eq!(
            p.forbidden_path_argument("echo hi >/etc/passwd"),
            Some("/etc/passwd".into())
        );
        assert_eq!(
            p.forbidden_path_argument("echo hi > /etc/passwd"),
            Some("/etc/passwd".into())
        );
        assert_eq!(
            p.forbidden_path_argument("echo hi >/dev/stderr.log"),
            Some("/dev/stderr.log".into())
        );
        assert_eq!(
            p.forbidden_path_argument("echo hi > /dev/stderr.log"),
            Some("/dev/stderr.log".into())
        );
        assert_eq!(
            p.forbidden_path_argument("cat </dev/zero/etc/passwd"),
            Some("/dev/zero/etc/passwd".into())
        );
        assert_eq!(
            p.forbidden_path_argument("echo hi >/dev/null/../../etc/passwd"),
            Some("/dev/null/../../etc/passwd".into())
        );
        assert_eq!(
            p.forbidden_path_argument("cat</dev/null /etc/passwd"),
            Some("/etc/passwd".into())
        );
        assert_eq!(
            p.forbidden_path_argument("cat /etc/passwd>/dev/null"),
            Some("/etc/passwd".into())
        );
        assert_eq!(
            p.forbidden_path_argument("cat /etc/passwd> /dev/null"),
            Some("/etc/passwd".into())
        );
        assert_eq!(
            p.forbidden_path_argument("cat /etc/passwd>&2"),
            Some("/etc/passwd".into())
        );
        assert_eq!(
            p.forbidden_path_argument("grep --file=/etc/passwd>/dev/null root"),
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
        assert!(!p.is_path_allowed(&tp_sys_sub("proc/self/root/etc/passwd")));
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
        assert!(!p.is_path_allowed(&tp_sys_sub("var/run/docker.sock")));
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
        assert!(p.reserve_action().is_none());
    }

    #[test]
    fn action_reservation_drop_releases_exact_slot() {
        let p = SecurityPolicy {
            max_actions_per_hour: 2,
            ..SecurityPolicy::default()
        };

        let first = p.reserve_action().expect("first reservation");
        let second = p.reserve_action().expect("second reservation");
        assert!(p.reserve_action().is_none(), "both slots are reserved");

        drop(first);
        let replacement = p.reserve_action().expect("only first slot was released");
        second.commit();
        assert!(
            p.reserve_action().is_none(),
            "second reservation committed while replacement remains in flight"
        );

        drop(replacement);
        let final_slot = p.reserve_action().expect("replacement slot was released");
        final_slot.commit();
        assert!(p.reserve_action().is_none(), "both slots are now committed");
    }

    #[test]
    fn action_reservations_are_isolated_by_sender() {
        let tracker = PerSenderTracker::new();
        let sender_a = tracker
            .reserve_within("sender-a".to_string(), 1)
            .expect("sender A reservation");

        assert!(
            tracker.reserve_within("sender-a".to_string(), 1).is_none(),
            "sender A has no second slot"
        );
        let sender_b = tracker
            .reserve_within("sender-b".to_string(), 1)
            .expect("sender B has an independent slot");

        sender_a.commit();
        drop(sender_b);
        assert!(
            tracker.reserve_within("sender-b".to_string(), 1).is_some(),
            "releasing sender B is independent from sender A's commit"
        );
    }

    #[test]
    fn released_reservation_removes_empty_sender_bucket() {
        let tracker = PerSenderTracker::new();
        let reservation = tracker
            .reserve_within("ephemeral-sender".to_string(), 1)
            .expect("reservation");
        assert!(tracker.buckets.lock().contains_key("ephemeral-sender"));

        drop(reservation);

        assert!(!tracker.buckets.lock().contains_key("ephemeral-sender"));
    }

    #[test]
    fn zero_budget_does_not_create_sender_bucket() {
        let tracker = PerSenderTracker::new();

        assert!(!tracker.record_within("zero-budget", 0));
        assert!(
            tracker
                .reserve_within("zero-budget".to_string(), 0)
                .is_none()
        );
        assert!(tracker.buckets.lock().is_empty());
    }

    #[test]
    fn expired_success_is_evicted_when_budget_is_read() {
        let tracker = PerSenderTracker::new();
        assert!(tracker.record_within("expired-sender", 1));
        {
            let buckets = tracker.buckets.lock();
            let action_tracker = buckets.get("expired-sender").expect("sender bucket");
            let mut state = action_tracker.state.lock();
            state.committed[0] = Instant::now()
                .checked_sub(ACTION_WINDOW + Duration::from_secs(1))
                .expect("test timestamp");
        }

        assert!(!tracker.is_exhausted("expired-sender", 1));
        assert!(!tracker.buckets.lock().contains_key("expired-sender"));
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
        assert!(!p.is_path_allowed(&tp_sys_sub("etc/shadow")));
        assert!(!p.is_path_allowed(&tp_sys_sub("root/.bashrc")));
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

    // ── is_resolved_path_readable: read-only allowlist + POSIX devs ──

    #[test]
    fn readable_includes_posix_device_files() {
        // /dev/null and friends are universally-readable system paths
        // operators expect to work for shell-idiom CLI tooling.
        let p = SecurityPolicy {
            workspace_dir: PathBuf::from("/tmp/zeroclaw-test-ws"),
            workspace_only: true,
            ..SecurityPolicy::default()
        };
        for device in ["/dev/null", "/dev/zero", "/dev/random", "/dev/urandom"] {
            assert!(
                p.is_resolved_path_readable(Path::new(device)),
                "POSIX device file {device} must be readable"
            );
        }
    }

    #[test]
    fn readable_includes_read_only_allowlist_paths() {
        let tmp = tempfile::tempdir().unwrap();
        let read_only_root = tmp.path().join("docs");
        std::fs::create_dir_all(&read_only_root).unwrap();
        let inside = read_only_root.join("guide.md");
        std::fs::write(&inside, "x").unwrap();

        let canonical_inside = inside.canonicalize().unwrap();
        let p = SecurityPolicy {
            workspace_dir: PathBuf::from("/tmp/elsewhere"),
            workspace_only: true,
            allowed_roots_read_only: vec![read_only_root.clone()],
            ..SecurityPolicy::default()
        };
        assert!(
            p.is_resolved_path_readable(&canonical_inside),
            "read-only allowlist entries must be readable"
        );
        // The same path is NOT writable (is_resolved_path_allowed is
        // strict-rw and does not consult allowed_roots_read_only).
        assert!(
            !p.is_resolved_path_allowed(&canonical_inside),
            "read-only allowlist entries must NOT be writable via is_resolved_path_allowed"
        );
    }

    // ── for_agent: workspace.access populates allowlist tiers ──

    #[test]
    fn for_agent_routes_workspace_access_into_correct_allowlist_tier() {
        use crate::multi_agent::{AccessMode, AgentAlias};
        use crate::schema::{AliasedAgentConfig, Config, RiskProfileConfig};

        let mut cfg = Config {
            data_dir: PathBuf::from("/tmp/zeroclaw-for-agent-test"),
            config_path: PathBuf::from("/tmp/zeroclaw-for-agent-test/config.toml"),
            ..Config::default()
        };
        cfg.risk_profiles.insert(
            "default".into(),
            RiskProfileConfig {
                workspace_only: true,
                ..RiskProfileConfig::default()
            },
        );

        cfg.agents.insert(
            "writable_sibling".into(),
            AliasedAgentConfig {
                risk_profile: "default".into(),
                ..AliasedAgentConfig::default()
            },
        );
        cfg.agents.insert(
            "readonly_sibling".into(),
            AliasedAgentConfig {
                risk_profile: "default".into(),
                ..AliasedAgentConfig::default()
            },
        );

        let mut test_agent = AliasedAgentConfig {
            risk_profile: "default".into(),
            ..AliasedAgentConfig::default()
        };
        test_agent
            .workspace
            .access
            .insert(AgentAlias::from("writable_sibling"), AccessMode::Write);
        test_agent
            .workspace
            .access
            .insert(AgentAlias::from("readonly_sibling"), AccessMode::Read);
        cfg.agents.insert("test_agent".into(), test_agent);

        let policy = SecurityPolicy::for_agent(&cfg, "test_agent").unwrap();

        let writable_sibling_dir = cfg.agent_workspace_dir("writable_sibling");
        let readonly_sibling_dir = cfg.agent_workspace_dir("readonly_sibling");

        assert!(
            policy
                .allowed_roots_write_only
                .contains(&writable_sibling_dir),
            "AccessMode::Write must land in allowed_roots_write_only; got {:?}",
            policy.allowed_roots_write_only
        );
        assert!(
            !policy.allowed_roots.contains(&writable_sibling_dir),
            "AccessMode::Write must NOT land in allowed_roots (read+write tier); got {:?}",
            policy.allowed_roots
        );
        assert!(
            policy
                .allowed_roots_read_only
                .contains(&readonly_sibling_dir),
            "AccessMode::Read must land in allowed_roots_read_only; got {:?}",
            policy.allowed_roots_read_only
        );
        assert!(
            !policy
                .allowed_roots_read_only
                .contains(&writable_sibling_dir),
            "Write-mode entry must NOT also appear on the read-only list"
        );
        assert!(
            !policy
                .allowed_roots_write_only
                .contains(&readonly_sibling_dir),
            "Read-mode entry must NOT also appear on the write-only list"
        );
        assert!(
            policy.workspace_only,
            "unrestricted_filesystem stays default-false → workspace_only stays true"
        );
    }

    #[test]
    fn write_only_root_blocks_reads_and_admits_writes() {
        // AccessMode::Write grants write access without read access.
        // is_resolved_path_allowed (write-side) must accept paths under
        // a write-only root; is_resolved_path_readable (read-side) must
        // refuse them.
        let mut policy = SecurityPolicy::default();
        let write_only_root =
            std::env::temp_dir().join(format!("zeroclaw_wo_root_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&write_only_root).unwrap();
        let canonical = write_only_root.canonicalize().unwrap();
        policy.allowed_roots_write_only.push(canonical.clone());
        policy.workspace_only = false;

        let target = canonical.join("write_only_target.txt");
        assert!(
            policy.is_resolved_path_allowed(&target),
            "write-only root must be writable via is_resolved_path_allowed"
        );
        assert!(
            !policy.is_resolved_path_readable(&target),
            "write-only root must NOT be readable via is_resolved_path_readable"
        );

        let _ = std::fs::remove_dir_all(canonical);
    }

    #[test]
    fn for_agent_creates_the_per_agent_workspace_dir() {
        use crate::schema::{AliasedAgentConfig, Config, RiskProfileConfig};

        let root =
            std::env::temp_dir().join(format!("zeroclaw-for-agent-mkdir-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let mut cfg = Config {
            data_dir: root.join("data"),
            config_path: root.join("config.toml"),
            ..Config::default()
        };
        cfg.risk_profiles
            .insert("default".into(), RiskProfileConfig::default());
        cfg.agents.insert(
            "agent_a".into(),
            AliasedAgentConfig {
                risk_profile: "default".into(),
                ..AliasedAgentConfig::default()
            },
        );

        let ws = cfg.agent_workspace_dir("agent_a");
        assert!(
            !ws.exists(),
            "precondition: workspace dir must not exist yet"
        );

        let policy = SecurityPolicy::for_agent(&cfg, "agent_a").unwrap();

        assert!(
            ws.exists(),
            "for_agent must create the per-agent workspace dir at the chokepoint"
        );
        assert_eq!(
            policy.workspace_dir, ws,
            "policy cwd/jail root must be the created workspace dir"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn for_agent_unrestricted_filesystem_disables_workspace_only() {
        use crate::schema::{AliasedAgentConfig, Config, RiskProfileConfig};

        let mut cfg = Config {
            data_dir: PathBuf::from("/tmp/zeroclaw-for-agent-unrestricted"),
            config_path: PathBuf::from("/tmp/zeroclaw-for-agent-unrestricted/config.toml"),
            ..Config::default()
        };
        cfg.risk_profiles.insert(
            "default".into(),
            RiskProfileConfig {
                workspace_only: true,
                ..RiskProfileConfig::default()
            },
        );
        let mut test_agent = AliasedAgentConfig {
            risk_profile: "default".into(),
            ..AliasedAgentConfig::default()
        };
        test_agent.workspace.unrestricted_filesystem = true;
        cfg.agents.insert("test_agent".into(), test_agent);

        let policy = SecurityPolicy::for_agent(&cfg, "test_agent").unwrap();

        assert!(
            !policy.workspace_only,
            "unrestricted_filesystem=true must flip workspace_only off at the policy level"
        );
    }

    // ── Edge cases: from_config preserves tracker ────────────

    #[test]
    fn from_config_creates_fresh_tracker() {
        let risk = crate::schema::RiskProfileConfig {
            level: AutonomyLevel::Full,
            workspace_only: false,
            allowed_commands: vec![],
            forbidden_paths: vec![],
            require_approval_for_medium_risk: true,
            block_high_risk_commands: true,
            ..crate::schema::RiskProfileConfig::default()
        };
        let runtime = crate::schema::RuntimeProfileConfig {
            max_actions_per_hour: 10,
            max_cost_per_day_cents: 100,
            ..crate::schema::RuntimeProfileConfig::default()
        };
        let workspace = PathBuf::from("/tmp/test");
        let policy = SecurityPolicy::from_profiles(&risk, Some(&runtime), &workspace);
        assert!(!policy.is_rate_limited());
    }

    // ── Checklist #3: Filesystem scoped (no /) ──────────────

    #[test]
    fn checklist_root_path_blocked() {
        let p = default_policy();
        assert!(!p.is_path_allowed(tp_sys()));
        assert!(!p.is_path_allowed(&tp_sys_sub("anything")));
    }

    #[test]
    fn checklist_all_system_dirs_blocked() {
        let p = SecurityPolicy {
            workspace_only: false,
            ..SecurityPolicy::default()
        };
        #[cfg(not(target_os = "windows"))]
        {
            for dir in ["/etc", "/root", "/proc", "/sys", "/dev", "/var", "/tmp"] {
                assert!(
                    p.forbidden_paths.iter().any(|f| f == dir),
                    "Default forbidden_paths must include {dir} on Unix"
                );
                assert!(
                    !p.is_path_allowed(dir),
                    "System dir should be blocked: {dir}"
                );
            }
        }
        #[cfg(target_os = "windows")]
        {
            for dir in [
                "C:\\Windows",
                "C:\\Windows\\System32",
                "C:\\Program Files",
                "C:\\ProgramData",
            ] {
                assert!(
                    p.forbidden_paths.iter().any(|f| f == dir),
                    "Default forbidden_paths must include {dir} on Windows"
                );
                assert!(
                    !p.is_path_allowed(dir),
                    "System dir should be blocked: {dir}"
                );
            }
        }
        for dot in &["~/.ssh", "~/.gnupg", "~/.aws"] {
            assert!(
                p.forbidden_paths.iter().any(|f| f == dot),
                "Default forbidden_paths must include {dot}"
            );
            assert!(
                !p.is_path_allowed(dot),
                "Sensitive dotfile dir should be blocked: {dot}"
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
        assert!(!p.is_path_allowed(&tp_sys_sub("any/absolute/path")));
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
        #[cfg(not(target_os = "windows"))]
        {
            for dir in ["/etc", "/root", "/proc", "/sys", "/dev", "/var", "/tmp"] {
                assert!(
                    p.forbidden_paths.iter().any(|f| f == dir),
                    "Default forbidden_paths must include {dir} on Unix"
                );
            }
        }
        #[cfg(target_os = "windows")]
        {
            for dir in [
                "C:\\Windows",
                "C:\\Windows\\System32",
                "C:\\Program Files",
                "C:\\ProgramData",
            ] {
                assert!(
                    p.forbidden_paths.iter().any(|f| f == dir),
                    "Default forbidden_paths must include {dir} on Windows"
                );
            }
        }
        for dot in &["~/.ssh", "~/.gnupg", "~/.aws", "~/.config"] {
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

    /// Regression for the shell workspace-boundary bypass: a direct path-shaped
    /// command argument that reaches outside the workspace through an
    /// in-workspace symlink must be blocked. The leaf may not exist yet (the
    /// command is about to create it), so resolution must follow the symlinked
    /// ancestor.
    #[cfg(unix)]
    #[test]
    fn forbidden_path_argument_blocks_symlink_escape() {
        use std::os::unix::fs::symlink;

        let root = std::env::temp_dir().join(format!(
            "zeroclaw_test_shell_symlink_escape_{}",
            std::process::id()
        ));
        let workspace = root.join("workspace");
        let outside = root.join("outside_target");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&outside).unwrap();

        // `link` inside the workspace points at the outside directory.
        symlink(&outside, workspace.join("link")).unwrap();

        let policy = SecurityPolicy {
            workspace_dir: workspace.clone(),
            ..SecurityPolicy::default()
        };

        // Writing a NEW file through the symlink (leaf does not exist yet).
        assert_eq!(
            policy
                .forbidden_workspace_path_argument("touch link/new.txt")
                .as_deref(),
            Some("link/new.txt"),
            "creating a file through an in-workspace symlink to outside must be blocked"
        );
        // Redirect target through the symlink.
        assert!(
            policy
                .forbidden_workspace_path_argument("echo hi > link/out.txt")
                .is_some(),
            "shell redirect through an escaping symlink must be blocked"
        );
        // Reading an existing file through the symlink.
        std::fs::write(outside.join("secret.txt"), b"x").unwrap();
        assert!(
            policy
                .forbidden_workspace_path_argument("cat link/secret.txt")
                .is_some(),
            "reading through an escaping symlink must be blocked"
        );
        assert_eq!(
            policy
                .forbidden_workspace_path_argument_for_shell(
                    r"cat link\secret.txt",
                    ShellDialect::PowerShell,
                )
                .as_deref(),
            Some(r"link\secret.txt"),
            "PowerShell backslash paths must retain workspace symlink resolution"
        );

        // A DANGLING symlink (its target directory does not exist yet) still
        // escapes on write, so it must be blocked even though it cannot be
        // `canonicalize`d.
        symlink(outside.join("nonexistent_dir"), workspace.join("dangling")).unwrap();
        assert!(
            policy
                .forbidden_workspace_path_argument("echo x > dangling/new.txt")
                .is_some(),
            "writing through a dangling symlink to outside must be blocked"
        );

        // A symlink CYCLE exhausts the resolver's hop budget. It must fail
        // CLOSED (block), not fall back to the pristine in-workspace path.
        symlink(workspace.join("cycle_b"), workspace.join("cycle_a")).unwrap();
        symlink(workspace.join("cycle_a"), workspace.join("cycle_b")).unwrap();
        assert!(
            policy
                .forbidden_workspace_path_argument("cat cycle_a/file")
                .is_some(),
            "an unresolvable symlink cycle must fail closed (be blocked)"
        );

        // Scoping check: the STRING-only guard (used by cron, whose cwd is NOT
        // the workspace) must NOT resolve symlinks, so it does not over-block a
        // workspace-relative path here. The resolve step is scoped to the
        // workspace-cwd variant used above.
        assert_eq!(
            policy.forbidden_path_argument("cat link/secret.txt"),
            None,
            "string-only guard must not resolve symlinks (keeps cron unaffected)"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// Non-regression: a symlink that stays INSIDE the workspace, and ordinary
    /// workspace-relative paths, must still be allowed after the resolve check.
    #[cfg(unix)]
    #[test]
    fn forbidden_path_argument_allows_in_workspace_paths() {
        use std::os::unix::fs::symlink;

        let root = std::env::temp_dir().join(format!(
            "zeroclaw_test_shell_in_workspace_{}",
            std::process::id()
        ));
        let workspace = root.join("workspace");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(workspace.join("real_dir")).unwrap();
        std::fs::create_dir_all(workspace.join("sub")).unwrap();

        // `inside` points at another directory WITHIN the workspace.
        symlink(workspace.join("real_dir"), workspace.join("inside")).unwrap();

        let policy = SecurityPolicy {
            workspace_dir: workspace.clone(),
            ..SecurityPolicy::default()
        };

        assert_eq!(
            policy.forbidden_workspace_path_argument("touch inside/new.txt"),
            None,
            "an in-workspace symlink target must remain allowed"
        );
        assert_eq!(
            policy.forbidden_workspace_path_argument("touch sub/out.txt"),
            None,
            "an ordinary workspace-relative path must remain allowed"
        );
        assert_eq!(
            policy.forbidden_workspace_path_argument("echo hi > sub/out.txt"),
            None,
            "an ordinary workspace-relative redirect target must remain allowed"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// Non-regression: a path under an `allowed_roots_read_only` grant (outside
    /// the workspace) must stay READABLE - the command guard checks read OR
    /// write, so the resolve step must not drop read-only roots.
    #[cfg(unix)]
    #[test]
    fn forbidden_path_argument_allows_read_only_root() {
        let root = std::env::temp_dir().join(format!(
            "zeroclaw_test_shell_read_only_root_{}",
            std::process::id()
        ));
        let workspace = root.join("workspace");
        let shared = root.join("shared_readonly");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&shared).unwrap();
        std::fs::write(shared.join("data.txt"), b"x").unwrap();

        let policy = SecurityPolicy {
            workspace_dir: workspace.clone(),
            allowed_roots_read_only: vec![shared.clone()],
            workspace_only: true,
            ..SecurityPolicy::default()
        };

        let cmd = format!("cat {}/data.txt", shared.display());
        assert_eq!(
            policy.forbidden_workspace_path_argument(&cmd),
            None,
            "reading under an allowed read-only root must remain allowed"
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
    fn resolve_tool_path_normalizes_windows_workspace_prefixed_relative_paths() {
        let workspace = PathBuf::from(r"C:\Users\me\.zeroclaw\agents\default\workspace");
        let p = SecurityPolicy {
            workspace_dir: workspace.clone(),
            ..SecurityPolicy::default()
        };
        let resolved =
            p.resolve_tool_path(r"Users\me\.zeroclaw\agents\default\workspace\nested\out.txt");

        assert_eq!(resolved, workspace.join("nested").join("out.txt"));
    }

    #[test]
    fn resolve_tool_path_does_not_normalize_mismatched_drive_prefixed_relative_paths() {
        let workspace = PathBuf::from(r"C:\Users\me\.zeroclaw\agents\default\workspace");
        let p = SecurityPolicy {
            workspace_dir: workspace.clone(),
            ..SecurityPolicy::default()
        };
        let resolved =
            p.resolve_tool_path(r"D:Users\me\.zeroclaw\agents\default\workspace\nested\out.txt");

        assert_ne!(resolved, workspace.join("nested").join("out.txt"));
    }

    #[test]
    fn is_under_allowed_root_matches_allowed_roots() {
        let p = SecurityPolicy {
            workspace_dir: tp_ws(),
            workspace_only: true,
            allowed_roots: vec![tp_proj(), tp_data()],
            ..SecurityPolicy::default()
        };
        assert!(p.is_under_allowed_root(&format!("{}/myapp/src/main.rs", tp_proj().display())));
        assert!(p.is_under_allowed_root(&format!("{}/file.csv", tp_data().display())));
        assert!(!p.is_under_allowed_root(&tp_sys_sub("etc/passwd")));
        assert!(!p.is_under_allowed_root("relative/path"));
    }

    #[test]
    fn is_under_allowed_root_returns_false_for_empty_roots() {
        let p = SecurityPolicy {
            workspace_dir: tp_ws(),
            workspace_only: true,
            allowed_roots: vec![],
            ..SecurityPolicy::default()
        };
        assert!(!p.is_under_allowed_root(&format!("{}/any/path", tp_proj().display())));
    }

    // ── SecurityPolicy read/read-write split ────────────────────────

    #[test]
    fn is_under_read_only_allowed_root_matches_only_read_only_list() {
        let p = SecurityPolicy {
            workspace_dir: tp_ws(),
            workspace_only: true,
            allowed_roots: vec![tp_rw()],
            allowed_roots_read_only: vec![tp_ro()],
            ..SecurityPolicy::default()
        };
        assert!(p.is_under_read_only_allowed_root(&format!("{}/notes.md", tp_ro().display())));
        assert!(!p.is_under_read_only_allowed_root(&format!("{}/file.csv", tp_rw().display())));
        assert!(!p.is_under_read_only_allowed_root(&tp_sys_sub("etc/passwd")));
        assert!(!p.is_under_read_only_allowed_root("relative"));
    }

    #[test]
    fn is_under_any_allowed_root_unions_read_only_and_read_write() {
        let p = SecurityPolicy {
            workspace_dir: tp_ws(),
            workspace_only: true,
            allowed_roots: vec![tp_rw()],
            allowed_roots_read_only: vec![tp_ro()],
            ..SecurityPolicy::default()
        };
        assert!(p.is_under_any_allowed_root(&format!("{}/file.csv", tp_rw().display())));
        assert!(p.is_under_any_allowed_root(&format!("{}/notes.md", tp_ro().display())));
        assert!(!p.is_under_any_allowed_root(&tp_sys_sub("etc/passwd")));
    }

    #[test]
    fn is_under_allowed_root_does_not_see_read_only_entries() {
        let p = SecurityPolicy {
            workspace_dir: tp_ws(),
            workspace_only: true,
            allowed_roots: vec![],
            allowed_roots_read_only: vec![tp_ro()],
            ..SecurityPolicy::default()
        };
        assert!(!p.is_under_allowed_root(&format!("{}/notes.md", tp_ro().display())));
        assert!(p.is_under_any_allowed_root(&format!("{}/notes.md", tp_ro().display())));
    }

    // ── SubAgent escalation validator ──────────────────────────────

    fn parent_policy_for_escalation_tests() -> SecurityPolicy {
        SecurityPolicy {
            workspace_dir: PathBuf::from("/workspace"),
            workspace_only: true,
            allowed_roots: vec![PathBuf::from("/projects"), PathBuf::from("/data")],
            allowed_roots_read_only: vec![PathBuf::from("/shared-docs")],
            allowed_commands: vec!["git".into(), "cargo".into(), "ls".into()],
            max_actions_per_hour: 100,
            max_cost_per_day_cents: 500,
            ..SecurityPolicy::default()
        }
    }

    #[test]
    fn ensure_no_escalation_accepts_identical_policy() {
        let parent = parent_policy_for_escalation_tests();
        let child = parent.clone();
        assert!(child.ensure_no_escalation_beyond(&parent).is_ok());
    }

    #[test]
    fn ensure_no_escalation_accepts_narrowed_child() {
        let parent = parent_policy_for_escalation_tests();
        let child = SecurityPolicy {
            allowed_roots: vec![PathBuf::from("/projects")],
            allowed_roots_read_only: vec![PathBuf::from("/shared-docs")],
            allowed_commands: vec!["git".into()],
            max_actions_per_hour: 50,
            max_cost_per_day_cents: 250,
            ..parent.clone()
        };
        assert!(child.ensure_no_escalation_beyond(&parent).is_ok());
    }

    #[test]
    fn ensure_no_escalation_accepts_case_equivalent_command_names() {
        let parent = SecurityPolicy {
            allowed_commands: vec!["Git".into(), "DOCKER".into()],
            ..parent_policy_for_escalation_tests()
        };
        let child = SecurityPolicy {
            allowed_commands: vec!["git".into(), "docker".into()],
            ..parent.clone()
        };

        assert!(child.ensure_no_escalation_beyond(&parent).is_ok());
    }

    #[test]
    fn ensure_no_escalation_keeps_command_paths_case_sensitive() {
        let parent = SecurityPolicy {
            allowed_commands: vec!["/usr/bin/Git".into()],
            ..parent_policy_for_escalation_tests()
        };
        let child = SecurityPolicy {
            allowed_commands: vec!["/usr/bin/git".into()],
            ..parent.clone()
        };

        let err = child
            .ensure_no_escalation_beyond(&parent)
            .expect_err("case-distinct paths must not be treated as equivalent");
        assert!(matches!(
            err,
            EscalationViolation::CommandNotInParent { ref command }
            if command == "/usr/bin/git"
        ));
    }

    #[test]
    fn ensure_no_escalation_accepts_rw_root_downgraded_to_read_only_on_child() {
        // A SubAgent giving up its write privilege is a narrowing,
        // not an escalation.
        let parent = parent_policy_for_escalation_tests();
        let child = SecurityPolicy {
            allowed_roots: Vec::new(),
            allowed_roots_read_only: vec![PathBuf::from("/projects")],
            ..parent.clone()
        };
        assert!(child.ensure_no_escalation_beyond(&parent).is_ok());
    }

    #[test]
    fn ensure_no_escalation_rejects_new_rw_root_not_in_parent() {
        let parent = parent_policy_for_escalation_tests();
        let child = SecurityPolicy {
            allowed_roots: vec![PathBuf::from("/projects"), PathBuf::from("/secrets")],
            ..parent.clone()
        };
        let err = child
            .ensure_no_escalation_beyond(&parent)
            .expect_err("new rw root must be rejected");
        assert!(matches!(
            err,
            EscalationViolation::ReadWriteRootNotInParent { ref path }
            if path == &PathBuf::from("/secrets")
        ));
    }

    #[test]
    fn ensure_no_escalation_rejects_new_read_only_root_not_in_parent() {
        let parent = parent_policy_for_escalation_tests();
        let child = SecurityPolicy {
            allowed_roots_read_only: vec![PathBuf::from("/etc")],
            ..parent.clone()
        };
        let err = child
            .ensure_no_escalation_beyond(&parent)
            .expect_err("new read-only root must be rejected");
        assert!(matches!(
            err,
            EscalationViolation::ReadOnlyRootNotInParent { ref path }
            if path == &PathBuf::from("/etc")
        ));
    }

    #[test]
    fn ensure_no_escalation_rejects_new_command_not_in_parent() {
        let parent = parent_policy_for_escalation_tests();
        let child = SecurityPolicy {
            allowed_commands: vec!["git".into(), "rm".into()],
            ..parent.clone()
        };
        let err = child
            .ensure_no_escalation_beyond(&parent)
            .expect_err("new command must be rejected");
        assert!(matches!(
            err,
            EscalationViolation::CommandNotInParent { ref command }
            if command == "rm"
        ));
    }

    #[test]
    fn ensure_no_escalation_rejects_workspace_only_disabled_by_child() {
        let parent = parent_policy_for_escalation_tests();
        let child = SecurityPolicy {
            workspace_only: false,
            ..parent.clone()
        };
        let err = child
            .ensure_no_escalation_beyond(&parent)
            .expect_err("disabling workspace_only when parent enforces it must be rejected");
        assert_eq!(err, EscalationViolation::WorkspaceOnlyDisabledByChild);
    }

    #[test]
    fn ensure_no_escalation_rejects_higher_max_actions() {
        let parent = parent_policy_for_escalation_tests();
        let child = SecurityPolicy {
            max_actions_per_hour: 200,
            ..parent.clone()
        };
        let err = child
            .ensure_no_escalation_beyond(&parent)
            .expect_err("higher max_actions_per_hour must be rejected");
        assert!(matches!(
            err,
            EscalationViolation::MaxActionsExceeded { child, parent } if child == 200 && parent == 100
        ));
    }

    #[test]
    fn ensure_no_escalation_rejects_higher_max_cost() {
        let parent = parent_policy_for_escalation_tests();
        let child = SecurityPolicy {
            max_cost_per_day_cents: 1000,
            ..parent.clone()
        };
        let err = child
            .ensure_no_escalation_beyond(&parent)
            .expect_err("higher max_cost_per_day_cents must be rejected");
        assert!(matches!(
            err,
            EscalationViolation::MaxCostExceeded { child, parent } if child == 1000 && parent == 500
        ));
    }

    #[test]
    fn ensure_no_escalation_rejects_higher_autonomy() {
        let parent = SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            ..parent_policy_for_escalation_tests()
        };
        let child = SecurityPolicy {
            autonomy: AutonomyLevel::Full,
            ..parent.clone()
        };
        let err = child
            .ensure_no_escalation_beyond(&parent)
            .expect_err("Full child under Supervised parent must be rejected");
        assert!(matches!(
            err,
            EscalationViolation::AutonomyAboveParent { child, parent }
            if child == AutonomyLevel::Full && parent == AutonomyLevel::Supervised
        ));
    }

    #[test]
    fn ensure_no_escalation_accepts_subpath_narrowing_inside_parent_root() {
        // Parent grants /projects rw; child narrows to /projects/repo —
        // a containment relation, not exact equality. Must accept.
        let parent = parent_policy_for_escalation_tests();
        let child = SecurityPolicy {
            allowed_roots: vec![PathBuf::from("/projects/repo")],
            allowed_roots_read_only: vec![],
            ..parent.clone()
        };
        assert!(child.ensure_no_escalation_beyond(&parent).is_ok());
    }

    #[test]
    fn ensure_no_escalation_rejects_dropped_forbidden_path() {
        let parent = SecurityPolicy {
            forbidden_paths: vec!["/etc/secrets".into(), "/root".into()],
            ..parent_policy_for_escalation_tests()
        };
        let child = SecurityPolicy {
            forbidden_paths: vec!["/root".into()],
            ..parent.clone()
        };
        let err = child
            .ensure_no_escalation_beyond(&parent)
            .expect_err("child dropping a parent's forbidden_paths entry must be rejected");
        assert!(matches!(
            err,
            EscalationViolation::ForbiddenPathDroppedByChild { ref path }
            if path == "/etc/secrets"
        ));
    }

    #[test]
    fn ensure_no_escalation_rejects_expanded_shell_env_passthrough() {
        let parent = SecurityPolicy {
            shell_env_passthrough: vec!["PATH".into()],
            ..parent_policy_for_escalation_tests()
        };
        let child = SecurityPolicy {
            shell_env_passthrough: vec!["PATH".into(), "AWS_SECRET_ACCESS_KEY".into()],
            ..parent.clone()
        };
        let err = child
            .ensure_no_escalation_beyond(&parent)
            .expect_err("child adding a shell_env_passthrough entry must be rejected");
        assert!(matches!(
            err,
            EscalationViolation::ShellEnvPassthroughExpanded { ref variable }
            if variable == "AWS_SECRET_ACCESS_KEY"
        ));
    }

    #[test]
    fn ensure_no_escalation_rejects_higher_shell_timeout() {
        let parent = SecurityPolicy {
            shell_timeout_secs: 30,
            ..parent_policy_for_escalation_tests()
        };
        let child = SecurityPolicy {
            shell_timeout_secs: 600,
            ..parent.clone()
        };
        let err = child
            .ensure_no_escalation_beyond(&parent)
            .expect_err("higher shell_timeout_secs must be rejected");
        assert!(matches!(
            err,
            EscalationViolation::ShellTimeoutExceeded { child, parent }
            if child == 600 && parent == 30
        ));
    }

    #[test]
    fn ensure_no_escalation_rejects_disabled_block_high_risk_commands() {
        let parent = SecurityPolicy {
            block_high_risk_commands: true,
            ..parent_policy_for_escalation_tests()
        };
        let child = SecurityPolicy {
            block_high_risk_commands: false,
            ..parent.clone()
        };
        let err = child
            .ensure_no_escalation_beyond(&parent)
            .expect_err("child flipping block_high_risk_commands off must be rejected");
        assert_eq!(
            err,
            EscalationViolation::BlockHighRiskCommandsDisabledByChild
        );
    }

    #[test]
    fn ensure_no_escalation_rejects_disabled_require_approval() {
        let parent = SecurityPolicy {
            require_approval_for_medium_risk: true,
            ..parent_policy_for_escalation_tests()
        };
        let child = SecurityPolicy {
            require_approval_for_medium_risk: false,
            ..parent.clone()
        };
        let err = child
            .ensure_no_escalation_beyond(&parent)
            .expect_err("child flipping require_approval_for_medium_risk off must be rejected");
        assert_eq!(err, EscalationViolation::RequireApprovalDisabledByChild);
    }

    #[test]
    fn from_risk_profile_leaves_allowed_roots_read_only_empty() {
        // RiskProfileConfig has no read-only-roots concept; it's
        // populated by the multi-agent runtime when it builds the
        // per-agent policy from workspace.access.
        let profile = crate::schema::RiskProfileConfig {
            allowed_roots: vec!["/projects".to_string()],
            ..crate::schema::RiskProfileConfig::default()
        };
        let policy = SecurityPolicy::from_risk_profile(&profile, Path::new("/workspace"));
        assert_eq!(policy.allowed_roots, vec![PathBuf::from("/projects")]);
        assert!(
            policy.allowed_roots_read_only.is_empty(),
            "read-only roots come from workspace.access, not RiskProfileConfig"
        );
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
        // The active_workspace.toml marker file was retired with the
        // [workspace] block; protection is no longer required and not
        // claimed.
        assert!(!policy.is_runtime_config_path(&config_dir.join("active_workspace.toml")));
    }

    #[test]
    fn runtime_state_files_in_config_dir_are_protected() {
        let workspace = PathBuf::from("/tmp/zeroclaw-profile/workspace");
        let policy = SecurityPolicy {
            workspace_dir: workspace.clone(),
            ..SecurityPolicy::default()
        };
        let config_dir = workspace.parent().unwrap();

        // The state files are protected when they live in a runtime config dir.
        assert!(policy.is_runtime_config_path(&config_dir.join("estop-state.json")));
        assert!(policy.is_runtime_config_path(&config_dir.join("webauthn_credentials.json")));
        assert!(policy.is_runtime_config_path(&config_dir.join("otp-secret")));

        // Same names inside the workspace itself are NOT protected — those
        // are user-owned files; only the runtime-state files at config_dir
        // are sensitive.
        assert!(!policy.is_runtime_config_path(&workspace.join("estop-state.json")));
        assert!(!policy.is_runtime_config_path(&workspace.join("webauthn_credentials.json")));
        assert!(!policy.is_runtime_config_path(&workspace.join("otp-secret")));

        // Unrelated filenames are unaffected.
        assert!(!policy.is_runtime_config_path(&config_dir.join("notes.md")));
        assert!(!policy.is_runtime_config_path(&config_dir.join("agent-state.json")));
        assert!(!policy.is_runtime_config_path(&config_dir.join("otp-state.json")));
    }

    #[test]
    fn runtime_state_files_in_data_dir_are_protected() {
        let workspace = PathBuf::from("/tmp/zeroclaw-profile/workspace");
        let data_dir = PathBuf::from("/tmp/zeroclaw-profile/data");
        let policy = SecurityPolicy {
            workspace_dir: workspace.clone(),
            data_dir: Some(data_dir.clone()),
            ..SecurityPolicy::default()
        };

        // The WebAuthn credentials file under data_dir is protected.
        assert!(policy.is_runtime_config_path(&data_dir.join("webauthn_credentials.json")));

        // The same file under the workspace is not — that's a user-owned
        // file, not a WebAuthn store.
        assert!(!policy.is_runtime_config_path(&workspace.join("webauthn_credentials.json")));

        // Unrelated files under data_dir are unaffected.
        assert!(!policy.is_runtime_config_path(&data_dir.join("logs.txt")));
        assert!(!policy.is_runtime_config_path(&data_dir.join("cache.bin")));

        // Without `data_dir` set, the predicate falls back to the
        // config-dir-only behavior (matches the legacy flat layout where
        // everything lives under one tree).
        let legacy_policy = SecurityPolicy {
            workspace_dir: workspace.clone(),
            ..SecurityPolicy::default()
        };
        assert!(!legacy_policy.is_runtime_config_path(&PathBuf::from(
            "/tmp/zeroclaw-profile/data/webauthn_credentials.json"
        )));
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

    #[test]
    fn is_runtime_config_path_protects_install_root_for_nested_agent_layout() {
        let install_root = PathBuf::from("/tmp/zeroclaw-install-nested");
        let workspace = install_root
            .join("agents")
            .join("agent-alpha")
            .join("workspace");
        let config_path = install_root.join("config.toml");

        let policy = SecurityPolicy {
            workspace_dir: workspace.clone(),
            config_path: Some(config_path.clone()),
            ..SecurityPolicy::default()
        };

        // The real config at the install root is now protected.
        assert!(policy.is_runtime_config_path(&config_path));
        assert!(policy.is_runtime_config_path(&install_root.join("config.toml.bak")));
        assert!(policy.is_runtime_config_path(&install_root.join(".config.toml.tmp-42")));

        // Legacy fallback (config_path = None) only guards
        // `workspace.parent()`, so it misses the nested install-root
        // config. This is exactly the gap the config_path field closes.
        let legacy = SecurityPolicy {
            workspace_dir: workspace.clone(),
            ..SecurityPolicy::default()
        };
        assert!(!legacy.is_runtime_config_path(&config_path));

        // A config.toml inside the agent workspace is still not a runtime
        // config path under either policy.
        assert!(!policy.is_runtime_config_path(&workspace.join("config.toml")));
        assert!(!legacy.is_runtime_config_path(&workspace.join("config.toml")));
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

    // ── Shell guard bypass with wildcard + unblocked ──────────

    #[test]
    fn wildcard_unblocked_allows_backticks() {
        let p = SecurityPolicy {
            allowed_commands: vec!["*".into()],
            block_high_risk_commands: false,
            ..SecurityPolicy::default()
        };
        assert!(p.is_command_allowed("echo `whoami`"));
        assert!(p.is_command_allowed("ls `which git`"));
    }

    #[test]
    fn wildcard_unblocked_allows_dollar_paren() {
        let p = SecurityPolicy {
            allowed_commands: vec!["*".into()],
            block_high_risk_commands: false,
            ..SecurityPolicy::default()
        };
        assert!(p.is_command_allowed("echo $(cat /etc/hostname)"));
        assert!(p.is_command_allowed("echo $(rm -rf /)"));
    }

    #[test]
    fn wildcard_unblocked_allows_dollar_brace() {
        let p = SecurityPolicy {
            allowed_commands: vec!["*".into()],
            block_high_risk_commands: false,
            ..SecurityPolicy::default()
        };
        assert!(p.is_command_allowed("echo ${HOME}"));
        assert!(p.is_command_allowed("echo ${PATH}"));
    }

    #[test]
    fn wildcard_unblocked_allows_process_substitution() {
        let p = SecurityPolicy {
            allowed_commands: vec!["*".into()],
            block_high_risk_commands: false,
            ..SecurityPolicy::default()
        };
        assert!(p.is_command_allowed("diff <(ls dir1) <(ls dir2)"));
        assert!(p.is_command_allowed("tee >(grep error > errors.log)"));
    }

    #[test]
    fn approved_read_root_returns_workspace_for_contained_paths() {
        let ws = tempfile::tempdir().unwrap();
        let ws_canon = ws.path().canonicalize().unwrap();
        let policy = SecurityPolicy {
            workspace_dir: ws.path().to_path_buf(),
            ..SecurityPolicy::default()
        };
        // A path inside the workspace binds to the canonical workspace root.
        assert_eq!(
            policy.approved_read_root(&ws_canon.join("sub").join("a.txt")),
            Some(ws_canon.clone())
        );
        // A path outside every allowlist has no bounded root.
        let outside = tempfile::tempdir().unwrap();
        let outside_canon = outside.path().canonicalize().unwrap();
        assert_eq!(policy.approved_read_root(&outside_canon.join("x")), None);
    }

    #[test]
    fn approved_read_root_honors_read_only_allowlist() {
        let ws = tempfile::tempdir().unwrap();
        let ro = tempfile::tempdir().unwrap();
        let ro_canon = ro.path().canonicalize().unwrap();
        let policy = SecurityPolicy {
            workspace_dir: ws.path().to_path_buf(),
            allowed_roots_read_only: vec![ro.path().to_path_buf()],
            ..SecurityPolicy::default()
        };
        assert_eq!(
            policy.approved_read_root(&ro_canon.join("doc.pdf")),
            Some(ro_canon.clone())
        );
    }

    #[test]
    fn wildcard_unblocked_allows_pipes_and_chains() {
        let p = SecurityPolicy {
            allowed_commands: vec!["*".into()],
            block_high_risk_commands: false,
            ..SecurityPolicy::default()
        };
        assert!(p.is_command_allowed("ps aux | grep python | wc -l"));
        assert!(p.is_command_allowed("echo hello && echo world"));
    }

    #[test]
    fn wildcard_blocked_still_runs_shell_guard() {
        // allowed_commands=["*"] but block_high_risk_commands=true (default)
        // — the shell expansion guard must still fire.
        let p = SecurityPolicy {
            allowed_commands: vec!["*".into()],
            block_high_risk_commands: true,
            ..SecurityPolicy::default()
        };
        assert!(!p.is_command_allowed("echo `whoami`"));
        assert!(!p.is_command_allowed("echo $(cat /etc/passwd)"));
        assert!(!p.is_command_allowed("echo ${HOME}"));
        assert!(!p.is_command_allowed("diff <(ls dir1) <(ls dir2)"));
    }

    #[test]
    fn specific_allowlist_still_runs_shell_guard() {
        // Non-wildcard allowlist — the guard must always run regardless
        // of block_high_risk_commands.
        let p = SecurityPolicy {
            allowed_commands: vec!["echo".into(), "ls".into(), "diff".into()],
            block_high_risk_commands: false,
            ..SecurityPolicy::default()
        };
        assert!(!p.is_command_allowed("echo `whoami`"));
        assert!(!p.is_command_allowed("echo $(cat /etc/passwd)"));
        assert!(!p.is_command_allowed("echo ${HOME}"));
        assert!(!p.is_command_allowed("diff <(ls dir1) <(ls dir2)"));
    }

    #[test]
    fn specific_allowlist_with_block_true_still_runs_shell_guard() {
        let p = SecurityPolicy {
            allowed_commands: vec!["echo".into(), "ls".into()],
            block_high_risk_commands: true,
            ..SecurityPolicy::default()
        };
        assert!(!p.is_command_allowed("echo `whoami`"));
        assert!(!p.is_command_allowed("echo $(rm -rf /)"));
        assert!(!p.is_command_allowed("echo ${HOME}"));
    }

    #[test]
    fn wildcard_unblocked_readonly_still_blocked() {
        // Even with wildcard + unblocked, ReadOnly trumps everything.
        let p = SecurityPolicy {
            autonomy: AutonomyLevel::ReadOnly,
            allowed_commands: vec!["*".into()],
            block_high_risk_commands: false,
            ..SecurityPolicy::default()
        };
        assert!(!p.is_command_allowed("ls"));
        assert!(!p.is_command_allowed("echo `whoami`"));
    }

    #[test]
    fn per_sender_tracker_isolates_counts() {
        let t = PerSenderTracker::new();
        // sender A rejects its third call without recording it
        assert!(t.record_within("chat_a", 2)); // count=1
        assert!(t.record_within("chat_a", 2)); // count=2
        assert!(!t.record_within("chat_a", 2)); // remains count=2
        // sender B is unaffected — its bucket is empty
        assert!(t.record_within("chat_b", 2)); // count=1
        assert!(t.record_within("chat_b", 2)); // count=2
        assert!(!t.record_within("chat_b", 2)); // remains count=2
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

    #[test]
    fn attached_short_option_value_handles_multibyte_token() {
        // A multibyte char immediately after the dash must not panic on a
        // byte-index slice. Regression for a char-boundary abort.
        assert_eq!(
            attached_short_option_value("-é/etc/passwd"),
            Some("/etc/passwd")
        );
        assert_eq!(attached_short_option_value("-—"), None);
        assert_eq!(
            attached_short_option_value("-f/etc/passwd"),
            Some("/etc/passwd")
        );
        assert_eq!(attached_short_option_value("-f"), None);
        assert_eq!(attached_short_option_value("--long"), None);
    }
}
