use async_trait::async_trait;

use crate::tools::traits::{Tool, ToolResult};

/// Manages BlueBubbles iMessage group chats (rename, participants, icon, leave).
///
/// Exposes a single tool with an `action` dispatch pattern so the LLM can
/// manage any group-chat operation through one call surface.
///
/// BB API endpoints used:
/// - rename_group:      PUT  `/api/v1/chat/{guid}`
/// - add_participant:   POST `/api/v1/chat/{guid}/participants/add`
/// - remove_participant:POST `/api/v1/chat/{guid}/participants/remove`
/// - leave_group:       POST `/api/v1/chat/{guid}/leave`
/// - set_group_icon:    POST `/api/v1/chat/{guid}/icon` (multipart)
pub struct BlueBubblesGroupTool {
    server_url: String,
    password: String,
    client: reqwest::Client,
}

impl BlueBubblesGroupTool {
    pub fn new(server_url: String, password: String) -> Self {
        Self {
            server_url: server_url.trim_end_matches('/').to_string(),
            password,
            client: reqwest::ClientBuilder::new()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .unwrap_or_else(|_| reqwest::Client::new()),
        }
    }

    fn api_url(&self, path: &str) -> String {
        format!("{}{path}", self.server_url)
    }
}

#[async_trait]
impl Tool for BlueBubblesGroupTool {
    fn name(&self) -> &str {
        "bluebubbles_group"
    }

    fn description(&self) -> &str {
        "Manage iMessage group chats via the BlueBubbles server. \
        Supported actions: rename_group, add_participant, remove_participant, \
        leave_group, set_group_icon."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "required": ["action", "chat_guid"],
            "properties": {
                "action": {
                    "type": "string",
                    "enum": [
                        "rename_group",
                        "add_participant",
                        "remove_participant",
                        "leave_group",
                        "set_group_icon"
                    ],
                    "description": "Group management action to perform."
                },
                "chat_guid": {
                    "type": "string",
                    "description": "BB chat GUID (e.g. `iMessage;+;group-abc`)."
                },
                "display_name": {
                    "type": "string",
                    "description": "New display name for rename_group."
                },
                "address": {
                    "type": "string",
                    "description": "Phone number or Apple ID for add/remove_participant."
                },
                "icon_base64": {
                    "type": "string",
                    "description": "Base64-encoded JPEG image bytes for set_group_icon (max 5 MB)."
                }
            },
            "additionalProperties": false
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
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
        let chat_guid = args
            .get("chat_guid")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();

        if chat_guid.is_empty() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("chat_guid is required".into()),
            });
        }

        let encoded_guid = urlencoding::encode(&chat_guid).into_owned();

        match action.as_str() {
            "rename_group" => {
                let name = match args.get("display_name").and_then(|v| v.as_str()) {
                    Some(n) if !n.trim().is_empty() => n.trim().to_string(),
                    _ => {
                        return Ok(ToolResult {
                            success: false,
                            output: String::new(),
                            error: Some("display_name is required for rename_group".into()),
                        })
                    }
                };
                let url = self.api_url(&format!("/api/v1/chat/{encoded_guid}"));
                let resp = self
                    .client
                    .put(&url)
                    .query(&[("password", &self.password)])
                    .json(&serde_json::json!({ "displayName": name }))
                    .send()
                    .await
                    .map_err(|e| {
                        anyhow::anyhow!("BB rename_group request failed: {}", e.without_url())
                    })?;
                if resp.status().is_success() {
                    Ok(ToolResult {
                        success: true,
                        output: format!("Group renamed to \"{name}\""),
                        error: None,
                    })
                } else {
                    let status = resp.status();
                    let body = resp.text().await.unwrap_or_default();
                    let sanitized = crate::providers::sanitize_api_error(&body);
                    Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!("BB rename_group failed ({status}): {sanitized}")),
                    })
                }
            }

            "add_participant" => {
                let address = match args.get("address").and_then(|v| v.as_str()) {
                    Some(a) if !a.trim().is_empty() => a.trim().to_string(),
                    _ => {
                        return Ok(ToolResult {
                            success: false,
                            output: String::new(),
                            error: Some("address is required for add_participant".into()),
                        })
                    }
                };
                let url = self.api_url(&format!("/api/v1/chat/{encoded_guid}/participants/add"));
                let resp = self
                    .client
                    .post(&url)
                    .query(&[("password", &self.password)])
                    .json(&serde_json::json!({ "address": address }))
                    .send()
                    .await
                    .map_err(|e| {
                        anyhow::anyhow!("BB add_participant request failed: {}", e.without_url())
                    })?;
                if resp.status().is_success() {
                    Ok(ToolResult {
                        success: true,
                        output: format!("Added {address} to group"),
                        error: None,
                    })
                } else {
                    let status = resp.status();
                    let body = resp.text().await.unwrap_or_default();
                    let sanitized = crate::providers::sanitize_api_error(&body);
                    Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!("BB add_participant failed ({status}): {sanitized}")),
                    })
                }
            }

            "remove_participant" => {
                let address = match args.get("address").and_then(|v| v.as_str()) {
                    Some(a) if !a.trim().is_empty() => a.trim().to_string(),
                    _ => {
                        return Ok(ToolResult {
                            success: false,
                            output: String::new(),
                            error: Some("address is required for remove_participant".into()),
                        })
                    }
                };
                let url = self.api_url(&format!("/api/v1/chat/{encoded_guid}/participants/remove"));
                let resp = self
                    .client
                    .post(&url)
                    .query(&[("password", &self.password)])
                    .json(&serde_json::json!({ "address": address }))
                    .send()
                    .await
                    .map_err(|e| {
                        anyhow::anyhow!("BB remove_participant request failed: {}", e.without_url())
                    })?;
                if resp.status().is_success() {
                    Ok(ToolResult {
                        success: true,
                        output: format!("Removed {address} from group"),
                        error: None,
                    })
                } else {
                    let status = resp.status();
                    let body = resp.text().await.unwrap_or_default();
                    let sanitized = crate::providers::sanitize_api_error(&body);
                    Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!(
                            "BB remove_participant failed ({status}): {sanitized}"
                        )),
                    })
                }
            }

            "leave_group" => {
                let url = self.api_url(&format!("/api/v1/chat/{encoded_guid}/leave"));
                let resp = self
                    .client
                    .post(&url)
                    .query(&[("password", &self.password)])
                    .send()
                    .await
                    .map_err(|e| {
                        anyhow::anyhow!("BB leave_group request failed: {}", e.without_url())
                    })?;
                if resp.status().is_success() {
                    Ok(ToolResult {
                        success: true,
                        output: "Left the group".into(),
                        error: None,
                    })
                } else {
                    let status = resp.status();
                    let body = resp.text().await.unwrap_or_default();
                    let sanitized = crate::providers::sanitize_api_error(&body);
                    Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!("BB leave_group failed ({status}): {sanitized}")),
                    })
                }
            }

            "set_group_icon" => {
                const MAX_ICON_B64_LEN: usize = 5 * 1024 * 1024; // 5 MiB base64 input
                let icon_b64 = match args.get("icon_base64").and_then(|v| v.as_str()) {
                    Some(b) if !b.trim().is_empty() => b.trim().to_string(),
                    _ => {
                        return Ok(ToolResult {
                            success: false,
                            output: String::new(),
                            error: Some("icon_base64 is required for set_group_icon".into()),
                        })
                    }
                };
                if icon_b64.len() > MAX_ICON_B64_LEN {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some("icon_base64 exceeds 5 MiB limit".into()),
                    });
                }
                let icon_bytes = match base64::Engine::decode(
                    &base64::engine::general_purpose::STANDARD,
                    &icon_b64,
                ) {
                    Ok(b) => b,
                    Err(e) => {
                        return Ok(ToolResult {
                            success: false,
                            output: String::new(),
                            error: Some(format!("icon_base64 is not valid base64: {e}")),
                        })
                    }
                };
                let url = self.api_url(&format!("/api/v1/chat/{encoded_guid}/icon"));
                let icon_part = match reqwest::multipart::Part::bytes(icon_bytes)
                    .file_name("icon.jpg")
                    .mime_str("image/jpeg")
                {
                    Ok(p) => p,
                    Err(e) => {
                        return Ok(ToolResult {
                            success: false,
                            output: String::new(),
                            error: Some(format!("failed to build icon multipart: {e}")),
                        })
                    }
                };
                let form = reqwest::multipart::Form::new().part("icon", icon_part);
                let resp = self
                    .client
                    .post(&url)
                    .query(&[("password", &self.password)])
                    .multipart(form)
                    .send()
                    .await
                    .map_err(|e| {
                        anyhow::anyhow!("BB set_group_icon request failed: {}", e.without_url())
                    })?;
                if resp.status().is_success() {
                    Ok(ToolResult {
                        success: true,
                        output: "Group icon updated".into(),
                        error: None,
                    })
                } else {
                    let status = resp.status();
                    let body = resp.text().await.unwrap_or_default();
                    let sanitized = crate::providers::sanitize_api_error(&body);
                    Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!("BB set_group_icon failed ({status}): {sanitized}")),
                    })
                }
            }

            other => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Unknown action \"{other}\". Supported: rename_group, add_participant, \
                    remove_participant, leave_group, set_group_icon"
                )),
            }),
        }
    }
}
