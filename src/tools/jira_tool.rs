use super::traits::{Tool, ToolResult};
use crate::security::{policy::ToolOperation, SecurityPolicy};
use async_trait::async_trait;
use reqwest::Client;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;

const JIRA_SEARCH_PAGE_SIZE: u32 = 100;
const MAX_ERROR_BODY_CHARS: usize = 500;

/// Controls how much data is returned by `get_ticket`.
enum LevelOfDetails {
    Basic,
    BasicSearch,
    Full,
    Changelog,
}

impl Default for LevelOfDetails {
    fn default() -> Self {
        LevelOfDetails::Basic
    }
}

/// Tool for interacting with the Jira REST API v3.
///
/// Supports three actions gated by `[jira].allowed_actions` in config:
/// - `get_ticket`   — always in the default allowlist; read-only.
/// - `search_tickets` — requires explicit opt-in; read-only.
/// - `comment_ticket` — requires explicit opt-in; mutating (Act policy).
pub struct JiraTool {
    base_url: String,
    email: String,
    api_token: String,
    allowed_actions: Vec<String>,
    http: Client,
    security: Arc<SecurityPolicy>,
    timeout_secs: u64,
}

impl JiraTool {
    pub fn new(
        base_url: String,
        email: String,
        api_token: String,
        allowed_actions: Vec<String>,
        security: Arc<SecurityPolicy>,
        timeout_secs: u64,
    ) -> Self {
        Self {
            base_url,
            email,
            api_token,
            allowed_actions,
            http: Client::new(),
            security,
            timeout_secs,
        }
    }

    fn is_action_allowed(&self, action: &str) -> bool {
        self.allowed_actions.iter().any(|a| a == action)
    }

    async fn get_ticket(
        &self,
        issue_key: &str,
        level: LevelOfDetails,
    ) -> anyhow::Result<ToolResult> {
        validate_issue_key(issue_key)?;
        let url = format!("{}/rest/api/3/issue/{}", self.base_url, issue_key);

        let query: Vec<(&str, &str)> = match &level {
            LevelOfDetails::Basic => vec![
                ("fields", "summary"),
                ("fields", "priority"),
                ("fields", "status"),
                ("fields", "assignee"),
                ("fields", "description"),
                ("fields", "created"),
                ("fields", "updated"),
                ("fields", "comment"),
                ("expand", "renderedFields"),
            ],
            LevelOfDetails::BasicSearch => vec![
                ("fields", "summary"),
                ("fields", "priority"),
                ("fields", "status"),
                ("fields", "assignee"),
                ("fields", "created"),
                ("fields", "updated"),
            ],
            LevelOfDetails::Full => vec![("expand", "renderedFields"), ("expand", "names")],
            LevelOfDetails::Changelog => vec![("expand", "changelog")],
        };

        let resp = self
            .http
            .get(&url)
            .basic_auth(&self.email, Some(&self.api_token))
            .query(&query)
            .timeout(std::time::Duration::from_secs(self.timeout_secs))
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("Jira get_ticket request failed: {e}"))?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!(
                "Jira get_ticket failed ({status}): {}",
                crate::util::truncate_with_ellipsis(&text, MAX_ERROR_BODY_CHARS)
            );
        }

        let raw: Value = resp
            .json()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to parse Jira get_ticket response: {e}"))?;

        let shaped = match level {
            LevelOfDetails::Basic => shape_basic(&raw),
            LevelOfDetails::BasicSearch => shape_basic_search(&raw),
            LevelOfDetails::Full => shape_full(&raw),
            LevelOfDetails::Changelog => shape_changelog(&raw),
        };

        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&shaped).unwrap_or_else(|_| shaped.to_string()),
            error: None,
        })
    }

    async fn search_tickets(
        &self,
        jql: &str,
        max_results: Option<u32>,
    ) -> anyhow::Result<ToolResult> {
        let url = format!("{}/rest/api/3/search/jql", self.base_url);
        let max_results = max_results.unwrap_or(25).clamp(1, 999);

        let mut issues: Vec<Value> = Vec::new();
        let mut next_page_token: Option<String> = None;

        loop {
            let remaining = max_results - issues.len() as u32;
            let page_size = remaining.min(JIRA_SEARCH_PAGE_SIZE);

            let mut body = json!({
                "jql": jql,
                "maxResults": page_size,
                "fields": ["summary", "priority", "status", "assignee", "created", "updated"]
            });

            if let Some(token) = &next_page_token {
                body["nextPageToken"] = json!(token);
            }

            let resp = self
                .http
                .post(&url)
                .basic_auth(&self.email, Some(&self.api_token))
                .json(&body)
                .timeout(std::time::Duration::from_secs(self.timeout_secs))
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("Jira search_tickets request failed: {e}"))?;

            let status = resp.status();
            if !status.is_success() {
                let text = resp.text().await.unwrap_or_default();
                anyhow::bail!(
                    "Jira search_tickets failed ({status}): {}",
                    crate::util::truncate_with_ellipsis(&text, MAX_ERROR_BODY_CHARS)
                );
            }

            let raw: Value = resp
                .json()
                .await
                .map_err(|e| anyhow::anyhow!("Failed to parse Jira search response: {e}"))?;

            if let Some(page) = raw["issues"].as_array() {
                issues.extend(page.iter().map(shape_basic_search));
            }

            let is_last = raw["isLast"].as_bool().unwrap_or(true);
            if is_last || issues.len() as u32 >= max_results {
                break;
            }

            next_page_token = raw["nextPageToken"].as_str().map(String::from);
            if next_page_token.is_none() {
                break;
            }
        }

        let output = json!(issues);
        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&output).unwrap_or_else(|_| output.to_string()),
            error: None,
        })
    }

    async fn comment_ticket(
        &self,
        issue_key: &str,
        comment_text: &str,
    ) -> anyhow::Result<ToolResult> {
        validate_issue_key(issue_key)?;

        let emails = extract_emails(comment_text);
        let mut mentions: HashMap<String, (String, String)> = HashMap::new();
        for email in emails {
            if let Some(info) = self.resolve_email(&email).await {
                mentions.insert(email, info);
            }
        }

        let adf = build_adf(comment_text, &mentions);

        let url = format!("{}/rest/api/3/issue/{}/comment", self.base_url, issue_key);
        let resp = self
            .http
            .post(&url)
            .basic_auth(&self.email, Some(&self.api_token))
            .json(&json!({ "body": adf }))
            .timeout(std::time::Duration::from_secs(self.timeout_secs))
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("Jira comment_ticket request failed: {e}"))?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!(
                "Jira comment_ticket failed ({status}): {}",
                crate::util::truncate_with_ellipsis(&text, MAX_ERROR_BODY_CHARS)
            );
        }

        let response: Value = resp
            .json()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to parse Jira comment response: {e}"))?;

        let shaped = shape_comment_response(&response);
        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&shaped).unwrap_or_else(|_| shaped.to_string()),
            error: None,
        })
    }

    async fn resolve_email(&self, email: &str) -> Option<(String, String)> {
        let url = format!("{}/rest/api/3/user/search", self.base_url);
        let result = self
            .http
            .get(&url)
            .basic_auth(&self.email, Some(&self.api_token))
            .query(&[("query", email)])
            .timeout(std::time::Duration::from_secs(self.timeout_secs))
            .send()
            .await
            .ok()?
            .json::<Value>()
            .await
            .ok()?;

        result.as_array()?.iter().find_map(|u| {
            let account_email = u["emailAddress"].as_str()?;
            if account_email.eq_ignore_ascii_case(email) {
                Some((
                    u["accountId"].as_str()?.to_string(),
                    u["displayName"].as_str()?.to_string(),
                ))
            } else {
                None
            }
        })
    }
}

#[async_trait]
impl Tool for JiraTool {
    fn name(&self) -> &str {
        "jira"
    }

    fn description(&self) -> &str {
        "Interact with Jira: get tickets with configurable detail level, search issues with JQL, and add comments with mention and formatting support."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["get_ticket", "search_tickets", "comment_ticket"],
                    "description": "The Jira action to perform. Enabled actions are configured in [jira].allowed_actions."
                },
                "issue_key": {
                    "type": "string",
                    "description": "Jira issue key, e.g. 'PROJ-123'. Required for get_ticket and comment_ticket."
                },
                "level_of_details": {
                    "type": "string",
                    "enum": ["basic", "basic_search", "full", "changelog"],
                    "description": "How much data to return for get_ticket. Omit to use the default ('basic'). Options: 'basic' — summary, status, priority, assignee, rendered description, and rendered comments (best for reading a ticket in full); 'basic_search' — lightweight fields only, no description or comments (best when you only need to identify the ticket); 'full' — all Jira fields plus rendered HTML (verbose, use sparingly); 'changelog' — issue key and full change history only."
                },
                "jql": {
                    "type": "string",
                    "description": "JQL query string for search_tickets. Example: 'project = PROJ AND status = \"In Progress\" ORDER BY updated DESC'."
                },
                "max_results": {
                    "type": "integer",
                    "description": "Maximum number of issues to return for search_tickets. Defaults to 25, capped at 999.",
                    "default": 25
                },
                "comment": {
                    "type": "string",
                    "description": "Comment body for comment_ticket. Supports a limited markdown-like syntax converted to Atlassian Document Format (ADF). Mention a user with @user@domain.com — the leading @ is required (a bare email without @ prefix is treated as plain text). Bold with **text**. Bullet list items with a leading '- '. Newlines become line breaks. Everything else is plain text. Example: 'Hi @john@company.com, this is **important**.\n- Check the logs\n- Rerun the pipeline'"
                }
            },
            "required": ["action"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let action = match args.get("action").and_then(|v| v.as_str()) {
            Some(a) => a,
            None => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some("Missing required parameter: action".into()),
                })
            }
        };

        // Reject unknown actions before the allowlist check so typos produce a
        // clear "unknown action" error rather than a misleading "not enabled" one.
        if !matches!(action, "get_ticket" | "search_tickets" | "comment_ticket") {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Unknown action: '{action}'. Valid actions: get_ticket, search_tickets, comment_ticket"
                )),
            });
        }

        if !self.is_action_allowed(action) {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Action '{action}' is not enabled. Add it to jira.allowed_actions in config.toml. \
                     Currently allowed: {}",
                    self.allowed_actions.join(", ")
                )),
            });
        }

        let operation = match action {
            "get_ticket" | "search_tickets" => ToolOperation::Read,
            "comment_ticket" => ToolOperation::Act,
            _ => unreachable!(),
        };

        if let Err(error) = self.security.enforce_tool_operation(operation, "jira") {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(error),
            });
        }

        let result = match action {
            "get_ticket" => {
                let issue_key = match args.get("issue_key").and_then(|v| v.as_str()) {
                    Some(k) => k,
                    None => {
                        return Ok(ToolResult {
                            success: false,
                            output: String::new(),
                            error: Some("get_ticket requires issue_key parameter".into()),
                        })
                    }
                };
                let level = match args.get("level_of_details").and_then(|v| v.as_str()) {
                    Some("basic_search") => LevelOfDetails::BasicSearch,
                    Some("full") => LevelOfDetails::Full,
                    Some("changelog") => LevelOfDetails::Changelog,
                    _ => LevelOfDetails::Basic,
                };
                self.get_ticket(issue_key, level).await
            }
            "search_tickets" => {
                let jql = match args.get("jql").and_then(|v| v.as_str()) {
                    Some(j) => j,
                    None => {
                        return Ok(ToolResult {
                            success: false,
                            output: String::new(),
                            error: Some("search_tickets requires jql parameter".into()),
                        })
                    }
                };
                let max_results = args
                    .get("max_results")
                    .and_then(|v| v.as_u64())
                    .map(|n| n as u32);
                self.search_tickets(jql, max_results).await
            }
            "comment_ticket" => {
                let issue_key = match args.get("issue_key").and_then(|v| v.as_str()) {
                    Some(k) => k,
                    None => {
                        return Ok(ToolResult {
                            success: false,
                            output: String::new(),
                            error: Some("comment_ticket requires issue_key parameter".into()),
                        })
                    }
                };
                let comment = match args.get("comment").and_then(|v| v.as_str()) {
                    Some(c) if !c.trim().is_empty() => c,
                    _ => {
                        return Ok(ToolResult {
                            success: false,
                            output: String::new(),
                            error: Some(
                                "comment_ticket requires a non-empty comment parameter".into(),
                            ),
                        })
                    }
                };
                self.comment_ticket(issue_key, comment).await
            }
            _ => unreachable!(),
        };

        match result {
            Ok(tool_result) => Ok(tool_result),
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(e.to_string()),
            }),
        }
    }
}

// ── Input validation ──────────────────────────────────────────────────────────

/// Validates that `issue_key` matches the Jira key format `PROJ-123` or `proj-123`.
/// Prevents path traversal if a crafted key like `../../other` were interpolated
/// directly into the URL.
fn validate_issue_key(key: &str) -> anyhow::Result<()> {
    let valid = key.split_once('-').is_some_and(|(project, number)| {
        !project.is_empty()
            && project.chars().all(|c| c.is_ascii_alphanumeric())
            && !number.is_empty()
            && number.chars().all(|c| c.is_ascii_digit())
    });
    if valid {
        Ok(())
    } else {
        anyhow::bail!(
            "Invalid issue key '{key}'. Expected format: PROJECT-123 (e.g. PROJ-42, proj-42)"
        )
    }
}

// ── Response shaping ──────────────────────────────────────────────────────────

fn shape_basic(raw: &Value) -> Value {
    let f = &raw["fields"];
    let rf = &raw["renderedFields"];

    // Build a lookup map from comment ID → rendered body for O(1) access
    // instead of scanning the rendered array for each comment (O(n²)).
    let rendered_by_id: HashMap<&str, &str> = rf["comment"]["comments"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|rc| Some((rc["id"].as_str()?, rc["body"].as_str()?)))
                .collect()
        })
        .unwrap_or_default();

    let comments: Vec<Value> = f["comment"]["comments"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .map(|c| {
                    let id = c["id"].as_str().unwrap_or("");
                    json!({
                        "author": c["author"]["displayName"],
                        "created": &c["created"].as_str().unwrap_or("")[ ..10],
                        "body": rendered_by_id.get(id).copied().unwrap_or("")
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    json!({
        "key":         raw["key"],
        "summary":     f["summary"],
        "status":      f["status"]["name"],
        "priority":    f["priority"]["name"],
        "assignee":    f["assignee"]["displayName"],
        "created":     &f["created"].as_str().unwrap_or("")[ ..10],
        "updated":     &f["updated"].as_str().unwrap_or("")[ ..10],
        "description": rf["description"].as_str().unwrap_or(""),
        "comments":    comments,
    })
}

fn shape_basic_search(raw: &Value) -> Value {
    let f = &raw["fields"];
    json!({
        "key":      raw["key"],
        "summary":  f["summary"],
        "status":   f["status"]["name"],
        "priority": f["priority"]["name"],
        "assignee": f["assignee"]["displayName"],
        "created":  &f["created"].as_str().unwrap_or("")[ ..10],
        "updated":  &f["updated"].as_str().unwrap_or("")[ ..10],
    })
}

fn shape_full(raw: &Value) -> Value {
    let mut result = raw.clone();
    let rf = &raw["renderedFields"];

    if let Some(desc) = rf["description"].as_str() {
        result["fields"]["description"] = json!(desc);
    }

    if let (Some(comments), Some(rendered_comments)) = (
        result["fields"]["comment"]["comments"].as_array_mut(),
        rf["comment"]["comments"].as_array(),
    ) {
        for (c, rc) in comments.iter_mut().zip(rendered_comments.iter()) {
            if let Some(body) = rc["body"].as_str() {
                c["body"] = json!(body);
            }
        }
    }

    result.as_object_mut().unwrap().remove("renderedFields");
    result
}

fn shape_changelog(raw: &Value) -> Value {
    json!({
        "key":       raw["key"],
        "changelog": raw["changelog"],
    })
}

/// Returns only the comment ID, author, and creation date — avoids
/// exposing internal Jira metadata back to the AI.
fn shape_comment_response(raw: &Value) -> Value {
    json!({
        "id":      raw["id"],
        "author":  raw["author"]["displayName"],
        "created": &raw["created"].as_str().unwrap_or("")[ ..10],
    })
}

// ── Comment / ADF builder ─────────────────────────────────────────────────────

/// Strips trailing punctuation that commonly appears after an email address
/// (e.g. `@john@co.com,` or `@john@co.com)`). Also strips leading bracket-like
/// punctuation so `@(john@co.com)` resolves correctly.
fn clean_email(s: &str) -> &str {
    s.trim_start_matches(|c: char| matches!(c, '(' | '['))
        .trim_end_matches(|c: char| matches!(c, ',' | '!' | '?' | ':' | ';' | ')' | ']'))
}

fn extract_emails(text: &str) -> Vec<String> {
    let mut emails = Vec::new();
    for word in text.split_whitespace() {
        if let Some(rest) = word.strip_prefix('@') {
            let email = clean_email(rest);
            if email.contains('@') {
                emails.push(email.to_string());
            }
        }
    }
    emails.dedup();
    emails
}

fn parse_inline(text: &str, mentions: &HashMap<String, (String, String)>) -> Vec<Value> {
    let mut nodes: Vec<Value> = Vec::new();
    let mut chars = text.chars().peekable();
    let mut current = String::new();

    while let Some(ch) = chars.next() {
        if ch == '*' && chars.peek() == Some(&'*') {
            chars.next(); // consume second *
            if !current.is_empty() {
                nodes.push(json!({ "type": "text", "text": current.clone() }));
                current.clear();
            }
            let mut bold = String::new();
            let mut closed = false;
            loop {
                match chars.next() {
                    Some('*') if chars.peek() == Some(&'*') => {
                        chars.next(); // consume second *
                        closed = true;
                        break;
                    }
                    Some(c) => bold.push(c),
                    None => break,
                }
            }
            if closed && !bold.is_empty() {
                nodes.push(json!({
                    "type": "text",
                    "text": bold,
                    "marks": [{ "type": "strong" }]
                }));
            } else if !bold.is_empty() {
                // Unmatched ** — emit as literal text
                current.push_str("**");
                current.push_str(&bold);
            }
        } else if ch == '@' {
            let mut raw = String::new();
            while let Some(&next) = chars.peek() {
                if next.is_whitespace() {
                    break;
                }
                raw.push(chars.next().unwrap());
            }
            let email = clean_email(&raw);
            // Compute the end position of `email` within `raw` via pointer
            // arithmetic so the suffix is correct even when leading chars were
            // stripped by clean_email.
            let email_end = (email.as_ptr() as usize - raw.as_ptr() as usize) + email.len();
            let suffix = &raw[email_end..];
            if email.contains('@') {
                if let Some((account_id, display_name)) = mentions.get(email) {
                    if !current.is_empty() {
                        nodes.push(json!({ "type": "text", "text": current.clone() }));
                        current.clear();
                    }
                    nodes.push(json!({
                        "type": "mention",
                        "attrs": {
                            "id": account_id,
                            "text": format!("@{}", display_name)
                        }
                    }));
                    if !suffix.is_empty() {
                        current.push_str(suffix);
                    }
                } else {
                    current.push('@');
                    current.push_str(&raw);
                }
            } else {
                current.push('@');
                current.push_str(&email);
            }
        } else {
            current.push(ch);
        }
    }

    if !current.is_empty() {
        nodes.push(json!({ "type": "text", "text": current }));
    }

    nodes
}

fn build_adf(text: &str, mentions: &HashMap<String, (String, String)>) -> Value {
    let mut content: Vec<Value> = Vec::new();
    let mut paragraph: Vec<Value> = Vec::new();
    let mut list_items: Vec<Value> = Vec::new();

    let flush_paragraph = |paragraph: &mut Vec<Value>, content: &mut Vec<Value>| {
        if !paragraph.is_empty() {
            content.push(json!({ "type": "paragraph", "content": paragraph.clone() }));
            paragraph.clear();
        }
    };

    let flush_list = |list_items: &mut Vec<Value>, content: &mut Vec<Value>| {
        if !list_items.is_empty() {
            content.push(json!({ "type": "bulletList", "content": list_items.clone() }));
            list_items.clear();
        }
    };

    for line in text.lines() {
        if line.trim().is_empty() {
            flush_paragraph(&mut paragraph, &mut content);
            flush_list(&mut list_items, &mut content);
        } else if let Some(item) = line.strip_prefix("- ") {
            flush_paragraph(&mut paragraph, &mut content);
            let inline = parse_inline(item, mentions);
            list_items.push(json!({
                "type": "listItem",
                "content": [{ "type": "paragraph", "content": inline }]
            }));
        } else {
            flush_list(&mut list_items, &mut content);
            if !paragraph.is_empty() {
                paragraph.push(json!({ "type": "hardBreak" }));
            }
            paragraph.extend(parse_inline(line, mentions));
        }
    }

    flush_paragraph(&mut paragraph, &mut content);
    flush_list(&mut list_items, &mut content);

    json!({ "type": "doc", "version": 1, "content": content })
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::{AutonomyLevel, SecurityPolicy};

    fn test_tool(allowed_actions: Vec<&str>) -> JiraTool {
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            ..SecurityPolicy::default()
        });
        JiraTool::new(
            "https://test.atlassian.net".into(),
            "test@example.com".into(),
            "test-token".into(),
            allowed_actions.into_iter().map(String::from).collect(),
            security,
            30,
        )
    }

    #[test]
    fn tool_name_is_jira() {
        assert_eq!(test_tool(vec!["get_ticket"]).name(), "jira");
    }

    #[test]
    fn parameters_schema_has_required_action() {
        let schema = test_tool(vec!["get_ticket"]).parameters_schema();
        let required = schema["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v.as_str() == Some("action")));
    }

    #[test]
    fn parameters_schema_defines_all_actions() {
        let schema = test_tool(vec!["get_ticket"]).parameters_schema();
        let actions = schema["properties"]["action"]["enum"].as_array().unwrap();
        let action_strs: Vec<&str> = actions.iter().filter_map(|v| v.as_str()).collect();
        assert!(action_strs.contains(&"get_ticket"));
        assert!(action_strs.contains(&"search_tickets"));
        assert!(action_strs.contains(&"comment_ticket"));
    }

    #[tokio::test]
    async fn execute_missing_action_returns_error() {
        let result = test_tool(vec!["get_ticket"])
            .execute(json!({}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap().contains("action"));
    }

    #[tokio::test]
    async fn execute_unknown_action_returns_error() {
        let result = test_tool(vec!["get_ticket"])
            .execute(json!({"action": "delete_ticket"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap().contains("Unknown action"));
    }

    #[tokio::test]
    async fn execute_disallowed_action_returns_error() {
        let result = test_tool(vec!["get_ticket"])
            .execute(json!({"action": "comment_ticket"}))
            .await
            .unwrap();
        assert!(!result.success);
        let err = result.error.unwrap();
        assert!(err.contains("not enabled"));
        assert!(err.contains("allowed_actions"));
    }

    #[tokio::test]
    async fn execute_get_ticket_missing_key_returns_error() {
        let result = test_tool(vec!["get_ticket"])
            .execute(json!({"action": "get_ticket"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap().contains("issue_key"));
    }

    #[tokio::test]
    async fn execute_search_tickets_missing_jql_returns_error() {
        let result = test_tool(vec!["get_ticket", "search_tickets"])
            .execute(json!({"action": "search_tickets"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap().contains("jql"));
    }

    #[tokio::test]
    async fn execute_comment_ticket_missing_key_returns_error() {
        let result = test_tool(vec!["get_ticket", "comment_ticket"])
            .execute(json!({"action": "comment_ticket", "comment": "hello"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap().contains("issue_key"));
    }

    #[tokio::test]
    async fn execute_comment_ticket_missing_comment_returns_error() {
        let result = test_tool(vec!["get_ticket", "comment_ticket"])
            .execute(json!({"action": "comment_ticket", "issue_key": "PROJ-1"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap().contains("comment"));
    }

    #[tokio::test]
    async fn execute_comment_ticket_empty_comment_returns_error() {
        let result = test_tool(vec!["get_ticket", "comment_ticket"])
            .execute(json!({"action": "comment_ticket", "issue_key": "PROJ-1", "comment": "   "}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap().contains("comment"));
    }

    #[tokio::test]
    async fn execute_comment_blocked_in_readonly_mode() {
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::ReadOnly,
            ..SecurityPolicy::default()
        });
        let tool = JiraTool::new(
            "https://test.atlassian.net".into(),
            "test@example.com".into(),
            "token".into(),
            vec!["get_ticket".into(), "comment_ticket".into()],
            security,
            30,
        );
        let result = tool
            .execute(json!({
                "action": "comment_ticket",
                "issue_key": "PROJ-1",
                "comment": "hello"
            }))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap().contains("read-only"));
    }

    // ── Issue key validation ──────────────────────────────────────────────────

    #[test]
    fn validate_issue_key_accepts_valid_keys() {
        assert!(validate_issue_key("PROJ-1").is_ok());
        assert!(validate_issue_key("PROJ-123").is_ok());
        assert!(validate_issue_key("AB-99").is_ok());
        assert!(validate_issue_key("MYPROJECT-1000").is_ok());
        assert!(validate_issue_key("proj-1").is_ok());
        assert!(validate_issue_key("proj-123").is_ok());
    }

    #[test]
    fn validate_issue_key_rejects_path_traversal() {
        assert!(validate_issue_key("../../etc/passwd").is_err());
        assert!(validate_issue_key("../other").is_err());
    }

    #[test]
    fn validate_issue_key_rejects_malformed() {
        assert!(validate_issue_key("PROJ").is_err()); // no number
        assert!(validate_issue_key("PROJ-").is_err()); // empty number
        assert!(validate_issue_key("-123").is_err()); // no project
        assert!(validate_issue_key("PROJ-12x").is_err()); // non-digit in number
    }

    // ── ADF builder unit tests ────────────────────────────────────────────────

    #[test]
    fn build_adf_plain_text() {
        let adf = build_adf("Hello world", &HashMap::new());
        assert_eq!(adf["type"], "doc");
        assert_eq!(adf["version"], 1);
        let para = &adf["content"][0];
        assert_eq!(para["type"], "paragraph");
        assert_eq!(para["content"][0]["text"], "Hello world");
    }

    #[test]
    fn build_adf_bold() {
        let adf = build_adf("**bold**", &HashMap::new());
        let text_node = &adf["content"][0]["content"][0];
        assert_eq!(text_node["text"], "bold");
        assert_eq!(text_node["marks"][0]["type"], "strong");
    }

    #[test]
    fn build_adf_unmatched_bold_is_literal() {
        let adf = build_adf("**no closing", &HashMap::new());
        let text = &adf["content"][0]["content"][0]["text"];
        assert!(text.as_str().unwrap().contains("**no closing"));
    }

    #[test]
    fn build_adf_bullet_list() {
        let adf = build_adf("- first\n- second", &HashMap::new());
        let list = &adf["content"][0];
        assert_eq!(list["type"], "bulletList");
        assert_eq!(list["content"].as_array().unwrap().len(), 2);
        assert_eq!(list["content"][0]["type"], "listItem");
    }

    #[test]
    fn build_adf_mention_resolved() {
        let mut mentions = HashMap::new();
        mentions.insert(
            "john@company.com".to_string(),
            ("acc-123".to_string(), "John Doe".to_string()),
        );
        let adf = build_adf("Hi @john@company.com done", &mentions);
        let content = &adf["content"][0]["content"];
        let mention = content
            .as_array()
            .unwrap()
            .iter()
            .find(|n| n["type"] == "mention")
            .unwrap();
        assert_eq!(mention["attrs"]["id"], "acc-123");
        assert_eq!(mention["attrs"]["text"], "@John Doe");
    }

    #[test]
    fn build_adf_unresolved_mention_rendered_as_plain_text() {
        let adf = build_adf("Hi @unknown@example.com", &HashMap::new());
        let text = &adf["content"][0]["content"][0]["text"];
        assert!(text.as_str().unwrap().contains("@unknown@example.com"));
    }

    #[test]
    fn extract_emails_finds_at_prefixed_emails() {
        let emails = extract_emails("Hello @john@company.com and @jane@corp.io done");
        assert_eq!(emails, vec!["john@company.com", "jane@corp.io"]);
    }

    #[test]
    fn extract_emails_deduplicates() {
        let emails = extract_emails("@a@b.com @a@b.com");
        assert_eq!(emails.len(), 1);
    }

    #[test]
    fn extract_emails_strips_trailing_punctuation() {
        let emails = extract_emails("@john@company.com,");
        assert_eq!(emails, vec!["john@company.com"]);
    }

    #[test]
    fn extract_emails_strips_leading_punctuation() {
        let emails = extract_emails("@(john@company.com)");
        assert_eq!(emails, vec!["john@company.com"]);
    }

    #[test]
    fn shape_basic_search_extracts_expected_fields() {
        let raw = json!({
            "key": "PROJ-1",
            "fields": {
                "summary": "Fix bug",
                "status": { "name": "In Progress" },
                "priority": { "name": "High" },
                "assignee": { "displayName": "Jane" },
                "created": "2024-01-15T10:00:00.000Z",
                "updated": "2024-03-01T12:00:00.000Z"
            }
        });
        let shaped = shape_basic_search(&raw);
        assert_eq!(shaped["key"], "PROJ-1");
        assert_eq!(shaped["summary"], "Fix bug");
        assert_eq!(shaped["status"], "In Progress");
        assert_eq!(shaped["priority"], "High");
        assert_eq!(shaped["assignee"], "Jane");
        assert_eq!(shaped["created"], "2024-01-15");
        assert_eq!(shaped["updated"], "2024-03-01");
    }

    #[test]
    fn shape_changelog_extracts_key_and_changelog() {
        let raw = json!({
            "key": "PROJ-42",
            "changelog": { "histories": [] },
            "fields": {}
        });
        let shaped = shape_changelog(&raw);
        assert_eq!(shaped["key"], "PROJ-42");
        assert!(shaped.get("changelog").is_some());
        assert!(shaped.get("fields").is_none());
    }

    #[test]
    fn shape_comment_response_extracts_id_author_created() {
        let raw = json!({
            "id": "12345",
            "author": { "displayName": "Alice", "accountId": "abc" },
            "created": "2024-06-01T09:00:00.000Z",
            "body": { "type": "doc" },
            "self": "https://internal.url"
        });
        let shaped = shape_comment_response(&raw);
        assert_eq!(shaped["id"], "12345");
        assert_eq!(shaped["author"], "Alice");
        assert_eq!(shaped["created"], "2024-06-01");
        assert!(shaped.get("body").is_none());
        assert!(shaped.get("self").is_none());
    }

    #[test]
    fn shape_basic_uses_o1_comment_lookup() {
        // Verify that comments are matched by ID, not by position.
        let raw = json!({
            "key": "PROJ-1",
            "fields": {
                "summary": "s", "priority": {"name":"P"}, "status": {"name":"S"},
                "assignee": {"displayName":"A"},
                "created": "2024-01-01T00:00:00.000Z",
                "updated": "2024-01-01T00:00:00.000Z",
                "comment": {
                    "comments": [
                        { "id": "2", "author": {"displayName":"Bob"}, "created": "2024-01-02T00:00:00.000Z" },
                        { "id": "1", "author": {"displayName":"Alice"}, "created": "2024-01-01T00:00:00.000Z" }
                    ]
                }
            },
            "renderedFields": {
                "description": "",
                "comment": {
                    "comments": [
                        { "id": "1", "body": "Alice's body" },
                        { "id": "2", "body": "Bob's body" }
                    ]
                }
            }
        });
        let shaped = shape_basic(&raw);
        // Comment with id "2" (Bob) should get Bob's rendered body, not Alice's
        assert_eq!(shaped["comments"][0]["author"], "Bob");
        assert_eq!(shaped["comments"][0]["body"], "Bob's body");
        assert_eq!(shaped["comments"][1]["author"], "Alice");
        assert_eq!(shaped["comments"][1]["body"], "Alice's body");
    }
}
