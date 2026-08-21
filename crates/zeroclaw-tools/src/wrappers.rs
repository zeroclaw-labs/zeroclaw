//! Generic tool wrappers for crosscutting concerns.

use async_trait::async_trait;
use std::sync::Arc;
use zeroclaw_api::attribution::{Attributable, Role};
use zeroclaw_api::tool::{Tool, ToolOutput, ToolResult};
use zeroclaw_config::policy::{SecurityPolicy, canonicalize_best_effort};

/// Type alias for a path-extraction closure used by [`PathGuardedTool`].
type PathExtractor = dyn Fn(&serde_json::Value) -> Option<String> + Send + Sync;

/// How [`PathGuardedTool`] should interpret and check the string it extracts
/// from a tool's arguments.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathAccessMode {
    /// The extracted string is a literal single path the inner tool reads.
    /// Resolved (workspace-relative join, best-effort canonicalize) and
    /// checked against the canonical `deny_read`/`allow_read` policy
    /// (`SecurityPolicy::is_resolved_path_readable`) before the inner tool
    /// runs — the same check the file tools apply at their own read
    /// boundary, so a `deny_read` entry holds even for tools that have no
    /// internal check of their own.
    Read,
    /// The extracted string is a literal single path the inner tool
    /// writes/mutates. Checked against the canonical `deny_write`/`allow_write`
    /// policy (`SecurityPolicy::is_resolved_path_allowed`).
    Write,
    /// The extracted string is not a literal path — a glob pattern or search
    /// query — so it cannot be resolved/canonicalized as one. Keeps the
    /// legacy string-level `is_path_allowed` pre-filter unchanged. Every
    /// tool registered with this mode performs its own precise canonical
    /// check per resolved match internally (`glob_search`, `content_search`),
    /// so this mode is a coarse pre-filter only, never the enforcement
    /// boundary.
    Legacy,
}

// ── RateLimitedTool ───────────────────────────────────────────────────────────

pub struct RateLimitedTool<T: Tool> {
    inner: T,
    security: Arc<SecurityPolicy>,
}

impl<T: Tool> RateLimitedTool<T> {
    pub fn new(inner: T, security: Arc<SecurityPolicy>) -> Self {
        Self { inner, security }
    }
}

impl<T: Tool> Attributable for RateLimitedTool<T> {
    fn role(&self) -> Role {
        self.inner.role()
    }
    fn alias(&self) -> &str {
        self.inner.alias()
    }
}

#[async_trait]
impl<T: Tool> Tool for RateLimitedTool<T> {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn description(&self) -> &str {
        self.inner.description()
    }

    fn parameters_schema(&self) -> serde_json::Value {
        self.inner.parameters_schema()
    }

    fn output_schema(&self) -> Option<serde_json::Value> {
        self.inner.output_schema()
    }

    fn param_domains(&self) -> Vec<(&'static str, zeroclaw_api::tool::OptionDomain)> {
        self.inner.param_domains()
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        if self.security.is_rate_limited() {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some("Rate limit exceeded: too many actions in the last hour".into()),
            });
        }

        let result = self.inner.execute(args).await?;

        if result.success && !self.security.record_action() {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some("Rate limit exceeded: action budget exhausted".into()),
            });
        }

        Ok(result)
    }
}

// ── PathGuardedTool ───────────────────────────────────────────────────────────

pub struct PathGuardedTool<T: Tool> {
    inner: T,
    security: Arc<SecurityPolicy>,
    mode: PathAccessMode,
    /// Optional override: extract a path string from the args JSON.
    extractor: Option<Box<PathExtractor>>,
}

impl<T: Tool> PathGuardedTool<T> {
    /// `mode` must name how the extracted string should be checked — every
    /// call site declares it explicitly (see [`PathAccessMode`]) rather than
    /// defaulting to the legacy string-level check, so adding a new wrapped
    /// tool forces a conscious choice instead of a silent bypass.
    pub fn new(inner: T, security: Arc<SecurityPolicy>, mode: PathAccessMode) -> Self {
        Self {
            inner,
            security,
            mode,
            extractor: None,
        }
    }

    /// Supply a custom path-extraction closure for tools with non-standard arg names.
    pub fn with_extractor<F>(mut self, f: F) -> Self
    where
        F: Fn(&serde_json::Value) -> Option<String> + Send + Sync + 'static,
    {
        self.extractor = Some(Box::new(f));
        self
    }

    fn extract_path_string(&self, args: &serde_json::Value) -> Option<String> {
        if let Some(ref f) = self.extractor {
            return f(args);
        }
        // Default: check common argument names used across ZeroClaw tools.
        for field in &["path", "command", "pattern", "query", "file"] {
            if let Some(s) = args.get(field).and_then(|v| v.as_str()) {
                return Some(s.to_string());
            }
        }
        None
    }

    /// Resolve `raw` (workspace-relative join, best-effort canonicalize —
    /// tolerant of a not-yet-existing write target, matching the pattern
    /// `file_write`/`file_edit` already use at their own boundary) and check
    /// it against the canonical policy for `self.mode`. Returns `Some(raw)`
    /// when the path is denied.
    async fn check_canonical(&self, raw: &str) -> Option<String> {
        if raw.contains('\0') {
            return Some(raw.to_string());
        }
        let candidate = self.security.resolve_tool_path(raw);
        let resolved = match tokio::fs::canonicalize(&candidate).await {
            Ok(p) => p,
            Err(_) => canonicalize_best_effort(&candidate),
        };
        let allowed = match self.mode {
            PathAccessMode::Read => self.security.is_resolved_path_readable(&resolved),
            PathAccessMode::Write => self.security.is_resolved_path_allowed(&resolved),
            PathAccessMode::Legacy => unreachable!("check_canonical is not called in Legacy mode"),
        };
        if allowed { None } else { Some(raw.to_string()) }
    }
}

impl<T: Tool> Attributable for PathGuardedTool<T> {
    fn role(&self) -> Role {
        self.inner.role()
    }
    fn alias(&self) -> &str {
        self.inner.alias()
    }
}

#[async_trait]
impl<T: Tool> Tool for PathGuardedTool<T> {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn description(&self) -> &str {
        self.inner.description()
    }

    fn parameters_schema(&self) -> serde_json::Value {
        self.inner.parameters_schema()
    }

    fn output_schema(&self) -> Option<serde_json::Value> {
        self.inner.output_schema()
    }

    fn param_domains(&self) -> Vec<(&'static str, zeroclaw_api::tool::OptionDomain)> {
        self.inner.param_domains()
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        if let Some(arg) = self.extract_path_string(&args) {
            // For shell command arguments, use the full token-aware scanner.
            // For a literal path in Read/Write mode, resolve and apply the
            // canonical deny_read/deny_write policy — the same check the
            // wrapped tool would apply at its own boundary, so a tool with no
            // internal check of its own is still covered. Legacy mode (glob
            // patterns, search queries) keeps the string-level pre-filter,
            // since those strings can't be resolved/canonicalized as a path.
            let blocked = if self.extractor.is_none()
                && args.get("command").and_then(|v| v.as_str()).is_some()
            {
                self.security.forbidden_workspace_path_argument(&arg)
            } else if self.mode == PathAccessMode::Legacy {
                if !self.security.is_path_allowed(&arg) {
                    Some(arg.clone())
                } else {
                    None
                }
            } else {
                self.check_canonical(&arg).await
            };

            if let Some(path) = blocked {
                return Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some(format!("Path blocked by security policy: {path}")),
                });
            }
        }

        self.inner.execute(args).await
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use zeroclaw_config::autonomy::AutonomyLevel;
    use zeroclaw_config::policy::SecurityPolicy;

    zeroclaw_api::mock_tool_attribution!(CountingTool);

    // ── Helpers ───────────────────────────────────────────────────────────────

    fn policy(autonomy: AutonomyLevel) -> Arc<SecurityPolicy> {
        Arc::new(SecurityPolicy {
            autonomy,
            workspace_dir: std::env::temp_dir(),
            ..SecurityPolicy::default()
        })
    }

    #[cfg(target_os = "windows")]
    fn absolute_path_outside_workspace() -> &'static str {
        r"C:\Windows\win.ini"
    }

    #[cfg(not(target_os = "windows"))]
    fn absolute_path_outside_workspace() -> &'static str {
        "/etc/passwd"
    }

    /// A minimal tool that records how many times `execute` was called.
    struct CountingTool {
        calls: Arc<AtomicUsize>,
    }

    impl CountingTool {
        fn new() -> (Self, Arc<AtomicUsize>) {
            let counter = Arc::new(AtomicUsize::new(0));
            (
                CountingTool {
                    calls: counter.clone(),
                },
                counter,
            )
        }
    }

    #[async_trait]
    impl Tool for CountingTool {
        fn name(&self) -> &str {
            "counting"
        }
        fn description(&self) -> &str {
            "counts calls"
        }
        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({})
        }
        async fn execute(&self, _args: serde_json::Value) -> anyhow::Result<ToolResult> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(ToolResult {
                success: true,
                output: "ok".into(),
                error: None,
            })
        }
    }

    // ── RateLimitedTool tests ─────────────────────────────────────────────────

    #[tokio::test]
    async fn rate_limited_allows_call_within_budget() {
        let (inner, counter) = CountingTool::new();
        let tool = RateLimitedTool::new(inner, policy(AutonomyLevel::Full));
        let result = tool
            .execute(serde_json::json!({}))
            .await
            .expect("should succeed");
        assert!(result.success);
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn rate_limited_delegates_name_and_schema() {
        let (inner, _) = CountingTool::new();
        let tool = RateLimitedTool::new(inner, policy(AutonomyLevel::Full));
        assert_eq!(tool.name(), "counting");
        assert_eq!(tool.description(), "counts calls");
        assert!(tool.parameters_schema().is_object());
    }

    #[tokio::test]
    async fn rate_limited_blocks_when_exhausted() {
        // Use a policy with a tiny action budget (1 action per window).
        let sec = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Full,
            workspace_dir: std::env::temp_dir(),
            max_actions_per_hour: 1,
            ..SecurityPolicy::default()
        });
        let (inner, counter) = CountingTool::new();
        let tool = RateLimitedTool::new(inner, sec);

        let r1 = tool.execute(serde_json::json!({})).await.unwrap();
        assert!(r1.success, "first call should succeed");

        let r2 = tool.execute(serde_json::json!({})).await.unwrap();
        assert!(!r2.success, "second call should be rate-limited");
        assert!(r2.error.unwrap().contains("Rate limit exceeded"));
        // Inner tool must NOT have been called on the blocked attempt.
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    // ── PathGuardedTool tests ─────────────────────────────────────────────────

    #[tokio::test]
    async fn path_guard_allows_safe_path() {
        let (inner, counter) = CountingTool::new();
        let tool = PathGuardedTool::new(inner, policy(AutonomyLevel::Full), PathAccessMode::Read);
        let result = tool
            .execute(serde_json::json!({"path": "src/main.rs"}))
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn path_guard_blocks_forbidden_path() {
        let (inner, counter) = CountingTool::new();
        let tool = PathGuardedTool::new(inner, policy(AutonomyLevel::Full), PathAccessMode::Read);
        let result = tool
            .execute(serde_json::json!({"command": format!("cat {}", absolute_path_outside_workspace())}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("Path blocked"));
        assert_eq!(
            counter.load(Ordering::SeqCst),
            0,
            "inner must not be called"
        );
    }

    #[tokio::test]
    async fn path_guard_no_path_arg_passes_through() {
        let (inner, counter) = CountingTool::new();
        let tool = PathGuardedTool::new(inner, policy(AutonomyLevel::Full), PathAccessMode::Read);
        // No recognised path field — wrapper must not block.
        let result = tool
            .execute(serde_json::json!({"value": "hello"}))
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn path_guard_custom_extractor() {
        let (inner, counter) = CountingTool::new();
        let tool = PathGuardedTool::new(inner, policy(AutonomyLevel::Full), PathAccessMode::Read)
            .with_extractor(|args| {
                args.get("target")
                    .and_then(|v| v.as_str())
                    .map(String::from)
            });
        let result = tool
            .execute(serde_json::json!({"target": absolute_path_outside_workspace()}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("Path blocked"));
        assert_eq!(counter.load(Ordering::SeqCst), 0);
    }

    // ── Canonical read/write enforcement (defense-in-depth boundary) ──────────
    //
    // `CountingTool` has no internal path check of its own, so these isolate
    // the wrapper's own decision — every currently-registered real tool
    // (file_read, file_write, ...) additionally re-checks internally, which
    // would mask a regression here if tested only end-to-end through those
    // tools.

    #[tokio::test]
    async fn path_guard_read_mode_denies_absolute_workspace_deny_read_target() {
        // The legacy `is_path_allowed` bug this mode replaces only manifests
        // for an ABSOLUTE in-workspace path: its `expanded_path.is_absolute()`
        // branch returns `true` for any in-workspace path before the
        // forbidden_paths/deny_read check ever runs. A relative arg does not
        // hit that branch, so the absolute form is the one that actually
        // exercises the defect.
        let tmp = tempfile::TempDir::new().unwrap();
        let sec = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Full,
            workspace_dir: tmp.path().to_path_buf(),
            forbidden_paths: vec!["secret.txt".to_string()],
            ..SecurityPolicy::default()
        });
        let (inner, counter) = CountingTool::new();
        let tool = PathGuardedTool::new(inner, sec, PathAccessMode::Read);

        let absolute = tmp.path().join("secret.txt");
        let result = tool
            .execute(serde_json::json!({"path": absolute.to_str().unwrap()}))
            .await
            .unwrap();

        assert!(
            !result.success,
            "an absolute in-workspace deny_read target must be refused by the wrapper itself"
        );
        assert_eq!(
            counter.load(Ordering::SeqCst),
            0,
            "inner must not be called when the wrapper denies the read"
        );
    }

    #[tokio::test]
    async fn path_guard_read_mode_allows_unrelated_absolute_workspace_target() {
        let tmp = tempfile::TempDir::new().unwrap();
        let sec = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Full,
            workspace_dir: tmp.path().to_path_buf(),
            forbidden_paths: vec!["secret.txt".to_string()],
            ..SecurityPolicy::default()
        });
        let (inner, counter) = CountingTool::new();
        let tool = PathGuardedTool::new(inner, sec, PathAccessMode::Read);

        let absolute = tmp.path().join("public.txt");
        let result = tool
            .execute(serde_json::json!({"path": absolute.to_str().unwrap()}))
            .await
            .unwrap();

        assert!(
            result.success,
            "an unrelated target must pass: {:?}",
            result.error
        );
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn path_guard_write_mode_denies_deny_write_target() {
        let tmp = tempfile::TempDir::new().unwrap();
        let denied = tmp.path().join("protected.txt");
        let sec = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Full,
            workspace_dir: tmp.path().to_path_buf(),
            deny_write: vec![denied],
            ..SecurityPolicy::default()
        });
        let (inner, counter) = CountingTool::new();
        let tool = PathGuardedTool::new(inner, sec, PathAccessMode::Write);

        let result = tool
            .execute(serde_json::json!({"path": "protected.txt"}))
            .await
            .unwrap();

        assert!(
            !result.success,
            "a deny_write target must be refused by the wrapper itself"
        );
        assert_eq!(
            counter.load(Ordering::SeqCst),
            0,
            "inner must not be called when the wrapper denies the write"
        );
    }

    #[tokio::test]
    async fn path_guard_write_mode_allows_unrelated_workspace_target() {
        let tmp = tempfile::TempDir::new().unwrap();
        let denied = tmp.path().join("protected.txt");
        let sec = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Full,
            workspace_dir: tmp.path().to_path_buf(),
            deny_write: vec![denied],
            ..SecurityPolicy::default()
        });
        let (inner, counter) = CountingTool::new();
        let tool = PathGuardedTool::new(inner, sec, PathAccessMode::Write);

        let result = tool
            .execute(serde_json::json!({"path": "new_file.txt"}))
            .await
            .unwrap();

        assert!(
            result.success,
            "an unrelated target must pass: {:?}",
            result.error
        );
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    // ── Composition test ──────────────────────────────────────────────────────

    #[tokio::test]
    async fn composed_wrappers_both_enforce() {
        // RateLimited(PathGuarded(CountingTool)) — path check happens inside
        // the rate-limit window, so a forbidden path must still be blocked
        // (and not consume a rate-limit slot).
        let sec = policy(AutonomyLevel::Full);
        let (inner, counter) = CountingTool::new();
        let tool = RateLimitedTool::new(
            PathGuardedTool::new(inner, sec.clone(), PathAccessMode::Read),
            sec,
        );

        let blocked = tool
            .execute(serde_json::json!({"path": absolute_path_outside_workspace()}))
            .await
            .unwrap();
        assert!(!blocked.success);
        assert_eq!(counter.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn rate_limited_does_not_consume_budget_on_failure() {
        // Inner tool that always reports failure (e.g. validation error).
        // record_action() must NOT fire, so the budget stays at full and
        // a subsequent successful call still goes through.
        struct AlwaysFails;
        impl ::zeroclaw_api::attribution::Attributable for AlwaysFails {
            fn role(&self) -> ::zeroclaw_api::attribution::Role {
                ::zeroclaw_api::attribution::Role::Tool(
                    ::zeroclaw_api::attribution::ToolKind::Plugin,
                )
            }
            fn alias(&self) -> &str {
                <Self as Tool>::name(self)
            }
        }
        #[async_trait]
        impl Tool for AlwaysFails {
            fn name(&self) -> &str {
                "always_fails"
            }
            fn description(&self) -> &str {
                ""
            }
            fn parameters_schema(&self) -> serde_json::Value {
                serde_json::json!({})
            }
            async fn execute(&self, _args: serde_json::Value) -> anyhow::Result<ToolResult> {
                Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some("validation failed".into()),
                })
            }
        }

        let sec = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Full,
            workspace_dir: std::env::temp_dir(),
            max_actions_per_hour: 1,
            ..SecurityPolicy::default()
        });
        let failing = RateLimitedTool::new(AlwaysFails, sec.clone());

        // Three failed calls — none should consume the single-slot budget.
        for _ in 0..3 {
            let r = failing.execute(serde_json::json!({})).await.unwrap();
            assert!(!r.success);
            assert!(r.error.unwrap().contains("validation failed"));
        }

        // Now a fresh successful tool wrapped against the same policy must
        // still have its slot available.
        let (success_inner, counter) = CountingTool::new();
        let succeeding = RateLimitedTool::new(success_inner, sec);
        let r = succeeding.execute(serde_json::json!({})).await.unwrap();
        assert!(r.success);
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn composed_wrappers_path_block_preserves_budget() {
        // RateLimited(PathGuarded(CountingTool)) — PathGuard blocks the call,
        // budget must NOT be consumed, so a subsequent allowed call still runs.
        let sec = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Full,
            workspace_dir: std::env::temp_dir(),
            max_actions_per_hour: 1,
            ..SecurityPolicy::default()
        });
        let (inner, counter) = CountingTool::new();
        let tool = RateLimitedTool::new(
            PathGuardedTool::new(inner, sec.clone(), PathAccessMode::Read),
            sec,
        );

        let blocked = tool
            .execute(serde_json::json!({"path": absolute_path_outside_workspace()}))
            .await
            .unwrap();
        assert!(!blocked.success);
        assert_eq!(counter.load(Ordering::SeqCst), 0);

        // Budget intact: an allowed call should still pass.
        let allowed = tool
            .execute(serde_json::json!({"path": "src/main.rs"}))
            .await
            .unwrap();
        assert!(allowed.success, "budget should still have a slot");
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }
}
