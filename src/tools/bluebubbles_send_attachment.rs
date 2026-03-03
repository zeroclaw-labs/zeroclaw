use std::sync::Arc;

use async_trait::async_trait;
use uuid::Uuid;

use crate::security::SecurityPolicy;
use crate::tools::traits::{Tool, ToolResult};

/// Send a media file (image, audio, document) via iMessage through BlueBubbles.
///
/// The agent provides base64-encoded file bytes; this tool POSTs them as a
/// multipart upload to the BB Private API. Supports optional captions and
/// voice-memo marking.
pub struct BlueBubblesSendAttachmentTool {
    security: Arc<SecurityPolicy>,
    server_url: String,
    password: String,
    client: reqwest::Client,
}

impl BlueBubblesSendAttachmentTool {
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
                .timeout(std::time::Duration::from_secs(60))
                .build()
                .map_err(|e| {
                    anyhow::anyhow!(
                        "Failed to build BlueBubblesSendAttachmentTool HTTP client: {e}"
                    )
                })?,
        })
    }

    fn api_url(&self, path: &str) -> String {
        format!("{}{path}", self.server_url)
    }
}

#[async_trait]
impl Tool for BlueBubblesSendAttachmentTool {
    fn name(&self) -> &str {
        "bluebubbles_send_attachment"
    }

    fn description(&self) -> &str {
        "Send a media attachment (image, audio, document) via iMessage through the \
        BlueBubbles server. Provide base64-encoded file bytes, a filename, and the \
        target chat GUID."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "required": ["chat_guid", "filename", "data_base64"],
            "properties": {
                "chat_guid": {
                    "type": "string",
                    "description": "BB chat GUID (e.g. `iMessage;-;+15551234567`)."
                },
                "filename": {
                    "type": "string",
                    "description": "Filename including extension (e.g. `photo.jpg`)."
                },
                "data_base64": {
                    "type": "string",
                    "description": "Base64-encoded file bytes."
                },
                "mime_type": {
                    "type": "string",
                    "description": "MIME type (e.g. `image/jpeg`, `audio/mp4`). Defaults to `application/octet-stream`."
                },
                "caption": {
                    "type": "string",
                    "description": "Optional text caption to accompany the attachment."
                },
                "as_voice": {
                    "type": "boolean",
                    "description": "Mark as a voice memo (default: false)."
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
        let chat_guid = match args.get("chat_guid").and_then(|v| v.as_str()) {
            Some(g) if !g.trim().is_empty() => g.trim().to_string(),
            _ => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some("chat_guid is required".into()),
                })
            }
        };
        let filename = match args.get("filename").and_then(|v| v.as_str()) {
            Some(f) if !f.trim().is_empty() => f.trim().to_string(),
            _ => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some("filename is required".into()),
                })
            }
        };
        const MAX_ATTACHMENT_B64_LEN: usize = 34 * 1024 * 1024; // ~25 MiB decoded (base64 overhead ~4/3)
        let data_b64 = match args
            .get("data_base64")
            .and_then(|v| v.as_str())
            .map(str::trim)
        {
            Some(b) if !b.is_empty() => {
                if b.len() > MAX_ATTACHMENT_B64_LEN {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!(
                            "data_base64 exceeds maximum allowed size ({MAX_ATTACHMENT_B64_LEN} bytes)"
                        )),
                    });
                }
                b
            }
            _ => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some("data_base64 is required".into()),
                })
            }
        };
        let mime_type = args
            .get("mime_type")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .unwrap_or("application/octet-stream")
            .to_string();
        let caption = args
            .get("caption")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let as_voice = args
            .get("as_voice")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let file_bytes =
            match base64::Engine::decode(&base64::engine::general_purpose::STANDARD, data_b64) {
                Ok(b) => b,
                Err(e) => {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!("data_base64 is not valid base64: {e}")),
                    })
                }
            };

        let temp_guid = Uuid::new_v4().to_string();
        let url = self.api_url("/api/v1/message/attachment");

        // Build multipart form fields required by BB Private API.
        let attachment_part = match reqwest::multipart::Part::bytes(file_bytes)
            .file_name(filename.clone())
            .mime_str(&mime_type)
        {
            Ok(p) => p,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("invalid mime_type \"{mime_type}\": {e}")),
                })
            }
        };
        let mut form = reqwest::multipart::Form::new()
            .text("chatGuid", chat_guid.clone())
            .text("tempGuid", temp_guid)
            .text("name", filename.clone())
            .text("method", "private-api")
            .part("attachment", attachment_part);

        if !caption.is_empty() {
            form = form.text("message", caption);
        }
        if as_voice {
            form = form.text("isAudioMessage", "true");
        }

        let resp = match self
            .client
            .post(&url)
            .query(&[("password", &self.password)])
            .multipart(form)
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!(
                        "BB send_attachment request failed: {}",
                        e.without_url()
                    )),
                })
            }
        };

        if resp.status().is_success() {
            Ok(ToolResult {
                success: true,
                output: format!("Attachment \"{filename}\" sent to {chat_guid}"),
                error: None,
            })
        } else {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            let sanitized = crate::providers::sanitize_api_error(&body);
            Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("BB send_attachment failed ({status}): {sanitized}")),
            })
        }
    }
}
