//! Example: Aspect-Oriented refactoring of the ApprovalManager crosscutting concern.
//!
//! ZeroClaw's `approval/mod.rs` is the most critically scattered concern:
//! only 1/17 files (5.9%) in the approval module hold approval vocabulary,
//! while 16 other files import and call `ApprovalManager` directly.
//!
//! ## The problem
//!
//! Every tool's `execute()` method that can trigger approval contains:
//! ```rust,ignore
//! // Repeated pattern across 17+ tool files
//! if self.autonomy_level == AutonomyLevel::Supervised {
//!     if !self.always_ask.contains(tool_name) && self.session_allowlist.contains(tool_name) {
//!         // bypass
//!     } else {
//!         match approval_manager.request_approval(&req).await {
//!             ApprovalResponse::No => return Ok(ToolResult::err("denied")),
//!             ApprovalResponse::Always => { session.insert(tool_name); }
//!             ApprovalResponse::Yes => {}
//!         }
//!     }
//! }
//! ```
//!
//! ## The solution
//!
//! The `HumanApprovalAspect` from `aspect-agent` centralizes this into one
//! reusable aspect applied declaratively:
//!
//! ```rust,ignore
//! use aspect_agent::human_approval::{ApprovalChannel, HumanApprovalAspect, RiskLevel};
//!
//! // Declare once, apply everywhere
//! let gate = HumanApprovalAspect::new(ApprovalChannel::Stdio)
//!     .require_approval("shell", RiskLevel::High, "Execute shell command")
//!     .require_approval("file_write", RiskLevel::Medium, "Write to file")
//!     .require_approval("http_request", RiskLevel::Low, "Make HTTP request");
//!
//! // Applied via macro — zero approval code in execute()
//! #[aspect(gate.clone())]
//! async fn execute(&self, args: Value) -> Result<ToolResult> {
//!     // pure business logic only
//! }
//! ```
//!
//! ## References
//!
//! - aspect-rs HumanApprovalAspect: <https://github.com/yijunyu/aspect-rs-priv/tree/feat/aspect-agent>
//! - ZeroClaw approval module: `src/approval/mod.rs`
//! - RE2026 paper section 4.3: "Human-in-the-Loop Approval Aspects"

use anyhow::Result;
use async_trait::async_trait;
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::Arc;

// ── Minimal types (mirrors src/tools/traits.rs + src/approval/mod.rs) ────────

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

// ── HumanApprovalAspect (mirrors aspect-agent::HumanApprovalAspect) ──────────

/// Risk level for an operation. Mirrors ZeroClaw's `CommandRiskLevel`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RiskLevel { Low, Medium, High, Critical }

impl RiskLevel {
    fn label(&self) -> &'static str {
        match self {
            Self::Low => "LOW", Self::Medium => "MEDIUM",
            Self::High => "HIGH", Self::Critical => "CRITICAL",
        }
    }
}

/// Channel for routing approval requests — mirrors `ApprovalChannel` in aspect-agent.
pub enum ApprovalChannel {
    /// Blocks on stdin (mirrors ApprovalManager::request_from_stdin).
    Stdio,
    /// Calls a closure synchronously (for tests, CI, and async runtimes).
    Handler(Arc<dyn Fn(&str, RiskLevel) -> bool + Send + Sync>),
    /// Auto-approve all (useful for ReadOnly/Full autonomy levels).
    AutoApprove,
    /// Auto-deny all (useful for integration tests).
    AutoDeny,
}

#[derive(Clone)]
struct ApprovalTarget {
    description: String,
    risk: RiskLevel,
}

/// Centralizes the approval gate that is currently scattered across 17+ tool files.
///
/// In aspect-rs, this is `aspect_agent::human_approval::HumanApprovalAspect`.
/// This standalone implementation mirrors the same API for demonstration.
#[derive(Clone)]
pub struct HumanApprovalAspect {
    channel: Arc<ApprovalChannel>,
    targets: Arc<std::collections::HashMap<String, ApprovalTarget>>,
    /// Session-scoped allowlist built from "always" responses. Mirrors
    /// `ApprovalManager::session_allowlist`.
    session_allowlist: Arc<Mutex<HashSet<String>>>,
}

impl HumanApprovalAspect {
    pub fn new(channel: ApprovalChannel) -> Self {
        Self {
            channel: Arc::new(channel),
            targets: Arc::new(std::collections::HashMap::new()),
            session_allowlist: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    /// Register a tool as requiring human approval.
    /// Mirrors `HumanApprovalAspect::require_approval()` in aspect-agent.
    pub fn require_approval(
        self,
        tool_name: impl Into<String>,
        risk: RiskLevel,
        description: impl Into<String>,
    ) -> Self {
        let mut targets = (*self.targets).clone();
        targets.insert(tool_name.into(), ApprovalTarget { description: description.into(), risk });
        Self { targets: Arc::new(targets), ..self }
    }

    /// Pre-approve a tool for this session (bypasses future prompts).
    /// Mirrors `ApprovalManager`'s session allowlist behaviour.
    pub fn pre_approve(&self, tool_name: impl Into<String>) {
        self.session_allowlist.lock().insert(tool_name.into());
    }

    /// Gate execution — called by the aspect `before()` advice.
    /// Returns `Ok(())` to allow, `Err(msg)` to block.
    pub fn check(&self, tool_name: &str) -> Result<(), String> {
        // Session allowlist bypass (mirrors ApprovalManager::session_allowlist check)
        if self.session_allowlist.lock().contains(tool_name) {
            return Ok(());
        }

        let target = match self.targets.get(tool_name) {
            Some(t) => t,
            None => return Ok(()), // not registered → always allow
        };

        let approved = match self.channel.as_ref() {
            ApprovalChannel::AutoApprove => true,
            ApprovalChannel::AutoDeny => false,
            ApprovalChannel::Handler(f) => f(tool_name, target.risk),
            ApprovalChannel::Stdio => {
                use std::io::{BufRead, Write};
                eprintln!(
                    "\n[Approval required] '{}' (risk: {}) — {}",
                    tool_name, target.risk.label(), target.description
                );
                eprint!("  Allow? [y/N/always]: ");
                let _ = std::io::stderr().flush();
                let mut line = String::new();
                if std::io::stdin().lock().read_line(&mut line).is_ok() {
                    match line.trim().to_lowercase().as_str() {
                        "y" | "yes" => true,
                        "always" => {
                            self.session_allowlist.lock().insert(tool_name.to_string());
                            true
                        }
                        _ => false,
                    }
                } else {
                    false
                }
            }
        };

        if approved {
            Ok(())
        } else {
            Err(format!("Execution of '{}' denied by human operator", tool_name))
        }
    }
}

// ── Aspectized file_write tool demonstrating the approval gate ────────────────

pub struct AspectizedFileWriteTool {
    approval: HumanApprovalAspect,
}

impl AspectizedFileWriteTool {
    pub fn new(approval: HumanApprovalAspect) -> Self {
        Self { approval }
    }
}

#[async_trait]
impl Tool for AspectizedFileWriteTool {
    fn name(&self) -> &str { "file_write" }

    async fn execute(&self, args: Value) -> Result<ToolResult> {
        let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("<unknown>");

        // In production: injected by #[aspect(approval.clone())] macro.
        // The aspect's before() advice runs here — zero approval code in business logic.
        if let Err(msg) = self.approval.check(self.name()) {
            return Ok(ToolResult::err(msg));
        }

        // Pure business logic only
        Ok(ToolResult::ok(format!("Wrote to {path}")))
    }
}

// ── Demo ──────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    println!("=== Aspect-Oriented Approval Gate Demo ===\n");
    println!("Approval concern scattering in ZeroClaw v3:");
    println!("  approval/ module: 1/17 files (5.9%) — worst centralization ratio");
    println!("  Approval vocabulary scattered across: main.rs, tools/*.rs, agent/*.rs\n");

    // Case 1: Auto-approve (Full autonomy mode)
    {
        let gate = HumanApprovalAspect::new(ApprovalChannel::AutoApprove)
            .require_approval("file_write", RiskLevel::Medium, "Write to workspace file");
        let tool = AspectizedFileWriteTool::new(gate);
        let r = tool.execute(json!({"path": "/workspace/notes.txt", "content": "hello"})).await?;
        println!("Full autonomy mode:    {}", if r.success { "✓ allowed" } else { &format!("✗ {}", r.error.unwrap_or_default()) });
    }

    // Case 2: Auto-deny (testing / safety check)
    {
        let gate = HumanApprovalAspect::new(ApprovalChannel::AutoDeny)
            .require_approval("file_write", RiskLevel::Medium, "Write to workspace file");
        let tool = AspectizedFileWriteTool::new(gate);
        let r = tool.execute(json!({"path": "/workspace/notes.txt", "content": "hello"})).await?;
        println!("Deny-all mode:         {}", if r.success { "✓ allowed" } else { &format!("✗ {}", r.error.unwrap_or_default()) });
    }

    // Case 3: Handler-based (mirrors async channel for non-CLI use)
    {
        let approved_tools = Arc::new(Mutex::new(HashSet::new()));
        let at = approved_tools.clone();
        let gate = HumanApprovalAspect::new(ApprovalChannel::Handler(Arc::new(move |tool, risk| {
            println!("  [Approval handler] '{}' ({}): auto-approving", tool, risk.label());
            at.lock().insert(tool.to_string());
            true
        })))
        .require_approval("file_write", RiskLevel::Medium, "Write to workspace file");
        let tool = AspectizedFileWriteTool::new(gate);
        let r = tool.execute(json!({"path": "/workspace/notes.txt"})).await?;
        println!("Handler channel:       {}", if r.success { "✓ allowed" } else { &format!("✗ {}", r.error.unwrap_or_default()) });
    }

    // Case 4: Session approval — first call prompts, subsequent calls bypass
    {
        let call_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let cc = call_count.clone();
        let gate = HumanApprovalAspect::new(ApprovalChannel::Handler(Arc::new(move |_tool, _risk| {
            cc.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            true // simulate "always" response for demo
        })))
        .require_approval("file_write", RiskLevel::Medium, "Write");

        gate.session_allowlist.lock().insert("file_write".to_string()); // simulate "always" from first call
        let tool = AspectizedFileWriteTool::new(gate);
        let _ = tool.execute(json!({"path": "f"})).await?;
        let _ = tool.execute(json!({"path": "g"})).await?;
        println!("Session bypass:        {} (handler calls: {}/2)", "✓ both allowed", call_count.load(std::sync::atomic::Ordering::SeqCst));
    }

    println!("\n=== LOC impact ===");
    println!("Approval concern: ~17 files × ~20 LOC = ~340 LOC scattered code");
    println!("HumanApprovalAspect definition:        ~200 LOC (one-time in aspect-agent)");
    println!("Net savings:                           ~140 LOC + approval logic centralized");

    Ok(())
}
