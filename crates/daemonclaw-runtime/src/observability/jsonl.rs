use super::traits::{Observer, ObserverEvent, ObserverMetric};
use chrono::Local;
use serde::Serialize;
use std::any::Any;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JsonlStorageMode {
    Rolling,
    Full,
}

#[derive(Serialize)]
struct JsonlEntry {
    timestamp: String,
    event_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    channel: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    hand: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    success: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    duration_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tokens_in: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tokens_out: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cost_usd: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    direction: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
}

pub struct JsonlObserver {
    path: PathBuf,
    mode: JsonlStorageMode,
    max_entries: usize,
    write_lock: Mutex<()>,
}

impl JsonlObserver {
    pub fn new(path: PathBuf, mode: JsonlStorageMode, max_entries: usize) -> Self {
        Self {
            path,
            mode,
            max_entries: max_entries.max(1),
            write_lock: Mutex::new(()),
        }
    }

    fn append(&self, entry: &JsonlEntry) {
        let _guard = self.write_lock.lock().unwrap_or_else(|e| e.into_inner());

        if let Some(parent) = self.path.parent() {
            let _ = fs::create_dir_all(parent);
        }

        let line = match serde_json::to_string(entry) {
            Ok(l) => l,
            Err(_) => return,
        };

        let mut opts = OpenOptions::new();
        opts.create(true).append(true);

        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o640);
        }

        let Ok(mut file) = opts.open(&self.path) else {
            return;
        };
        let _ = writeln!(file, "{line}");
        let _ = file.sync_data();

        if self.mode == JsonlStorageMode::Rolling {
            self.trim();
        }
    }

    fn trim(&self) {
        let raw = fs::read_to_string(&self.path).unwrap_or_default();
        let lines: Vec<&str> = raw.lines().filter(|l| !l.trim().is_empty()).collect();
        if lines.len() <= self.max_entries {
            return;
        }
        let keep_from = lines.len().saturating_sub(self.max_entries);
        let mut kept = lines[keep_from..].join("\n");
        kept.push('\n');

        let tmp = self.path.with_extension("tmp");
        if fs::write(&tmp, &kept).is_ok() {
            let _ = fs::rename(&tmp, &self.path);
        }
    }

    fn dur_ms(d: &Duration) -> u64 {
        u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
    }
}

impl Observer for JsonlObserver {
    fn record_event(&self, event: &ObserverEvent) {
        let entry = match event {
            ObserverEvent::AgentStart { provider, model } => JsonlEntry {
                timestamp: Local::now().to_rfc3339(),
                event_type: "agent.start".into(),
                provider: Some(provider.clone()),
                model: Some(model.clone()),
                ..default_entry()
            },
            ObserverEvent::AgentEnd {
                provider,
                model,
                duration,
                tokens_used,
                cost_usd,
            } => JsonlEntry {
                timestamp: Local::now().to_rfc3339(),
                event_type: "agent.end".into(),
                provider: Some(provider.clone()),
                model: Some(model.clone()),
                duration_ms: Some(Self::dur_ms(duration)),
                tokens_in: *tokens_used,
                cost_usd: *cost_usd,
                ..default_entry()
            },
            ObserverEvent::LlmRequest {
                provider,
                model,
                messages_count,
            } => JsonlEntry {
                timestamp: Local::now().to_rfc3339(),
                event_type: "llm.request".into(),
                provider: Some(provider.clone()),
                model: Some(model.clone()),
                detail: Some(format!("messages={messages_count}")),
                ..default_entry()
            },
            ObserverEvent::LlmResponse {
                provider,
                model,
                duration,
                success,
                error_message,
                input_tokens,
                output_tokens,
            } => JsonlEntry {
                timestamp: Local::now().to_rfc3339(),
                event_type: "llm.response".into(),
                provider: Some(provider.clone()),
                model: Some(model.clone()),
                duration_ms: Some(Self::dur_ms(duration)),
                success: Some(*success),
                message: error_message.clone(),
                tokens_in: *input_tokens,
                tokens_out: *output_tokens,
                ..default_entry()
            },
            ObserverEvent::ToolCallStart { tool, arguments } => JsonlEntry {
                timestamp: Local::now().to_rfc3339(),
                event_type: "tool.start".into(),
                tool: Some(tool.clone()),
                detail: arguments.clone(),
                ..default_entry()
            },
            ObserverEvent::ToolCall {
                tool,
                duration,
                success,
            } => JsonlEntry {
                timestamp: Local::now().to_rfc3339(),
                event_type: "tool.call".into(),
                tool: Some(tool.clone()),
                duration_ms: Some(Self::dur_ms(duration)),
                success: Some(*success),
                ..default_entry()
            },
            ObserverEvent::TurnComplete => JsonlEntry {
                timestamp: Local::now().to_rfc3339(),
                event_type: "turn.complete".into(),
                ..default_entry()
            },
            ObserverEvent::ChannelMessage { channel, direction } => JsonlEntry {
                timestamp: Local::now().to_rfc3339(),
                event_type: "channel.message".into(),
                channel: Some(channel.clone()),
                direction: Some(direction.clone()),
                ..default_entry()
            },
            ObserverEvent::HeartbeatTick => JsonlEntry {
                timestamp: Local::now().to_rfc3339(),
                event_type: "heartbeat.tick".into(),
                ..default_entry()
            },
            ObserverEvent::CacheHit {
                cache_type,
                tokens_saved,
            } => JsonlEntry {
                timestamp: Local::now().to_rfc3339(),
                event_type: "cache.hit".into(),
                detail: Some(format!("type={cache_type}")),
                tokens_in: Some(*tokens_saved),
                ..default_entry()
            },
            ObserverEvent::CacheMiss { cache_type } => JsonlEntry {
                timestamp: Local::now().to_rfc3339(),
                event_type: "cache.miss".into(),
                detail: Some(format!("type={cache_type}")),
                ..default_entry()
            },
            ObserverEvent::Error { component, message } => JsonlEntry {
                timestamp: Local::now().to_rfc3339(),
                event_type: "error".into(),
                detail: Some(component.clone()),
                message: Some(message.clone()),
                ..default_entry()
            },
            ObserverEvent::HandStarted { hand_name } => JsonlEntry {
                timestamp: Local::now().to_rfc3339(),
                event_type: "hand.started".into(),
                hand: Some(hand_name.clone()),
                ..default_entry()
            },
            ObserverEvent::HandCompleted {
                hand_name,
                duration_ms,
                findings_count,
            } => JsonlEntry {
                timestamp: Local::now().to_rfc3339(),
                event_type: "hand.completed".into(),
                hand: Some(hand_name.clone()),
                duration_ms: Some(*duration_ms),
                detail: Some(format!("findings={findings_count}")),
                ..default_entry()
            },
            ObserverEvent::HandFailed {
                hand_name,
                error,
                duration_ms,
            } => JsonlEntry {
                timestamp: Local::now().to_rfc3339(),
                event_type: "hand.failed".into(),
                hand: Some(hand_name.clone()),
                duration_ms: Some(*duration_ms),
                message: Some(error.clone()),
                success: Some(false),
                ..default_entry()
            },
            ObserverEvent::DeploymentStarted { deploy_id } => JsonlEntry {
                timestamp: Local::now().to_rfc3339(),
                event_type: "deployment.started".into(),
                detail: Some(deploy_id.clone()),
                ..default_entry()
            },
            ObserverEvent::DeploymentCompleted {
                deploy_id,
                commit_sha,
            } => JsonlEntry {
                timestamp: Local::now().to_rfc3339(),
                event_type: "deployment.completed".into(),
                detail: Some(format!("{deploy_id} @ {commit_sha}")),
                success: Some(true),
                ..default_entry()
            },
            ObserverEvent::DeploymentFailed { deploy_id, reason } => JsonlEntry {
                timestamp: Local::now().to_rfc3339(),
                event_type: "deployment.failed".into(),
                detail: Some(deploy_id.clone()),
                message: Some(reason.clone()),
                success: Some(false),
                ..default_entry()
            },
            ObserverEvent::RecoveryCompleted { deploy_id } => JsonlEntry {
                timestamp: Local::now().to_rfc3339(),
                event_type: "recovery.completed".into(),
                detail: Some(deploy_id.clone()),
                ..default_entry()
            },
            ObserverEvent::SkillCreated { skill_name } => JsonlEntry {
                timestamp: Local::now().to_rfc3339(),
                event_type: "skill.created".into(),
                detail: Some(skill_name.clone()),
                ..default_entry()
            },
            ObserverEvent::SkillPatched {
                skill_name,
                sections_changed,
            } => JsonlEntry {
                timestamp: Local::now().to_rfc3339(),
                event_type: "skill.patched".into(),
                detail: Some(format!("{skill_name}: {}", sections_changed.join(", "))),
                ..default_entry()
            },
            ObserverEvent::SkillArchived { skill_name } => JsonlEntry {
                timestamp: Local::now().to_rfc3339(),
                event_type: "skill.archived".into(),
                detail: Some(skill_name.clone()),
                ..default_entry()
            },
            ObserverEvent::SkillRestored { skill_name } => JsonlEntry {
                timestamp: Local::now().to_rfc3339(),
                event_type: "skill.restored".into(),
                detail: Some(skill_name.clone()),
                ..default_entry()
            },
            ObserverEvent::UserModelUpdated { fields_changed } => JsonlEntry {
                timestamp: Local::now().to_rfc3339(),
                event_type: "user_model.updated".into(),
                detail: Some(fields_changed.join(", ")),
                ..default_entry()
            },
            ObserverEvent::CuratorRunCompleted {
                skills_reviewed,
                skills_archived,
                skills_consolidated,
            } => JsonlEntry {
                timestamp: Local::now().to_rfc3339(),
                event_type: "curator.run_completed".into(),
                detail: Some(format!(
                    "reviewed={skills_reviewed} archived={skills_archived} consolidated={skills_consolidated}"
                )),
                ..default_entry()
            },
        };

        self.append(&entry);
    }

    fn record_metric(&self, _metric: &ObserverMetric) {
        // Metrics are high-frequency; skip them to keep the log readable.
        // Use prometheus/otel backends for metric aggregation.
    }

    fn name(&self) -> &str {
        "jsonl"
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

fn default_entry() -> JsonlEntry {
    JsonlEntry {
        timestamp: String::new(),
        event_type: String::new(),
        provider: None,
        model: None,
        channel: None,
        tool: None,
        hand: None,
        success: None,
        duration_ms: None,
        message: None,
        tokens_in: None,
        tokens_out: None,
        cost_usd: None,
        direction: None,
        detail: None,
    }
}

/// Resolve JSONL log path from config, relative to workspace_dir.
pub fn resolve_jsonl_path(raw: &str, workspace_dir: &Path) -> PathBuf {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return workspace_dir.join("state/events.jsonl");
    }
    let p = PathBuf::from(trimmed);
    if p.is_absolute() {
        p
    } else {
        workspace_dir.join(p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn jsonl_observer_writes_events() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("events.jsonl");
        let obs = JsonlObserver::new(path.clone(), JsonlStorageMode::Full, 1000);

        obs.record_event(&ObserverEvent::LlmResponse {
            provider: "zai".into(),
            model: "glm-5".into(),
            duration: Duration::from_millis(200),
            success: true,
            error_message: None,
            input_tokens: Some(100),
            output_tokens: Some(50),
        });
        obs.record_event(&ObserverEvent::Error {
            component: "gateway".into(),
            message: "port in use".into(),
        });

        let content = fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("llm.response"));
        assert!(lines[1].contains("error"));
        assert!(lines[1].contains("port in use"));
    }

    #[test]
    fn jsonl_rolling_trims() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("events.jsonl");
        let obs = JsonlObserver::new(path.clone(), JsonlStorageMode::Rolling, 3);

        for i in 0..6 {
            obs.record_event(&ObserverEvent::Error {
                component: "test".into(),
                message: format!("err-{i}"),
            });
        }

        let content = fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = content.lines().filter(|l| !l.is_empty()).collect();
        assert_eq!(lines.len(), 3);
        assert!(lines[0].contains("err-3"));
        assert!(lines[2].contains("err-5"));
    }

    #[test]
    fn jsonl_skips_metrics() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("events.jsonl");
        let obs = JsonlObserver::new(path.clone(), JsonlStorageMode::Full, 1000);

        obs.record_metric(&ObserverMetric::TokensUsed(42));

        assert!(!path.exists());
    }

    #[test]
    fn resolve_path_relative() {
        let ws = PathBuf::from("/var/lib/daemonclaw/.daemonclaw/workspace");
        assert_eq!(
            resolve_jsonl_path("state/events.jsonl", &ws),
            ws.join("state/events.jsonl")
        );
    }

    #[test]
    fn resolve_path_absolute() {
        let ws = PathBuf::from("/var/lib/daemonclaw/.daemonclaw/workspace");
        assert_eq!(
            resolve_jsonl_path("/tmp/events.jsonl", &ws),
            PathBuf::from("/tmp/events.jsonl")
        );
    }
}
