use super::traits::{Tool, ToolResult};
use crate::security::{policy::ToolOperation, SecurityPolicy};
use async_trait::async_trait;
use reqwest::Client;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

const JIRA_SEARCH_PAGE_SIZE: u32 = 100;
const MAX_ERROR_BODY_CHARS: usize = 500;
const CONFLUENCE_PAGE_MAX_CHARS: usize = 10_000;
const CONFLUENCE_CHILDREN_MAX: usize = 250;
const CONFLUENCE_SPACE_PAGE_MAX: usize = 1_000;

/// Controls how much data is returned by `get_ticket`.
#[derive(Default)]
enum LevelOfDetails {
    Basic,
    #[default]
    BasicSearch,
    Full,
    Changelog,
}

/// Tool for interacting with the Jira REST API v3.
///
/// Supports five actions gated by `[jira].allowed_actions` in config:
/// - `get_ticket`     — always in the default allowlist; read-only.
/// - `search_tickets` — requires explicit opt-in; read-only.
/// - `comment_ticket` — requires explicit opt-in; mutating (Act policy).
/// - `list_projects`  — requires explicit opt-in; read-only.
/// - `myself`         — requires explicit opt-in; read-only. Verifies credentials.
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
            base_url: base_url.trim_end_matches('/').to_string(),
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

    #[allow(clippy::cast_possible_truncation)]
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
            let remaining = max_results.saturating_sub(issues.len() as u32);

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

    async fn list_projects(&self) -> anyhow::Result<ToolResult> {
        let url = format!("{}/rest/api/3/project", self.base_url);

        let resp = self
            .http
            .get(&url)
            .basic_auth(&self.email, Some(&self.api_token))
            .timeout(std::time::Duration::from_secs(self.timeout_secs))
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("Jira list_projects request failed: {e}"))?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!(
                "Jira list_projects failed ({status}): {}",
                crate::util::truncate_with_ellipsis(&text, MAX_ERROR_BODY_CHARS)
            );
        }

        let projects: Vec<Value> = resp
            .json()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to parse Jira list_projects response: {e}"))?;

        let keys: Vec<String> = projects
            .iter()
            .filter_map(|p| p["key"].as_str().map(String::from))
            .collect();

        const STATUS_CONCURRENCY: usize = 5;

        let users_url = format!(
            "{}/rest/api/3/user/assignable/multiProjectSearch",
            self.base_url
        );

        let users_resp = self
            .http
            .get(&users_url)
            .basic_auth(&self.email, Some(&self.api_token))
            .query(&[
                ("projectKeys", keys.join(",").as_str()),
                ("maxResults", "50"),
            ])
            .timeout(std::time::Duration::from_secs(self.timeout_secs))
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("Jira list_projects users request failed: {e}"))?;

        let users: Vec<Value> = if users_resp.status().is_success() {
            users_resp.json().await.map_err(|e| {
                anyhow::anyhow!("Failed to parse Jira list_projects users response: {e}")
            })?
        } else {
            let status = users_resp.status();
            let text = users_resp.text().await.unwrap_or_default();
            anyhow::bail!(
                "Jira list_projects users failed ({status}): {}",
                crate::util::truncate_with_ellipsis(&text, MAX_ERROR_BODY_CHARS)
            );
        };

        let mut set: tokio::task::JoinSet<(usize, anyhow::Result<Value>)> =
            tokio::task::JoinSet::new();
        let mut statuses_results = vec![json!([]); keys.len()];

        for (i, key) in keys.iter().enumerate() {
            if set.len() >= STATUS_CONCURRENCY {
                if let Some(Ok((idx, result))) = set.join_next().await {
                    statuses_results[idx] =
                        result.map_err(|e| anyhow::anyhow!("Jira statuses failed: {e}"))?;
                }
            }

            let client = self.http.clone();
            let request_url = format!("{url}/{key}/statuses");
            let email = self.email.clone();
            let token = self.api_token.clone();
            let timeout = self.timeout_secs;

            set.spawn(async move {
                let result = async {
                    let resp = client
                        .get(&request_url)
                        .basic_auth(&email, Some(&token))
                        .timeout(std::time::Duration::from_secs(timeout))
                        .send()
                        .await
                        .map_err(|e| anyhow::anyhow!("statuses request failed: {e}"))?;

                    if !resp.status().is_success() {
                        anyhow::bail!("statuses request returned {}", resp.status());
                    }

                    resp.json::<Value>()
                        .await
                        .map_err(|e| anyhow::anyhow!("failed to parse statuses response: {e}"))
                }
                .await;
                (i, result)
            });
        }

        while let Some(Ok((idx, result))) = set.join_next().await {
            statuses_results[idx] =
                result.map_err(|e| anyhow::anyhow!("Jira statuses failed: {e}"))?;
        }

        let shaped_projects = shape_projects(&projects, &statuses_results);
        let shaped_users: Vec<Value> = users
            .iter()
            .filter_map(|u| {
                let display = u["displayName"].as_str()?;
                let email = u["emailAddress"].as_str()?;
                Some(json!({ "displayName": display, "emailAddress": email }))
            })
            .collect();

        let output = json!({ "projects": shaped_projects, "users": shaped_users });
        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&output).unwrap_or_else(|_| output.to_string()),
            error: None,
        })
    }

    async fn get_myself(&self) -> anyhow::Result<ToolResult> {
        let url = format!("{}/rest/api/3/myself", self.base_url);

        let resp = self
            .http
            .get(&url)
            .basic_auth(&self.email, Some(&self.api_token))
            .timeout(std::time::Duration::from_secs(self.timeout_secs))
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("Jira myself request failed: {e}"))?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!(
                "Jira myself failed ({status}): {}",
                crate::util::truncate_with_ellipsis(&text, MAX_ERROR_BODY_CHARS)
            );
        }

        let raw: Value = resp
            .json()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to parse Jira myself response: {e}"))?;

        let shaped = json!({
            "accountId":    raw["accountId"],
            "displayName":  raw["displayName"],
            "emailAddress": raw["emailAddress"],
            "active":       raw["active"],
        });

        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&shaped).unwrap_or_else(|_| shaped.to_string()),
            error: None,
        })
    }

    async fn confluence_get_space(&self, space_key: &str) -> anyhow::Result<ToolResult> {
        validate_space_key(space_key)?;
        let url = format!("{}/wiki/rest/api/content", self.base_url);
        let limit_str = CONFLUENCE_SPACE_PAGE_MAX.to_string();

        let resp = self
            .http
            .get(&url)
            .basic_auth(&self.email, Some(&self.api_token))
            .query(&[
                ("spaceKey", space_key),
                ("type", "page"),
                ("limit", &limit_str),
                ("expand", "ancestors"),
            ])
            .timeout(std::time::Duration::from_secs(self.timeout_secs))
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("Confluence get_space request failed: {e}"))?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!(
                "Confluence get_space failed ({status}): {}",
                crate::util::truncate_with_ellipsis(&text, MAX_ERROR_BODY_CHARS)
            );
        }

        let raw: Value = resp
            .json()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to parse Confluence get_space response: {e}"))?;

        let pages = raw["results"].as_array().cloned().unwrap_or_default();
        let total = pages.len();
        let truncated = total >= CONFLUENCE_SPACE_PAGE_MAX;

        let mut id_to_title: HashMap<String, String> = HashMap::new();
        let mut parent_to_children: HashMap<String, Vec<String>> = HashMap::new();
        let mut roots: Vec<String> = Vec::new();

        for p in &pages {
            let pid = p["id"].as_str().unwrap_or("").to_string();
            let title = p["title"].as_str().unwrap_or("").to_string();
            id_to_title.insert(pid.clone(), title);
            let ancestors = p["ancestors"].as_array();
            if let Some(ancs) = ancestors {
                if let Some(parent) = ancs.last() {
                    let parent_id = parent["id"].as_str().unwrap_or("").to_string();
                    parent_to_children.entry(parent_id).or_default().push(pid);
                } else {
                    roots.push(pid);
                }
            } else {
                roots.push(pid);
            }
        }

        let tree: Vec<Value> = roots
            .iter()
            .map(|r| build_confluence_tree(r, &id_to_title, &parent_to_children))
            .collect();

        let output = json!({
            "space_key": space_key,
            "total": total,
            "truncated": truncated,
            "pages": tree,
        });

        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&output).unwrap_or_else(|_| output.to_string()),
            error: None,
        })
    }

    async fn confluence_get_page(&self, page_id: &str) -> anyhow::Result<ToolResult> {
        validate_page_id(page_id)?;
        let url = format!("{}/wiki/rest/api/content/{}", self.base_url, page_id);
        let children_limit_str = CONFLUENCE_CHILDREN_MAX.to_string();

        let resp = self
            .http
            .get(&url)
            .basic_auth(&self.email, Some(&self.api_token))
            .query(&[
                ("expand", "body.view,ancestors,children.page,space"),
                ("children.page.limit", &children_limit_str),
            ])
            .timeout(std::time::Duration::from_secs(self.timeout_secs))
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("Confluence get_page request failed: {e}"))?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!(
                "Confluence get_page failed ({status}): {}",
                crate::util::truncate_with_ellipsis(&text, MAX_ERROR_BODY_CHARS)
            );
        }

        let raw: Value = resp
            .json()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to parse Confluence get_page response: {e}"))?;

        let html = raw["body"]["view"]["value"].as_str().unwrap_or("");
        let text = nanohtml2text::html2text(html);
        let truncated = text.len() > CONFLUENCE_PAGE_MAX_CHARS;
        let content = if truncated {
            crate::util::truncate_with_ellipsis(&text, CONFLUENCE_PAGE_MAX_CHARS).to_string()
        } else {
            text
        };

        let parent = raw["ancestors"]
            .as_array()
            .and_then(|a| a.last())
            .map(|p| json!({ "id": p["id"], "title": p["title"] }));

        let children: Vec<Value> = raw["children"]["page"]["results"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .map(|c| json!({ "id": c["id"], "title": c["title"] }))
                    .collect()
            })
            .unwrap_or_default();
        let children_truncated = children.len() >= CONFLUENCE_CHILDREN_MAX;

        let output = json!({
            "id": raw["id"],
            "title": raw["title"],
            "space_key": raw["space"]["key"],
            "parent": parent,
            "children": children,
            "children_truncated": children_truncated,
            "content": content,
            "truncated": truncated,
        });

        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&output).unwrap_or_else(|_| output.to_string()),
            error: None,
        })
    }

    async fn confluence_search(
        &self,
        query: &str,
        max_results: Option<u32>,
    ) -> anyhow::Result<ToolResult> {
        let max_results = max_results.unwrap_or(25).clamp(1, 50);
        let escaped = query.replace('"', "\\\"");
        let cql = format!(
            "(title ~ \"{escaped}\" OR text ~ \"{escaped}\") AND type = page"
        );
        let limit_str = max_results.to_string();
        let url = format!("{}/wiki/rest/api/content/search", self.base_url);

        let resp = self
            .http
            .get(&url)
            .basic_auth(&self.email, Some(&self.api_token))
            .query(&[
                ("cql", cql.as_str()),
                ("limit", &limit_str),
                ("expand", "space"),
            ])
            .timeout(std::time::Duration::from_secs(self.timeout_secs))
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("Confluence search request failed: {e}"))?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!(
                "Confluence search failed ({status}): {}",
                crate::util::truncate_with_ellipsis(&text, MAX_ERROR_BODY_CHARS)
            );
        }

        let raw: Value = resp
            .json()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to parse Confluence search response: {e}"))?;

        let results: Vec<Value> = raw["results"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .map(|r| {
                        json!({
                            "id": r["id"],
                            "title": r["title"],
                            "type": r["type"],
                            "space_key": r["space"]["key"],
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        let output = json!(results);
        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&output).unwrap_or_else(|_| output.to_string()),
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
        "Interact with Jira and Confluence (Atlassian). Jira: get tickets with configurable detail level, search issues with JQL, add comments with mention and formatting support. Confluence: list all pages in a space as a tree, read a page with its parent and child navigation links, search pages by title and body text."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": [
                        "get_ticket", "search_tickets", "comment_ticket", "list_projects", "myself",
                        "confluence_get_space", "confluence_get_page", "confluence_search"
                    ],
                    "description": "The action to perform. Jira actions: 'get_ticket' — fetch a ticket by key; 'search_tickets' — search with JQL; 'comment_ticket' — post a comment; 'list_projects' — list all projects; 'myself' — verify credentials. Confluence actions: 'confluence_get_space' — list all pages in a space as a tree; 'confluence_get_page' — read a page with parent and child links; 'confluence_search' — fuzzy search pages by title and body. All actions must be enabled in [jira].allowed_actions."
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
                    "description": "Maximum number of results to return. For search_tickets: defaults to 25, capped at 999. For confluence_search: defaults to 25, capped at 50.",
                    "default": 25
                },
                "comment": {
                    "type": "string",
                    "description": "Comment body for comment_ticket. Supports a limited markdown-like syntax converted to Atlassian Document Format (ADF). Mention a user with @user@domain.com — the leading @ is required (a bare email without @ prefix is treated as plain text). Bold with **text**. Bullet list items with a leading '- '. Newlines become line breaks. Everything else is plain text. Example: 'Hi @john@company.com, this is **important**.\n- Check the logs\n- Rerun the pipeline'"
                },
                "space_key": {
                    "type": "string",
                    "description": "Confluence space key, e.g. 'ENGINEERING' or 'DP'. Required for confluence_get_space."
                },
                "page_id": {
                    "type": "string",
                    "description": "Confluence page ID (numeric string), e.g. '5403115521'. Required for confluence_get_page. Obtain IDs from confluence_get_space or confluence_search."
                },
                "query": {
                    "type": "string",
                    "description": "Search term for confluence_search. Matched against both page titles and body text (fuzzy). Example: 'MLT Planning'."
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
        if !matches!(
            action,
            "get_ticket"
                | "search_tickets"
                | "comment_ticket"
                | "list_projects"
                | "myself"
                | "confluence_get_space"
                | "confluence_get_page"
                | "confluence_search"
        ) {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Unknown action: '{action}'. Valid actions: get_ticket, search_tickets, \
                     comment_ticket, list_projects, myself, confluence_get_space, \
                     confluence_get_page, confluence_search"
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
            "get_ticket"
            | "search_tickets"
            | "list_projects"
            | "myself"
            | "confluence_get_space"
            | "confluence_get_page"
            | "confluence_search" => ToolOperation::Read,
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
                    .map(|n| u32::try_from(n).unwrap_or(u32::MAX));
                self.search_tickets(jql, max_results).await
            }
            "myself" => self.get_myself().await,
            "list_projects" => self.list_projects().await,
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
            "confluence_get_space" => {
                let space_key = match args.get("space_key").and_then(|v| v.as_str()) {
                    Some(k) => k,
                    None => {
                        return Ok(ToolResult {
                            success: false,
                            output: String::new(),
                            error: Some(
                                "confluence_get_space requires space_key parameter".into(),
                            ),
                        })
                    }
                };
                self.confluence_get_space(space_key).await
            }
            "confluence_get_page" => {
                let page_id = match args.get("page_id").and_then(|v| v.as_str()) {
                    Some(k) => k,
                    None => {
                        return Ok(ToolResult {
                            success: false,
                            output: String::new(),
                            error: Some(
                                "confluence_get_page requires page_id parameter".into(),
                            ),
                        })
                    }
                };
                self.confluence_get_page(page_id).await
            }
            "confluence_search" => {
                let query = match args.get("query").and_then(|v| v.as_str()) {
                    Some(q) if !q.trim().is_empty() => q,
                    _ => {
                        return Ok(ToolResult {
                            success: false,
                            output: String::new(),
                            error: Some(
                                "confluence_search requires a non-empty query parameter".into(),
                            ),
                        })
                    }
                };
                let max_results = args
                    .get("max_results")
                    .and_then(|v| v.as_u64())
                    .map(|n| u32::try_from(n).unwrap_or(u32::MAX));
                self.confluence_search(query, max_results).await
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

/// Validates that `space_key` contains only safe characters to prevent injection.
/// Allows alphanumeric, underscore, hyphen, and tilde (used in personal space keys).
fn validate_space_key(key: &str) -> anyhow::Result<()> {
    let valid = !key.is_empty()
        && key.len() <= 255
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '~'));
    if valid {
        Ok(())
    } else {
        anyhow::bail!(
            "Invalid space key '{key}'. Expected alphanumeric characters (e.g. ENGINEERING, DP)"
        )
    }
}

/// Validates that `page_id` is a non-empty numeric string (Confluence page IDs are integers).
fn validate_page_id(id: &str) -> anyhow::Result<()> {
    let valid = !id.is_empty() && id.chars().all(|c| c.is_ascii_digit());
    if valid {
        Ok(())
    } else {
        anyhow::bail!("Invalid page ID '{id}'. Expected a numeric ID (e.g. 5403115521)")
    }
}

// ── Response shaping ──────────────────────────────────────────────────────────

/// Safely extracts the first 10 characters (date prefix) from a string.
/// Returns the full string if it is shorter than 10 characters instead of
/// panicking on out-of-bounds slice indexing.
fn date_prefix(s: &str) -> &str {
    s.get(..10).unwrap_or(s)
}

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
                        "created": date_prefix(c["created"].as_str().unwrap_or("")),
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
        "created":     date_prefix(f["created"].as_str().unwrap_or("")),
        "updated":     date_prefix(f["updated"].as_str().unwrap_or("")),
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
        "created":  date_prefix(f["created"].as_str().unwrap_or("")),
        "updated":  date_prefix(f["updated"].as_str().unwrap_or("")),
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
        "created": date_prefix(raw["created"].as_str().unwrap_or("")),
    })
}

fn shape_projects(projects: &[Value], statuses_per_project: &[Value]) -> Vec<Value> {
    projects
        .iter()
        .zip(statuses_per_project.iter())
        .map(|(p, statuses)| {
            let mut issue_types: Vec<String> = Vec::new();
            let mut all_statuses: HashSet<String> = HashSet::new();

            if let Some(arr) = statuses.as_array() {
                for it in arr {
                    if let Some(name) = it["name"].as_str() {
                        issue_types.push(name.to_string());
                    }
                    if let Some(ss) = it["statuses"].as_array() {
                        for s in ss {
                            if let Some(sn) = s["name"].as_str() {
                                all_statuses.insert(sn.to_string());
                            }
                        }
                    }
                }
            }

            let mut ordered: Vec<String> = all_statuses.into_iter().collect();
            ordered.sort();

            json!({
                "key":         p["key"],
                "name":        p["name"],
                "projectType": p["projectTypeKey"],
                "style":       p["style"],
                "issueTypes":  issue_types,
                "statuses":    ordered,
            })
        })
        .collect()
}

// ── Comment / ADF builder ─────────────────────────────────────────────────────

/// Strips trailing punctuation that commonly appears after an email address
/// (e.g. `@john@co.com,` or `@john@co.com)`). Also strips leading bracket-like
/// punctuation so `@(john@co.com)` resolves correctly.
fn clean_email(s: &str) -> &str {
    s.trim_start_matches(['(', '['])
        .trim_end_matches([',', '!', '?', ':', ';', ')', ']'])
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
    let mut seen = std::collections::HashSet::new();
    emails.retain(|e| seen.insert(e.clone()));
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
                current.push_str(email);
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

// ── Confluence helpers ────────────────────────────────────────────────────────

/// Recursively builds a JSON tree node for a Confluence page and its descendants.
fn build_confluence_tree(
    node_id: &str,
    id_to_title: &HashMap<String, String>,
    parent_to_children: &HashMap<String, Vec<String>>,
) -> Value {
    let title = id_to_title.get(node_id).map(String::as_str).unwrap_or("");
    let children: Vec<Value> = parent_to_children
        .get(node_id)
        .map(|kids| {
            kids.iter()
                .map(|k| build_confluence_tree(k, id_to_title, parent_to_children))
                .collect()
        })
        .unwrap_or_default();
    json!({ "id": node_id, "title": title, "children": children })
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

    // ── myself action ────────────────────────────────────────────────────────

    #[test]
    fn parameters_schema_includes_myself_action() {
        let schema = test_tool(vec!["myself"]).parameters_schema();
        let actions = schema["properties"]["action"]["enum"].as_array().unwrap();
        let action_strs: Vec<&str> = actions.iter().filter_map(|v| v.as_str()).collect();
        assert!(action_strs.contains(&"myself"));
    }

    #[tokio::test]
    async fn execute_myself_disallowed_returns_error() {
        let result = test_tool(vec!["get_ticket"])
            .execute(json!({"action": "myself"}))
            .await
            .unwrap();
        assert!(!result.success);
        let err = result.error.unwrap();
        assert!(err.contains("not enabled"));
        assert!(err.contains("allowed_actions"));
    }

    #[tokio::test]
    async fn execute_myself_not_blocked_in_readonly_mode() {
        // myself is a Read operation — the security policy should not block it.
        // The call will fail at the HTTP level (no real server), not at the
        // policy level, so the error must NOT contain "read-only".
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::ReadOnly,
            ..SecurityPolicy::default()
        });
        let tool = JiraTool::new(
            "https://test.atlassian.net".into(),
            "test@example.com".into(),
            "token".into(),
            vec!["myself".into()],
            security,
            30,
        );
        let result = tool.execute(json!({"action": "myself"})).await.unwrap();
        assert!(!result.success);
        assert!(!result.error.as_deref().unwrap_or("").contains("read-only"));
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
    fn extract_emails_deduplicates_non_adjacent() {
        let emails = extract_emails("@a@b.com @c@d.com @a@b.com");
        assert_eq!(emails, vec!["a@b.com", "c@d.com"]);
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

    // ── date_prefix helper ─────────────────────────────────────────────────

    #[test]
    fn date_prefix_normal_date_string() {
        assert_eq!(date_prefix("2024-01-15T10:00:00.000Z"), "2024-01-15");
    }

    #[test]
    fn date_prefix_empty_string() {
        assert_eq!(date_prefix(""), "");
    }

    #[test]
    fn date_prefix_short_string() {
        assert_eq!(date_prefix("2024"), "2024");
    }

    #[test]
    fn date_prefix_exactly_ten_chars() {
        assert_eq!(date_prefix("2024-01-15"), "2024-01-15");
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

    // ── list_projects action ────────────────────────────────────────────────

    #[test]
    fn parameters_schema_includes_list_projects_action() {
        let schema = test_tool(vec!["list_projects"]).parameters_schema();
        let actions = schema["properties"]["action"]["enum"].as_array().unwrap();
        let action_strs: Vec<&str> = actions.iter().filter_map(|v| v.as_str()).collect();
        assert!(action_strs.contains(&"list_projects"));
    }

    #[tokio::test]
    async fn execute_list_projects_disallowed_returns_error() {
        let result = test_tool(vec!["get_ticket"])
            .execute(json!({"action": "list_projects"}))
            .await
            .unwrap();
        assert!(!result.success);
        let err = result.error.unwrap();
        assert!(err.contains("not enabled"));
        assert!(err.contains("allowed_actions"));
    }

    #[tokio::test]
    async fn execute_list_projects_not_blocked_in_readonly_mode() {
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::ReadOnly,
            ..SecurityPolicy::default()
        });
        let tool = JiraTool::new(
            "https://127.0.0.1:1".into(),
            "test@example.com".into(),
            "token".into(),
            vec!["list_projects".into()],
            security,
            30,
        );
        let result = tool
            .execute(json!({"action": "list_projects"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(
            !result.error.as_deref().unwrap_or("").contains("read-only"),
            "error should not mention read-only policy: {:?}",
            result.error
        );
    }

    #[test]
    fn shape_projects_extracts_expected_fields() {
        let projects = json!([
            { "key": "AT", "name": "ALL TASKS", "projectTypeKey": "business", "style": "next-gen" },
            { "key": "GP", "name": "G-PROJECT", "projectTypeKey": "software", "style": "next-gen" }
        ]);
        let statuses: Vec<Value> = vec![
            json!([
                { "name": "Task", "statuses": [
                    { "name": "To Do" }, { "name": "In Progress" }, { "name": "Collecting Intel" }, { "name": "Done" }
                ]},
                { "name": "Sub-task", "statuses": [
                    { "name": "To Do" }, { "name": "Verification" }
                ]}
            ]),
            json!([
                { "name": "Task", "statuses": [
                    { "name": "To Do" }, { "name": "Design" }, { "name": "Done" }
                ]},
                { "name": "Epic", "statuses": [
                    { "name": "To Do" }, { "name": "Done" }
                ]}
            ]),
        ];
        let shaped = shape_projects(projects.as_array().unwrap(), &statuses);
        let arr = &shaped;

        assert_eq!(arr.len(), 2);

        assert_eq!(arr[0]["key"], "AT");
        assert_eq!(arr[0]["name"], "ALL TASKS");
        assert_eq!(arr[0]["projectType"], "business");
        let at_statuses: Vec<&str> = arr[0]["statuses"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert_eq!(
            at_statuses,
            vec![
                "Collecting Intel",
                "Done",
                "In Progress",
                "To Do",
                "Verification",
            ]
        );
        let at_types: Vec<&str> = arr[0]["issueTypes"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert!(at_types.contains(&"Task"));
        assert!(at_types.contains(&"Sub-task"));

        assert_eq!(arr[1]["key"], "GP");
        assert_eq!(arr[1]["projectType"], "software");
        let gp_statuses: Vec<&str> = arr[1]["statuses"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert_eq!(gp_statuses, vec!["Design", "Done", "To Do"]);

        assert!(
            arr[0].get("users").is_none(),
            "users should not be in per-project data"
        );
    }

    #[test]
    fn shape_projects_sorts_statuses_alphabetically() {
        let projects = json!([
            { "key": "P", "name": "P", "projectTypeKey": "software", "style": "next-gen" }
        ]);
        let statuses: Vec<Value> = vec![json!([
            { "name": "Task", "statuses": [
                { "name": "Done" }, { "name": "Custom" }, { "name": "To Do" }, { "name": "Alpha" }
            ]}
        ])];
        let shaped = shape_projects(projects.as_array().unwrap(), &statuses);
        let ordered: Vec<&str> = shaped[0]["statuses"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert_eq!(ordered, vec!["Alpha", "Custom", "Done", "To Do"]);
    }

    #[test]
    fn shape_projects_empty_inputs() {
        let shaped = shape_projects(&[], &[]);
        assert_eq!(shaped.len(), 0);
    }

    // ── Confluence actions ─────────────────────────────────────────────────────

    #[test]
    fn parameters_schema_includes_confluence_actions() {
        let schema = test_tool(vec!["confluence_get_space"]).parameters_schema();
        let actions = schema["properties"]["action"]["enum"].as_array().unwrap();
        let action_strs: Vec<&str> = actions.iter().filter_map(|v| v.as_str()).collect();
        assert!(action_strs.contains(&"confluence_get_space"));
        assert!(action_strs.contains(&"confluence_get_page"));
        assert!(action_strs.contains(&"confluence_search"));
    }

    #[tokio::test]
    async fn execute_confluence_get_space_disallowed_returns_error() {
        let result = test_tool(vec!["get_ticket"])
            .execute(json!({"action": "confluence_get_space", "space_key": "PROJ"}))
            .await
            .unwrap();
        assert!(!result.success);
        let err = result.error.unwrap();
        assert!(err.contains("not enabled"));
        assert!(err.contains("allowed_actions"));
    }

    #[tokio::test]
    async fn execute_confluence_get_space_missing_space_key_returns_error() {
        let result = test_tool(vec!["confluence_get_space"])
            .execute(json!({"action": "confluence_get_space"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap().contains("space_key"));
    }

    #[tokio::test]
    async fn execute_confluence_get_page_disallowed_returns_error() {
        let result = test_tool(vec!["get_ticket"])
            .execute(json!({"action": "confluence_get_page", "page_id": "12345"}))
            .await
            .unwrap();
        assert!(!result.success);
        let err = result.error.unwrap();
        assert!(err.contains("not enabled"));
        assert!(err.contains("allowed_actions"));
    }

    #[tokio::test]
    async fn execute_confluence_get_page_missing_page_id_returns_error() {
        let result = test_tool(vec!["confluence_get_page"])
            .execute(json!({"action": "confluence_get_page"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap().contains("page_id"));
    }

    #[tokio::test]
    async fn execute_confluence_search_disallowed_returns_error() {
        let result = test_tool(vec!["get_ticket"])
            .execute(json!({"action": "confluence_search", "query": "test"}))
            .await
            .unwrap();
        assert!(!result.success);
        let err = result.error.unwrap();
        assert!(err.contains("not enabled"));
        assert!(err.contains("allowed_actions"));
    }

    #[tokio::test]
    async fn execute_confluence_search_missing_query_returns_error() {
        let result = test_tool(vec!["confluence_search"])
            .execute(json!({"action": "confluence_search"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap().contains("query"));
    }

    #[tokio::test]
    async fn execute_confluence_search_empty_query_returns_error() {
        let result = test_tool(vec!["confluence_search"])
            .execute(json!({"action": "confluence_search", "query": "  "}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap().contains("query"));
    }

    #[tokio::test]
    async fn execute_confluence_not_blocked_in_readonly_mode() {
        // Confluence actions are Read operations — should not be blocked by ReadOnly policy.
        // The call will fail at the HTTP level, so error must NOT mention "read-only".
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::ReadOnly,
            ..SecurityPolicy::default()
        });
        let tool = JiraTool::new(
            "https://127.0.0.1:1".into(),
            "test@example.com".into(),
            "token".into(),
            vec!["confluence_search".into()],
            security,
            30,
        );
        let result = tool
            .execute(json!({"action": "confluence_search", "query": "test"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(
            !result.error.as_deref().unwrap_or("").contains("read-only"),
            "confluence actions should not be blocked by read-only policy: {:?}",
            result.error
        );
    }

    // ── validate_space_key ─────────────────────────────────────────────────────

    #[test]
    fn validate_space_key_accepts_valid_keys() {
        assert!(validate_space_key("ENGINEERING").is_ok());
        assert!(validate_space_key("DP").is_ok());
        assert!(validate_space_key("DevOps").is_ok());
        assert!(validate_space_key("MY_SPACE").is_ok());
        assert!(validate_space_key("~personalspace").is_ok());
    }

    #[test]
    fn validate_space_key_rejects_invalid_keys() {
        assert!(validate_space_key("").is_err());
        assert!(validate_space_key("has space").is_err());
        assert!(validate_space_key("has/slash").is_err());
        assert!(validate_space_key("../traversal").is_err());
    }

    // ── validate_page_id ───────────────────────────────────────────────────────

    #[test]
    fn validate_page_id_accepts_numeric_ids() {
        assert!(validate_page_id("5403115521").is_ok());
        assert!(validate_page_id("1").is_ok());
        assert!(validate_page_id("123456789").is_ok());
    }

    #[test]
    fn validate_page_id_rejects_non_numeric() {
        assert!(validate_page_id("").is_err());
        assert!(validate_page_id("abc").is_err());
        assert!(validate_page_id("123abc").is_err());
        assert!(validate_page_id("12-34").is_err());
    }

    // ── build_confluence_tree ──────────────────────────────────────────────────

    #[test]
    fn build_confluence_tree_single_root_no_children() {
        let mut id_to_title = HashMap::new();
        id_to_title.insert("1".to_string(), "Root".to_string());
        let parent_to_children = HashMap::new();
        let tree = build_confluence_tree("1", &id_to_title, &parent_to_children);
        assert_eq!(tree["id"], "1");
        assert_eq!(tree["title"], "Root");
        assert_eq!(tree["children"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn build_confluence_tree_with_nested_children() {
        let mut id_to_title = HashMap::new();
        id_to_title.insert("1".to_string(), "Root".to_string());
        id_to_title.insert("2".to_string(), "Child".to_string());
        id_to_title.insert("3".to_string(), "Grandchild".to_string());
        let mut parent_to_children = HashMap::new();
        parent_to_children.insert("1".to_string(), vec!["2".to_string()]);
        parent_to_children.insert("2".to_string(), vec!["3".to_string()]);
        let tree = build_confluence_tree("1", &id_to_title, &parent_to_children);
        let children = tree["children"].as_array().unwrap();
        assert_eq!(children.len(), 1);
        assert_eq!(children[0]["id"], "2");
        let grandchildren = children[0]["children"].as_array().unwrap();
        assert_eq!(grandchildren.len(), 1);
        assert_eq!(grandchildren[0]["id"], "3");
        assert_eq!(grandchildren[0]["title"], "Grandchild");
    }
}
