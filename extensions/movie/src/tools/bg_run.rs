//! Background tool execution — fire-and-forget tool calls with result polling.
//!
//! This module provides two synthetic tools (`bg_run` and `bg_status`) that enable
//! asynchronous tool execution. Long-running tools can be dispatched in the background
//! while the agent continues reasoning, with results auto-injected into subsequent turns.
//!
//! # Architecture
//!
//! - `BgJobStore`: Shared state (Arc<Mutex<HashMap>>) holding all background jobs
//! - `BgRunTool`: Validates tool exists, spawns execution, returns job_id immediately
//! - `BgStatusTool`: Queries job status by ID or lists all jobs
//!
//! # Timeout Policy
//!
//! - Foreground tools: 180s default, per-server override via `tool_timeout_secs`, max 600s
//! - Background tools: 600s hard cap (safety ceiling)
//!
//! # Auto-Injection
//!
//! Completed jobs are drained from the store before each LLM turn and injected as
//! `<bg_result>` XML messages. Delivered jobs expire after 5 minutes.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tokio::time::{timeout, Duration};

use super::traits::{Tool, ToolResult};

/// Hard timeout for background tool execution (seconds).
const BG_TOOL_TIMEOUT_SECS: u64 = 600;

/// Time after delivery before a job is eligible for cleanup (seconds).
const DELIVERED_JOB_EXPIRY_SECS: u64 = 300;

/// Maximum concurrent background jobs per session.
/// Prevents resource exhaustion from unbounded parallel tool execution.
const MAX_CONCURRENT_JOBS: usize = 5;

// ── Job Status ──────────────────────────────────────────────────────────────

/// Status of a background job.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum BgJobStatus {
    /// Tool is currently executing.
    Running,
    /// Tool completed successfully.
    Complete,
    /// Tool failed or timed out.
    Failed,
}

// ── Background Job ───────────────────────────────────────────────────────────

/// A single background job record.
#[derive(Debug, Clone)]
pub struct BgJob {
    /// Unique job identifier (format: "j-<16-hex-chars>").
    pub id: String,
    /// Name of the tool being executed.
    pub tool_name: String,
    /// Sender/conversation identifier for scope isolation.
    /// Jobs are drained only for the matching sender to prevent cross-conversation injection.
    pub sender: Option<String>,
    /// Current status of the job.
    pub status: BgJobStatus,
    /// Result output (populated when Complete or Failed).
    pub result: Option<String>,
    /// Error message (populated when Failed).
    pub error: Option<String>,
    /// When the job was started.
    pub started_at: Instant,
    /// When the job completed (set when status changes from Running).
    pub completed_at: Option<Instant>,
    /// Whether the result has been auto-injected into agent history.
    pub delivered: bool,
    /// When the result was delivered (for expiry calculation).
    pub delivered_at: Option<Instant>,
}

impl BgJob {
    /// Elapsed time in seconds since job start.
    pub fn elapsed_secs(&self) -> f64 {
        let end = self.completed_at.unwrap_or_else(Instant::now);
        end.duration_since(self.started_at).as_secs_f64()
    }

    /// Check if a delivered job has expired (5 minutes after delivery).
    pub fn is_expired(&self) -> bool {
        if let Some(delivered_at) = self.delivered_at {
            delivered_at.elapsed().as_secs() >= DELIVERED_JOB_EXPIRY_SECS
        } else {
            false
        }
    }
}

// ── Job Store ────────────────────────────────────────────────────────────────

/// Shared store for background jobs.
///
/// Clonable via Arc, thread-safe via Mutex. Used by:
/// - `BgRunTool` to insert new jobs
/// - `BgStatusTool` to query job status
/// - Agent loop to drain completed jobs for auto-injection
#[derive(Clone)]
pub struct BgJobStore {
    jobs: Arc<Mutex<HashMap<String, BgJob>>>,
}

impl BgJobStore {
    /// Create a new empty job store.
    pub fn new() -> Self {
        Self {
            jobs: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Insert a new job into the store.
    pub async fn insert(&self, job: BgJob) {
        let mut jobs = self.jobs.lock().await;
        jobs.insert(job.id.clone(), job);
    }

    /// Get a job by ID.
    pub async fn get(&self, job_id: &str) -> Option<BgJob> {
        let jobs = self.jobs.lock().await;
        jobs.get(job_id).cloned()
    }

    /// Get all jobs.
    pub async fn all(&self) -> Vec<BgJob> {
        let jobs = self.jobs.lock().await;
        jobs.values().cloned().collect()
    }

    /// Count currently running jobs.
    pub async fn running_count(&self) -> usize {
        let jobs = self.jobs.lock().await;
        jobs.values()
            .filter(|j| j.status == BgJobStatus::Running)
            .count()
    }

    /// Update a job's status and result.
    pub async fn update(
        &self,
        job_id: &str,
        status: BgJobStatus,
        result: Option<String>,
        error: Option<String>,
    ) {
        let mut jobs = self.jobs.lock().await;
        if let Some(job) = jobs.get_mut(job_id) {
            job.status = status;
            job.result = result;
            job.error = error;
            job.completed_at = Some(Instant::now());
        }
    }

    /// Drain completed jobs that haven't been delivered yet, scoped by sender.
    ///
    /// Marks jobs as delivered (one-time injection guarantee).
    /// Only returns jobs matching the given sender to prevent cross-conversation injection.
    /// If sender is None, returns all completed jobs (backwards-compatible behavior).
    pub async fn drain_completed(&self, sender: Option<&str>) -> Vec<BgJob> {
        let mut jobs = self.jobs.lock().await;
        let mut completed = Vec::new();

        for job in jobs.values_mut() {
            // Skip running or already delivered jobs
            if job.status == BgJobStatus::Running || job.delivered {
                continue;
            }
            // Scope isolation: only drain jobs for the matching sender
            if let Some(filter_sender) = sender {
                if job.sender.as_deref() != Some(filter_sender) {
                    continue;
                }
            }
            job.delivered = true;
            job.delivered_at = Some(Instant::now());
            completed.push(job.clone());
        }

        completed
    }

    /// Remove expired delivered jobs.
    pub async fn cleanup_expired(&self) {
        let mut jobs = self.jobs.lock().await;
        jobs.retain(|_, job| !job.is_expired());
    }
}

impl Default for BgJobStore {
    fn default() -> Self {
        Self::new()
    }
}

// ── Generate Job ID ──────────────────────────────────────────────────────────

/// Generate a unique job ID.
///
/// Format: "j-<16-hex-chars>" (e.g., "j-0123456789abcdef").
/// Uses random u64 for simplicity (no ulid crate dependency).
fn generate_job_id() -> String {
    let id: u64 = rand::random();
    format!("j-{id:016x}")
}

// ── BgRun Tool ───────────────────────────────────────────────────────────────

/// Tool to dispatch a background job.
///
/// Validates the target tool exists, spawns execution with a 600s timeout,
/// and returns the job ID immediately.
pub struct BgRunTool {
    /// Shared job store for tracking background jobs.
    job_store: BgJobStore,
    /// Reference to the tool registry for finding and cloning tools.
    tools: Arc<Vec<Arc<dyn Tool>>>,
}

impl BgRunTool {
    /// Create a new bg_run tool.
    pub fn new(job_store: BgJobStore, tools: Arc<Vec<Arc<dyn Tool>>>) -> Self {
        Self { job_store, tools }
    }

    /// Find a tool by name in the registry.
    fn find_tool(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.iter().find(|t| t.name() == name).cloned()
    }
}

#[async_trait]
impl Tool for BgRunTool {
    fn name(&self) -> &str {
        "bg_run"
    }

    fn description(&self) -> &str {
        "Execute a tool in the background and return a job ID immediately. \
         Use this for long-running operations where you don't want to block. \
         Check results with bg_status or wait for auto-injection in the next turn. \
         Background tools have a 600-second maximum timeout."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "tool": {
                    "type": "string",
                    "description": "Name of the tool to execute in the background"
                },
                "arguments": {
                    "type": "object",
                    "description": "Arguments to pass to the tool"
                }
            },
            "required": ["tool"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let tool_name = args
            .get("tool")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing or invalid 'tool' parameter"))?;

        let arguments = args
            .get("arguments")
            .cloned()
            .unwrap_or(serde_json::json!({}));

        // Validate arguments is an object (matches schema declaration)
        if !arguments.is_object() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("'arguments' must be an object".to_string()),
            });
        }

        // Validate tool exists
        let tool = match self.find_tool(tool_name) {
            Some(t) => t,
            None => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("unknown tool: {tool_name}")),
                });
            }
        };

        // Don't allow bg_run to spawn itself (prevent recursion)
        if tool_name == "bg_run" || tool_name == "bg_status" {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("cannot run bg_run or bg_status in background".to_string()),
            });
        }

        // Enforce concurrent job limit to prevent resource exhaustion
        let running_count = self.job_store.running_count().await;
        if running_count >= MAX_CONCURRENT_JOBS {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Maximum concurrent background jobs reached ({MAX_CONCURRENT_JOBS}). \
                     Wait for existing jobs to complete."
                )),
            });
        }

        let job_id = generate_job_id();
        let job_store = self.job_store.clone();
        let job_id_for_task = job_id.clone();

        // Insert job in Running state
        // Note: sender is set to None here; when used from channels, the caller
        // should create the job with sender context for proper scope isolation.
        job_store
            .insert(BgJob {
                id: job_id.clone(),
                tool_name: tool_name.to_string(),
                sender: None,
                status: BgJobStatus::Running,
                result: None,
                error: None,
                started_at: Instant::now(),
                completed_at: None,
                delivered: false,
                delivered_at: None,
            })
            .await;

        // Spawn background execution
        tokio::spawn(async move {
            let result = timeout(
                Duration::from_secs(BG_TOOL_TIMEOUT_SECS),
                tool.execute(arguments),
            )
            .await;

            match result {
                Ok(Ok(tool_result)) => {
                    let (status, output, error) = if tool_result.success {
                        (
                            BgJobStatus::Complete,
                            Some(tool_result.output),
                            tool_result.error,
                        )
                    } else {
                        (
                            BgJobStatus::Failed,
                            Some(tool_result.output),
                            tool_result.error,
                        )
                    };
                    job_store
                        .update(&job_id_for_task, status, output, error)
                        .await;
                }
                Ok(Err(e)) => {
                    job_store
                        .update(
                            &job_id_for_task,
                            BgJobStatus::Failed,
                            None,
                            Some(e.to_string()),
                        )
                        .await;
                }
                Err(_) => {
                    job_store
                        .update(
                            &job_id_for_task,
                            BgJobStatus::Failed,
                            None,
                            Some(format!("timed out after {BG_TOOL_TIMEOUT_SECS}s")),
                        )
                        .await;
                }
            }
        });

        let output = serde_json::json!({
            "job_id": job_id,
            "tool": tool_name,
            "status": "running"
        });

        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&output).unwrap_or_default(),
            error: None,
        })
    }
}

// ── BgStatus Tool ────────────────────────────────────────────────────────────

/// Tool to query background job status.
///
/// Can query a specific job by ID or list all jobs.
pub struct BgStatusTool {
    /// Shared job store for querying status.
    job_store: BgJobStore,
}

impl BgStatusTool {
    /// Create a new bg_status tool.
    pub fn new(job_store: BgJobStore) -> Self {
        Self { job_store }
    }
}

#[async_trait]
impl Tool for BgStatusTool {
    fn name(&self) -> &str {
        "bg_status"
    }

    fn description(&self) -> &str {
        "Query the status of a background job by ID, or list all jobs if no ID provided. \
         Returns job status (running/complete/failed), result output, and elapsed time."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "job_id": {
                    "type": "string",
                    "description": "Optional job ID to query. If omitted, returns all jobs."
                }
            }
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let job_id = args.get("job_id").and_then(|v| v.as_str());

        let output = if let Some(id) = job_id {
            // Query specific job
            match self.job_store.get(id).await {
                Some(job) => format_job(&job),
                None => {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!("job not found: {id}")),
                    });
                }
            }
        } else {
            // List all jobs
            let jobs = self.job_store.all().await;
            if jobs.is_empty() {
                "No background jobs.".to_string()
            } else {
                let entries: Vec<String> = jobs.iter().map(format_job).collect();
                entries.join("\n\n")
            }
        };

        Ok(ToolResult {
            success: true,
            output,
            error: None,
        })
    }
}

/// Format a job for display.
fn format_job(job: &BgJob) -> String {
    let status_emoji = match job.status {
        BgJobStatus::Running => "\u{1f504}",
        BgJobStatus::Complete => "\u{2705}",
        BgJobStatus::Failed => "\u{274c}",
    };

    let mut lines = vec![
        format!("{status_emoji} Job {} ({})", job.id, job.tool_name),
        format!("  Status: {:?}", job.status),
        format!("  Elapsed: {:.1}s", job.elapsed_secs()),
    ];

    if let Some(ref result) = job.result {
        lines.push(format!("  Result: {result}"));
    }

    if let Some(ref error) = job.error {
        lines.push(format!("  Error: {error}"));
    }

    if job.delivered {
        lines.push("  Delivered: yes".to_string());
    }

    lines.join("\n")
}

/// Format a bg_result for auto-injection into agent history.
pub fn format_bg_result_for_injection(job: &BgJob) -> String {
    let output = job.result.as_deref().unwrap_or("");
    let error = job.error.as_deref();

    let content = if let Some(e) = error {
        format!("Error: {e}\n{output}")
    } else {
        output.to_string()
    };

    format!(
        "<bg_result job_id=\"{}\" tool=\"{}\" elapsed=\"{:.1}s\">\n{}\n</bg_result>",
        escape_xml(&job.id),
        escape_xml(&job.tool_name),
        job.elapsed_secs(),
        escape_xml(content.trim())
    )
}

/// Escape XML special characters to prevent injection attacks.
/// Tool output may contain arbitrary text including XML-like structures.
fn escape_xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn job_id_format() {
        let id = generate_job_id();
        assert!(id.starts_with("j-"));
        assert_eq!(id.len(), 18); // "j-" + 16 hex chars
    }

    #[tokio::test]
    async fn job_store_insert_and_get() {
        let store = BgJobStore::new();
        let job = BgJob {
            id: "j-test123".to_string(),
            tool_name: "test_tool".to_string(),
            sender: None,
            status: BgJobStatus::Running,
            result: None,
            error: None,
            started_at: Instant::now(),
            completed_at: None,
            delivered: false,
            delivered_at: None,
        };

        store.insert(job).await;
        let retrieved = store.get("j-test123").await;

        assert!(retrieved.is_some());
        assert_eq!(retrieved.unwrap().tool_name, "test_tool");
    }

    #[tokio::test]
    async fn job_store_update() {
        let store = BgJobStore::new();
        store
            .insert(BgJob {
                id: "j-update".to_string(),
                tool_name: "test".to_string(),
                sender: None,
                status: BgJobStatus::Running,
                result: None,
                error: None,
                started_at: Instant::now(),
                completed_at: None,
                delivered: false,
                delivered_at: None,
            })
            .await;

        store
            .update(
                "j-update",
                BgJobStatus::Complete,
                Some("done".to_string()),
                None,
            )
            .await;

        let job = store.get("j-update").await.unwrap();
        assert_eq!(job.status, BgJobStatus::Complete);
        assert_eq!(job.result, Some("done".to_string()));
        assert!(job.completed_at.is_some());
    }

    #[tokio::test]
    async fn job_store_drain_completed() {
        let store = BgJobStore::new();

        // Insert running job
        store
            .insert(BgJob {
                id: "j-running".to_string(),
                tool_name: "test".to_string(),
                sender: Some("user_a".to_string()),
                status: BgJobStatus::Running,
                result: None,
                error: None,
                started_at: Instant::now(),
                completed_at: None,
                delivered: false,
                delivered_at: None,
            })
            .await;

        // Insert completed job
        store
            .insert(BgJob {
                id: "j-done".to_string(),
                tool_name: "test".to_string(),
                sender: Some("user_a".to_string()),
                status: BgJobStatus::Complete,
                result: Some("output".to_string()),
                error: None,
                started_at: Instant::now(),
                completed_at: Some(Instant::now()),
                delivered: false,
                delivered_at: None,
            })
            .await;

        let drained = store.drain_completed(None).await;
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].id, "j-done");
        assert!(drained[0].delivered);

        // Second drain should return nothing (already delivered)
        let drained2 = store.drain_completed(None).await;
        assert!(drained2.is_empty());
    }

    #[test]
    fn format_bg_result() {
        let job = BgJob {
            id: "j-abc123".to_string(),
            tool_name: "scan_codebase".to_string(),
            sender: Some("test_user".to_string()),
            status: BgJobStatus::Complete,
            result: Some("Found 42 files".to_string()),
            error: None,
            started_at: Instant::now(),
            completed_at: Some(Instant::now()),
            delivered: true,
            delivered_at: Some(Instant::now()),
        };

        let formatted = format_bg_result_for_injection(&job);
        assert!(formatted.contains("j-abc123"));
        assert!(formatted.contains("scan_codebase"));
        assert!(formatted.contains("Found 42 files"));
        assert!(formatted.starts_with("<bg_result"));
        assert!(formatted.ends_with("</bg_result>"));
    }
}
