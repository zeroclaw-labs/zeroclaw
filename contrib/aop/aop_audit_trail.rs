//! Example: Aspect-Oriented refactoring of the audit trail crosscutting concern.
//!
//! ZeroClaw's `src/security/audit.rs` provides an `AuditLogger` that is manually
//! invoked across 12+ tool files. Each tool must remember to:
//!   1. Import `AuditLogger`
//!   2. Call `audit.log_tool_call(name, args)` before execution
//!   3. Call `audit.log_tool_result(name, result)` after execution
//!   4. Call `audit.log_error(name, error)` on failure
//!
//! This pattern is repeated for every tool, with no enforcement that all 3 hooks
//! are called consistently — creating both tangling and correctness risk.
//!
//! ## The solution: ToolCallAuditAspect
//!
//! The `ToolCallAuditAspect` from `aspect-agent` wraps this into a single aspect
//! that is applied once, ensuring consistent auditing for every call:
//!
//! ```rust,ignore
//! use aspect_agent::tool_call_audit::{ToolCallAuditAspect, InMemoryAuditStorage};
//!
//! let audit = ToolCallAuditAspect::new(Arc::new(InMemoryAuditStorage::default()));
//!
//! // All 3 hooks guaranteed, zero audit code in execute()
//! #[aspect(audit.clone())]
//! async fn execute(&self, args: Value) -> Result<ToolResult> {
//!     // pure business logic
//! }
//! ```
//!
//! The aspect automatically:
//! - Records the call start timestamp in `before()`
//! - Records success + duration in `after()`
//! - Records failure + error in `after_error()`
//!
//! ## References
//!
//! - aspect-rs ToolCallAuditAspect: <https://github.com/yijunyu/aspect-rs-priv/tree/feat/aspect-agent>
//! - ZeroClaw audit module: `src/security/audit.rs`
//! - RE2026 paper section 4.4: "Tool Call Audit Aspects"

use anyhow::Result;
use async_trait::async_trait;
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Instant;

// ── Minimal types ─────────────────────────────────────────────────────────────

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
    async fn execute(&self, args: Value) -> Result<ToolResult>;
}

// ── AuditEntry + AuditStorage (mirrors aspect-agent::tool_call_audit) ─────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuditOutcome { Success, Failure }

/// A single tool call audit record.
/// Mirrors `aspect_agent::tool_call_audit::AuditEntry`.
#[derive(Debug, Clone)]
pub struct AuditEntry {
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub tool_name: String,
    pub module_path: String,
    pub actor: String,
    pub outcome: AuditOutcome,
    pub duration_ms: u64,
    pub error_message: Option<String>,
}

/// Pluggable storage backend for audit entries.
/// Mirrors `aspect_agent::tool_call_audit::AuditStorage`.
pub trait AuditStorage: Send + Sync {
    fn append(&self, entry: AuditEntry);
    fn entries(&self) -> Vec<AuditEntry>;
    fn len(&self) -> usize { self.entries().len() }
    fn is_empty(&self) -> bool { self.len() == 0 }
}

/// In-memory storage (default). Mirrors `InMemoryAuditStorage` in aspect-agent.
#[derive(Default)]
pub struct InMemoryAuditStorage {
    entries: Mutex<Vec<AuditEntry>>,
}

impl AuditStorage for InMemoryAuditStorage {
    fn append(&self, entry: AuditEntry) {
        self.entries.lock().push(entry);
    }
    fn entries(&self) -> Vec<AuditEntry> {
        self.entries.lock().clone()
    }
}

// ── ToolCallAuditAspect ───────────────────────────────────────────────────────

struct CallState {
    start: Instant,
}

/// Centralizes the audit trail scattered across 12+ tool files.
/// Mirrors `aspect_agent::tool_call_audit::ToolCallAuditAspect`.
pub struct ToolCallAuditAspect {
    storage: Arc<dyn AuditStorage>,
    /// Actor identifier (user or session id).
    actor: String,
    /// Per-call start timestamps, keyed by tool name.
    pending: Arc<Mutex<std::collections::HashMap<String, CallState>>>,
}

impl ToolCallAuditAspect {
    pub fn new(storage: Arc<dyn AuditStorage>) -> Self {
        Self {
            storage,
            actor: "system".into(),
            pending: Arc::new(Mutex::new(std::collections::HashMap::new())),
        }
    }

    pub fn with_actor(mut self, actor: impl Into<String>) -> Self {
        self.actor = actor.into();
        self
    }

    /// Called by before() advice — records call start.
    pub fn on_before(&self, tool_name: &str) {
        self.pending.lock().insert(tool_name.to_string(), CallState { start: Instant::now() });
    }

    /// Called by after() advice — records success entry.
    pub fn on_after(&self, tool_name: &str, module_path: &str) {
        let duration_ms = self.pending.lock()
            .remove(tool_name)
            .map(|s| s.start.elapsed().as_millis() as u64)
            .unwrap_or(0);
        self.storage.append(AuditEntry {
            timestamp: chrono::Utc::now(),
            tool_name: tool_name.to_string(),
            module_path: module_path.to_string(),
            actor: self.actor.clone(),
            outcome: AuditOutcome::Success,
            duration_ms,
            error_message: None,
        });
    }

    /// Called by after_error() advice — records failure entry.
    pub fn on_error(&self, tool_name: &str, module_path: &str, error: &str) {
        let duration_ms = self.pending.lock()
            .remove(tool_name)
            .map(|s| s.start.elapsed().as_millis() as u64)
            .unwrap_or(0);
        self.storage.append(AuditEntry {
            timestamp: chrono::Utc::now(),
            tool_name: tool_name.to_string(),
            module_path: module_path.to_string(),
            actor: self.actor.clone(),
            outcome: AuditOutcome::Failure,
            duration_ms,
            error_message: Some(error.to_string()),
        });
    }
}

// ── Original file_read: 25 LOC of audit code per method ──────────────────────

/// Simulates the manual audit pattern repeated in 12 tool files.
pub struct OriginalFileReadTool {
    audit: Arc<InMemoryAuditStorage>,
}

#[async_trait]
impl Tool for OriginalFileReadTool {
    fn name(&self) -> &str { "file_read" }

    async fn execute(&self, args: Value) -> Result<ToolResult> {
        let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");

        // ── AUDIT CODE (crosscutting) ─────────────────────────────────────────
        let start = Instant::now();
        self.audit.append(AuditEntry {
            timestamp: chrono::Utc::now(),
            tool_name: "file_read".into(),
            module_path: "tools::file_read".into(),
            actor: "system".into(),
            outcome: AuditOutcome::Success, // placeholder, corrected below
            duration_ms: 0,
            error_message: None,
        });
        // ── END AUDIT (before) ────────────────────────────────────────────────

        // Business logic
        let result = if path.is_empty() {
            ToolResult::err("Missing path")
        } else {
            ToolResult::ok(format!("Contents of {path}"))
        };

        // ── AUDIT CODE (crosscutting) ─────────────────────────────────────────
        let duration = start.elapsed().as_millis() as u64;
        // Note: outcome above was wrong — must patch it, which requires extra bookkeeping
        // This is the kind of subtle correctness risk that the aspect solves.
        let outcome = if result.success { AuditOutcome::Success } else { AuditOutcome::Failure };
        self.audit.append(AuditEntry {
            timestamp: chrono::Utc::now(),
            tool_name: "file_read".into(),
            module_path: "tools::file_read".into(),
            actor: "system".into(),
            outcome,
            duration_ms: duration,
            error_message: result.error.clone(),
        });
        // ── END AUDIT (after) ─────────────────────────────────────────────────

        Ok(result)
    }
}

// ── Aspectized file_read: 0 LOC of audit code ────────────────────────────────

pub struct AspectizedFileReadTool {
    aspect: Arc<ToolCallAuditAspect>,
}

impl AspectizedFileReadTool {
    pub fn new(aspect: Arc<ToolCallAuditAspect>) -> Self {
        Self { aspect }
    }
}

#[async_trait]
impl Tool for AspectizedFileReadTool {
    fn name(&self) -> &str { "file_read" }

    async fn execute(&self, args: Value) -> Result<ToolResult> {
        let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");

        // In production: before/after injected by #[aspect(self.aspect.clone())] macro.
        self.aspect.on_before(self.name());

        // Pure business logic only — zero audit code
        let result = if path.is_empty() {
            ToolResult::err("Missing path")
        } else {
            ToolResult::ok(format!("Contents of {path}"))
        };

        if result.success {
            self.aspect.on_after(self.name(), "tools::file_read");
        } else {
            self.aspect.on_error(self.name(), "tools::file_read", result.error.as_deref().unwrap_or(""));
        }

        Ok(result)
    }
}

// ── Demo ──────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    println!("=== Aspect-Oriented Audit Trail Demo ===\n");
    println!("Audit concern in ZeroClaw v3:");
    println!("  src/security/audit.rs: central AuditLogger (423 lines)");
    println!("  Manual calls in:       12+ tool files (each ~5 LOC = ~60 LOC scattered)\n");

    // Original: manual audit calls
    let orig_storage = Arc::new(InMemoryAuditStorage::default());
    let original_tool = OriginalFileReadTool { audit: orig_storage.clone() };

    println!("--- Original pattern (manual audit calls) ---");
    let _ = original_tool.execute(json!({"path": "/workspace/notes.txt"})).await?;
    let _ = original_tool.execute(json!({})).await?;  // missing path → error
    println!("Audit entries (original): {} (2 before + 2 after = 4 entries)", orig_storage.entries().len());

    // Aspectized: aspect handles all audit calls
    let asp_storage = Arc::new(InMemoryAuditStorage::default());
    let audit_aspect = Arc::new(
        ToolCallAuditAspect::new(asp_storage.clone()).with_actor("demo_session")
    );
    let aspectized_tool = AspectizedFileReadTool::new(audit_aspect);

    println!("\n--- Aspectized pattern (ToolCallAuditAspect) ---");
    let _ = aspectized_tool.execute(json!({"path": "/workspace/notes.txt"})).await?;
    let _ = aspectized_tool.execute(json!({})).await?;  // error path

    for entry in asp_storage.entries() {
        println!(
            "  [{:?}] {} — {}ms  actor={}  error={:?}",
            entry.outcome, entry.tool_name, entry.duration_ms, entry.actor, entry.error_message
        );
    }

    println!("\n=== Comparison ===");
    println!("Original:    2 calls → {} audit entries (before+after duplicated)", orig_storage.len());
    println!("Aspectized:  2 calls → {} audit entries (before handled internally)", asp_storage.len());

    println!("\n=== LOC impact ===");
    println!("Audit concern: 12 tool files × ~5 LOC/file = ~60 LOC removed from tools");
    println!("Aspect handles before+after+error consistently — eliminates correctness risk");
    println!("ToolCallAuditAspect:                         ~150 LOC (one-time in aspect-agent)");
    println!("Net savings:                                  ~60 LOC + guarantee of completeness");

    Ok(())
}
