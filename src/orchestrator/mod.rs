pub mod commands;
pub mod systemd;

use crate::config::OrchestratorConfig;
use anyhow::{bail, Context, Result};
use chrono::Utc;
use commands::{parse_command, OrchestratorCommand};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tokio::time::{sleep, Duration};
use uuid::Uuid;

use self::systemd::SystemdController;

const DEFAULT_QUEUE_ROOT: &str = "/var/lib/clawpilot/queue";
const DEFAULT_RESULTS_ROOT: &str = "/var/lib/clawpilot/results";
const JOB_TIMEOUT_SECONDS: u64 = 120;

#[derive(Debug, Clone)]
pub struct Orchestrator {
    config: OrchestratorConfig,
    systemd: SystemdController,
    queue_root: PathBuf,
    results_root: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentJob {
    pub id: String,
    pub agent: String,
    pub text: String,
    pub created_at: String,
    #[serde(default)]
    pub workspace_path: Option<String>,
    #[serde(default)]
    pub global_instructions: Option<String>,
    #[serde(default)]
    pub folder_instructions: Option<Vec<FolderInstruction>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentJobResult {
    pub id: String,
    pub agent: String,
    pub status: String,
    pub summary: String,
    pub created_at: String,
    pub finished_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FolderInstruction {
    pub folder_path: String,
    pub instructions: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Queued,
    Running,
    Completed,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunToolEvent {
    pub tool: String,
    pub success: Option<bool>,
    pub duration_ms: Option<u64>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunArtifact {
    pub path: String,
    pub artifact_type: String,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunState {
    pub id: String,
    pub agent: String,
    pub workspace_path: String,
    pub goal: String,
    pub status: RunStatus,
    pub current_step: String,
    pub plan_state: String,
    pub tool_events: Vec<RunToolEvent>,
    pub file_changes: Vec<String>,
    pub artifacts: Vec<RunArtifact>,
    pub created_at: String,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub error: Option<String>,
}

struct RunObserver {
    state: Arc<Mutex<RunState>>,
    status_path: PathBuf,
    events_path: PathBuf,
}

impl RunObserver {
    fn new(state: Arc<Mutex<RunState>>, status_path: PathBuf, events_path: PathBuf) -> Self {
        Self {
            state,
            status_path,
            events_path,
        }
    }

    fn update_state<F>(&self, updater: F)
    where
        F: FnOnce(&mut RunState),
    {
        let cloned = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            updater(&mut state);
            state.clone()
        };
        let _ = write_json_file(&self.status_path, &cloned);
    }

    fn append_event(&self, event_type: &str, message: &str, payload: serde_json::Value) {
        let entry = serde_json::json!({
            "created_at": Utc::now().to_rfc3339(),
            "event_type": event_type,
            "message": message,
            "payload": payload,
        });
        let _ = append_jsonl_line(&self.events_path, &entry);
    }
}

impl crate::observability::Observer for RunObserver {
    fn record_event(&self, event: &crate::observability::ObserverEvent) {
        use crate::observability::ObserverEvent;

        match event {
            ObserverEvent::AgentStart { provider, model } => {
                self.update_state(|s| {
                    s.status = RunStatus::Running;
                    s.current_step = "agent_started".to_string();
                    s.plan_state = "running".to_string();
                    s.started_at = Some(Utc::now().to_rfc3339());
                });
                self.append_event(
                    "agent_start",
                    "Agent execution started",
                    serde_json::json!({ "provider": provider, "model": model }),
                );
            }
            ObserverEvent::LlmRequest { .. } => {
                self.update_state(|s| {
                    s.current_step = "llm_request".to_string();
                    s.plan_state = "thinking".to_string();
                });
                self.append_event("llm_request", "LLM request sent", serde_json::json!({}));
            }
            ObserverEvent::LlmResponse { success, .. } => {
                self.update_state(|s| {
                    s.current_step = "llm_response".to_string();
                    s.plan_state = if *success {
                        "interpreting".to_string()
                    } else {
                        "error".to_string()
                    };
                });
                self.append_event(
                    "llm_response",
                    "LLM response received",
                    serde_json::json!({ "success": success }),
                );
            }
            ObserverEvent::ToolCallStart { tool } => {
                self.update_state(|s| {
                    s.current_step = format!("tool:{tool}");
                    s.plan_state = "executing_tool".to_string();
                    s.tool_events.push(RunToolEvent {
                        tool: tool.clone(),
                        success: None,
                        duration_ms: None,
                        created_at: Utc::now().to_rfc3339(),
                    });
                });
                self.append_event(
                    "tool_start",
                    "Tool execution started",
                    serde_json::json!({ "tool": tool }),
                );
            }
            ObserverEvent::ToolCall {
                tool,
                duration,
                success,
            } => {
                self.update_state(|s| {
                    s.current_step = format!("tool:{tool}");
                    s.plan_state = "processing_tool_output".to_string();
                    s.tool_events.push(RunToolEvent {
                        tool: tool.clone(),
                        success: Some(*success),
                        duration_ms: Some(u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)),
                        created_at: Utc::now().to_rfc3339(),
                    });
                });
                self.append_event(
                    "tool_complete",
                    "Tool execution finished",
                    serde_json::json!({
                        "tool": tool,
                        "success": success,
                        "duration_ms": u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
                    }),
                );
            }
            ObserverEvent::TurnComplete => {
                self.update_state(|s| {
                    s.current_step = "turn_complete".to_string();
                    s.plan_state = "responding".to_string();
                });
                self.append_event("turn_complete", "Turn completed", serde_json::json!({}));
            }
            ObserverEvent::Error { component, message } => {
                self.update_state(|s| {
                    s.current_step = "error".to_string();
                    s.plan_state = "error".to_string();
                    s.error = Some(format!("{component}: {message}"));
                });
                self.append_event(
                    "error",
                    "Runtime error observed",
                    serde_json::json!({ "component": component, "message": message }),
                );
            }
            ObserverEvent::AgentEnd { .. } => {
                self.append_event("agent_end", "Agent execution finished", serde_json::json!({}));
            }
            ObserverEvent::ChannelMessage { .. } | ObserverEvent::HeartbeatTick => {}
        }
    }

    fn record_metric(&self, _metric: &crate::observability::traits::ObserverMetric) {}

    fn name(&self) -> &str {
        "run-observer"
    }
}

impl Orchestrator {
    pub fn from_config(config: OrchestratorConfig) -> Self {
        Self {
            config,
            systemd: SystemdController,
            queue_root: PathBuf::from(DEFAULT_QUEUE_ROOT),
            results_root: PathBuf::from(DEFAULT_RESULTS_ROOT),
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.config.enabled
    }

    pub async fn handle_message(&self, message: &str) -> Result<String> {
        let cmd = parse_command(message).map_err(|e| anyhow::anyhow!("{e}. Try /help"))?;
        match cmd {
            OrchestratorCommand::Help => Ok(self.help_text()),
            OrchestratorCommand::Status => self.status().await,
            OrchestratorCommand::Logs { agent, lines } => self.logs(&agent, lines).await,
            OrchestratorCommand::Restart { agent } => self.act(&agent, "restart").await,
            OrchestratorCommand::Start { agent } => self.act(&agent, "start").await,
            OrchestratorCommand::Stop { agent } => self.act(&agent, "stop").await,
            OrchestratorCommand::Run { agent, text } => self.run_job(&agent, &text).await,
        }
    }

    fn help_text(&self) -> String {
        format!(
            "Orchestrator commands:\n/help\n/status\n/logs <agent> [N]\n/start <agent>\n/stop <agent>\n/restart <agent>\n/run <agent> <text...>\n\nAllowed agents: {}",
            self.config.allowed_agents.join(", ")
        )
    }

    async fn status(&self) -> Result<String> {
        let mut out = String::from("Orchestrator service status:\n");
        for agent in &self.config.allowed_agents {
            let service = self.service_name(agent)?;
            let active = self
                .systemd
                .is_active(&service)
                .await
                .unwrap_or_else(|e| format!("error: {e}"));
            let logs = self
                .systemd
                .logs(&service, 2)
                .await
                .unwrap_or_else(|e| format!("log error: {e}"));
            out.push_str(&format!("\n- {agent}: {active}\n{logs}\n"));
        }
        Ok(out)
    }

    async fn logs(&self, agent: &str, lines: Option<usize>) -> Result<String> {
        let service = self.service_name(agent)?;
        let requested = lines.unwrap_or(self.config.max_log_lines);
        let safe_lines = requested.min(self.config.max_log_lines).max(1);
        let output = self.systemd.logs(&service, safe_lines).await?;
        Ok(format!("Logs for {agent} ({service}):\n{output}"))
    }

    async fn act(&self, agent: &str, action: &str) -> Result<String> {
        let service = self.service_name(agent)?;
        match action {
            "restart" => {
                self.systemd.restart(&service).await?;
            }
            "start" => {
                self.systemd.start(&service).await?;
            }
            "stop" => {
                self.systemd.stop(&service).await?;
            }
            _ => bail!("unsupported action"),
        }
        let active = self.systemd.is_active(&service).await.unwrap_or_default();
        Ok(format!("{action} requested for {service}. Current state: {active}"))
    }

    async fn run_job(&self, agent: &str, text: &str) -> Result<String> {
        self.ensure_allowed(agent)?;
        let job_id = Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();

        let job = AgentJob {
            id: job_id.clone(),
            agent: agent.to_string(),
            text: text.to_string(),
            created_at: now,
            workspace_path: None,
            global_instructions: None,
            folder_instructions: None,
        };

        let queue_dir = self.queue_root.join(agent);
        tokio::fs::create_dir_all(&queue_dir).await?;

        let queue_file = queue_dir.join(format!("{job_id}.json"));
        let payload = serde_json::to_vec_pretty(&job)?;
        tokio::fs::write(&queue_file, payload).await?;

        let result_dir = self.results_root.join(agent);
        tokio::fs::create_dir_all(&result_dir).await?;
        let result_file = result_dir.join(format!("{job_id}.json"));

        let deadline = std::time::Instant::now() + Duration::from_secs(JOB_TIMEOUT_SECONDS);
        while std::time::Instant::now() < deadline {
            if result_file.exists() {
                let content = tokio::fs::read_to_string(&result_file).await?;
                let parsed: AgentJobResult = serde_json::from_str(&content)
                    .with_context(|| format!("invalid result payload at {}", result_file.display()))?;
                return Ok(format!(
                    "Job {} completed: {}\nSummary: {}\nResult path: {}",
                    parsed.id,
                    parsed.status,
                    parsed.summary,
                    result_file.display()
                ));
            }
            sleep(Duration::from_secs(2)).await;
        }

        Ok(format!(
            "Job {job_id} queued for {agent}, still processing. Queue path: {}",
            queue_file.display()
        ))
    }

    fn service_name(&self, agent: &str) -> Result<String> {
        self.ensure_allowed(agent)?;
        if !is_safe_service_prefix(&self.config.service_prefix) {
            bail!("invalid orchestrator service_prefix")
        }
        Ok(format!("{}{}.service", self.config.service_prefix, agent))
    }

    fn ensure_allowed(&self, agent: &str) -> Result<()> {
        if !is_safe_name(agent) {
            bail!("invalid agent name")
        }
        if !self.config.allowed_agents.iter().any(|a| a == agent) {
            bail!("agent is not allowlisted")
        }
        Ok(())
    }
}

pub fn is_safe_name(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

pub async fn run_queue_worker(
    queue_dir: &Path,
    results_dir: &Path,
    config: crate::config::Config,
) -> Result<()> {
    tokio::fs::create_dir_all(queue_dir).await?;
    tokio::fs::create_dir_all(results_dir).await?;

    loop {
        let mut entries = tokio::fs::read_dir(queue_dir).await?;
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }

            let body = tokio::fs::read_to_string(&path).await?;
            let job: AgentJob = serde_json::from_str(&body)
                .with_context(|| format!("invalid job payload: {}", path.display()))?;

            let started = Utc::now().to_rfc3339();
            let workspace_path = job
                .workspace_path
                .clone()
                .unwrap_or_else(|| config.workspace_dir.display().to_string());

            let status_path = results_dir.join(format!("{}.status.json", job.id));
            let events_path = results_dir.join(format!("{}.events.jsonl", job.id));
            let initial_state = RunState {
                id: job.id.clone(),
                agent: job.agent.clone(),
                workspace_path: workspace_path.clone(),
                goal: job.text.clone(),
                status: RunStatus::Queued,
                current_step: "queued".to_string(),
                plan_state: "queued".to_string(),
                tool_events: Vec::new(),
                file_changes: Vec::new(),
                artifacts: Vec::new(),
                created_at: job.created_at.clone(),
                started_at: None,
                finished_at: None,
                error: None,
            };
            write_json_file(&status_path, &initial_state)?;

            let files_before = git_changed_files(Path::new(&workspace_path)).unwrap_or_default();

            let state_ref = Arc::new(Mutex::new(initial_state));
            let run_observer = Arc::new(RunObserver::new(
                state_ref.clone(),
                status_path.clone(),
                events_path,
            ));

            let result = crate::agent::run_with_observer(
                config.clone(),
                Some(job.text.clone()),
                None,
                None,
                config.default_temperature,
                vec![],
                Some(run_observer),
            )
            .await;

            let files_after = git_changed_files(Path::new(&workspace_path)).unwrap_or_default();
            let changed_files: Vec<String> = files_after
                .difference(&files_before)
                .cloned()
                .collect();

            let (status, summary, failed_message) = match result {
                Ok(()) => ("ok".to_string(), "job completed".to_string(), None),
                Err(e) => (
                    "error".to_string(),
                    format!("job failed: {e}"),
                    Some(e.to_string()),
                ),
            };

            {
                let mut state = state_ref
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                state.status = if status == "ok" {
                    RunStatus::Completed
                } else {
                    RunStatus::Failed
                };
                state.current_step = if status == "ok" {
                    "completed".to_string()
                } else {
                    "failed".to_string()
                };
                state.plan_state = if status == "ok" {
                    "done".to_string()
                } else {
                    "error".to_string()
                };
                state.finished_at = Some(Utc::now().to_rfc3339());
                state.file_changes = changed_files.clone();
                state.artifacts = changed_files
                    .iter()
                    .map(|path| RunArtifact {
                        path: path.clone(),
                        artifact_type: "changed_file".to_string(),
                        status: "updated".to_string(),
                    })
                    .collect();
                if let Some(msg) = failed_message {
                    state.error = Some(msg);
                }
            }
            let final_state = state_ref
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            write_json_file(&status_path, &final_state)?;

            let output = AgentJobResult {
                id: job.id.clone(),
                agent: job.agent,
                status,
                summary,
                created_at: started,
                finished_at: Utc::now().to_rfc3339(),
            };

            let result_path = results_dir.join(format!("{}.json", output.id));
            tokio::fs::write(&result_path, serde_json::to_vec_pretty(&output)?).await?;
            tokio::fs::remove_file(&path).await?;
        }

        sleep(Duration::from_secs(3)).await;
    }
}

fn is_safe_service_prefix(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '@')
}

fn write_json_file<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, serde_json::to_vec_pretty(value)?)?;
    Ok(())
}

fn append_jsonl_line(path: &Path, value: &serde_json::Value) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(file, "{}", serde_json::to_string(value)?)?;
    Ok(())
}

fn git_changed_files(workspace: &Path) -> Result<BTreeSet<String>> {
    if !workspace.join(".git").exists() {
        return Ok(BTreeSet::new());
    }

    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(workspace)
        .args(["status", "--porcelain"])
        .output()
        .context("failed to run git status")?;
    if !output.status.success() {
        return Ok(BTreeSet::new());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let files = stdout
        .lines()
        .filter_map(|line| line.get(3..).map(str::trim))
        .filter(|path| !path.is_empty())
        .map(ToOwned::to_owned)
        .collect();
    Ok(files)
}
