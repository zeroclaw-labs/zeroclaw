//! Orchestrator channel adapter — Redis Streams integration.
//!
//! Augusta connects to the Elixir orchestrator as a first-class execution target
//! via Redis Streams. The orchestrator publishes tasks to `augusta:tasks` and
//! Augusta publishes results to `augusta:results`.
//!
//! Requires the `orchestrator` feature flag and a running Redis instance.

use super::traits::{Channel, ChannelMessage, SendMessage};
use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// Redis Streams-based channel for orchestrator integration.
pub struct OrchestratorChannel {
    redis_url: String,
    tasks_stream: String,
    results_stream: String,
    consumer_group: String,
    consumer_name: String,
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
}

fn default_timeout() -> u64 {
    30_000
}

/// Result message to the orchestrator.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrchestratorResult {
    pub run_id: String,
    pub status: String,
    pub output: String,
    #[serde(default)]
    pub tool_results: Vec<serde_json::Value>,
    #[serde(default)]
    pub evidence: Vec<serde_json::Value>,
    pub duration_ms: u64,
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
            results_stream: format!("{prefix}:results"),
            consumer_group: format!("augusta-{id}"),
            consumer_name: id,
        }
    }
}

#[async_trait]
impl Channel for OrchestratorChannel {
    fn name(&self) -> &str {
        "orchestrator"
    }

    async fn send(&self, message: &SendMessage) -> Result<()> {
        // Parse the result from the message content
        #[cfg(feature = "orchestrator")]
        {
            let client = redis::Client::open(self.redis_url.as_str())
                .map_err(|e| anyhow::anyhow!("Redis connection failed: {}", e))?;
            let mut conn = client
                .get_multiplexed_async_connection()
                .await
                .map_err(|e| anyhow::anyhow!("Redis async connection failed: {}", e))?;

            // The recipient field carries the run_id
            let result = OrchestratorResult {
                run_id: message.recipient.clone(),
                status: "completed".to_string(),
                output: message.content.clone(),
                tool_results: Vec::new(),
                evidence: Vec::new(),
                duration_ms: 0,
            };

            let result_json = serde_json::to_string(&result)?;

            redis::cmd("XADD")
                .arg(&self.results_stream)
                .arg("*")
                .arg("data")
                .arg(&result_json)
                .query_async::<String>(&mut conn)
                .await
                .map_err(|e| anyhow::anyhow!("Failed to publish result: {}", e))?;

            tracing::info!(
                run_id = %result.run_id,
                stream = %self.results_stream,
                "Published result to orchestrator"
            );
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
            let client = redis::Client::open(self.redis_url.as_str())
                .map_err(|e| anyhow::anyhow!("Redis connection failed: {}", e))?;
            let mut conn = client
                .get_multiplexed_async_connection()
                .await
                .map_err(|e| anyhow::anyhow!("Redis async connection failed: {}", e))?;

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
                // XREADGROUP to consume messages
                let result: redis::RedisResult<Vec<redis::Value>> = redis::cmd("XREADGROUP")
                    .arg("GROUP")
                    .arg(&self.consumer_group)
                    .arg(&self.consumer_name)
                    .arg("COUNT")
                    .arg("1")
                    .arg("BLOCK")
                    .arg("5000") // 5s block
                    .arg("STREAMS")
                    .arg(&self.tasks_stream)
                    .arg(">")
                    .query_async(&mut conn)
                    .await;

                match result {
                    Ok(entries) => {
                        // Parse Redis Stream entries
                        if let Some(task_data) = parse_stream_entries(&entries) {
                            match serde_json::from_str::<OrchestratorTask>(&task_data) {
                                Ok(task) => {
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
                                    };
                                    if tx.send(msg).await.is_err() {
                                        tracing::error!("Channel receiver dropped");
                                        break;
                                    }
                                }
                                Err(e) => {
                                    tracing::warn!(error = %e, "Failed to parse orchestrator task");
                                }
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

/// Parse Redis Stream XREADGROUP response to extract the "data" field value.
#[cfg(feature = "orchestrator")]
fn parse_stream_entries(entries: &[redis::Value]) -> Option<String> {
    use redis::Value;

    // XREADGROUP returns: [[stream_name, [[entry_id, [field, value, ...]]]]]
    if let Some(Value::Array(streams)) = entries.first() {
        if let Some(Value::Array(stream_data)) = streams.get(1) {
            if let Some(Value::Array(entry)) = stream_data.first() {
                if let Some(Value::Array(fields)) = entry.get(1) {
                    // Fields are [key, value, key, value, ...]
                    let mut i = 0;
                    while i + 1 < fields.len() {
                        if let (Value::BulkString(key), Value::BulkString(val)) =
                            (&fields[i], &fields[i + 1])
                        {
                            if String::from_utf8_lossy(key) == "data" {
                                return Some(String::from_utf8_lossy(val).to_string());
                            }
                        }
                        i += 2;
                    }
                }
            }
        }
    }
    None
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
        assert_eq!(ch.results_stream, "augusta:results");
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
        assert_eq!(ch.results_stream, "myprefix:results");
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
        assert_eq!(task.timeout_ms, 30_000); // default
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
    }

    #[test]
    fn default_timeout_is_30s() {
        assert_eq!(default_timeout(), 30_000);
    }
}
