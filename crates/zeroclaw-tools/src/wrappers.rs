//! Generic tool wrappers for crosscutting concerns.
//!
//! Each wrapper implements [`Tool`] by delegating to an inner tool while
//! applying one crosscutting concern around the `execute` call.  Wrappers
//! compose: stack them at construction time in `tools/mod.rs` rather than
//! repeating the same guard blocks inside every tool's `execute` method.
//!
//! # Composition order (outermost first)
//!
//! ```text
//! RateLimitedTool
//!   └─ PathGuardedTool
//!        └─ <concrete tool>
//! ```
//!
//! # Example
//!
//! ```rust,ignore
//! let tool = RateLimitedTool::new(
//!     PathGuardedTool::new(ShellTool::new(security.clone(), runtime), security.clone()),
//!     security.clone(),
//! );
//! ```

use async_trait::async_trait;
use std::sync::Arc;
use zeroclaw_api::tool::{Tool, ToolResult};
use zeroclaw_config::policy::SecurityPolicy;

/// Type alias for a path-extraction closure used by [`PathGuardedTool`].
type PathExtractor = dyn Fn(&serde_json::Value) -> Option<String> + Send + Sync;

// ── RateLimitedTool ───────────────────────────────────────────────────────────

/// Wraps any [`Tool`] and enforces the [`SecurityPolicy`] rate limit.
///
/// Replaces the repeated `is_rate_limited()` / `record_action()` guard blocks
/// previously inlined in every tool's `execute` method (~30 files, ~50 call
/// sites).
///
/// # Budget semantics
///
/// `record_action()` runs **after** the inner tool returns and only when
/// `ToolResult.success == true`.  This matches the pre-wrapper behaviour: only
/// calls that actually performed work consumed the action budget.  Validation,
/// policy, path-allowlist, read-only, and command-validation failures all
/// surface as `success: false` from the inner tool (or inner wrapper) and do
/// not consume a slot.
///
/// ## Read-tool exception (anti-probing)
///
/// `FileReadTool` (`zeroclaw-runtime::tools::file_read`) and `PdfReadTool` in
/// this crate intentionally call `record_action()` *themselves* on the
/// post-`PathGuardedTool` `resolve_candidate` / `canonicalize` failure paths.
/// This prevents an attacker from probing path existence for free: each
/// attempt — successful or failed — consumes exactly one slot.  The outer
/// `RateLimitedTool` only records on `success: true`, so the totals stay at
/// one slot per attempt.  When introducing a new read-style tool, follow the
/// same pattern.
pub struct RateLimitedTool<T: Tool> {
    inner: T,
    security: Arc<SecurityPolicy>,
}

impl<T: Tool> RateLimitedTool<T> {
    pub fn new(inner: T, security: Arc<SecurityPolicy>) -> Self {
        Self { inner, security }
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

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        if self.security.is_rate_limited() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Rate limit exceeded: too many actions in the last hour".into()),
            });
        }

        // Delegate first; only record against the budget when the inner tool
        // actually performed work (ToolResult.success == true).  This preserves
        // the pre-wrapper semantics where validation/policy failures (forbidden
        // paths, malformed args, disabled config, read-only blocks, command
        // validation) did not consume the action budget.
        let result = self.inner.execute(args).await?;

        if result.success && !self.security.record_action() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Rate limit exceeded: action budget exhausted".into()),
            });
        }

        Ok(result)
    }
}

// ── PathGuardedTool ───────────────────────────────────────────────────────────

/// Wraps any [`Tool`] and blocks calls whose arguments contain a forbidden path.
///
/// Replaces the `forbidden_path_argument()` guard blocks previously inlined in
/// tools that accept a path-like argument (`shell`, `file_read`, `file_write`,
/// `file_edit`, `pdf_read`, `content_search`, `glob_search`, `image_info`).
///
/// Path extraction is argument-name-driven: the wrapper inspects the `"path"`,
/// `"command"`, `"pattern"`, and `"query"` fields of the JSON argument object.
/// Tools whose path argument uses a different field name can pass a custom
/// extractor at construction via [`PathGuardedTool::with_extractor`].
pub struct PathGuardedTool<T: Tool> {
    inner: T,
    security: Arc<SecurityPolicy>,
    /// Optional override: extract a path string from the args JSON.
    extractor: Option<Box<PathExtractor>>,
}

impl<T: Tool> PathGuardedTool<T> {
    pub fn new(inner: T, security: Arc<SecurityPolicy>) -> Self {
        Self {
            inner,
            security,
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

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        if let Some(arg) = self.extract_path_string(&args) {
            // For shell command arguments, use the full token-aware scanner.
            // For plain path values (e.g. "path" or custom extractor), fall back
            // to the direct path check.
            let blocked = if self.extractor.is_none()
                && args.get("command").and_then(|v| v.as_str()).is_some()
            {
                self.security.forbidden_path_argument(&arg)
            } else if !self.security.is_path_allowed(&arg) {
                Some(arg.clone())
            } else {
                None
            };

            if let Some(path) = blocked {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
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

    // ── Helpers ───────────────────────────────────────────────────────────────

    fn policy(autonomy: AutonomyLevel) -> Arc<SecurityPolicy> {
        Arc::new(SecurityPolicy {
            autonomy,
            workspace_dir: std::env::temp_dir(),
            ..SecurityPolicy::default()
        })
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
        let tool = PathGuardedTool::new(inner, policy(AutonomyLevel::Full));
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
        let tool = PathGuardedTool::new(inner, policy(AutonomyLevel::Full));
        let result = tool
            .execute(serde_json::json!({"command": "cat /etc/passwd"}))
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
        let tool = PathGuardedTool::new(inner, policy(AutonomyLevel::Full));
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
        let tool =
            PathGuardedTool::new(inner, policy(AutonomyLevel::Full)).with_extractor(|args| {
                args.get("target")
                    .and_then(|v| v.as_str())
                    .map(String::from)
            });
        let result = tool
            .execute(serde_json::json!({"target": "/etc/shadow"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("Path blocked"));
        assert_eq!(counter.load(Ordering::SeqCst), 0);
    }

    // ── Composition test ──────────────────────────────────────────────────────

    #[tokio::test]
    async fn composed_wrappers_both_enforce() {
        // RateLimited(PathGuarded(CountingTool)) — path check happens inside
        // the rate-limit window, so a forbidden path must still be blocked
        // (and not consume a rate-limit slot).
        let sec = policy(AutonomyLevel::Full);
        let (inner, counter) = CountingTool::new();
        let tool = RateLimitedTool::new(PathGuardedTool::new(inner, sec.clone()), sec);

        let blocked = tool
            .execute(serde_json::json!({"path": "/etc/passwd"}))
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
                    output: String::new(),
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
        let tool = RateLimitedTool::new(PathGuardedTool::new(inner, sec.clone()), sec);

        let blocked = tool
            .execute(serde_json::json!({"path": "/etc/passwd"}))
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
