use super::traits::{Tool, ToolResult};
use crate::dashboard::NewInboxItem;
use async_trait::async_trait;
use serde_json::Value;

#[derive(Clone)]
pub struct InboxNotifyTool {
    db: crate::aria::db::AriaDb,
    tenant_id: String,
}

impl InboxNotifyTool {
    pub fn new(db: crate::aria::db::AriaDb, tenant_id: impl Into<String>) -> Self {
        Self {
            db,
            tenant_id: tenant_id.into(),
        }
    }
}

#[async_trait]
impl Tool for InboxNotifyTool {
    fn name(&self) -> &str {
        "inbox_notify"
    }

    fn description(&self) -> &str {
        "Create a durable inbox message for the user with metadata and optional chat linkage"
    }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "title": {"type": "string", "description": "Short headline shown in inbox list"},
                "message": {"type": "string", "description": "Full message body shown in expanded view"},
                "preview": {"type": "string", "description": "Optional compact summary"},
                "sourceType": {"type": "string", "description": "Origin type: agent, subagent, or system"},
                "sourceId": {"type": "string", "description": "Optional origin identifier"},
                "runId": {"type": "string", "description": "Optional run identifier"},
                "chatId": {"type": "string", "description": "Optional existing chat id"},
                "priority": {"type": "string", "description": "Optional priority label"},
                "category": {"type": "string", "description": "Optional category label"},
                "metadata": {"type": "object", "description": "Arbitrary metadata to store with the inbox item"}
            },
            "required": ["title", "message"]
        })
    }

    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        let title = args
            .get("title")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_string)
            .ok_or_else(|| anyhow::anyhow!("title is required"))?;

        let message = args
            .get("message")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_string)
            .ok_or_else(|| anyhow::anyhow!("message is required"))?;

        let source_type = args
            .get("sourceType")
            .and_then(Value::as_str)
            .unwrap_or("agent")
            .to_string();

        let mut metadata = args
            .get("metadata")
            .cloned()
            .unwrap_or_else(|| serde_json::json!({}));
        if !metadata.is_object() {
            metadata = serde_json::json!({ "value": metadata });
        }
        if let Some(priority) = args.get("priority").and_then(Value::as_str) {
            metadata["priority"] = Value::String(priority.to_string());
        }
        if let Some(category) = args.get("category").and_then(Value::as_str) {
            metadata["category"] = Value::String(category.to_string());
        }

        let item = NewInboxItem {
            source_type,
            source_id: args
                .get("sourceId")
                .and_then(Value::as_str)
                .map(str::to_string),
            run_id: args
                .get("runId")
                .and_then(Value::as_str)
                .map(str::to_string),
            chat_id: args
                .get("chatId")
                .and_then(Value::as_str)
                .map(str::to_string),
            title: title.clone(),
            preview: args
                .get("preview")
                .and_then(Value::as_str)
                .map(str::to_string)
                .or_else(|| Some(message.chars().take(160).collect::<String>())),
            body: Some(message.clone()),
            metadata,
            status: Some("unread".to_string()),
        };

        let inbox_id = crate::dashboard::create_inbox_item(&self.db, &self.tenant_id, &item)?;

        crate::status_events::emit(
            "inbox.item.created",
            serde_json::json!({
                "tenantId": self.tenant_id,
                "id": inbox_id,
                "title": title,
                "sourceType": item.source_type,
            }),
        );

        Ok(ToolResult {
            success: true,
            output: serde_json::json!({
                "id": inbox_id,
                "status": "unread",
            })
            .to_string(),
            error: None,
        })
    }
}
