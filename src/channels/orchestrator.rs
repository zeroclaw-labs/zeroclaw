//! Orchestrator channel adapter — Redis Streams integration.
//!
//! Augusta connects to the Elixir orchestrator as a first-class execution target
//! via Redis Streams. The orchestrator publishes tasks to `augusta:tasks` and
//! Augusta publishes results to `augusta:results:{run_id}`.
//!
//! Requires the `orchestrator` feature flag and a running Redis instance.

use super::traits::{Channel, ChannelMessage, SendMessage};
use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// Heartbeat key prefix in Redis. The full key is `{prefix}:heartbeat`.
const HEARTBEAT_KEY_SUFFIX: &str = "heartbeat";
/// Heartbeat interval in seconds.
const HEARTBEAT_INTERVAL_SECS: u64 = 10;
/// Heartbeat TTL — if no heartbeat for this many seconds, Augusta is considered dead.
const HEARTBEAT_TTL_SECS: u64 = 30;

/// Redis Streams-based channel for orchestrator integration.
pub struct OrchestratorChannel {
    redis_url: String,
    tasks_stream: String,
    results_prefix: String,
    consumer_group: String,
    consumer_name: String,
    pub(crate) streams_prefix: String,
}

/// Task message from the orchestrator.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrchestratorTask {
    pub run_id: String,
    pub agent_type: String,
    pub prompt: String,
    #[serde(default)]
    pub context: serde_json::Value,
    #[serde(default)]
    pub tools_allowed: Vec<String>,
    #[serde(default = "default_timeout")]
    pub timeout_ms: u64,
    /// Session context injected by the orchestrator (session .md + diagrams).
    /// Prepended to the system prompt for workspace-aware execution.
    #[serde(default)]
    pub system_context: Option<String>,
}

fn default_timeout() -> u64 {
    30_000
}

/// Structured error codes for Augusta results.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// LLM provider returned an error or is unreachable.
    ProviderError,
    /// Task exceeded its timeout.
    Timeout,
    /// A tool execution failed.
    ToolError,
    /// The requested agent type is not supported.
    UnsupportedAgent,
    /// Internal Augusta error.
    InternalError,
}

impl std::fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ProviderError => write!(f, "provider_error"),
            Self::Timeout => write!(f, "timeout"),
            Self::ToolError => write!(f, "tool_error"),
            Self::UnsupportedAgent => write!(f, "unsupported_agent"),
            Self::InternalError => write!(f, "internal_error"),
        }
    }
}

/// Result message to the orchestrator.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrchestratorResult {
    pub run_id: String,
    pub status: String,
    pub output: String,
    #[serde(default)]
    pub error_code: Option<String>,
    #[serde(default)]
    pub tool_results: Vec<serde_json::Value>,
    #[serde(default)]
    pub evidence: Vec<serde_json::Value>,
    pub duration_ms: u64,
}

/// Observer that publishes progress events to Redis pub/sub for streaming.
///
/// Publishes JSON events to `augusta:progress:{run_id}` so the Elixir side
/// can relay them as SSE chunks. Events include tool call start/end,
/// LLM request/response, and turn completions.
#[cfg(feature = "orchestrator")]
pub struct StreamingObserver {
    conn: redis::aio::MultiplexedConnection,
    channel: String,
    runtime: tokio::runtime::Handle,
}

#[cfg(feature = "orchestrator")]
impl StreamingObserver {
    pub async fn new(redis_url: &str, run_id: &str, prefix: &str) -> Result<Self> {
        let client = redis::Client::open(redis_url)
            .map_err(|e| anyhow::anyhow!("StreamingObserver Redis connect failed: {}", e))?;
        let conn = client
            .get_multiplexed_async_connection()
            .await
            .map_err(|e| anyhow::anyhow!("StreamingObserver async connect failed: {}", e))?;

        Ok(Self {
            conn,
            channel: format!("{}:progress:{}", prefix, run_id),
            runtime: tokio::runtime::Handle::current(),
        })
    }

    fn publish(&self, event: serde_json::Value) {
        let channel = self.channel.clone();
        let mut conn = self.conn.clone();
        self.runtime.spawn(async move {
            let msg = event.to_string();
            let _: redis::RedisResult<i64> = redis::cmd("PUBLISH")
                .arg(&channel)
                .arg(&msg)
                .query_async(&mut conn)
                .await;
        });
    }
}

#[cfg(feature = "orchestrator")]
impl crate::observability::Observer for StreamingObserver {
    fn record_event(&self, event: &crate::observability::ObserverEvent) {
        use crate::observability::ObserverEvent;

        let payload = match event {
            ObserverEvent::ToolCallStart { tool, .. } => {
                serde_json::json!({"type": "tool_start", "tool": tool})
            }
            ObserverEvent::ToolCall {
                tool,
                duration,
                success,
            } => {
                serde_json::json!({
                    "type": "tool_end",
                    "tool": tool,
                    "duration_ms": duration.as_millis() as u64,
                    "success": success,
                })
            }
            ObserverEvent::LlmRequest { model, .. } => {
                serde_json::json!({"type": "llm_request", "model": model})
            }
            ObserverEvent::LlmResponse {
                duration, success, ..
            } => {
                serde_json::json!({
                    "type": "llm_response",
                    "duration_ms": duration.as_millis() as u64,
                    "success": success,
                })
            }
            ObserverEvent::TurnComplete => {
                serde_json::json!({"type": "turn_complete"})
            }
            _ => return,
        };

        self.publish(payload);
    }
}

impl OrchestratorChannel {
    pub fn new(
        redis_url: String,
        streams_prefix: Option<String>,
        instance_id: Option<String>,
    ) -> Self {
        let prefix = streams_prefix.unwrap_or_else(|| "augusta".to_string());
        let id = instance_id.unwrap_or_else(|| {
            hostname::get()
                .map(|h| h.to_string_lossy().to_string())
                .unwrap_or_else(|_| "default".to_string())
        });

        Self {
            redis_url,
            tasks_stream: format!("{prefix}:tasks"),
            results_prefix: format!("{prefix}:results"),
            consumer_group: format!("augusta-{id}"),
            consumer_name: id,
            streams_prefix: prefix,
        }
    }

    /// Get a pooled Redis connection. Uses `get_multiplexed_async_connection`
    /// which internally multiplexes over a single TCP connection.
    #[cfg(feature = "orchestrator")]
    async fn get_connection(&self) -> Result<redis::aio::MultiplexedConnection> {
        let client = redis::Client::open(self.redis_url.as_str())
            .map_err(|e| anyhow::anyhow!("Redis connection failed: {}", e))?;
        client
            .get_multiplexed_async_connection()
            .await
            .map_err(|e| anyhow::anyhow!("Redis async connection failed: {}", e))
    }

    /// Publish a result with duration and optional error code.
    #[cfg(feature = "orchestrator")]
    async fn publish_result(
        &self,
        conn: &mut redis::aio::MultiplexedConnection,
        run_id: &str,
        status: &str,
        output: &str,
        duration_ms: u64,
        error_code: Option<&ErrorCode>,
    ) -> Result<()> {
        let result_stream = format!("{}:{}", self.results_prefix, run_id);

        let mut cmd = redis::cmd("XADD");
        cmd.arg(&result_stream)
            .arg("*")
            .arg("run_id")
            .arg(run_id)
            .arg("status")
            .arg(status)
            .arg("output")
            .arg(output)
            .arg("duration_ms")
            .arg(duration_ms.to_string());

        if let Some(code) = error_code {
            cmd.arg("error_code").arg(code.to_string());
        }

        cmd.query_async::<String>(conn)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to publish result: {}", e))?;

        tracing::info!(
            run_id = %run_id,
            stream = %result_stream,
            duration_ms = duration_ms,
            status = status,
            "Published result to orchestrator"
        );

        Ok(())
    }

    /// Start the heartbeat loop. Publishes instance info to Redis every
    /// HEARTBEAT_INTERVAL_SECS with a TTL of HEARTBEAT_TTL_SECS.
    #[cfg(feature = "orchestrator")]
    pub fn start_heartbeat(&self, tools_count: usize) {
        let redis_url = self.redis_url.clone();
        let heartbeat_key = format!("{}:{}", self.streams_prefix, HEARTBEAT_KEY_SUFFIX);
        let consumer_name = self.consumer_name.clone();

        tokio::spawn(async move {
            let client = match redis::Client::open(redis_url.as_str()) {
                Ok(c) => c,
                Err(e) => {
                    tracing::error!(error = %e, "Heartbeat: Redis connection failed");
                    return;
                }
            };
            let mut conn = match client.get_multiplexed_async_connection().await {
                Ok(c) => c,
                Err(e) => {
                    tracing::error!(error = %e, "Heartbeat: Redis async connection failed");
                    return;
                }
            };

            let start = std::time::Instant::now();

            loop {
                let uptime_secs = start.elapsed().as_secs();
                let payload = serde_json::json!({
                    "instance": consumer_name,
                    "tools_count": tools_count,
                    "uptime_secs": uptime_secs,
                    "timestamp": std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0),
                });

                let result: redis::RedisResult<()> = redis::pipe()
                    .cmd("SET")
                    .arg(&heartbeat_key)
                    .arg(payload.to_string())
                    .cmd("EXPIRE")
                    .arg(&heartbeat_key)
                    .arg(HEARTBEAT_TTL_SECS)
                    .query_async(&mut conn)
                    .await;

                if let Err(e) = result {
                    tracing::warn!(error = %e, "Heartbeat publish failed");
                }

                tokio::time::sleep(std::time::Duration::from_secs(HEARTBEAT_INTERVAL_SECS)).await;
            }
        });
    }
}

#[async_trait]
impl Channel for OrchestratorChannel {
    fn name(&self) -> &str {
        "orchestrator"
    }

    async fn send(&self, message: &SendMessage) -> Result<()> {
        #[cfg(feature = "orchestrator")]
        {
            let mut conn = self.get_connection().await?;
            let run_id = &message.recipient;

            // Parse duration tag and classify output
            let (status, duration_ms, error_code, clean_output) = classify_output(&message.content);

            self.publish_result(
                &mut conn,
                run_id,
                status,
                clean_output,
                duration_ms,
                error_code.as_ref(),
            )
            .await?;
        }

        #[cfg(not(feature = "orchestrator"))]
        {
            let _ = message;
            tracing::warn!(
                "Orchestrator channel send called but 'orchestrator' feature not enabled"
            );
        }

        Ok(())
    }

    async fn listen(&self, tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> Result<()> {
        #[cfg(feature = "orchestrator")]
        {
            let mut conn = self.get_connection().await?;

            // Create consumer group (ignore error if already exists)
            let _: Result<String, _> = redis::cmd("XGROUP")
                .arg("CREATE")
                .arg(&self.tasks_stream)
                .arg(&self.consumer_group)
                .arg("$")
                .arg("MKSTREAM")
                .query_async(&mut conn)
                .await;

            tracing::info!(
                stream = %self.tasks_stream,
                group = %self.consumer_group,
                consumer = %self.consumer_name,
                "Orchestrator channel listening"
            );

            loop {
                let result: redis::RedisResult<Vec<redis::Value>> = redis::cmd("XREADGROUP")
                    .arg("GROUP")
                    .arg(&self.consumer_group)
                    .arg(&self.consumer_name)
                    .arg("COUNT")
                    .arg("1")
                    .arg("BLOCK")
                    .arg("5000")
                    .arg("STREAMS")
                    .arg(&self.tasks_stream)
                    .arg(">")
                    .query_async(&mut conn)
                    .await;

                match result {
                    Ok(entries) => {
                        if let Some(task) = parse_stream_entries(&entries) {
                            let mut metadata = std::collections::HashMap::new();
                            if let Some(ctx) = task.system_context {
                                metadata.insert("system_context".to_string(), ctx);
                            }

                            let msg = ChannelMessage {
                                id: task.run_id.clone(),
                                sender: format!("orchestrator:{}", task.agent_type),
                                reply_target: task.run_id.clone(),
                                content: task.prompt,
                                channel: "orchestrator".to_string(),
                                timestamp: std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .map(|d| d.as_secs())
                                    .unwrap_or(0),
                                thread_ts: None,
                                metadata,
                            };
                            if tx.send(msg).await.is_err() {
                                tracing::error!("Channel receiver dropped");
                                break;
                            }
                        }
                    }
                    Err(e) => {
                        tracing::error!(error = %e, "Redis XREADGROUP failed");
                        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                    }
                }
            }
        }

        #[cfg(not(feature = "orchestrator"))]
        {
            let _ = tx;
            tracing::warn!(
                "Orchestrator channel listen called but 'orchestrator' feature not enabled"
            );
        }

        Ok(())
    }
}

/// Parse the duration tag and classify output to determine status and error code.
///
/// The `start_orchestrator` loop prepends `__duration_ms:{N}__\n` to the output.
/// This function strips it, extracts the duration, and classifies the remaining
/// content as success or error with a typed error code.
fn classify_output(raw: &str) -> (&str, u64, Option<ErrorCode>, &str) {
    let (duration_ms, output) = if let Some(rest) = raw.strip_prefix("__duration_ms:") {
        if let Some(end) = rest.find("__\n") {
            let ms = rest[..end].parse::<u64>().unwrap_or(0);
            (ms, &rest[end + 3..])
        } else {
            (0, raw)
        }
    } else {
        (0, raw)
    };

    if output.starts_with("Error: ") {
        let code = if output.contains("providers/models failed") || output.contains("API error") {
            Some(ErrorCode::ProviderError)
        } else if output.contains("timed out") || output.contains("timeout") {
            Some(ErrorCode::Timeout)
        } else if output.contains("tool") {
            Some(ErrorCode::ToolError)
        } else {
            Some(ErrorCode::InternalError)
        };
        ("error", duration_ms, code, output)
    } else {
        ("completed", duration_ms, None, output)
    }
}

/// Parse Redis Stream XREADGROUP response to extract flat field pairs.
///
/// The Elixir orchestrator publishes flat key-value fields:
///   XADD augusta:tasks * run_id <id> agent_type <type> prompt <msg> ...
///
/// This function extracts those fields into a HashMap, then builds an
/// OrchestratorTask from them.
#[cfg(feature = "orchestrator")]
fn parse_stream_entries(entries: &[redis::Value]) -> Option<OrchestratorTask> {
    use redis::Value;
    use std::collections::HashMap;

    // XREADGROUP returns: [[stream_name, [[entry_id, [field, value, ...]]]]]
    let streams = match entries.first()? {
        Value::Array(s) => s,
        _ => return None,
    };
    let stream_data = match streams.get(1)? {
        Value::Array(s) => s,
        _ => return None,
    };
    let entry = match stream_data.first()? {
        Value::Array(e) => e,
        _ => return None,
    };
    let fields = match entry.get(1)? {
        Value::Array(f) => f,
        _ => return None,
    };

    // Build key-value map from flat field pairs
    let mut map = HashMap::new();
    let mut i = 0;
    while i + 1 < fields.len() {
        if let (Value::BulkString(key), Value::BulkString(val)) = (&fields[i], &fields[i + 1]) {
            let k = String::from_utf8_lossy(key).to_string();
            let v = String::from_utf8_lossy(val).to_string();
            map.insert(k, v);
        }
        i += 2;
    }

    // Build OrchestratorTask from flat fields
    let run_id = map.get("run_id")?.clone();
    let agent_type = map.get("agent_type").cloned().unwrap_or_default();
    let prompt = map.get("prompt").cloned().unwrap_or_default();
    let context = map
        .get("context")
        .and_then(|c| serde_json::from_str(c).ok())
        .unwrap_or(serde_json::Value::Null);
    let tools_allowed = map
        .get("tools_allowed")
        .and_then(|t| serde_json::from_str(t).ok())
        .unwrap_or_default();
    let timeout_ms = map
        .get("timeout_ms")
        .and_then(|t| t.parse().ok())
        .unwrap_or_else(default_timeout);

    let system_context = map.get("system_context").cloned().filter(|s| !s.is_empty());

    Some(OrchestratorTask {
        run_id,
        agent_type,
        prompt,
        context,
        tools_allowed,
        timeout_ms,
        system_context,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orchestrator_channel_default_streams() {
        let ch = OrchestratorChannel::new(
            "redis://localhost:6379".to_string(),
            None,
            Some("test-node".to_string()),
        );
        assert_eq!(ch.name(), "orchestrator");
        assert_eq!(ch.tasks_stream, "augusta:tasks");
        assert_eq!(ch.results_prefix, "augusta:results");
        assert_eq!(ch.consumer_group, "augusta-test-node");
        assert_eq!(ch.consumer_name, "test-node");
    }

    #[test]
    fn orchestrator_channel_custom_prefix() {
        let ch = OrchestratorChannel::new(
            "redis://localhost:6379".to_string(),
            Some("myprefix".to_string()),
            Some("node1".to_string()),
        );
        assert_eq!(ch.tasks_stream, "myprefix:tasks");
        assert_eq!(ch.results_prefix, "myprefix:results");
        assert_eq!(ch.consumer_group, "augusta-node1");
    }

    #[test]
    fn orchestrator_task_deserialize_minimal() {
        let json = r#"{
            "run_id": "abc-123",
            "agent_type": "v_devops",
            "prompt": "check disk space"
        }"#;
        let task: OrchestratorTask = serde_json::from_str(json).unwrap();
        assert_eq!(task.run_id, "abc-123");
        assert_eq!(task.agent_type, "v_devops");
        assert_eq!(task.prompt, "check disk space");
        assert_eq!(task.timeout_ms, 30_000);
        assert!(task.tools_allowed.is_empty());
        assert!(task.context.is_null());
    }

    #[test]
    fn orchestrator_task_deserialize_full() {
        let json = r#"{
            "run_id": "run-456",
            "agent_type": "infrastructure_ops_auditor",
            "prompt": "audit nginx config",
            "context": {"skills": ["shell"]},
            "tools_allowed": ["shell", "file_read"],
            "timeout_ms": 60000
        }"#;
        let task: OrchestratorTask = serde_json::from_str(json).unwrap();
        assert_eq!(task.run_id, "run-456");
        assert_eq!(task.tools_allowed, vec!["shell", "file_read"]);
        assert_eq!(task.timeout_ms, 60000);
        assert_eq!(task.context["skills"][0], "shell");
    }

    #[test]
    fn orchestrator_result_roundtrip() {
        let result = OrchestratorResult {
            run_id: "run-789".to_string(),
            status: "completed".to_string(),
            output: "All good".to_string(),
            error_code: None,
            tool_results: vec![serde_json::json!({"tool": "shell", "ok": true})],
            evidence: vec![],
            duration_ms: 1500,
        };
        let json = serde_json::to_string(&result).unwrap();
        let deserialized: OrchestratorResult = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.run_id, "run-789");
        assert_eq!(deserialized.status, "completed");
        assert_eq!(deserialized.duration_ms, 1500);
        assert_eq!(deserialized.tool_results.len(), 1);
        assert!(deserialized.error_code.is_none());
    }

    #[test]
    fn orchestrator_result_with_error_code() {
        let result = OrchestratorResult {
            run_id: "run-err".to_string(),
            status: "error".to_string(),
            output: "Provider unreachable".to_string(),
            error_code: Some("provider_error".to_string()),
            tool_results: vec![],
            evidence: vec![],
            duration_ms: 150,
        };
        let json = serde_json::to_string(&result).unwrap();
        let deserialized: OrchestratorResult = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.error_code.as_deref(), Some("provider_error"));
    }

    #[test]
    fn classify_output_success() {
        let (status, _, code, output) = classify_output("Here is the answer: 42");
        assert_eq!(status, "completed");
        assert!(code.is_none());
        assert_eq!(output, "Here is the answer: 42");
    }

    #[test]
    fn classify_output_with_duration_tag() {
        let (status, duration, code, output) =
            classify_output("__duration_ms:1500__\nHere is the answer: 42");
        assert_eq!(status, "completed");
        assert_eq!(duration, 1500);
        assert!(code.is_none());
        assert_eq!(output, "Here is the answer: 42");
    }

    #[test]
    fn classify_output_provider_error() {
        let (status, _, code, _) =
            classify_output("Error: All providers/models failed. Attempts: ...");
        assert_eq!(status, "error");
        assert!(matches!(code, Some(ErrorCode::ProviderError)));
    }

    #[test]
    fn classify_output_timeout() {
        let (status, _, code, _) = classify_output("Error: Task timed out after 30000ms");
        assert_eq!(status, "error");
        assert!(matches!(code, Some(ErrorCode::Timeout)));
    }

    #[test]
    fn default_timeout_is_30s() {
        assert_eq!(default_timeout(), 30_000);
    }
}
