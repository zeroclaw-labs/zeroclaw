use std::sync::Arc;

use anyhow::Result;

use super::engine::now_iso8601;
use super::types::{SopRun, SopStepResult, SopStepStatus, SopTriggerSource};
use crate::agent::history::truncate_tool_result;
use crate::agent::turn::redact::scrub_credentials;
use zeroclaw_memory::traits::{Memory, MemoryCategory};

const SOP_CATEGORY: &str = "sop";
const MAX_STEP_LOG_OUTPUT_CHARS: usize = 4096;
const MAX_STEP_LOG_SUMMARY_CHARS: usize = 240;

pub struct SopAuditLogger {
    memory: Arc<dyn Memory>,
}

/// The audit logger a session's SOP tool writes through. An unowned
/// session keeps the logger its tools were built with. Once its session is
/// pinned to an owner, the logger is one over the owner's routed memory, so
/// the run payloads and step outputs an audit row carries stay on that
/// owner's plane rather than the shared one the daemon's logger writes. A
/// tool built without an audit logger stays without one.
/// The audit logger that may record a run with `owner`. An unowned run
/// keeps `audit`. An owned run keeps a logger already on its owner's plane,
/// gets the shared logger re-scoped to its owner, and gets no logger when
/// `audit` belongs to another owner, since that owner's plane is the wrong
/// place for its rows and the rows cannot be moved from there.
///
/// Every driver resolves a run's logger through this, from the owner stored
/// on the run, so the session or surface that happens to resume a run (an
/// approval on the daemon, another principal's session) cannot move its
/// payload and step outputs off the owner's plane.
pub(crate) fn audit_for_run(
    audit: Option<Arc<SopAuditLogger>>,
    owner: Option<&zeroclaw_api::memory_traits::PrincipalScope>,
) -> Option<Arc<SopAuditLogger>> {
    let audit = audit?;
    let Some(owner) = owner else {
        return Some(audit);
    };
    match audit.memory.principal_scope() {
        Some(scope) if scope == *owner => Some(audit),
        None => Some(Arc::new(SopAuditLogger::new(Arc::new(
            zeroclaw_memory::PrincipalPlaneMemory::new(Arc::clone(&audit.memory), owner.clone()),
        )))),
        Some(_) => {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                "SOP audit: this run belongs to another owner's memory plane; not auditing it here"
            );
            None
        }
    }
}

pub(crate) fn session_audit(
    captured: Option<&Arc<SopAuditLogger>>,
    session_memory: Option<&Arc<zeroclaw_tools::session_memory::SessionMemoryRoute>>,
) -> Option<Arc<SopAuditLogger>> {
    let captured = captured?;
    match session_memory.and_then(|route| route.routed()) {
        Some(routed) => Some(Arc::new(SopAuditLogger::new(Arc::clone(&routed.memory)))),
        None => Some(Arc::clone(captured)),
    }
}

impl SopAuditLogger {
    pub fn new(memory: Arc<dyn Memory>) -> Self {
        Self { memory }
    }

    /// Log the start of a new SOP run.
    pub async fn log_run_start(&self, run: &SopRun) -> Result<()> {
        let key = run_key(&run.run_id);
        let content = serde_json::to_string_pretty(run)?;
        self.memory.store(&key, &content, category(), None).await?;
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_attrs(::serde_json::json!({"run_id": run.run_id.as_str()})),
            &format!(
                "SOP audit: run {} started for '{}'",
                run.run_id, run.sop_name
            )
        );
        Ok(())
    }

    /// Log a step result.
    pub async fn log_step_result(&self, run_id: &str, result: &SopStepResult) -> Result<()> {
        let key = step_key(run_id, result.step_number);
        let content = serde_json::to_string_pretty(result)?;
        self.memory.store(&key, &content, category(), None).await?;

        // The durable audit record retains the canonical result. The structured
        // event is a bounded, credential-scrubbed projection for run-log surfaces.
        let output = truncate_tool_result(
            &scrub_credentials(&result.output),
            MAX_STEP_LOG_OUTPUT_CHARS,
        );
        let summary = truncate_tool_result(
            output
                .lines()
                .find(|line| !line.trim().is_empty())
                .unwrap_or(""),
            MAX_STEP_LOG_SUMMARY_CHARS,
        );
        let message = if summary.is_empty() {
            format!("SOP audit: step {} {}", result.step_number, result.status)
        } else {
            format!(
                "SOP audit: step {} {}: {summary}",
                result.step_number, result.status
            )
        };
        let event = ::zeroclaw_log::Event::new(
            module_path!(),
            match result.status {
                SopStepStatus::Completed | SopStepStatus::Skipped => {
                    ::zeroclaw_log::Action::Complete
                }
                SopStepStatus::Failed => ::zeroclaw_log::Action::Fail,
            },
        )
        .with_outcome(match result.status {
            SopStepStatus::Completed => ::zeroclaw_log::EventOutcome::Success,
            SopStepStatus::Failed => ::zeroclaw_log::EventOutcome::Failure,
            SopStepStatus::Skipped => ::zeroclaw_log::EventOutcome::Unknown,
        })
        .with_attrs(::serde_json::json!({
            "run_id": run_id,
            "step": result.step_number,
            "status": result.status.to_string(),
            "effective_agent": result.effective_agent.as_deref(),
            "tool_call_count": result.tool_calls.len(),
            "output": output,
        }));
        match result.status {
            SopStepStatus::Failed => ::zeroclaw_log::record!(WARN, event, &message),
            SopStepStatus::Completed | SopStepStatus::Skipped => {
                ::zeroclaw_log::record!(INFO, event, &message)
            }
        }
        Ok(())
    }

    /// Log a suspicious but allowed untrusted SOP event.
    pub async fn log_suspicious_untrusted(
        &self,
        source: SopTriggerSource,
        topic: Option<&str>,
        patterns: &[String],
        score: f64,
    ) -> Result<()> {
        let now = now_iso8601();
        let key = event_key("suspicious_untrusted", &now);
        let content = serde_json::to_string_pretty(&serde_json::json!({
            "kind": "suspicious_untrusted",
            "source": source,
            "topic": topic,
            "patterns": patterns,
            "score": score,
            "timestamp": now,
        }))?;
        self.memory.store(&key, &content, category(), None).await?;
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                .with_attrs(::serde_json::json!({
                    "source": source,
                    "topic": topic,
                    "patterns": patterns,
                    "score": score,
                })),
            "SOP audit: suspicious untrusted trigger content allowed"
        );
        Ok(())
    }

    /// Log a blocked unsafe SOP event.
    pub async fn log_blocked_unsafe(
        &self,
        sop_name: Option<&str>,
        source: SopTriggerSource,
        topic: Option<&str>,
        reason: &str,
    ) -> Result<()> {
        let now = now_iso8601();
        let key = event_key("blocked_unsafe", &now);
        let content = serde_json::to_string_pretty(&serde_json::json!({
            "kind": "blocked_unsafe",
            "sop_name": sop_name,
            "source": source,
            "topic": topic,
            "reason": reason,
            "timestamp": now,
        }))?;
        self.memory.store(&key, &content, category(), None).await?;
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                .with_attrs(::serde_json::json!({
                    "sop_name": sop_name,
                    "source": source,
                    "topic": topic,
                    "reason": reason,
                })),
            "SOP audit: blocked unsafe untrusted trigger content"
        );
        Ok(())
    }

    /// Log run completion (updates the run record with final state).
    pub async fn log_run_complete(&self, run: &SopRun) -> Result<()> {
        let key = run_key(&run.run_id);
        let content = serde_json::to_string_pretty(run)?;
        self.memory.store(&key, &content, category(), None).await?;
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_attrs(::serde_json::json!({"run_id": run.run_id.as_str()})),
            &format!(
                "SOP audit: run {} finished with status {}",
                run.run_id, run.status
            )
        );
        Ok(())
    }

    /// Retrieve a stored run by ID (if it exists in memory).
    pub async fn get_run(&self, run_id: &str) -> Result<Option<SopRun>> {
        let key = run_key(run_id);
        match self.memory.get(&key).await? {
            Some(entry) => {
                let run: SopRun = serde_json::from_str(&entry.content).map_err(|e| {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                            .with_attrs(
                                ::serde_json::json!({"error": format!("{}", e), "run_id": run_id})
                            ),
                        "SOP audit: failed to parse run "
                    );
                    e
                })?;
                Ok(Some(run))
            }
            None => Ok(None),
        }
    }

    /// List all stored SOP run keys.
    pub async fn list_runs(&self) -> Result<Vec<String>> {
        let entries = self.memory.list(Some(&category()), None).await?;
        let run_keys: Vec<String> = entries
            .into_iter()
            .filter(|e| e.key.starts_with("sop_run_"))
            .map(|e| e.key)
            .collect();
        Ok(run_keys)
    }
}

fn run_key(run_id: &str) -> String {
    format!("sop_run_{run_id}")
}

fn step_key(run_id: &str, step_number: u32) -> String {
    format!("sop_step_{run_id}_{step_number}")
}

fn event_key(kind: &str, timestamp: &str) -> String {
    let safe_timestamp: String = timestamp
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '_' })
        .collect();
    let suffix = rand::random::<u32>();
    format!("sop_event_{kind}_{safe_timestamp}_{suffix:08x}")
}

fn category() -> MemoryCategory {
    MemoryCategory::Custom(SOP_CATEGORY.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_runs_audit_is_resolved_from_its_owner() {
        use zeroclaw_api::memory_traits::PrincipalScope;
        let shared: Arc<dyn Memory> = Arc::new(zeroclaw_memory::NoneMemory::new("shared"));
        let shared_audit = Arc::new(SopAuditLogger::new(Arc::clone(&shared)));
        let alice = PrincipalScope::new("user:alice").with_agent(Some("a".into()));
        let bob = PrincipalScope::new("user:bob").with_agent(Some("a".into()));

        let unowned = audit_for_run(Some(Arc::clone(&shared_audit)), None).unwrap();
        assert!(
            Arc::ptr_eq(&unowned, &shared_audit),
            "an unowned run keeps the logger"
        );

        let rescoped = audit_for_run(Some(Arc::clone(&shared_audit)), Some(&alice)).unwrap();
        assert_eq!(
            rescoped.memory.principal_scope(),
            Some(alice.clone()),
            "the shared logger is re-scoped to the run's owner"
        );

        let alices: Arc<dyn Memory> = Arc::new(zeroclaw_memory::PrincipalPlaneMemory::new(
            Arc::clone(&shared),
            alice.clone(),
        ));
        let alices_audit = Arc::new(SopAuditLogger::new(alices));
        let kept = audit_for_run(Some(Arc::clone(&alices_audit)), Some(&alice)).unwrap();
        assert!(
            Arc::ptr_eq(&kept, &alices_audit),
            "the owner's own logger is kept"
        );
        assert!(
            audit_for_run(Some(alices_audit), Some(&bob)).is_none(),
            "another owner's logger may not record the run"
        );
        assert!(audit_for_run(None, Some(&alice)).is_none());
    }

    #[test]
    fn a_session_audit_follows_its_pinned_route() {
        let shared: Arc<dyn Memory> = Arc::new(zeroclaw_memory::NoneMemory::new("shared"));
        let captured = Arc::new(SopAuditLogger::new(Arc::clone(&shared)));
        let route = Arc::new(zeroclaw_tools::session_memory::SessionMemoryRoute::default());
        let unpinned = session_audit(Some(&captured), Some(&route)).unwrap();
        assert!(
            Arc::ptr_eq(&unpinned, &captured),
            "unowned: the captured logger"
        );
        let owner: Arc<dyn Memory> = Arc::new(zeroclaw_memory::NoneMemory::new("owner"));
        route
            .pin(
                Arc::clone(&owner),
                Arc::new(zeroclaw_config::policy::SecurityPolicy::default()),
            )
            .unwrap();
        let pinned = session_audit(Some(&captured), Some(&route)).unwrap();
        assert!(
            Arc::ptr_eq(&pinned.memory, &owner),
            "pinned: a logger over the owner's memory"
        );
        assert!(
            session_audit(None, Some(&route)).is_none(),
            "no captured logger, no audit"
        );
    }
    use crate::sop::types::{SopEvent, SopRunStatus, SopStepStatus, SopTriggerSource};

    fn test_run() -> SopRun {
        SopRun {
            run_id: "run-test-001".into(),
            sop_name: "test-sop".into(),
            initiating_agent: None,
            memory_owner: None,
            trigger_event: SopEvent {
                source: SopTriggerSource::Manual,
                topic: None,
                payload: None,
                timestamp: "2026-02-19T12:00:00Z".into(),
            },
            frame_marker_id: "marker-test".into(),
            status: SopRunStatus::Running,
            current_step: 1,
            total_steps: 3,
            started_at: "2026-02-19T12:00:00Z".into(),
            completed_at: None,
            failure_reason: None,
            step_results: Vec::new(),
            waiting_since: None,
            llm_calls_saved: 0,
            revision: 0,
            revision_base: 0,
            decided_mode: None,
            decisions: std::collections::BTreeMap::new(),
        }
    }

    fn test_step_result(n: u32) -> SopStepResult {
        SopStepResult {
            effective_agent: None,
            step_number: n,
            status: SopStepStatus::Completed,
            output: format!("Step {n} completed"),
            started_at: "2026-02-19T12:00:00Z".into(),
            completed_at: Some("2026-02-19T12:00:05Z".into()),
            tool_calls: Vec::new(),
        }
    }

    #[tokio::test]
    async fn audit_roundtrip() {
        let mem_cfg = zeroclaw_config::schema::MemoryConfig {
            backend: "sqlite".into(),
            ..zeroclaw_config::schema::MemoryConfig::default()
        };
        let tmp = tempfile::tempdir().unwrap();
        let memory: Arc<dyn Memory> =
            Arc::from(zeroclaw_memory::create_memory(&mem_cfg, tmp.path(), None).unwrap());

        let logger = SopAuditLogger::new(memory);

        // Log run start
        let run = test_run();
        logger.log_run_start(&run).await.unwrap();

        // Log step result
        let step = test_step_result(1);
        logger.log_step_result(&run.run_id, &step).await.unwrap();

        // Log run complete
        let mut completed_run = run.clone();
        completed_run.status = SopRunStatus::Completed;
        completed_run.completed_at = Some("2026-02-19T12:05:00Z".into());
        completed_run.step_results = vec![step];
        logger.log_run_complete(&completed_run).await.unwrap();

        // Retrieve
        let retrieved = logger.get_run("run-test-001").await.unwrap().unwrap();
        assert_eq!(retrieved.run_id, "run-test-001");
        assert_eq!(retrieved.status, SopRunStatus::Completed);
        assert_eq!(retrieved.step_results.len(), 1);

        // List runs
        let keys = logger.list_runs().await.unwrap();
        assert!(keys.contains(&"sop_run_run-test-001".to_string()));
    }

    #[tokio::test]
    async fn get_nonexistent_run_returns_none() {
        let mem_cfg = zeroclaw_config::schema::MemoryConfig {
            backend: "sqlite".into(),
            ..zeroclaw_config::schema::MemoryConfig::default()
        };
        let tmp = tempfile::tempdir().unwrap();
        let memory: Arc<dyn Memory> =
            Arc::from(zeroclaw_memory::create_memory(&mem_cfg, tmp.path(), None).unwrap());

        let logger = SopAuditLogger::new(memory);
        let result = logger.get_run("nonexistent").await.unwrap();
        assert!(result.is_none());
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn step_result_emits_bounded_scrubbed_run_log_event() {
        let _writer_guard = zeroclaw_log::__private_test_writer_lock();
        let _hook_guard = zeroclaw_log::__private_test_hook_lock();
        zeroclaw_log::try_install_capture_subscriber();
        let mut rx = zeroclaw_log::subscribe_or_install();
        while rx.try_recv().is_ok() {}

        let mem_cfg = zeroclaw_config::schema::MemoryConfig {
            backend: "sqlite".into(),
            ..zeroclaw_config::schema::MemoryConfig::default()
        };
        let tmp = tempfile::tempdir().unwrap();
        let memory: Arc<dyn Memory> =
            Arc::from(zeroclaw_memory::create_memory(&mem_cfg, tmp.path(), None).unwrap());
        let logger = SopAuditLogger::new(memory);
        let secret = "super-secret-credential";
        let mut step = test_step_result(2);
        step.output = format!("token={secret}\n{}", "x".repeat(6000));

        logger
            .log_step_result("run-log-proof", &step)
            .await
            .unwrap();

        let mut selected = None;
        loop {
            match rx.try_recv() {
                Ok(value)
                    if value
                        .get("message")
                        .and_then(serde_json::Value::as_str)
                        .is_some_and(|message| {
                            message.starts_with("SOP audit: step 2 completed")
                        }) =>
                {
                    selected = Some(value);
                    break;
                }
                Ok(_) | Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => {}
                Err(tokio::sync::broadcast::error::TryRecvError::Empty) => break,
                Err(tokio::sync::broadcast::error::TryRecvError::Closed) => break,
            }
        }
        let event = selected.expect("step result should emit a structured log event");
        assert_eq!(event["attributes"]["run_id"], "run-log-proof");
        assert_eq!(event["attributes"]["step"], 2);
        assert_eq!(event["event"]["action"], "complete");
        assert_eq!(event["event"]["outcome"], "success");
        let output = event["attributes"]["output"].as_str().unwrap();
        assert!(output.contains("[REDACTED]"));
        assert!(!output.contains(secret));
        assert!(output.chars().count() <= MAX_STEP_LOG_OUTPUT_CHARS + 64);
        assert!(output.contains("truncated"));

        zeroclaw_log::clear_broadcast_hook();
    }
}
