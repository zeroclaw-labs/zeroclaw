use crate::channels::traits::ChannelMessage;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone)]
pub enum SubagentOutcome {
    Success,
    Error(String),
    Cancelled,
}

/// Context of the parent conversation that spawned the subagent,
/// used for push-based completion announcements.
#[derive(Debug, Clone)]
pub struct ParentContext {
    pub sender: String,
    pub reply_target: String,
    pub channel: String,
}

#[derive(Debug, Clone)]
pub struct SubagentRunRecord {
    pub run_id: String,
    pub task: String,
    pub label: Option<String>,
    pub model: String,
    pub started_at: std::time::Instant,
    pub ended_at: Option<std::time::Instant>,
    pub outcome: Option<SubagentOutcome>,
    pub result_text: Option<String>,
    pub cancellation_token: CancellationToken,
    pub parent_context: Option<ParentContext>,
}

#[derive(Clone)]
pub struct SubagentRegistry {
    records: Arc<Mutex<HashMap<String, SubagentRunRecord>>>,
    max_concurrent: usize,
    max_depth: usize,
    /// Channel message sender for push-based completion announcements.
    /// When set, completed subagents automatically inject a synthetic message
    /// into the parent conversation's channel message queue.
    announce_tx: Arc<Mutex<Option<tokio::sync::mpsc::Sender<ChannelMessage>>>>,
    /// Current parent context, set by the channel message processor before
    /// each tool loop. Spawn tools read this to populate parent_context on records.
    current_context: Arc<Mutex<Option<ParentContext>>>,
}

impl SubagentRegistry {
    pub fn new(max_concurrent: usize, max_depth: usize) -> Self {
        Self {
            records: Arc::new(Mutex::new(HashMap::new())),
            max_concurrent,
            max_depth,
            announce_tx: Arc::new(Mutex::new(None)),
            current_context: Arc::new(Mutex::new(None)),
        }
    }

    pub fn default() -> Self {
        Self::new(5, 1)
    }

    /// Set the channel message sender for push-based completion announcements.
    pub async fn set_announce_tx(&self, tx: tokio::sync::mpsc::Sender<ChannelMessage>) {
        *self.announce_tx.lock().await = Some(tx);
    }

    /// Set the current parent context (called before each tool loop).
    pub async fn set_current_context(&self, ctx: ParentContext) {
        *self.current_context.lock().await = Some(ctx);
    }

    /// Get the current parent context for spawn tools to use.
    pub async fn current_context(&self) -> Option<ParentContext> {
        self.current_context.lock().await.clone()
    }

    pub async fn active_count(&self) -> usize {
        let records = self.records.lock().await;
        records.values().filter(|r| r.outcome.is_none()).count()
    }

    pub async fn can_spawn(&self) -> bool {
        self.active_count().await < self.max_concurrent
    }

    pub fn max_depth(&self) -> usize {
        self.max_depth
    }

    pub async fn register(&self, record: SubagentRunRecord) {
        let mut records = self.records.lock().await;
        records.insert(record.run_id.clone(), record);
    }

    pub async fn complete(
        &self,
        run_id: &str,
        outcome: SubagentOutcome,
        result_text: Option<String>,
    ) {
        let parent_ctx;
        let label;
        let task;
        let status_str;
        {
            let mut records = self.records.lock().await;
            if let Some(record) = records.get_mut(run_id) {
                record.ended_at = Some(std::time::Instant::now());
                record.outcome = Some(outcome.clone());
                record.result_text = result_text.clone();
                parent_ctx = record.parent_context.clone();
                label = record.label.clone();
                task = record.task.clone();
            } else {
                return;
            }
        }

        status_str = match &outcome {
            SubagentOutcome::Success => "completed",
            SubagentOutcome::Error(_) => "error",
            SubagentOutcome::Cancelled => "cancelled",
        };

        // Push-based announce: send synthetic message to parent conversation
        if let Some(ref ctx) = parent_ctx {
            let tx_guard = self.announce_tx.lock().await;
            if let Some(ref tx) = *tx_guard {
                let agent_label = label.as_deref().unwrap_or("subagent");
                let result_summary = result_text.as_deref().unwrap_or("(no output)");
                // Truncate long results to avoid flooding the conversation
                let truncated = if result_summary.len() > 2000 {
                    format!("{}...(truncated)", &result_summary[..2000])
                } else {
                    result_summary.to_string()
                };

                let announce_content = format!(
                    "[Subagent {status_str}] agent={agent_label} run_id={run_id}\n\n{truncated}"
                );

                let msg = ChannelMessage {
                    id: format!("subagent-announce-{run_id}"),
                    sender: ctx.sender.clone(),
                    reply_target: ctx.reply_target.clone(),
                    content: announce_content,
                    channel: ctx.channel.clone(),
                    timestamp: std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs(),
                    thread_ts: None,
                };

                if let Err(e) = tx.try_send(msg) {
                    tracing::warn!(
                        run_id = %run_id,
                        error = %e,
                        "Failed to send subagent completion announce"
                    );
                } else {
                    tracing::info!(
                        run_id = %run_id,
                        agent = %agent_label,
                        status = %status_str,
                        "Subagent completion announced to parent"
                    );
                }
            }
        }
    }

    pub async fn get(&self, run_id: &str) -> Option<SubagentRunRecord> {
        let records = self.records.lock().await;
        records.get(run_id).cloned()
    }

    pub async fn list_all(&self) -> Vec<SubagentRunRecord> {
        let records = self.records.lock().await;
        records.values().cloned().collect()
    }

    pub async fn cancel(&self, run_id: &str) -> bool {
        let records = self.records.lock().await;
        if let Some(record) = records.get(run_id) {
            if record.outcome.is_none() {
                record.cancellation_token.cancel();
                return true;
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn registry_default_limits() {
        let registry = SubagentRegistry::default();
        assert_eq!(registry.max_depth(), 1);
        assert!(registry.can_spawn().await);
        assert_eq!(registry.active_count().await, 0);
    }

    #[tokio::test]
    async fn register_and_get() {
        let registry = SubagentRegistry::new(5, 1);
        let record = SubagentRunRecord {
            run_id: "test-1".to_string(),
            task: "do something".to_string(),
            label: Some("test".to_string()),
            model: "test-model".to_string(),
            started_at: std::time::Instant::now(),
            ended_at: None,
            outcome: None,
            result_text: None,
            cancellation_token: CancellationToken::new(),
            parent_context: None,
        };
        registry.register(record).await;
        assert_eq!(registry.active_count().await, 1);

        let fetched = registry.get("test-1").await.unwrap();
        assert_eq!(fetched.task, "do something");
        assert!(fetched.outcome.is_none());
    }

    #[tokio::test]
    async fn complete_updates_record() {
        let registry = SubagentRegistry::new(5, 1);
        let record = SubagentRunRecord {
            run_id: "test-2".to_string(),
            task: "task".to_string(),
            label: None,
            model: "model".to_string(),
            started_at: std::time::Instant::now(),
            ended_at: None,
            outcome: None,
            result_text: None,
            cancellation_token: CancellationToken::new(),
            parent_context: None,
        };
        registry.register(record).await;
        assert_eq!(registry.active_count().await, 1);

        registry
            .complete("test-2", SubagentOutcome::Success, Some("done".to_string()))
            .await;
        assert_eq!(registry.active_count().await, 0);

        let fetched = registry.get("test-2").await.unwrap();
        assert!(fetched.ended_at.is_some());
        assert!(matches!(fetched.outcome, Some(SubagentOutcome::Success)));
        assert_eq!(fetched.result_text.as_deref(), Some("done"));
    }

    #[tokio::test]
    async fn can_spawn_respects_max_concurrent() {
        let registry = SubagentRegistry::new(1, 1);
        let record = SubagentRunRecord {
            run_id: "test-3".to_string(),
            task: "task".to_string(),
            label: None,
            model: "model".to_string(),
            started_at: std::time::Instant::now(),
            ended_at: None,
            outcome: None,
            result_text: None,
            cancellation_token: CancellationToken::new(),
            parent_context: None,
        };
        registry.register(record).await;
        assert!(!registry.can_spawn().await);
    }

    #[tokio::test]
    async fn cancel_active_subagent() {
        let registry = SubagentRegistry::new(5, 1);
        let token = CancellationToken::new();
        let token_clone = token.clone();
        let record = SubagentRunRecord {
            run_id: "test-4".to_string(),
            task: "task".to_string(),
            label: None,
            model: "model".to_string(),
            started_at: std::time::Instant::now(),
            ended_at: None,
            outcome: None,
            result_text: None,
            cancellation_token: token,
            parent_context: None,
        };
        registry.register(record).await;
        assert!(registry.cancel("test-4").await);
        assert!(token_clone.is_cancelled());
    }

    #[tokio::test]
    async fn cancel_completed_subagent_returns_false() {
        let registry = SubagentRegistry::new(5, 1);
        let record = SubagentRunRecord {
            run_id: "test-5".to_string(),
            task: "task".to_string(),
            label: None,
            model: "model".to_string(),
            started_at: std::time::Instant::now(),
            ended_at: None,
            outcome: None,
            result_text: None,
            cancellation_token: CancellationToken::new(),
            parent_context: None,
        };
        registry.register(record).await;
        registry
            .complete("test-5", SubagentOutcome::Success, None)
            .await;
        assert!(!registry.cancel("test-5").await);
    }

    #[tokio::test]
    async fn list_all_returns_all_records() {
        let registry = SubagentRegistry::new(5, 1);
        for i in 0..3 {
            let record = SubagentRunRecord {
                run_id: format!("list-{i}"),
                task: format!("task-{i}"),
                label: None,
                model: "model".to_string(),
                started_at: std::time::Instant::now(),
                ended_at: None,
                outcome: None,
                result_text: None,
                cancellation_token: CancellationToken::new(),
                parent_context: None,
            };
            registry.register(record).await;
        }
        assert_eq!(registry.list_all().await.len(), 3);
    }

    #[tokio::test]
    async fn get_nonexistent_returns_none() {
        let registry = SubagentRegistry::new(5, 1);
        assert!(registry.get("nonexistent").await.is_none());
    }
}
