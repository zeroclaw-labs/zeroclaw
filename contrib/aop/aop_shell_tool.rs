//! Example: Aspect-Oriented refactoring of ShellTool crosscutting concerns.
//!
//! This example demonstrates how the crosscutting concerns scattered across
//! `src/tools/shell.rs` can be extracted into reusable aspect-rs aspects,
//! reducing tangling from ~42% to ~0% while preserving identical behavior.
//!
//! ## The problem (from zeroclaw-analysis-report.md)
//!
//! `shell.rs` (662 lines) contains 3 interleaved concerns:
//!
//! 1. **Rate limiting** (lines 98–130): Two separate `is_rate_limited()` +
//!    `record_action()` checks duplicated across 35 tool files
//! 2. **Path validation** (lines 131–145): `forbidden_path_argument()` check
//!    duplicated in every file-touching tool
//! 3. **Audit logging** (lines 146–162): Manual `AuditLogger` calls scattered
//!    across 12 tool files
//!
//! ## The solution
//!
//! Replace the inline checks with declarative aspects applied at call sites:
//!
//! ```rust,ignore
//! // Before: shell.rs execute() has ~120 LOC of crosscutting code
//! // After: execute() has ~8 LOC of pure business logic
//! #[aspect(rate_limiter.clone())]
//! #[aspect(scope_guard.clone())]
//! #[aspect(audit.clone())]
//! async fn execute_inner(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
//!     // Only business logic here
//!     self.runtime.run_command(&cmd, SHELL_TIMEOUT_SECS, MAX_OUTPUT_BYTES).await
//! }
//! ```
//!
//! ## References
//!
//! - aspect-rs: <https://github.com/yijunyu/aspect-rs-priv>
//! - RE2026 paper: "Aspect-Oriented Patterns for AI Agent Systems"
//! - Crosscutting concern analysis: `zeroclaw-analysis-report.md`

use anyhow::Result;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};
use std::time::{Duration, Instant};

// ── Minimal Tool trait (mirrors src/tools/traits.rs) ─────────────────────────

#[derive(Debug, Clone)]
pub struct ToolResult {
    pub success: bool,
    pub output: String,
    pub error: Option<String>,
}

impl ToolResult {
    fn ok(output: impl Into<String>) -> Self {
        Self { success: true, output: output.into(), error: None }
    }
    fn err(msg: impl Into<String>) -> Self {
        Self { success: false, output: String::new(), error: Some(msg.into()) }
    }
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters_schema(&self) -> Value;
    async fn execute(&self, args: Value) -> Result<ToolResult>;
}

// ── Aspect interfaces (subset of aspect-rs API) ───────────────────────────────

/// Simulates aspect_core::Aspect::before() for sync pre-call checks.
/// In production, use: `use aspect_core::prelude::*;`
trait BeforeAspect: Send + Sync {
    /// Returns `Ok(())` if execution should proceed, `Err(msg)` to block.
    fn before(&self, tool_name: &str) -> Result<(), String>;
}

/// Simulates aspect_core::Aspect::after() for post-call bookkeeping.
trait AfterAspect: Send + Sync {
    fn after(&self, tool_name: &str, success: bool);
}

// ── RateLimitAspect (mirrors aspect-std::RateLimitAspect) ────────────────────
//
// Replaces the two inline checks in shell.rs:
//   - Line  98: `if self.security.is_rate_limited() { return err }`
//   - Line 125: `if !self.security.record_action() { return err }`
//
// Scattering in ZeroClaw v3: rate-limiting concern appears in 28/192 files (14.6%).
// With this aspect, it is declared once and applied via macro.

pub struct RateLimitAspect {
    max_per_hour: u64,
    /// Sliding-window timestamps (epoch seconds). Mirrors SecurityPolicy::ActionTracker.
    timestamps: Arc<parking_lot::Mutex<Vec<Instant>>>,
}

impl RateLimitAspect {
    pub fn new(max_per_hour: u64) -> Self {
        Self {
            max_per_hour,
            timestamps: Arc::new(parking_lot::Mutex::new(Vec::new())),
        }
    }
}

impl BeforeAspect for RateLimitAspect {
    fn before(&self, _tool_name: &str) -> Result<(), String> {
        let mut ts = self.timestamps.lock();
        let now = Instant::now();
        // Prune entries older than 1 hour (mirrors ActionTracker)
        let window = Duration::from_secs(3600);
        ts.retain(|t| now.duration_since(*t) < window);
        if ts.len() as u64 >= self.max_per_hour {
            return Err("Rate limit exceeded: too many actions in the last hour".into());
        }
        ts.push(now);
        Ok(())
    }
}

// ── ToolScopeAspect (mirrors aspect-agent::ToolScopeAspect) ──────────────────
//
// Replaces the inline `forbidden_path_argument()` call in shell.rs line 131.
// Scattering: path-validation concern appears in 65/192 files (33.9%).

pub struct ToolScopeAspect {
    forbidden_prefixes: Vec<String>,
}

impl ToolScopeAspect {
    /// Default forbidden paths — mirrors SecurityPolicy::forbidden_paths.
    pub fn default_policy() -> Self {
        Self {
            forbidden_prefixes: vec![
                "/etc".into(),
                "/root".into(),
                "/proc".into(),
                "/sys".into(),
                "/dev".into(),
                "/boot".into(),
                "~/.ssh".into(),
                "~/.aws".into(),
                "~/.gnupg".into(),
                "~/.config".into(),
            ],
        }
    }
}

impl BeforeAspect for ToolScopeAspect {
    fn before(&self, _tool_name: &str) -> Result<(), String> {
        // In production with aspect-rs macros, the path argument is extracted via
        // a thread-local injected before the aspect executes:
        //   set_tool_path(&path);
        //   aspect.before(&join_point);
        //   clear_tool_path();
        // Here we demonstrate the policy check in isolation.
        Ok(())
    }

    // Real path check (called with extracted path arg)
}

impl ToolScopeAspect {
    pub fn check_path(&self, path: &str) -> Result<(), String> {
        let expanded = path.replace('~', &std::env::var("HOME").unwrap_or_default());
        for prefix in &self.forbidden_prefixes {
            let exp_prefix = prefix.replace('~', &std::env::var("HOME").unwrap_or_default());
            if expanded.starts_with(&exp_prefix) {
                return Err(format!("Path blocked by security policy: {path}"));
            }
        }
        Ok(())
    }
}

// ── AuditAspect (mirrors aspect-agent::ToolCallAuditAspect) ──────────────────
//
// Replaces the manual AuditLogger calls scattered across 12 files.
// Scattering: audit concern appears in 12 tool files but all go through
// `src/security/audit.rs` — still duplicated per-call-site.

#[derive(Debug, Clone)]
pub struct AuditEntry {
    pub tool_name: String,
    pub success: bool,
    pub duration_ms: u64,
    pub timestamp: String,
}

pub struct AuditAspect {
    entries: Arc<parking_lot::Mutex<Vec<AuditEntry>>>,
    call_starts: Arc<parking_lot::Mutex<std::collections::HashMap<String, Instant>>>,
}

impl AuditAspect {
    pub fn new() -> Self {
        Self {
            entries: Arc::new(parking_lot::Mutex::new(Vec::new())),
            call_starts: Arc::new(parking_lot::Mutex::new(std::collections::HashMap::new())),
        }
    }

    pub fn entries(&self) -> Vec<AuditEntry> {
        self.entries.lock().clone()
    }
}

impl BeforeAspect for AuditAspect {
    fn before(&self, tool_name: &str) -> Result<(), String> {
        self.call_starts.lock().insert(tool_name.to_string(), Instant::now());
        Ok(())
    }
}

impl AfterAspect for AuditAspect {
    fn after(&self, tool_name: &str, success: bool) {
        let duration_ms = self
            .call_starts
            .lock()
            .remove(tool_name)
            .map(|t| t.elapsed().as_millis() as u64)
            .unwrap_or(0);
        self.entries.lock().push(AuditEntry {
            tool_name: tool_name.to_string(),
            success,
            duration_ms,
            timestamp: chrono::Utc::now().to_rfc3339(),
        });
    }
}

// ── Aspectized ShellTool ──────────────────────────────────────────────────────
//
// The aspectized execute() method contains ONLY business logic.
// All crosscutting concerns are handled by the aspect chain.
//
// LOC comparison:
//   Original shell.rs execute(): ~120 lines (crosscutting: ~50, business: ~70)
//   Aspectized execute():          ~20 lines (crosscutting:  ~0, business: ~20)

pub struct AspectizedShellTool {
    rate_limiter: Arc<RateLimitAspect>,
    scope_guard: Arc<ToolScopeAspect>,
    audit: Arc<AuditAspect>,
    /// In production this would be Arc<dyn RuntimeAdapter>
    simulate_success: bool,
}

impl AspectizedShellTool {
    pub fn new(max_per_hour: u64) -> Self {
        Self {
            rate_limiter: Arc::new(RateLimitAspect::new(max_per_hour)),
            scope_guard: Arc::new(ToolScopeAspect::default_policy()),
            audit: Arc::new(AuditAspect::new()),
            simulate_success: true,
        }
    }

    fn apply_before(&self, tool_name: &str, command: &str) -> Result<(), ToolResult> {
        // In production: applied by the #[aspect(...)] macro, not inline code.
        // Aspect chain: ToolScope → RateLimit → Audit (per RE2026 composition order)

        // 1. Scope check: extract path arg and verify
        // (Simplified: scan command for forbidden path prefixes)
        let words: Vec<&str> = command.split_whitespace().collect();
        for word in &words {
            if let Err(e) = self.scope_guard.check_path(word) {
                return Err(ToolResult::err(e));
            }
        }

        // 2. Rate limit check
        if let Err(e) = self.rate_limiter.before(tool_name) {
            return Err(ToolResult::err(e));
        }

        // 3. Audit: record call start
        let _ = self.audit.before(tool_name);

        Ok(())
    }

    fn apply_after(&self, tool_name: &str, success: bool) {
        self.audit.after(tool_name, success);
    }

    /// Pure business logic — zero crosscutting code.
    fn execute_inner(&self, command: &str) -> ToolResult {
        // In production: self.runtime.run_command(command, TIMEOUT, MAX_BYTES)
        if self.simulate_success {
            ToolResult::ok(format!("$ {command}\n[simulated output]"))
        } else {
            ToolResult::err("command failed")
        }
    }

    pub fn audit_entries(&self) -> Vec<AuditEntry> {
        self.audit.entries()
    }
}

#[async_trait]
impl Tool for AspectizedShellTool {
    fn name(&self) -> &str { "shell" }

    fn description(&self) -> &str {
        "Execute a shell command — aspects handle rate limiting, path scope, and audit"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "Shell command to execute" }
            },
            "required": ["command"]
        })
    }

    async fn execute(&self, args: Value) -> Result<ToolResult> {
        let command = args
            .get("command")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'command' parameter"))?;

        // Apply before-aspects (in production: injected by #[aspect] macros)
        if let Err(blocked) = self.apply_before(self.name(), command) {
            return Ok(blocked);
        }

        // Pure business logic
        let result = self.execute_inner(command);

        // Apply after-aspects
        self.apply_after(self.name(), result.success);

        Ok(result)
    }
}

// ── Demo ──────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    let tool = AspectizedShellTool::new(20); // 20 actions/hour, mirrors SecurityPolicy default

    println!("=== Aspect-Oriented ShellTool Demo ===\n");
    println!("Aspects applied: ToolScopeAspect + RateLimitAspect + AuditAspect");
    println!("Original shell.rs crosscutting LOC: ~50  Aspectized: ~0\n");

    let commands = vec![
        "ls -la /workspace",
        "cat /etc/passwd",         // blocked by ToolScopeAspect
        "echo hello world",
        "grep -r TODO /workspace",
        "cat ~/.ssh/id_rsa",       // blocked by ToolScopeAspect
    ];

    for cmd in &commands {
        print!("$ {cmd:<40} → ");
        match tool.execute(json!({"command": cmd})).await? {
            r if r.success => println!("✓ allowed"),
            r => println!("✗ blocked: {}", r.error.unwrap_or_default()),
        }
    }

    println!("\n--- Rate limiting demo (limit=3) ---");
    let limited_tool = AspectizedShellTool::new(3);
    for i in 1..=5 {
        let result = limited_tool.execute(json!({"command": "echo test"})).await?;
        print!("Call {i}: ");
        if result.success {
            println!("✓ allowed");
        } else {
            println!("✗ {}", result.error.unwrap_or_default());
        }
    }

    println!("\n--- Audit trail ---");
    for entry in tool.audit_entries() {
        println!("  {} — {} ({}ms)", entry.tool_name, if entry.success { "ok" } else { "err" }, entry.duration_ms);
    }

    println!("\n=== LOC impact ===");
    println!("Rate limiting concern: 28 files × ~8 LOC = ~224 LOC removed");
    println!("Path validation:       65 files × ~6 LOC = ~390 LOC removed");
    println!("Audit logging:         12 files × ~5 LOC =  ~60 LOC removed");
    println!("Aspect definitions:                        ~300 LOC (one-time)");
    println!("Net savings:                              ~374 LOC, tangling ~0%");

    Ok(())
}
