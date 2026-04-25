use anyhow::Result;
use chrono::Local;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use uuid::Uuid;
use zeroclaw_config::schema::{FileRotationConfig, ObservabilityConfig};
use zeroclaw_file_rotation::{RotatingFileWriter, RotationConfig};

const DEFAULT_TRACE_REL_PATH: &str = "state/runtime-trace.jsonl";

/// Runtime trace storage policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeTraceStorageMode {
    None,
    Rolling,
    Full,
}

impl RuntimeTraceStorageMode {
    fn from_raw(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "rolling" => Self::Rolling,
            "full" => Self::Full,
            _ => Self::None,
        }
    }
}

/// Structured runtime trace event for tool-call and model-reply diagnostics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeTraceEvent {
    pub id: String,
    pub timestamp: String,
    pub event_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub success: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default)]
    pub payload: Value,
}

struct RuntimeTraceLogger {
    mode: RuntimeTraceStorageMode,
    writer: RotatingFileWriter,
}

impl RuntimeTraceLogger {
    async fn new(
        mode: RuntimeTraceStorageMode,
        path: std::path::PathBuf,
        rotation_config: &FileRotationConfig,
    ) -> Result<Self> {
        let rot_cfg = RotationConfig {
            max_file_size_bytes: rotation_config.max_file_size_mb * 1024 * 1024,
            max_age_days: rotation_config.max_age_days,
            max_rotated_files: rotation_config.max_rotated_files,
            #[cfg(unix)]
            file_permissions: Some(0o600),
            sync_on_write: true,
        };

        let writer = RotatingFileWriter::new(path.clone(), rot_cfg).await?;

        Ok(Self { mode, writer })
    }

    async fn append(&self, event: &RuntimeTraceEvent) -> Result<()> {
        if self.mode == RuntimeTraceStorageMode::None {
            return Ok(());
        }

        let line = serde_json::to_string(event)?;
        self.writer.append(line).await?;

        Ok(())
    }

    async fn shutdown(&self) -> Result<()> {
        self.writer.shutdown().await?;
        Ok(())
    }
}

static TRACE_LOGGER: std::sync::LazyLock<tokio::sync::RwLock<Option<Arc<RuntimeTraceLogger>>>> =
    std::sync::LazyLock::new(|| tokio::sync::RwLock::new(None));

/// Resolve runtime trace storage mode from config.
pub fn storage_mode_from_config(config: &ObservabilityConfig) -> RuntimeTraceStorageMode {
    let mode = RuntimeTraceStorageMode::from_raw(&config.runtime_trace_mode);
    if mode == RuntimeTraceStorageMode::None
        && !config.runtime_trace_mode.trim().is_empty()
        && !config.runtime_trace_mode.eq_ignore_ascii_case("none")
    {
        tracing::warn!(
            mode = %config.runtime_trace_mode,
            "Unknown observability.runtime_trace_mode; falling back to none"
        );
    }
    mode
}

/// Resolve runtime trace path from config.
pub fn resolve_trace_path(config: &ObservabilityConfig, workspace_dir: &Path) -> PathBuf {
    let raw = config.runtime_trace_path.trim();
    let fallback = workspace_dir.join(DEFAULT_TRACE_REL_PATH);
    if raw.is_empty() {
        return fallback;
    }

    let configured = PathBuf::from(raw);
    if configured.is_absolute() {
        configured
    } else {
        workspace_dir.join(configured)
    }
}

/// Initialize (or disable) runtime trace logging.
pub async fn init_from_config(config: &ObservabilityConfig, workspace_dir: &Path) {
    let mode = storage_mode_from_config(config);
    let logger = if mode == RuntimeTraceStorageMode::None {
        None
    } else {
        match RuntimeTraceLogger::new(
            mode,
            resolve_trace_path(config, workspace_dir),
            &config.runtime_trace_rotation,
        )
        .await
        {
            Ok(l) => Some(Arc::new(l)),
            Err(err) => {
                tracing::warn!("Failed to initialize runtime trace logger: {err}");
                None
            }
        }
    };

    let mut guard = TRACE_LOGGER.write().await;
    *guard = logger;
}

/// Record a runtime trace event.
pub fn record_event(
    event_type: &str,
    channel: Option<&str>,
    provider: Option<&str>,
    model: Option<&str>,
    turn_id: Option<&str>,
    success: Option<bool>,
    message: Option<&str>,
    payload: Value,
) {
    let logger = TRACE_LOGGER.try_read().ok().and_then(|g| g.clone());

    let Some(logger) = logger else {
        return;
    };

    let event = RuntimeTraceEvent {
        id: Uuid::new_v4().to_string(),
        timestamp: Local::now().to_rfc3339(),
        event_type: event_type.to_string(),
        channel: channel.map(str::to_string),
        provider: provider.map(str::to_string),
        model: model.map(str::to_string),
        turn_id: turn_id.map(str::to_string),
        success,
        message: message.map(str::to_string),
        payload,
    };

    // Spawn a fire-and-forget task so append() can run on the tokio runtime
    // without blocking the caller (which may be on a non-async code path).
    tokio::spawn(async move {
        if let Err(err) = logger.append(&event).await {
            tracing::warn!("Failed to write runtime trace event: {err}");
        }
    });
}

/// Gracefully shut down the trace logger, flushing any buffered writes.
pub async fn shutdown() {
    let logger = TRACE_LOGGER.write().await.take();

    if let Some(logger) = logger
        && let Err(err) = logger.shutdown().await
    {
        tracing::warn!("Failed to shut down runtime trace logger: {err}");
    }
}

/// Load recent runtime trace events from storage.
pub fn load_events(
    path: &Path,
    limit: usize,
    event_filter: Option<&str>,
    contains: Option<&str>,
) -> Result<Vec<RuntimeTraceEvent>> {
    if !path.exists() {
        return Ok(Vec::new());
    }

    let raw = std::fs::read_to_string(path)?;
    let mut events = Vec::new();

    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        match serde_json::from_str::<RuntimeTraceEvent>(trimmed) {
            Ok(event) => events.push(event),
            Err(err) => tracing::warn!("Skipping malformed runtime trace line: {err}"),
        }
    }

    if let Some(filter) = event_filter.map(str::trim).filter(|f| !f.is_empty()) {
        let normalized = filter.to_ascii_lowercase();
        events.retain(|event| event.event_type.to_ascii_lowercase() == normalized);
    }

    if let Some(needle) = contains.map(str::trim).filter(|s| !s.is_empty()) {
        let needle = needle.to_ascii_lowercase();
        events.retain(|event| {
            let mut haystack = format!(
                "{} {} {}",
                event.event_type,
                event.message.as_deref().unwrap_or_default(),
                event.payload
            );
            if let Some(channel) = &event.channel {
                haystack.push_str(channel);
            }
            if let Some(provider) = &event.provider {
                haystack.push_str(provider);
            }
            if let Some(model) = &event.model {
                haystack.push_str(model);
            }
            haystack.to_ascii_lowercase().contains(&needle)
        });
    }

    if events.len() > limit {
        let keep_from = events.len() - limit;
        events = events.split_off(keep_from);
    }

    events.reverse();
    Ok(events)
}

/// Find a runtime trace event by id.
pub fn find_event_by_id(path: &Path, id: &str) -> Result<Option<RuntimeTraceEvent>> {
    if !path.exists() {
        return Ok(None);
    }

    let raw = std::fs::read_to_string(path)?;
    for line in raw.lines().rev() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Ok(event) = serde_json::from_str::<RuntimeTraceEvent>(trimmed)
            && event.id == id
        {
            return Ok(Some(event));
        }
    }

    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn test_observability_config() -> ObservabilityConfig {
        ObservabilityConfig {
            backend: "none".to_string(),
            otel_endpoint: None,
            otel_service_name: None,
            otel_headers: None,
            runtime_trace_mode: "rolling".to_string(),
            runtime_trace_path: "state/runtime-trace.jsonl".to_string(),
            runtime_trace_max_entries: 3,
            runtime_trace_rotation: FileRotationConfig::default(),
        }
    }

    #[test]
    fn resolve_trace_path_relative_joins_workspace() {
        let cfg = test_observability_config();
        let workspace = tempfile::tempdir().unwrap();
        let path = resolve_trace_path(&cfg, workspace.path());
        assert_eq!(path, workspace.path().join("state/runtime-trace.jsonl"));
    }

    #[test]
    fn storage_mode_parses_known_values() {
        let mut cfg = test_observability_config();
        cfg.runtime_trace_mode = "none".into();
        assert_eq!(
            storage_mode_from_config(&cfg),
            RuntimeTraceStorageMode::None
        );

        cfg.runtime_trace_mode = "rolling".into();
        assert_eq!(
            storage_mode_from_config(&cfg),
            RuntimeTraceStorageMode::Rolling
        );

        cfg.runtime_trace_mode = "full".into();
        assert_eq!(
            storage_mode_from_config(&cfg),
            RuntimeTraceStorageMode::Full
        );
    }

    #[tokio::test]
    async fn rolling_mode_writes_via_rotation_writer() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("trace.jsonl");

        let rot_config = FileRotationConfig {
            max_file_size_mb: 100,
            max_age_days: 30,
            max_rotated_files: 100,
        };
        let logger =
            RuntimeTraceLogger::new(RuntimeTraceStorageMode::Rolling, path.clone(), &rot_config)
                .await
                .unwrap();

        for i in 0..5 {
            let event = RuntimeTraceEvent {
                id: format!("id-{i}"),
                timestamp: Utc::now().to_rfc3339(),
                event_type: "test".into(),
                channel: None,
                provider: None,
                model: None,
                turn_id: None,
                success: None,
                message: Some(format!("event-{i}")),
                payload: serde_json::json!({ "i": i }),
            };
            logger.append(&event).await.unwrap();
        }
        logger.shutdown().await.unwrap();

        // The active file plus potentially rotated files should contain all events
        let all_events = load_events(&path, 10, None, None).unwrap_or_default();
        assert_eq!(
            all_events.len(),
            5,
            "All 5 events should be readable across active + rotated files"
        );
    }

    #[tokio::test]
    async fn find_event_by_id_returns_match() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("trace.jsonl");

        let rot_config = FileRotationConfig {
            max_file_size_mb: 100,
            max_age_days: 30,
            max_rotated_files: 100,
        };
        let logger =
            RuntimeTraceLogger::new(RuntimeTraceStorageMode::Full, path.clone(), &rot_config)
                .await
                .unwrap();

        let target_id = "target-event";
        let event = RuntimeTraceEvent {
            id: target_id.into(),
            timestamp: Utc::now().to_rfc3339(),
            event_type: "tool_call_result".into(),
            channel: Some("telegram".into()),
            provider: Some("openrouter".into()),
            model: Some("x".into()),
            turn_id: Some("turn-1".into()),
            success: Some(false),
            message: Some("boom".into()),
            payload: serde_json::json!({ "error": "boom" }),
        };
        logger.append(&event).await.unwrap();
        logger.shutdown().await.unwrap();

        let found = find_event_by_id(&path, target_id).unwrap();
        assert!(found.is_some());
        assert_eq!(found.unwrap().id, target_id);
    }
}
