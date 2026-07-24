use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use futures_util::TryStreamExt;
use mail_parser::{MessageParser, MimeHeaders};
use zeroclaw_api::attribution::ToolKind;
use zeroclaw_api::tool::{Tool, ToolResult};
use zeroclaw_config::scattered_types::EmailConfig;

use crate::email_imap::imap_connect;
use crate::email_search::{format_address, resolve_channel};

zeroclaw_api::tool_attribution!(EmailReadTool, ToolKind::Plugin);

/// Fetch the full body of a specific email by UID. Never sets \Seen.
pub struct EmailReadTool {
    email_configs: Arc<HashMap<String, EmailConfig>>,
    auth_service: Option<Arc<zeroclaw_providers::auth::AuthService>>,
}

impl EmailReadTool {
    pub fn new(
        email_configs: Arc<HashMap<String, EmailConfig>>,
        auth_service: Option<Arc<zeroclaw_providers::auth::AuthService>>,
    ) -> Self {
        Self {
            email_configs,
            auth_service,
        }
    }
}

#[async_trait]
impl Tool for EmailReadTool {
    fn name(&self) -> &str {
        "email_read"
    }

    fn description(&self) -> &str {
        "Fetch the full content of an email by its UID (from email_search results). Returns sender, subject, date, body text, and attachment names. Never marks the email as read."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "required": ["uid"],
            "properties": {
                "uid": {
                    "type": "integer",
                    "description": "The IMAP UID of the email to read (returned by email_search)."
                },
                "channel": {
                    "type": "string",
                    "description": "Email channel alias (e.g. 'hotmail', 'default'). Omit to use the first enabled channel."
                },
                "folder": {
                    "type": "string",
                    "description": "Mailbox folder containing the email. Defaults to INBOX."
                }
            }
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let uid: u32 = args
            .get("uid")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| anyhow::Error::msg("uid is required"))?
            .try_into()
            .map_err(|_| {
                anyhow::Error::msg(
                    "uid exceeds maximum IMAP UID (4294967295); \
                     check that the uid came from email_search on this mailbox",
                )
            })?;
        let channel_alias = args.get("channel").and_then(|v| v.as_str());
        let folder = args
            .get("folder")
            .and_then(|v| v.as_str())
            .unwrap_or("INBOX");

        let (alias, cfg) = resolve_channel(&self.email_configs, channel_alias)?;

        let mut session = imap_connect(&cfg, self.auth_service.as_ref(), &alias).await?;
        // EXAMINE opens the mailbox read-only: no \Recent reset, no implicit
        // flag changes. Combined with BODY.PEEK below this guarantees the
        // observer invariant — this tool never mutates server state.
        session.examine(folder).await?;

        // BODY.PEEK[] — never sets \Seen. We do not touch flags under any circumstance.
        let messages = session.uid_fetch(uid.to_string(), "BODY.PEEK[]").await?;
        let messages: Vec<_> = messages.try_collect().await?;
        let _ = session.logout().await;

        let msg = messages.first().ok_or_else(|| {
            anyhow::Error::msg(format!(
                "no message found with uid {} in {}/{}",
                uid, alias, folder
            ))
        })?;

        let body_bytes = msg
            .body()
            .ok_or_else(|| anyhow::Error::msg(format!("empty response for uid {}", uid)))?;

        let parser = MessageParser::default();
        let parsed = parser
            .parse(body_bytes)
            .ok_or_else(|| anyhow::Error::msg(format!("failed to parse message uid {}", uid)))?;

        let from = format_address(&parsed);
        let subject = parsed.subject().unwrap_or("(no subject)").to_string();
        let date = parsed
            .date()
            .map(|d| {
                format!(
                    "{:04}-{:02}-{:02} {:02}:{:02}",
                    d.year, d.month, d.day, d.hour, d.minute
                )
            })
            .unwrap_or_else(|| "unknown".into());

        let to: Vec<String> = parsed
            .to()
            .map(|addrs| {
                addrs
                    .iter()
                    .filter_map(|a| a.address())
                    .map(|s| s.to_string())
                    .collect()
            })
            .unwrap_or_default();

        let body_text = if let Some(text) = parsed.body_text(0) {
            text.to_string()
        } else if let Some(html) = parsed.body_html(0) {
            strip_html(html.as_ref())
        } else {
            "(no readable body)".into()
        };

        let attachments: Vec<String> = parsed
            .attachments()
            .filter_map(|part| {
                let part: &mail_parser::MessagePart = part;
                MimeHeaders::attachment_name(part).map(|n| n.to_string())
            })
            .collect();

        let mut output = format!(
            "From:    {}\nTo:      {}\nDate:    {}\nSubject: {}\n",
            from,
            to.join(", "),
            date,
            subject,
        );
        if !attachments.is_empty() {
            output.push_str(&format!("Attachments: {}\n", attachments.join(", ")));
        }
        output.push_str("\n---\n\n");
        output.push_str(&body_text);

        Ok(ToolResult {
            success: true,
            output: output.into(),
            error: None,
        })
    }
}

fn strip_html(html: &str) -> String {
    let mut result = String::new();
    let mut in_tag = false;
    for ch in html.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => result.push(ch),
            _ => {}
        }
    }
    result.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    #[test]
    fn uid_rejects_value_above_u32_max() {
        // 4294967297 = u32::MAX + 2; must not silently wrap to a different UID.
        let args = serde_json::json!({"uid": 4294967297u64});
        let raw = args.get("uid").and_then(|v| v.as_u64()).unwrap();
        let result: Result<u32, _> = raw.try_into();
        assert!(
            result.is_err(),
            "oversized UID must fail checked conversion"
        );
    }
}
