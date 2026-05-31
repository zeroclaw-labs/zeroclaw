use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use futures_util::TryStreamExt;
use mail_parser::MessageParser;
use zeroclaw_api::attribution::ToolKind;
use zeroclaw_api::tool::{Tool, ToolResult};
use zeroclaw_config::scattered_types::EmailConfig;

use crate::email_imap::imap_connect;

zeroclaw_api::tool_attribution!(EmailSearchTool, ToolKind::Plugin);

/// Search an IMAP mailbox without touching the \Seen flag on any message.
pub struct EmailSearchTool {
    email_configs: Arc<HashMap<String, EmailConfig>>,
    auth_service: Option<Arc<zeroclaw_providers::auth::AuthService>>,
}

impl EmailSearchTool {
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
impl Tool for EmailSearchTool {
    fn name(&self) -> &str {
        "email_search"
    }

    fn description(&self) -> &str {
        "Search emails in a configured IMAP mailbox. Never modifies any email (read-state is preserved). Use to check if someone sent a message, find emails by subject, or look up threads. Returns sender, subject, date, and UID for each match."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "channel": {
                    "type": "string",
                    "description": "Email channel alias (e.g. 'hotmail', 'default'). Omit to use the first enabled channel."
                },
                "from": {
                    "type": "string",
                    "description": "Filter by sender address or domain (e.g. 'alice@example.com' or 'example.com')."
                },
                "subject": {
                    "type": "string",
                    "description": "Filter by subject keyword (IMAP SUBJECT search, case-insensitive)."
                },
                "since": {
                    "type": "string",
                    "description": "Return emails on or after this date (YYYY-MM-DD)."
                },
                "before": {
                    "type": "string",
                    "description": "Return emails before this date (YYYY-MM-DD)."
                },
                "unseen_only": {
                    "type": "boolean",
                    "description": "When true, only return unread emails. Default: false (search all)."
                },
                "folder": {
                    "type": "string",
                    "description": "Mailbox folder to search. Defaults to INBOX."
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of results to return (default: 10, max: 50)."
                }
            }
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let channel_alias = args.get("channel").and_then(|v| v.as_str());
        let from_filter = args.get("from").and_then(|v| v.as_str());
        let subject_filter = args.get("subject").and_then(|v| v.as_str());
        let since = args.get("since").and_then(|v| v.as_str());
        let before = args.get("before").and_then(|v| v.as_str());
        let unseen_only = args
            .get("unseen_only")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let folder = args
            .get("folder")
            .and_then(|v| v.as_str())
            .unwrap_or("INBOX");
        let limit = args
            .get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(10)
            .min(50) as usize;

        // Reject injection in agent-supplied filters before any network round
        // trip. IMAP SEARCH criteria are built by string concatenation below,
        // so an unvalidated value could escape its quoted group and widen the
        // result set.
        if let Some(f) = from_filter {
            validate_search_filter("from", f)?;
        }
        if let Some(s) = subject_filter {
            validate_search_filter("subject", s)?;
        }

        let (alias, cfg) = resolve_channel(&self.email_configs, channel_alias)?;

        let mut session = imap_connect(&cfg, self.auth_service.as_ref(), &alias).await?;
        // EXAMINE opens the mailbox read-only: no \Recent reset, no implicit
        // flag changes. Combined with BODY.PEEK below this guarantees the
        // observer invariant — this tool never mutates server state.
        session.examine(folder).await?;

        let mut criteria_parts: Vec<String> = Vec::new();
        if unseen_only {
            criteria_parts.push("UNSEEN".into());
        } else {
            criteria_parts.push("ALL".into());
        }
        if let Some(f) = from_filter {
            criteria_parts.push(format!("FROM \"{}\"", f));
        }
        if let Some(s) = subject_filter {
            criteria_parts.push(format!("SUBJECT \"{}\"", s));
        }
        if let Some(d) = since {
            let imap_date = to_imap_date(d).ok_or_else(|| {
                anyhow::Error::msg(format!(
                    "invalid since date {:?}: expected YYYY-MM-DD with a valid day (1–31)",
                    d
                ))
            })?;
            criteria_parts.push(format!("SINCE {}", imap_date));
        }
        if let Some(d) = before {
            let imap_date = to_imap_date(d).ok_or_else(|| {
                anyhow::Error::msg(format!(
                    "invalid before date {:?}: expected YYYY-MM-DD with a valid day (1–31)",
                    d
                ))
            })?;
            criteria_parts.push(format!("BEFORE {}", imap_date));
        }

        let criteria = criteria_parts.join(" ");
        let uids = session.uid_search(&criteria).await?;

        if uids.is_empty() {
            let _ = session.logout().await;
            return Ok(ToolResult {
                success: true,
                output: "No emails found matching your criteria.".into(),
                error: None,
            });
        }

        let mut uid_vec: Vec<u32> = uids.into_iter().collect();
        uid_vec.sort_unstable();
        let uid_list: Vec<u32> = uid_vec.into_iter().rev().take(limit).collect();
        let uid_set = uid_list
            .iter()
            .map(|u| u.to_string())
            .collect::<Vec<_>>()
            .join(",");

        let messages = session.uid_fetch(&uid_set, "BODY.PEEK[HEADER]").await?;
        let messages: Vec<_> = messages.try_collect().await?;
        let _ = session.logout().await;

        let parser = MessageParser::default();
        let mut results: Vec<serde_json::Value> = Vec::new();

        for msg in &messages {
            let uid = msg.uid.unwrap_or(0);
            let Some(header_bytes) = msg.header() else {
                continue;
            };
            let Some(parsed) = parser.parse(header_bytes) else {
                continue;
            };

            let from = format_address(&parsed);
            let subject = parsed.subject().unwrap_or("(no subject)").to_string();
            let date = parsed
                .date()
                .map(|d| format!("{:04}-{:02}-{:02}", d.year, d.month, d.day))
                .unwrap_or_else(|| "unknown".into());

            results.push(
                serde_json::json!({ "uid": uid, "from": from, "subject": subject, "date": date }),
            );
        }

        results.sort_by(|a, b| b["date"].as_str().cmp(&a["date"].as_str()));

        let mut output = format!("{} email(s) found in {}/{}:", results.len(), alias, folder);
        for r in &results {
            output.push_str(&format!(
                "\n- [uid:{}] [{}] From: {} | Subject: {}",
                r["uid"],
                r["date"].as_str().unwrap_or(""),
                r["from"].as_str().unwrap_or(""),
                r["subject"].as_str().unwrap_or(""),
            ));
        }
        output.push_str("\n\nUse email_read with a uid to fetch the full body.");

        Ok(ToolResult {
            success: true,
            output,
            error: None,
        })
    }
}

pub fn resolve_channel(
    configs: &HashMap<String, EmailConfig>,
    alias: Option<&str>,
) -> anyhow::Result<(String, EmailConfig)> {
    if let Some(a) = alias {
        let cfg = configs
            .get(a)
            .ok_or_else(|| anyhow::Error::msg(format!("no email channel named '{}'", a)))?;
        Ok((a.to_string(), cfg.clone()))
    } else {
        configs
            .iter()
            .find(|(_, c)| c.enabled)
            .map(|(a, c)| (a.clone(), c.clone()))
            .ok_or_else(|| anyhow::Error::msg("no enabled email channels configured"))
    }
}

pub fn format_address(parsed: &mail_parser::Message) -> String {
    parsed
        .from()
        .and_then(|a| a.first())
        .map(|a| {
            let name = a.name().map(|n| format!("{} ", n)).unwrap_or_default();
            let addr = a.address().unwrap_or("unknown");
            format!("{}<{}>", name, addr)
        })
        .unwrap_or_else(|| "unknown".into())
}

pub fn to_imap_date(s: &str) -> Option<String> {
    let parts: Vec<&str> = s.splitn(3, '-').collect();
    if parts.len() != 3 {
        return None;
    }
    // Year: exactly 4 ASCII decimal digits (e.g. "2026").
    let year = parts[0];
    if year.len() != 4 || !year.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let month = match parts[1] {
        "01" => "Jan",
        "02" => "Feb",
        "03" => "Mar",
        "04" => "Apr",
        "05" => "May",
        "06" => "Jun",
        "07" => "Jul",
        "08" => "Aug",
        "09" => "Sep",
        "10" => "Oct",
        "11" => "Nov",
        "12" => "Dec",
        _ => return None,
    };
    // Day: 1–2 ASCII decimal digits, value 1–31. IMAP date-text is unquoted
    // so a non-numeric or out-of-range value would corrupt the SEARCH command.
    let day_str = parts[2];
    if day_str.is_empty() || day_str.len() > 2 || !day_str.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let day: u32 = day_str.parse().ok()?;
    if day == 0 || day > 31 {
        return None;
    }
    Some(format!("{}-{}-{}", day, month, year))
}

/// Allowlist-validates an agent-supplied IMAP SEARCH filter value. The search
/// criteria string is assembled by concatenation, so a value containing `(`,
/// `)`, `"`, or IMAP keywords (`OR`, `NOT`, `ALL`, `UNSEEN`) could escape its
/// quoted group and change the search semantics. We accept only the characters
/// that occur in real addresses and subject keywords and reject anything else
/// with a descriptive error, so a crafted filter fails loudly rather than
/// silently returning a different message set.
fn validate_search_filter(field: &str, value: &str) -> anyhow::Result<()> {
    if let Some(bad) = value
        .chars()
        .find(|c| !(c.is_alphanumeric() || matches!(c, '@' | '.' | '-' | '_' | '+' | ' ' | '*')))
    {
        return Err(anyhow::Error::msg(format!(
            "invalid character {:?} in '{}' filter; allowed: letters, digits, and @ . - _ + space *",
            bad, field
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{to_imap_date, validate_search_filter};

    #[test]
    fn validate_search_filter_accepts_addresses_and_keywords() {
        assert!(validate_search_filter("from", "alice.smith+tag@example.com").is_ok());
        assert!(validate_search_filter("subject", "Q3 report *2026*").is_ok());
        // Unicode letters in names/subjects are alphanumeric and allowed.
        assert!(validate_search_filter("from", "Élisa Müller").is_ok());
        assert!(validate_search_filter("subject", "").is_ok());
    }

    #[test]
    fn validate_search_filter_rejects_imap_metacharacters() {
        // Quotes and parens are the escape vectors for the FROM "…" group.
        for bad in ["\"", "(", ")", "a\" OR ALL (", "x)(NOT UNSEEN"] {
            let err = validate_search_filter("subject", bad)
                .expect_err("expected rejection")
                .to_string();
            assert!(err.contains("invalid character"), "got: {err}");
            assert!(err.contains("subject"), "field name in message: {err}");
        }
    }

    #[test]
    fn to_imap_date_converts_valid_dates() {
        assert_eq!(to_imap_date("2026-05-31"), Some("31-May-2026".into()));
        assert_eq!(to_imap_date("2026-01-01"), Some("1-Jan-2026".into()));
        assert_eq!(to_imap_date("2024-12-09"), Some("9-Dec-2024".into()));
    }

    #[test]
    fn to_imap_date_rejects_invalid_year() {
        assert_eq!(to_imap_date("26-05-31"), None); // 2-digit year
        assert_eq!(to_imap_date("abcd-05-01"), None); // non-numeric year
        assert_eq!(to_imap_date("-05-01"), None); // empty year
    }

    #[test]
    fn to_imap_date_rejects_invalid_day() {
        assert_eq!(to_imap_date("2026-05-00"), None); // day zero
        assert_eq!(to_imap_date("2026-05-32"), None); // day out of range
        assert_eq!(to_imap_date("2026-05-xx"), None); // non-numeric day
        assert_eq!(to_imap_date("2026-05-"), None); // empty day
    }

    #[test]
    fn to_imap_date_rejects_invalid_month() {
        assert_eq!(to_imap_date("2026-00-01"), None);
        assert_eq!(to_imap_date("2026-13-01"), None);
    }
}
