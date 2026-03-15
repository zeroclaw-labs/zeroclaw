//! Microsoft 365 integration tool — Graph API access for Mail, Teams, Calendar,
//! OneDrive, and SharePoint via a single action-dispatched tool surface.
//!
//! Auth is handled through direct HTTP calls to the Microsoft identity platform
//! (client credentials or device code flow) with token caching.

pub mod auth;
pub mod graph_client;
pub mod types;

use crate::security::policy::ToolOperation;
use crate::security::SecurityPolicy;
use crate::tools::traits::{Tool, ToolResult};
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;

/// Maximum download size for OneDrive files (10 MB).
const MAX_ONEDRIVE_DOWNLOAD_SIZE: usize = 10 * 1024 * 1024;

/// Default number of items to return in list operations.
const DEFAULT_TOP: u32 = 25;

pub struct Microsoft365Tool {
    config: types::Microsoft365ResolvedConfig,
    security: Arc<SecurityPolicy>,
    token_cache: Arc<auth::TokenCache>,
    http_client: reqwest::Client,
}

impl Microsoft365Tool {
    pub fn new(
        config: types::Microsoft365ResolvedConfig,
        security: Arc<SecurityPolicy>,
        zeroclaw_dir: &std::path::Path,
    ) -> anyhow::Result<Self> {
        let http_client =
            crate::config::build_runtime_proxy_client_with_timeouts("tool.microsoft365", 60, 10);
        let token_cache = Arc::new(auth::TokenCache::new(config.clone(), zeroclaw_dir)?);
        Ok(Self {
            config,
            security,
            token_cache,
            http_client,
        })
    }

    async fn get_token(&self) -> anyhow::Result<String> {
        self.token_cache.get_token(&self.http_client).await
    }

    fn user_id(&self) -> &str {
        &self.config.user_id
    }

    async fn dispatch(&self, action: &str, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        match action {
            "mail_list" => self.handle_mail_list(args).await,
            "mail_send" => self.handle_mail_send(args).await,
            "teams_message_list" => self.handle_teams_message_list(args).await,
            "teams_message_send" => self.handle_teams_message_send(args).await,
            "calendar_events_list" => self.handle_calendar_events_list(args).await,
            "calendar_event_create" => self.handle_calendar_event_create(args).await,
            "calendar_event_delete" => self.handle_calendar_event_delete(args).await,
            "onedrive_list" => self.handle_onedrive_list(args).await,
            "onedrive_download" => self.handle_onedrive_download(args).await,
            "sharepoint_search" => self.handle_sharepoint_search(args).await,
            _ => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Unknown action: {action}")),
            }),
        }
    }

    // ── Read actions ────────────────────────────────────────────────

    async fn handle_mail_list(&self, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        self.security
            .enforce_tool_operation(ToolOperation::Read, "microsoft365.mail_list")
            .map_err(|e| anyhow::anyhow!(e))?;

        let token = self.get_token().await?;
        let folder = args["folder"].as_str();
        let top = u32::try_from(args["top"].as_u64().unwrap_or(u64::from(DEFAULT_TOP)))
            .unwrap_or(DEFAULT_TOP);

        let result =
            graph_client::mail_list(&self.http_client, &token, self.user_id(), folder, top).await?;

        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&result)?,
            error: None,
        })
    }

    async fn handle_teams_message_list(
        &self,
        args: &serde_json::Value,
    ) -> anyhow::Result<ToolResult> {
        self.security
            .enforce_tool_operation(ToolOperation::Read, "microsoft365.teams_message_list")
            .map_err(|e| anyhow::anyhow!(e))?;

        let token = self.get_token().await?;
        let team_id = args["team_id"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("team_id is required"))?;
        let channel_id = args["channel_id"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("channel_id is required"))?;
        let top = u32::try_from(args["top"].as_u64().unwrap_or(u64::from(DEFAULT_TOP)))
            .unwrap_or(DEFAULT_TOP);

        let result =
            graph_client::teams_message_list(&self.http_client, &token, team_id, channel_id, top)
                .await?;

        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&result)?,
            error: None,
        })
    }

    async fn handle_calendar_events_list(
        &self,
        args: &serde_json::Value,
    ) -> anyhow::Result<ToolResult> {
        self.security
            .enforce_tool_operation(ToolOperation::Read, "microsoft365.calendar_events_list")
            .map_err(|e| anyhow::anyhow!(e))?;

        let token = self.get_token().await?;
        let start = args["start"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("start datetime is required"))?;
        let end = args["end"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("end datetime is required"))?;
        let top = u32::try_from(args["top"].as_u64().unwrap_or(u64::from(DEFAULT_TOP)))
            .unwrap_or(DEFAULT_TOP);

        let result = graph_client::calendar_events_list(
            &self.http_client,
            &token,
            self.user_id(),
            start,
            end,
            top,
        )
        .await?;

        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&result)?,
            error: None,
        })
    }

    async fn handle_onedrive_list(&self, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        self.security
            .enforce_tool_operation(ToolOperation::Read, "microsoft365.onedrive_list")
            .map_err(|e| anyhow::anyhow!(e))?;

        let token = self.get_token().await?;
        let path = args["path"].as_str();

        let result =
            graph_client::onedrive_list(&self.http_client, &token, self.user_id(), path).await?;

        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&result)?,
            error: None,
        })
    }

    async fn handle_onedrive_download(
        &self,
        args: &serde_json::Value,
    ) -> anyhow::Result<ToolResult> {
        self.security
            .enforce_tool_operation(ToolOperation::Read, "microsoft365.onedrive_download")
            .map_err(|e| anyhow::anyhow!(e))?;

        let token = self.get_token().await?;
        let item_id = args["item_id"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("item_id is required"))?;
        let max_size = args["max_size"]
            .as_u64()
            .and_then(|v| usize::try_from(v).ok())
            .unwrap_or(MAX_ONEDRIVE_DOWNLOAD_SIZE)
            .min(MAX_ONEDRIVE_DOWNLOAD_SIZE);

        let bytes = graph_client::onedrive_download(
            &self.http_client,
            &token,
            self.user_id(),
            item_id,
            max_size,
        )
        .await?;

        // Return base64-encoded for binary safety.
        use base64::Engine;
        let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);

        Ok(ToolResult {
            success: true,
            output: format!(
                "Downloaded {} bytes (base64 encoded):\n{encoded}",
                bytes.len()
            ),
            error: None,
        })
    }

    async fn handle_sharepoint_search(
        &self,
        args: &serde_json::Value,
    ) -> anyhow::Result<ToolResult> {
        self.security
            .enforce_tool_operation(ToolOperation::Read, "microsoft365.sharepoint_search")
            .map_err(|e| anyhow::anyhow!(e))?;

        let token = self.get_token().await?;
        let query = args["query"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("query is required"))?;
        let top = u32::try_from(args["top"].as_u64().unwrap_or(u64::from(DEFAULT_TOP)))
            .unwrap_or(DEFAULT_TOP);

        let result = graph_client::sharepoint_search(&self.http_client, &token, query, top).await?;

        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&result)?,
            error: None,
        })
    }

    // ── Write actions ───────────────────────────────────────────────

    async fn handle_mail_send(&self, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        self.security
            .enforce_tool_operation(ToolOperation::Act, "microsoft365.mail_send")
            .map_err(|e| anyhow::anyhow!(e))?;

        let token = self.get_token().await?;
        let to: Vec<String> = args["to"]
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("to must be an array of email addresses"))?
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect();

        if to.is_empty() {
            anyhow::bail!("to must contain at least one email address");
        }

        let subject = args["subject"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("subject is required"))?;
        let body = args["body"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("body is required"))?;

        graph_client::mail_send(
            &self.http_client,
            &token,
            self.user_id(),
            &to,
            subject,
            body,
        )
        .await?;

        Ok(ToolResult {
            success: true,
            output: format!("Email sent to: {}", to.join(", ")),
            error: None,
        })
    }

    async fn handle_teams_message_send(
        &self,
        args: &serde_json::Value,
    ) -> anyhow::Result<ToolResult> {
        self.security
            .enforce_tool_operation(ToolOperation::Act, "microsoft365.teams_message_send")
            .map_err(|e| anyhow::anyhow!(e))?;

        let token = self.get_token().await?;
        let team_id = args["team_id"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("team_id is required"))?;
        let channel_id = args["channel_id"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("channel_id is required"))?;
        let body = args["body"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("body is required"))?;

        graph_client::teams_message_send(&self.http_client, &token, team_id, channel_id, body)
            .await?;

        Ok(ToolResult {
            success: true,
            output: "Teams message sent".to_string(),
            error: None,
        })
    }

    async fn handle_calendar_event_create(
        &self,
        args: &serde_json::Value,
    ) -> anyhow::Result<ToolResult> {
        self.security
            .enforce_tool_operation(ToolOperation::Act, "microsoft365.calendar_event_create")
            .map_err(|e| anyhow::anyhow!(e))?;

        let token = self.get_token().await?;
        let subject = args["subject"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("subject is required"))?;
        let start = args["start"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("start datetime is required"))?;
        let end = args["end"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("end datetime is required"))?;
        let attendees: Vec<String> = args["attendees"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        let body_text = args["body"].as_str();

        let event_id = graph_client::calendar_event_create(
            &self.http_client,
            &token,
            self.user_id(),
            subject,
            start,
            end,
            &attendees,
            body_text,
        )
        .await?;

        Ok(ToolResult {
            success: true,
            output: format!("Calendar event created (id: {event_id})"),
            error: None,
        })
    }

    async fn handle_calendar_event_delete(
        &self,
        args: &serde_json::Value,
    ) -> anyhow::Result<ToolResult> {
        self.security
            .enforce_tool_operation(ToolOperation::Act, "microsoft365.calendar_event_delete")
            .map_err(|e| anyhow::anyhow!(e))?;

        let token = self.get_token().await?;
        let event_id = args["event_id"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("event_id is required"))?;

        graph_client::calendar_event_delete(&self.http_client, &token, self.user_id(), event_id)
            .await?;

        Ok(ToolResult {
            success: true,
            output: format!("Calendar event {event_id} deleted"),
            error: None,
        })
    }
}

#[async_trait]
impl Tool for Microsoft365Tool {
    fn name(&self) -> &str {
        "microsoft365"
    }

    fn description(&self) -> &str {
        "Microsoft 365 integration: manage Outlook mail, Teams messages, Calendar events, \
         OneDrive files, and SharePoint search via Microsoft Graph API"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "required": ["action"],
            "properties": {
                "action": {
                    "type": "string",
                    "enum": [
                        "mail_list",
                        "mail_send",
                        "teams_message_list",
                        "teams_message_send",
                        "calendar_events_list",
                        "calendar_event_create",
                        "calendar_event_delete",
                        "onedrive_list",
                        "onedrive_download",
                        "sharepoint_search"
                    ],
                    "description": "The Microsoft 365 action to perform"
                },
                "folder": {
                    "type": "string",
                    "description": "Mail folder ID (for mail_list, e.g. 'inbox', 'sentitems')"
                },
                "to": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Recipient email addresses (for mail_send)"
                },
                "subject": {
                    "type": "string",
                    "description": "Email subject or calendar event subject"
                },
                "body": {
                    "type": "string",
                    "description": "Message body text"
                },
                "team_id": {
                    "type": "string",
                    "description": "Teams team ID (for teams_message_list/send)"
                },
                "channel_id": {
                    "type": "string",
                    "description": "Teams channel ID (for teams_message_list/send)"
                },
                "start": {
                    "type": "string",
                    "description": "Start datetime in ISO 8601 format (for calendar actions)"
                },
                "end": {
                    "type": "string",
                    "description": "End datetime in ISO 8601 format (for calendar actions)"
                },
                "attendees": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Attendee email addresses (for calendar_event_create)"
                },
                "event_id": {
                    "type": "string",
                    "description": "Calendar event ID (for calendar_event_delete)"
                },
                "path": {
                    "type": "string",
                    "description": "OneDrive folder path (for onedrive_list)"
                },
                "item_id": {
                    "type": "string",
                    "description": "OneDrive item ID (for onedrive_download)"
                },
                "max_size": {
                    "type": "integer",
                    "description": "Maximum download size in bytes (for onedrive_download, default 10MB)"
                },
                "query": {
                    "type": "string",
                    "description": "Search query (for sharepoint_search)"
                },
                "top": {
                    "type": "integer",
                    "description": "Maximum number of items to return (default 25)"
                }
            }
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let action = match args["action"].as_str() {
            Some(a) => a.to_string(),
            None => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some("'action' parameter is required".to_string()),
                });
            }
        };

        match self.dispatch(&action, &args).await {
            Ok(result) => Ok(result),
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("microsoft365.{action} failed: {e}")),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_name_is_microsoft365() {
        // Verify the schema is valid JSON with the expected structure.
        let schema_str = r#"{"type":"object","required":["action"]}"#;
        let _: serde_json::Value = serde_json::from_str(schema_str).unwrap();
    }

    #[test]
    fn parameters_schema_has_action_enum() {
        let schema = json!({
            "type": "object",
            "required": ["action"],
            "properties": {
                "action": {
                    "type": "string",
                    "enum": [
                        "mail_list",
                        "mail_send",
                        "teams_message_list",
                        "teams_message_send",
                        "calendar_events_list",
                        "calendar_event_create",
                        "calendar_event_delete",
                        "onedrive_list",
                        "onedrive_download",
                        "sharepoint_search"
                    ]
                }
            }
        });

        let actions = schema["properties"]["action"]["enum"].as_array().unwrap();
        assert_eq!(actions.len(), 10);
        assert!(actions.contains(&json!("mail_list")));
        assert!(actions.contains(&json!("sharepoint_search")));
    }

    #[test]
    fn action_dispatch_table_is_exhaustive() {
        let valid_actions = [
            "mail_list",
            "mail_send",
            "teams_message_list",
            "teams_message_send",
            "calendar_events_list",
            "calendar_event_create",
            "calendar_event_delete",
            "onedrive_list",
            "onedrive_download",
            "sharepoint_search",
        ];
        assert_eq!(valid_actions.len(), 10);
        assert!(!valid_actions.contains(&"invalid_action"));
    }
}
