use super::traits::{Tool, ToolResult};
use crate::config::Config;
use crate::proactive_messaging::store;
use crate::security::SecurityPolicy;
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;

pub struct ManageOutboundQueueTool {
    config: Arc<Config>,
    security: Arc<SecurityPolicy>,
}

impl ManageOutboundQueueTool {
    pub fn new(config: Arc<Config>, security: Arc<SecurityPolicy>) -> Self {
        Self { config, security }
    }
}

#[async_trait]
impl Tool for ManageOutboundQueueTool {
    fn name(&self) -> &str {
        "manage_outbound_queue"
    }

    fn description(&self) -> &str {
        "Manage the outbound message queue for proactive messaging. Use 'list' to view pending \
         messages, 'clear' to remove them (e.g. after addressing them in conversation), or \
         'keep' to acknowledge without clearing."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["list", "clear", "keep"],
                    "description": "Action to perform: 'list' shows pending messages, 'clear' removes them, 'keep' acknowledges without clearing"
                },
                "ids": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Optional list of message IDs to clear (for 'clear' action). If omitted, clears all matching."
                },
                "recipient": {
                    "type": "string",
                    "description": "Optional filter by recipient"
                },
                "channel": {
                    "type": "string",
                    "description": "Optional filter by channel"
                }
            },
            "required": ["action"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let action = args
            .get("action")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .ok_or_else(|| anyhow::anyhow!("Missing 'action' parameter"))?;

        let recipient = args
            .get("recipient")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|v| !v.is_empty());

        let channel = args
            .get("channel")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|v| !v.is_empty());

        let workspace_dir = &self.config.workspace_dir;

        match action {
            "list" => {
                let pending = store::list_pending(workspace_dir, recipient, channel)?;
                if pending.is_empty() {
                    return Ok(ToolResult {
                        success: true,
                        output: "No pending outbound messages.".to_string(),
                        error: None,
                    });
                }

                let mut output = format!("{} pending message(s):\n", pending.len());
                for msg in &pending {
                    let ts = msg.created_at.format("%Y-%m-%d %H:%M UTC");
                    output.push_str(&format!(
                        "- [{}] {}/{} (queued {ts}, priority: {}): {}\n",
                        msg.id,
                        msg.channel,
                        msg.recipient,
                        msg.priority,
                        truncate_preview(&msg.message, 100),
                    ));
                }
                Ok(ToolResult {
                    success: true,
                    output,
                    error: None,
                })
            }
            "clear" => {
                // Clearing requires write autonomy
                if !self.security.can_act() {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some("Action blocked: autonomy is read-only".into()),
                    });
                }
                if !self.security.record_action() {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some("Action blocked: rate limit exceeded".into()),
                    });
                }

                let ids: Option<Vec<String>> = args.get("ids").and_then(|v| {
                    v.as_array().map(|arr| {
                        arr.iter()
                            .filter_map(|item| item.as_str().map(String::from))
                            .collect()
                    })
                });

                let count = if let Some(ref ids) = ids {
                    let id_refs: Vec<&str> = ids.iter().map(String::as_str).collect();
                    store::clear_messages(workspace_dir, Some(&id_refs), None, None)?
                } else {
                    store::clear_messages(workspace_dir, None, recipient, channel)?
                };

                Ok(ToolResult {
                    success: true,
                    output: format!("Cleared {count} message(s) from the outbound queue."),
                    error: None,
                })
            }
            "keep" => Ok(ToolResult {
                success: true,
                output: "Acknowledged. Queued messages will be kept for later delivery.".to_string(),
                error: None,
            }),
            other => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Unknown action '{other}'. Use 'list', 'clear', or 'keep'."
                )),
            }),
        }
    }
}

fn truncate_preview(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("{}...", &s[..max_len])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::AutonomyLevel;

    fn test_security(level: AutonomyLevel, max_actions: u32) -> Arc<SecurityPolicy> {
        Arc::new(SecurityPolicy {
            autonomy: level,
            max_actions_per_hour: max_actions,
            workspace_dir: std::env::temp_dir(),
            ..SecurityPolicy::default()
        })
    }

    #[test]
    fn tool_name() {
        let config = Arc::new(Config::default());
        let security = test_security(AutonomyLevel::Full, 100);
        let tool = ManageOutboundQueueTool::new(config, security);
        assert_eq!(tool.name(), "manage_outbound_queue");
    }

    #[test]
    fn schema_requires_action() {
        let config = Arc::new(Config::default());
        let security = test_security(AutonomyLevel::Full, 100);
        let tool = ManageOutboundQueueTool::new(config, security);
        let schema = tool.parameters_schema();
        let required = schema["required"].as_array().unwrap();
        assert!(required.contains(&json!("action")));
    }

    #[tokio::test]
    async fn list_empty_queue() {
        let mut config = Config::default();
        let tmp = tempfile::TempDir::new().unwrap();
        config.workspace_dir = tmp.path().to_path_buf();

        let security = test_security(AutonomyLevel::Full, 100);
        let tool = ManageOutboundQueueTool::new(Arc::new(config), security);

        let result = tool.execute(json!({"action": "list"})).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("No pending"));
    }

    #[tokio::test]
    async fn clear_blocks_readonly() {
        let config = Arc::new(Config::default());
        let security = test_security(AutonomyLevel::ReadOnly, 100);
        let tool = ManageOutboundQueueTool::new(config, security);

        let result = tool.execute(json!({"action": "clear"})).await.unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("read-only"));
    }

    #[tokio::test]
    async fn keep_always_succeeds() {
        let config = Arc::new(Config::default());
        let security = test_security(AutonomyLevel::Full, 100);
        let tool = ManageOutboundQueueTool::new(config, security);

        let result = tool.execute(json!({"action": "keep"})).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("Acknowledged"));
    }

    #[tokio::test]
    async fn unknown_action_fails() {
        let config = Arc::new(Config::default());
        let security = test_security(AutonomyLevel::Full, 100);
        let tool = ManageOutboundQueueTool::new(config, security);

        let result = tool.execute(json!({"action": "delete"})).await.unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("Unknown action"));
    }
}
