use std::sync::Arc;

use async_trait::async_trait;
use uuid::Uuid;

use crate::security::SecurityPolicy;
use crate::tools::traits::{Tool, ToolResult};

/// Thread-aware reply, message edit, and message retract for BlueBubbles iMessage.
///
/// Single tool with action dispatch so the LLM can perform all three operations
/// through one call surface.
///
/// BB API endpoints used:
/// - reply:  POST `/api/v1/message/text`  (with `selectedMessageGuid`)
/// - edit:   POST `/api/v1/message/{guid}/edit`
/// - unsend: POST `/api/v1/message/{guid}/unsend`
pub struct BlueBubblesMessageTool {
    security: Arc<SecurityPolicy>,
    server_url: String,
    password: String,
    client: reqwest::Client,
}

impl BlueBubblesMessageTool {
    pub fn new(
        security: Arc<SecurityPolicy>,
        server_url: String,
        password: String,
    ) -> anyhow::Result<Self> {
        Ok(Self {
            security,
            server_url: server_url.trim_end_matches('/').to_string(),
            password,
            client: reqwest::ClientBuilder::new()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .map_err(|e| {
                    anyhow::anyhow!("Failed to build BlueBubblesMessageTool HTTP client: {e}")
                })?,
        })
    }

    fn api_url(&self, path: &str) -> String {
        format!("{}{path}", self.server_url)
    }
}

#[async_trait]
impl Tool for BlueBubblesMessageTool {
    fn name(&self) -> &str {
        "bluebubbles_message"
    }

    fn description(&self) -> &str {
        "Reply to, edit, or unsend an iMessage via the BlueBubbles server. \
        Actions: reply (thread-aware reply to a specific message), \
        edit (edit an already-sent message), unsend (retract a sent message)."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "required": ["action", "message_id"],
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["reply", "edit", "unsend"],
                    "description": "Message action to perform."
                },
                "message_id": {
                    "type": "string",
                    "description": "BB message GUID to reply to / edit / unsend."
                },
                "chat_guid": {
                    "type": "string",
                    "description": "BB chat GUID (e.g. `iMessage;-;+15551234567`). Required only for reply."
                },
                "text": {
                    "type": "string",
                    "description": "Message text — required for reply and edit."
                }
            },
            "additionalProperties": false
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        if !self.security.can_act() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Action blocked: read-only autonomy level".into()),
            });
        }
        if !self.security.record_action() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Rate limit exceeded: too many actions in the last hour".into()),
            });
        }
        let action = match args.get("action").and_then(|v| v.as_str()) {
            Some(a) if !a.trim().is_empty() => a.trim().to_string(),
            _ => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some("action is required".into()),
                })
            }
        };
        let message_id = match args.get("message_id").and_then(|v| v.as_str()) {
            Some(id) if !id.trim().is_empty() => id.trim().to_string(),
            _ => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some("message_id is required".into()),
                })
            }
        };

        match action.as_str() {
            "reply" => {
                let chat_guid = match args.get("chat_guid").and_then(|v| v.as_str()) {
                    Some(g) if !g.trim().is_empty() => g.trim().to_string(),
                    _ => {
                        return Ok(ToolResult {
                            success: false,
                            output: String::new(),
                            error: Some("chat_guid is required for reply".into()),
                        })
                    }
                };
                let text = match args.get("text").and_then(|v| v.as_str()) {
                    Some(t) if !t.trim().is_empty() => t.trim().to_string(),
                    _ => {
                        return Ok(ToolResult {
                            success: false,
                            output: String::new(),
                            error: Some("text is required for reply".into()),
                        })
                    }
                };
                let url = self.api_url("/api/v1/message/text");
                let body = serde_json::json!({
                    "chatGuid": chat_guid,
                    "tempGuid": Uuid::new_v4().to_string(),
                    "message": text,
                    "method": "private-api",
                    "selectedMessageGuid": message_id,
                });
                let resp = match self
                    .client
                    .post(&url)
                    .query(&[("password", &self.password)])
                    .json(&body)
                    .send()
                    .await
                {
                    Ok(r) => r,
                    Err(e) => {
                        return Ok(ToolResult {
                            success: false,
                            output: String::new(),
                            error: Some(format!("BB reply request failed: {}", e.without_url())),
                        })
                    }
                };
                if resp.status().is_success() {
                    Ok(ToolResult {
                        success: true,
                        output: format!("Replied to message {message_id}"),
                        error: None,
                    })
                } else {
                    let status = resp.status();
                    let body_text = resp.text().await.unwrap_or_default();
                    let sanitized = crate::providers::sanitize_api_error(&body_text);
                    Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!("BB reply failed ({status}): {sanitized}")),
                    })
                }
            }

            "edit" => {
                let text = match args.get("text").and_then(|v| v.as_str()) {
                    Some(t) if !t.trim().is_empty() => t.trim().to_string(),
                    _ => {
                        return Ok(ToolResult {
                            success: false,
                            output: String::new(),
                            error: Some("text is required for edit".into()),
                        })
                    }
                };
                let encoded_id = urlencoding::encode(&message_id).into_owned();
                let url = self.api_url(&format!("/api/v1/message/{encoded_id}/edit"));
                let body = serde_json::json!({
                    "editedMessage": text,
                    "backwardsCompatibilityMessage": text,
                });
                let resp = match self
                    .client
                    .post(&url)
                    .query(&[("password", &self.password)])
                    .json(&body)
                    .send()
                    .await
                {
                    Ok(r) => r,
                    Err(e) => {
                        return Ok(ToolResult {
                            success: false,
                            output: String::new(),
                            error: Some(format!("BB edit request failed: {}", e.without_url())),
                        })
                    }
                };
                if resp.status().is_success() {
                    Ok(ToolResult {
                        success: true,
                        output: format!("Edited message {message_id}"),
                        error: None,
                    })
                } else {
                    let status = resp.status();
                    let body_text = resp.text().await.unwrap_or_default();
                    let sanitized = crate::providers::sanitize_api_error(&body_text);
                    Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!("BB edit failed ({status}): {sanitized}")),
                    })
                }
            }

            "unsend" => {
                let encoded_id = urlencoding::encode(&message_id).into_owned();
                let url = self.api_url(&format!("/api/v1/message/{encoded_id}/unsend"));
                // partIndex 0 targets the text body; multi-part unsend is not yet exposed.
                let body = serde_json::json!({ "partIndex": 0 });
                let resp = match self
                    .client
                    .post(&url)
                    .query(&[("password", &self.password)])
                    .json(&body)
                    .send()
                    .await
                {
                    Ok(r) => r,
                    Err(e) => {
                        return Ok(ToolResult {
                            success: false,
                            output: String::new(),
                            error: Some(format!("BB unsend request failed: {}", e.without_url())),
                        })
                    }
                };
                if resp.status().is_success() {
                    Ok(ToolResult {
                        success: true,
                        output: format!("Unsent message {message_id}"),
                        error: None,
                    })
                } else {
                    let status = resp.status();
                    let body_text = resp.text().await.unwrap_or_default();
                    let sanitized = crate::providers::sanitize_api_error(&body_text);
                    Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!("BB unsend failed ({status}): {sanitized}")),
                    })
                }
            }

            other => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Unknown action \"{other}\". Supported: reply, edit, unsend"
                )),
            }),
        }
    }
}
