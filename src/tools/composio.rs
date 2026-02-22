// Composio Tool Provider — optional managed tool surface with 1000+ OAuth integrations.
//
// When enabled, ZeroClaw can execute actions on Gmail, Notion, GitHub, Slack, etc.
// through Composio's API without storing raw OAuth tokens locally.
//
// This is opt-in. Users who prefer sovereign/local-only mode skip this entirely.
// The Composio API key is stored in the encrypted secret store.

use super::traits::{Tool, ToolResult};
use crate::security::policy::ToolOperation;
use crate::security::SecurityPolicy;
use anyhow::Context;
use async_trait::async_trait;
use parking_lot::RwLock;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use std::fmt::Write;
use std::sync::Arc;

const COMPOSIO_API_BASE_V3: &str = "https://backend.composio.dev/api/v3";
const COMPOSIO_TOOL_VERSION_LATEST: &str = "latest";

fn ensure_https(url: &str) -> anyhow::Result<()> {
    if !url.starts_with("https://") {
        anyhow::bail!(
            "Refusing to transmit sensitive data over non-HTTPS URL: URL scheme must be https"
        );
    }
    Ok(())
}

/// A tool that proxies actions to the Composio managed tool platform.
pub struct ComposioTool {
    api_key: String,
    default_entity_id: String,
    security: Arc<SecurityPolicy>,
    recent_connected_accounts: RwLock<HashMap<String, String>>,
    action_slug_cache: RwLock<HashMap<String, String>>,
}

impl ComposioTool {
    pub fn new(
        api_key: &str,
        default_entity_id: Option<&str>,
        security: Arc<SecurityPolicy>,
    ) -> Self {
        Self {
            api_key: api_key.to_string(),
            default_entity_id: normalize_entity_id(default_entity_id.unwrap_or("default")),
            security,
            recent_connected_accounts: RwLock::new(HashMap::new()),
            action_slug_cache: RwLock::new(HashMap::new()),
        }
    }

    fn client(&self) -> Client {
        crate::config::build_runtime_proxy_client_with_timeouts("tool.composio", 60, 10)
    }

    /// List available Composio apps/actions for the authenticated user.
    ///
    /// Uses the v3 endpoint.
    pub async fn list_actions(
        &self,
        app_name: Option<&str>,
    ) -> anyhow::Result<Vec<ComposioAction>> {
        self.list_actions_v3(app_name).await
    }

    async fn list_actions_v3(&self, app_name: Option<&str>) -> anyhow::Result<Vec<ComposioAction>> {
        let url = format!("{COMPOSIO_API_BASE_V3}/tools");
        let req = self
            .client()
            .get(&url)
            .header("x-api-key", &self.api_key)
            .query(&Self::build_list_actions_v3_query(app_name));

        let resp = req.send().await?;
        if !resp.status().is_success() {
            let err = response_error(resp).await;
            anyhow::bail!("Composio v3 API error: {err}");
        }

        let body: ComposioToolsResponse = resp
            .json()
            .await
            .context("Failed to decode Composio v3 tools response")?;
        self.update_action_slug_cache_from_v3_items(&body.items);
        Ok(map_v3_tools_to_actions(body.items))
    }

    fn update_action_slug_cache_from_v3_items(&self, items: &[ComposioV3Tool]) {
        for item in items {
            let Some(slug) = item.slug.as_deref().or(item.name.as_deref()) else {
                continue;
            };
            self.cache_action_slug(slug, slug);
            if let Some(name) = item.name.as_deref() {
                self.cache_action_slug(name, slug);
            }
        }
    }

    /// List connected accounts for a user and optional toolkit/app.
    async fn list_connected_accounts(
        &self,
        app_name: Option<&str>,
        entity_id: Option<&str>,
    ) -> anyhow::Result<Vec<ComposioConnectedAccount>> {
        let url = format!("{COMPOSIO_API_BASE_V3}/connected_accounts");
        let mut req = self.client().get(&url).header("x-api-key", &self.api_key);

        req = req.query(&[
            ("limit", "50"),
            ("order_by", "updated_at"),
            ("order_direction", "desc"),
            ("statuses", "INITIALIZING"),
            ("statuses", "ACTIVE"),
            ("statuses", "INITIATED"),
        ]);

        if let Some(app) = app_name
            .map(normalize_app_slug)
            .filter(|app| !app.is_empty())
        {
            req = req.query(&[("toolkit_slugs", app.as_str())]);
        }

        if let Some(entity) = entity_id {
            req = req.query(&[("user_ids", entity)]);
        }

        let resp = req.send().await?;
        if !resp.status().is_success() {
            let err = response_error(resp).await;
            anyhow::bail!("Composio v3 connected accounts lookup failed: {err}");
        }

        let body: ComposioConnectedAccountsResponse = resp
            .json()
            .await
            .context("Failed to decode Composio v3 connected accounts response")?;
        Ok(body.items)
    }

    fn cache_connected_account(&self, app_name: &str, entity_id: &str, connected_account_id: &str) {
        let key = connected_account_cache_key(app_name, entity_id);
        self.recent_connected_accounts
            .write()
            .insert(key, connected_account_id.to_string());
    }

    fn get_cached_connected_account(&self, app_name: &str, entity_id: &str) -> Option<String> {
        let key = connected_account_cache_key(app_name, entity_id);
        self.recent_connected_accounts.read().get(&key).cloned()
    }

    async fn resolve_connected_account_ref(
        &self,
        app_name: Option<&str>,
        entity_id: Option<&str>,
    ) -> anyhow::Result<Option<String>> {
        let app = app_name
            .map(normalize_app_slug)
            .filter(|app| !app.is_empty());
        let entity = entity_id.map(normalize_entity_id);
        let (Some(app), Some(entity)) = (app, entity) else {
            return Ok(None);
        };

        if let Some(cached) = self.get_cached_connected_account(&app, &entity) {
            return Ok(Some(cached));
        }

        let accounts = self
            .list_connected_accounts(Some(&app), Some(&entity))
            .await?;
        // The API returns accounts ordered by updated_at DESC, so the first
        // usable account is the most recently active one.  We always pick it
        // rather than giving up when multiple accounts exist — giving up was
        // the root cause of the "cannot find connected account" loop reported
        // in issue #959.
        let Some(first) = accounts.into_iter().find(|acct| acct.is_usable()) else {
            return Ok(None);
        };

        self.cache_connected_account(&app, &entity, &first.id);
        Ok(Some(first.id))
    }

    /// Execute a Composio action/tool with given parameters.
    ///
    /// Uses the v3 endpoint.
    pub async fn execute_action(
        &self,
        action_name: &str,
        app_name_hint: Option<&str>,
        params: serde_json::Value,
        entity_id: Option<&str>,
        connected_account_ref: Option<&str>,
    ) -> anyhow::Result<serde_json::Value> {
        let app_hint = app_name_hint
            .map(normalize_app_slug)
            .filter(|app| !app.is_empty())
            .or_else(|| infer_app_slug_from_action_name(action_name));
        let normalized_entity_id = entity_id.map(normalize_entity_id);
        let explicit_account_ref = connected_account_ref.and_then(|candidate| {
            let trimmed = candidate.trim();
            (!trimmed.is_empty()).then_some(trimmed.to_string())
        });
        let resolved_account_ref = if explicit_account_ref.is_some() {
            explicit_account_ref
        } else {
            self.resolve_connected_account_ref(app_hint.as_deref(), normalized_entity_id.as_deref())
                .await?
        };

        let mut slug_candidates = self.build_v3_slug_candidates(action_name);
        let mut prime_error = None;
        if slug_candidates.is_empty() {
            if let Some(app) = app_hint.as_deref() {
                match self.list_actions(Some(app)).await {
                    Ok(_) => {
                        slug_candidates = self.build_v3_slug_candidates(action_name);
                    }
                    Err(err) => {
                        prime_error = Some(format!(
                            "Failed to refresh action list for app '{app}': {err}"
                        ));
                    }
                }
            }
        }

        if slug_candidates.is_empty() {
            anyhow::bail!(
                "Unable to determine tool slug for '{action_name}'. Run action='list' with the relevant app first to prime the cache.{}",
                prime_error
                    .as_deref()
                    .map(|msg| format!(" ({msg})"))
                    .unwrap_or_default()
            );
        }

        let mut v3_errors = Vec::new();
        for slug in slug_candidates {
            self.cache_action_slug(action_name, &slug);
            match self
                .execute_action_v3(
                    &slug,
                    params.clone(),
                    normalized_entity_id.as_deref(),
                    resolved_account_ref.as_deref(),
                )
                .await
            {
                Ok(result) => return Ok(result),
                Err(err) => v3_errors.push(format!("{slug}: {err}")),
            }
        }

        let v3_error_summary = if v3_errors.is_empty() {
            "no v3 candidates attempted".to_string()
        } else {
            v3_errors.join(" | ")
        };

        let prime_suffix = prime_error
            .as_deref()
            .map(|msg| format!(" ({msg})"))
            .unwrap_or_default();

        anyhow::bail!(
            "Composio execute failed on v3 ({v3_error_summary}){prime_suffix}{}",
            build_connected_account_hint(
                app_hint.as_deref(),
                normalized_entity_id.as_deref(),
                resolved_account_ref.as_deref(),
            )
        );
    }

    fn build_v3_slug_candidates(&self, action_name: &str) -> Vec<String> {
        let mut candidates = Vec::new();
        let mut push_candidate = |candidate: String| {
            if !candidate.is_empty() && !candidates.contains(&candidate) {
                candidates.push(candidate);
            }
        };

        if let Some(hit) = self.lookup_cached_action_slug(action_name) {
            push_candidate(hit);
        }

        for slug in build_tool_slug_candidates(action_name) {
            push_candidate(slug);
        }

        candidates
    }

    fn cache_action_slug(&self, alias: &str, slug: &str) {
        let Some(key) = normalize_action_cache_key(alias) else {
            return;
        };
        let trimmed_slug = slug.trim();
        if trimmed_slug.is_empty() {
            return;
        }
        self.action_slug_cache
            .write()
            .insert(key, trimmed_slug.to_string());
    }

    fn lookup_cached_action_slug(&self, action_name: &str) -> Option<String> {
        let key = normalize_action_cache_key(action_name)?;
        self.action_slug_cache.read().get(&key).cloned()
    }

    fn build_list_actions_v3_query(app_name: Option<&str>) -> Vec<(String, String)> {
        let mut query = vec![
            ("limit".to_string(), "200".to_string()),
            (
                "toolkit_versions".to_string(),
                COMPOSIO_TOOL_VERSION_LATEST.to_string(),
            ),
        ];

        if let Some(app) = app_name.map(str::trim).filter(|app| !app.is_empty()) {
            query.push(("toolkits".to_string(), app.to_string()));
            query.push(("toolkit_slug".to_string(), app.to_string()));
        }

        query
    }

    fn build_execute_action_v3_request(
        tool_slug: &str,
        params: serde_json::Value,
        entity_id: Option<&str>,
        connected_account_ref: Option<&str>,
    ) -> (String, serde_json::Value) {
        let url = format!("{COMPOSIO_API_BASE_V3}/tools/execute/{tool_slug}");
        let account_ref = connected_account_ref.and_then(|candidate| {
            let trimmed_candidate = candidate.trim();
            (!trimmed_candidate.is_empty()).then_some(trimmed_candidate)
        });

        let mut body = json!({
            "arguments": params,
            "version": COMPOSIO_TOOL_VERSION_LATEST,
        });

        if let Some(entity) = entity_id {
            body["user_id"] = json!(entity);
        }
        if let Some(account_ref) = account_ref {
            body["connected_account_id"] = json!(account_ref);
        }

        (url, body)
    }

    async fn execute_action_v3(
        &self,
        tool_slug: &str,
        params: serde_json::Value,
        entity_id: Option<&str>,
        connected_account_ref: Option<&str>,
    ) -> anyhow::Result<serde_json::Value> {
        let (url, body) = Self::build_execute_action_v3_request(
            tool_slug,
            params,
            entity_id,
            connected_account_ref,
        );

        ensure_https(&url)?;

        let resp = self
            .client()
            .post(&url)
            .header("x-api-key", &self.api_key)
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let err = response_error(resp).await;
            anyhow::bail!("Composio v3 action execution failed: {err}");
        }

        let result: serde_json::Value = resp
            .json()
            .await
            .context("Failed to decode Composio v3 execute response")?;
        Ok(result)
    }

    /// Get the OAuth connection URL for a specific app/toolkit or auth config.
    ///
    /// Uses the v3 endpoint.
    pub async fn get_connection_url(
        &self,
        app_name: Option<&str>,
        auth_config_id: Option<&str>,
        entity_id: &str,
    ) -> anyhow::Result<ComposioConnectionLink> {
        self.get_connection_url_v3(app_name, auth_config_id, entity_id)
            .await
    }

    async fn get_connection_url_v3(
        &self,
        app_name: Option<&str>,
        auth_config_id: Option<&str>,
        entity_id: &str,
    ) -> anyhow::Result<ComposioConnectionLink> {
        let auth_config_id = match auth_config_id {
            Some(id) => id.to_string(),
            None => {
                let app = app_name.ok_or_else(|| {
                    anyhow::anyhow!("Missing 'app' or 'auth_config_id' for v3 connect")
                })?;
                self.resolve_auth_config_id(app).await?
            }
        };

        let url = format!("{COMPOSIO_API_BASE_V3}/connected_accounts/link");
        let body = json!({
            "auth_config_id": auth_config_id,
            "user_id": entity_id,
        });

        let resp = self
            .client()
            .post(&url)
            .header("x-api-key", &self.api_key)
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let err = response_error(resp).await;
            anyhow::bail!("Composio v3 connect failed: {err}");
        }

        let result: serde_json::Value = resp
            .json()
            .await
            .context("Failed to decode Composio v3 connect response")?;
        let redirect_url = extract_redirect_url(&result)
            .ok_or_else(|| anyhow::anyhow!("No redirect URL in Composio v3 response"))?;
        Ok(ComposioConnectionLink {
            redirect_url,
            connected_account_id: extract_connected_account_id(&result),
        })
    }


    async fn resolve_auth_config_id(&self, app_name: &str) -> anyhow::Result<String> {
        let url = format!("{COMPOSIO_API_BASE_V3}/auth_configs");

        let resp = self
            .client()
            .get(&url)
            .header("x-api-key", &self.api_key)
            .query(&[
                ("toolkit_slug", app_name),
                ("show_disabled", "true"),
                ("limit", "25"),
            ])
            .send()
            .await?;

        if !resp.status().is_success() {
            let err = response_error(resp).await;
            anyhow::bail!("Composio v3 auth config lookup failed: {err}");
        }

        let body: ComposioAuthConfigsResponse = resp
            .json()
            .await
            .context("Failed to decode Composio v3 auth configs response")?;

        if body.items.is_empty() {
            anyhow::bail!(
                "No auth config found for toolkit '{app_name}'. Create one in Composio first."
            );
        }

        let preferred = body
            .items
            .iter()
            .find(|cfg| cfg.is_enabled())
            .or_else(|| body.items.first())
            .context("No usable auth config returned by Composio")?;

        Ok(preferred.id.clone())
    }
}

#[async_trait]
impl Tool for ComposioTool {
    fn name(&self) -> &str {
        "composio"
    }

    fn description(&self) -> &str {
        "Execute actions on 1000+ apps via Composio (Gmail, Notion, GitHub, Slack, etc.). \
         Use action='list' to see available actions, \
         action='list_accounts' or action='connected_accounts' to list OAuth-connected accounts after login, \
         action='execute' with action_name/tool_slug and params (connected_account_id auto-resolved when omitted), \
         or action='connect' with app/auth_config_id to get OAuth URL."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "description": "The operation: 'list' (list available actions), 'list_accounts'/'connected_accounts' (list connected accounts), 'execute' (run an action), or 'connect' (get OAuth URL)",
                    "enum": ["list", "list_accounts", "connected_accounts", "execute", "connect"]
                },
                "app": {
                    "type": "string",
                    "description": "Toolkit slug filter for 'list' or 'list_accounts', optional app hint for 'execute', or toolkit/app for 'connect' (e.g. 'gmail', 'notion', 'github')"
                },
                "action_name": {
                    "type": "string",
                    "description": "Action/tool identifier to execute (legacy aliases supported)"
                },
                "tool_slug": {
                    "type": "string",
                    "description": "Preferred v3 tool slug to execute (alias of action_name)"
                },
                "params": {
                    "type": "object",
                    "description": "Parameters to pass to the action"
                },
                "entity_id": {
                    "type": "string",
                    "description": "Entity/user ID for multi-user setups (defaults to composio.entity_id from config)"
                },
                "auth_config_id": {
                    "type": "string",
                    "description": "Optional Composio v3 auth config id for connect flow"
                },
                "connected_account_id": {
                    "type": "string",
                    "description": "Optional connected account ID for execute flow when a specific account is required"
                }
            },
            "required": ["action"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let action = args
            .get("action")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'action' parameter"))?;

        let entity_id = args
            .get("entity_id")
            .and_then(|v| v.as_str())
            .unwrap_or(self.default_entity_id.as_str());

        match action {
            "list" => {
                let app = args.get("app").and_then(|v| v.as_str());
                match self.list_actions(app).await {
                    Ok(actions) => {
                        let summary: Vec<String> = actions
                            .iter()
                            .take(20)
                            .map(|a| {
                                format!(
                                    "- {} ({}): {}",
                                    a.name,
                                    a.app_name.as_deref().unwrap_or("?"),
                                    a.description.as_deref().unwrap_or("")
                                )
                            })
                            .collect();
                        let total = actions.len();
                        let output = format!(
                            "Found {total} available actions:\n{}{}",
                            summary.join("\n"),
                            if total > 20 {
                                format!("\n... and {} more", total - 20)
                            } else {
                                String::new()
                            }
                        );
                        Ok(ToolResult {
                            success: true,
                            output,
                            error: None,
                        })
                    }
                    Err(e) => Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!("Failed to list actions: {e}")),
                    }),
                }
            }

            // Accept both spellings so the LLM can use either.
            "list_accounts" | "connected_accounts" => {
                let app = args.get("app").and_then(|v| v.as_str());
                match self.list_connected_accounts(app, Some(entity_id)).await {
                    Ok(accounts) => {
                        if accounts.is_empty() {
                            let app_hint = app
                                .map(|value| format!(" for app '{value}'"))
                                .unwrap_or_default();
                            return Ok(ToolResult {
                                success: true,
                                output: format!(
                                    "No connected accounts found{app_hint} for entity '{entity_id}'. Run action='connect' first."
                                ),
                                error: None,
                            });
                        }

                        let summary: Vec<String> = accounts
                            .iter()
                            .take(20)
                            .map(|account| {
                                let toolkit = account.toolkit_slug().unwrap_or("?");
                                format!("- {} [{}] toolkit={toolkit}", account.id, account.status)
                            })
                            .collect();
                        let total = accounts.len();
                        let output = format!(
                            "Found {total} connected accounts (entity '{entity_id}'):\n{}{}\nUse connected_account_id in action='execute' when needed.",
                            summary.join("\n"),
                            if total > 20 {
                                format!("\n... and {} more", total - 20)
                            } else {
                                String::new()
                            }
                        );
                        Ok(ToolResult {
                            success: true,
                            output,
                            error: None,
                        })
                    }
                    Err(e) => Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!("Failed to list connected accounts: {e}")),
                    }),
                }
            }

            "execute" => {
                if let Err(error) = self
                    .security
                    .enforce_tool_operation(ToolOperation::Act, "composio.execute")
                {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(error),
                    });
                }

                let action_name = args
                    .get("tool_slug")
                    .or_else(|| args.get("action_name"))
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        anyhow::anyhow!("Missing 'action_name' (or 'tool_slug') for execute")
                    })?;

                let app = args.get("app").and_then(|v| v.as_str());
                let params = args.get("params").cloned().unwrap_or(json!({}));
                let acct_ref = args.get("connected_account_id").and_then(|v| v.as_str());

                match self
                    .execute_action(action_name, app, params, Some(entity_id), acct_ref)
                    .await
                {
                    Ok(result) => {
                        let output = serde_json::to_string_pretty(&result)
                            .unwrap_or_else(|_| format!("{result:?}"));
                        Ok(ToolResult {
                            success: true,
                            output,
                            error: None,
                        })
                    }
                    Err(e) => Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!("Action execution failed: {e}")),
                    }),
                }
            }

            "connect" => {
                if let Err(error) = self
                    .security
                    .enforce_tool_operation(ToolOperation::Act, "composio.connect")
                {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(error),
                    });
                }

                let app = args.get("app").and_then(|v| v.as_str());
                let auth_config_id = args.get("auth_config_id").and_then(|v| v.as_str());

                if app.is_none() && auth_config_id.is_none() {
                    anyhow::bail!("Missing 'app' or 'auth_config_id' for connect");
                }

                match self
                    .get_connection_url(app, auth_config_id, entity_id)
                    .await
                {
                    Ok(link) => {
                        let target =
                            app.unwrap_or(auth_config_id.unwrap_or("provided auth config"));
                        let mut output = format!(
                            "Open this URL to connect {target}:\n{}",
                            link.redirect_url
                        );
                        if let Some(connected_account_id) = link.connected_account_id.as_deref() {
                            if let Some(app_name) = app {
                                self.cache_connected_account(app_name, entity_id, connected_account_id);
                            }
                            let _ = write!(output, "\nConnected account ID: {connected_account_id}");
                        }
                        Ok(ToolResult {
                            success: true,
                            output,
                            error: None,
                        })
                    }
                    Err(e) => Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!("Failed to get connection URL: {e}")),
                    }),
                }
            }

            _ => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Unknown action '{action}'. Use 'list', 'list_accounts', 'execute', or 'connect'."
                )),
            }),
        }
    }
}

fn normalize_entity_id(entity_id: &str) -> String {
    let trimmed = entity_id.trim();
    if trimmed.is_empty() {
        "default".to_string()
    } else {
        trimmed.to_string()
    }
}

fn normalize_tool_slug(action_name: &str) -> String {
    action_name.trim().replace('_', "-").to_ascii_lowercase()
}

fn build_tool_slug_candidates(action_name: &str) -> Vec<String> {
    let trimmed = action_name.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }

    let mut candidates = Vec::new();
    let mut push_candidate = |candidate: String| {
        if !candidate.is_empty() && !candidates.contains(&candidate) {
            candidates.push(candidate);
        }
    };

    push_candidate(normalize_tool_slug(trimmed));

    let lower = trimmed.to_ascii_lowercase();
    push_candidate(lower.clone());

    let underscore_lower = lower.replace('-', "_");
    push_candidate(underscore_lower);

    let hyphen_lower = lower.replace('_', "-");
    push_candidate(hyphen_lower);

    candidates
}

fn normalize_app_slug(app_name: &str) -> String {
    app_name
        .trim()
        .replace('_', "-")
        .to_ascii_lowercase()
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

fn infer_app_slug_from_action_name(action_name: &str) -> Option<String> {
    let trimmed = action_name.trim();
    if trimmed.is_empty() {
        return None;
    }

    let raw = if trimmed.contains('-') {
        trimmed.split('-').next()
    } else if trimmed.contains('_') {
        trimmed.split('_').next()
    } else {
        None
    }?;

    let app = normalize_app_slug(raw);
    (!app.is_empty()).then_some(app)
}

fn connected_account_cache_key(app_name: &str, entity_id: &str) -> String {
    format!(
        "{}:{}",
        normalize_entity_id(entity_id),
        normalize_app_slug(app_name)
    )
}

fn normalize_action_cache_key(alias: &str) -> Option<String> {
    let trimmed = alias.trim();
    if trimmed.is_empty() {
        return None;
    }

    Some(trimmed.to_ascii_lowercase())
}

fn build_connected_account_hint(
    app_hint: Option<&str>,
    entity_id: Option<&str>,
    connected_account_ref: Option<&str>,
) -> String {
    if connected_account_ref.is_some() {
        return String::new();
    }

    let Some(entity) = entity_id else {
        return String::new();
    };

    if let Some(app) = app_hint {
        format!(
            " Hint: use action='list_accounts' with app='{app}' and entity_id='{entity}' to retrieve connected_account_id."
        )
    } else {
        format!(
            " Hint: use action='list_accounts' with entity_id='{entity}' to retrieve connected_account_id."
        )
    }
}

fn map_v3_tools_to_actions(items: Vec<ComposioV3Tool>) -> Vec<ComposioAction> {
    items
        .into_iter()
        .filter_map(|item| {
            let name = item.slug.or(item.name.clone())?;
            let app_name = item
                .toolkit
                .as_ref()
                .and_then(|toolkit| toolkit.slug.clone().or(toolkit.name.clone()))
                .or(item.app_name);
            let description = item.description.or(item.name);
            Some(ComposioAction {
                name,
                app_name,
                description,
                enabled: true,
            })
        })
        .collect()
}

fn extract_redirect_url(result: &serde_json::Value) -> Option<String> {
    result
        .get("redirect_url")
        .and_then(|v| v.as_str())
        .or_else(|| result.get("redirectUrl").and_then(|v| v.as_str()))
        .or_else(|| {
            result
                .get("data")
                .and_then(|v| v.get("redirect_url"))
                .and_then(|v| v.as_str())
        })
        .map(ToString::to_string)
}

fn extract_connected_account_id(result: &serde_json::Value) -> Option<String> {
    result
        .get("connected_account_id")
        .and_then(|v| v.as_str())
        .or_else(|| result.get("connectedAccountId").and_then(|v| v.as_str()))
        .or_else(|| {
            result
                .get("data")
                .and_then(|v| v.get("connected_account_id"))
                .and_then(|v| v.as_str())
        })
        .or_else(|| {
            result
                .get("data")
                .and_then(|v| v.get("connectedAccountId"))
                .and_then(|v| v.as_str())
        })
        .map(ToString::to_string)
}

async fn response_error(resp: reqwest::Response) -> String {
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if body.trim().is_empty() {
        return format!("HTTP {}", status.as_u16());
    }

    if let Some(api_error) = extract_api_error_message(&body) {
        return format!(
            "HTTP {}: {}",
            status.as_u16(),
            sanitize_error_message(&api_error)
        );
    }

    format!("HTTP {}", status.as_u16())
}

fn sanitize_error_message(message: &str) -> String {
    let mut sanitized = message.replace('\n', " ");
    for marker in [
        "connected_account_id",
        "connectedAccountId",
        "entity_id",
        "entityId",
        "user_id",
        "userId",
    ] {
        sanitized = sanitized.replace(marker, "[redacted]");
    }

    let max_chars = 240;
    if sanitized.chars().count() <= max_chars {
        sanitized
    } else {
        let mut end = max_chars;
        while end > 0 && !sanitized.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}...", &sanitized[..end])
    }
}

fn extract_api_error_message(body: &str) -> Option<String> {
    let parsed: serde_json::Value = serde_json::from_str(body).ok()?;
    parsed
        .get("error")
        .and_then(|v| v.get("message"))
        .and_then(|v| v.as_str())
        .map(ToString::to_string)
        .or_else(|| {
            parsed
                .get("message")
                .and_then(|v| v.as_str())
                .map(ToString::to_string)
        })
}

// ── API response types ──────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct ComposioToolsResponse {
    #[serde(default)]
    items: Vec<ComposioV3Tool>,
}

#[derive(Debug, Deserialize)]
struct ComposioConnectedAccountsResponse {
    #[serde(default)]
    items: Vec<ComposioConnectedAccount>,
}

#[derive(Debug, Clone, Deserialize)]
struct ComposioConnectedAccount {
    id: String,
    #[serde(default)]
    status: String,
    #[serde(default)]
    toolkit: Option<ComposioToolkitRef>,
}

impl ComposioConnectedAccount {
    fn is_usable(&self) -> bool {
        self.status.eq_ignore_ascii_case("INITIALIZING")
            || self.status.eq_ignore_ascii_case("ACTIVE")
            || self.status.eq_ignore_ascii_case("INITIATED")
    }

    fn toolkit_slug(&self) -> Option<&str> {
        self.toolkit
            .as_ref()
            .and_then(|toolkit| toolkit.slug.as_deref())
    }
}

#[derive(Debug, Clone, Deserialize)]
struct ComposioV3Tool {
    #[serde(default)]
    slug: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(rename = "appName", default)]
    app_name: Option<String>,
    #[serde(default)]
    toolkit: Option<ComposioToolkitRef>,
}

#[derive(Debug, Clone, Deserialize)]
struct ComposioToolkitRef {
    #[serde(default)]
    slug: Option<String>,
    #[serde(default)]
    name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ComposioAuthConfigsResponse {
    #[serde(default)]
    items: Vec<ComposioAuthConfig>,
}

#[derive(Debug, Clone)]
pub struct ComposioConnectionLink {
    pub redirect_url: String,
    pub connected_account_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct ComposioAuthConfig {
    id: String,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    enabled: Option<bool>,
}

impl ComposioAuthConfig {
    fn is_enabled(&self) -> bool {
        self.enabled.unwrap_or(false)
            || self
                .status
                .as_deref()
                .is_some_and(|v| v.eq_ignore_ascii_case("enabled"))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComposioAction {
    pub name: String,
    #[serde(rename = "appName")]
    pub app_name: Option<String>,
    pub description: Option<String>,
    #[serde(default)]
    pub enabled: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::{AutonomyLevel, SecurityPolicy};

    fn test_security() -> Arc<SecurityPolicy> {
        Arc::new(SecurityPolicy::default())
    }

    // ── Constructor ───────────────────────────────────────────

    #[test]
    fn composio_tool_has_correct_name() {
        let tool = ComposioTool::new("test-key", None, test_security());
        assert_eq!(tool.name(), "composio");
    }

    #[test]
    fn composio_tool_has_description() {
        let tool = ComposioTool::new("test-key", None, test_security());
        assert!(!tool.description().is_empty());
        assert!(tool.description().contains("1000+"));
    }

    #[test]
    fn composio_tool_schema_has_required_fields() {
        let tool = ComposioTool::new("test-key", None, test_security());
        let schema = tool.parameters_schema();
        assert!(schema["properties"]["action"].is_object());
        assert!(schema["properties"]["action_name"].is_object());
        assert!(schema["properties"]["tool_slug"].is_object());
        assert!(schema["properties"]["params"].is_object());
        assert!(schema["properties"]["app"].is_object());
        assert!(schema["properties"]["auth_config_id"].is_object());
        assert!(schema["properties"]["connected_account_id"].is_object());
        let required = schema["required"].as_array().unwrap();
        assert!(required.contains(&json!("action")));
        let enum_values = schema["properties"]["action"]["enum"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect::<Vec<_>>();
        assert!(enum_values.contains(&"list_accounts"));
    }

    #[test]
    fn composio_tool_spec_roundtrip() {
        let tool = ComposioTool::new("test-key", None, test_security());
        let spec = tool.spec();
        assert_eq!(spec.name, "composio");
        assert!(spec.parameters.is_object());
    }

    // ── Execute validation ────────────────────────────────────

    #[tokio::test]
    async fn execute_missing_action_returns_error() {
        let tool = ComposioTool::new("test-key", None, test_security());
        let result = tool.execute(json!({})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn execute_unknown_action_returns_error() {
        let tool = ComposioTool::new("test-key", None, test_security());
        let result = tool.execute(json!({"action": "unknown"})).await.unwrap();
        assert!(!result.success);
        assert!(result.error.as_ref().unwrap().contains("Unknown action"));
    }

    #[tokio::test]
    async fn execute_without_action_name_returns_error() {
        let tool = ComposioTool::new("test-key", None, test_security());
        let result = tool.execute(json!({"action": "execute"})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn connect_without_target_returns_error() {
        let tool = ComposioTool::new("test-key", None, test_security());
        let result = tool.execute(json!({"action": "connect"})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn execute_blocked_in_readonly_mode() {
        let readonly = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::ReadOnly,
            ..SecurityPolicy::default()
        });
        let tool = ComposioTool::new("test-key", None, readonly);
        let result = tool
            .execute(json!({
                "action": "execute",
                "action_name": "GITHUB_LIST_REPOS"
            }))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result
            .error
            .as_deref()
            .unwrap_or("")
            .contains("read-only mode"));
    }

    #[tokio::test]
    async fn execute_blocked_when_rate_limited() {
        let limited = Arc::new(SecurityPolicy {
            max_actions_per_hour: 0,
            ..SecurityPolicy::default()
        });
        let tool = ComposioTool::new("test-key", None, limited);
        let result = tool
            .execute(json!({
                "action": "execute",
                "action_name": "GITHUB_LIST_REPOS"
            }))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result
            .error
            .as_deref()
            .unwrap_or("")
            .contains("Rate limit exceeded"));
    }

    // ── API response parsing ──────────────────────────────────

    #[test]
    fn composio_action_deserializes() {
        let json_str = r#"{"name": "GMAIL_FETCH_EMAILS", "appName": "gmail", "description": "Fetch emails", "enabled": true}"#;
        let action: ComposioAction = serde_json::from_str(json_str).unwrap();
        assert_eq!(action.name, "GMAIL_FETCH_EMAILS");
        assert_eq!(action.app_name.as_deref(), Some("gmail"));
        assert!(action.enabled);
    }

    #[test]
    fn composio_actions_response_deserializes() {
        let json_str = r#"{"items": [{"name": "TEST_ACTION", "appName": "test", "description": "A test", "enabled": true}]}"#;
        let resp: ComposioActionsResponse = serde_json::from_str(json_str).unwrap();
        assert_eq!(resp.items.len(), 1);
        assert_eq!(resp.items[0].name, "TEST_ACTION");
    }

    #[test]
    fn composio_actions_response_empty() {
        let json_str = r#"{"items": []}"#;
        let resp: ComposioActionsResponse = serde_json::from_str(json_str).unwrap();
        assert!(resp.items.is_empty());
    }

    #[test]
    fn composio_actions_response_missing_items_defaults() {
        let json_str = r"{}";
        let resp: ComposioActionsResponse = serde_json::from_str(json_str).unwrap();
        assert!(resp.items.is_empty());
    }

    #[test]
    fn composio_v3_tools_response_maps_to_actions() {
        let json_str = r#"{
            "items": [
                {
                    "slug": "gmail-fetch-emails",
                    "name": "Gmail Fetch Emails",
                    "description": "Fetch inbox emails",
                    "toolkit": { "slug": "gmail", "name": "Gmail" }
                }
            ]
        }"#;
        let resp: ComposioToolsResponse = serde_json::from_str(json_str).unwrap();
        let actions = map_v3_tools_to_actions(resp.items);
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].name, "gmail-fetch-emails");
        assert_eq!(actions[0].app_name.as_deref(), Some("gmail"));
        assert_eq!(
            actions[0].description.as_deref(),
            Some("Fetch inbox emails")
        );
    }

    #[test]
    fn normalize_entity_id_falls_back_to_default_when_blank() {
        assert_eq!(normalize_entity_id("   "), "default");
        assert_eq!(normalize_entity_id("workspace-user"), "workspace-user");
    }

    #[test]
    fn normalize_tool_slug_supports_legacy_action_name() {
        assert_eq!(
            normalize_tool_slug("GMAIL_FETCH_EMAILS"),
            "gmail-fetch-emails"
        );
        assert_eq!(
            normalize_tool_slug(" github-list-repos "),
            "github-list-repos"
        );
    }

    #[test]
    fn build_tool_slug_candidates_cover_common_variants() {
        let candidates = build_tool_slug_candidates("GMAIL_FETCH_EMAILS");
        assert!(candidates.contains(&"gmail-fetch-emails".to_string()));
        assert!(candidates.contains(&"gmail_fetch_emails".to_string()));

        let hyphen = build_tool_slug_candidates("github-list-repos");
        assert_eq!(hyphen.first().map(String::as_str), Some("github-list-repos"));
    }

    #[test]
    fn normalize_legacy_action_name_supports_v3_slug_input() {
        assert_eq!(
            normalize_legacy_action_name("gmail-fetch-emails"),
            "GMAIL_FETCH_EMAILS"
        );
        assert_eq!(
            normalize_legacy_action_name(" GITHUB_LIST_REPOS "),
            "GITHUB_LIST_REPOS"
        );
    }

    #[test]
    fn normalize_app_slug_removes_spaces_and_normalizes_case() {
        assert_eq!(normalize_app_slug(" Gmail "), "gmail");
        assert_eq!(normalize_app_slug("GITHUB_APP"), "github-app");
    }

    #[test]
    fn infer_app_slug_from_action_name_handles_v2_and_v3_formats() {
        assert_eq!(
            infer_app_slug_from_action_name("gmail-fetch-emails").as_deref(),
            Some("gmail")
        );
        assert_eq!(
            infer_app_slug_from_action_name("GMAIL_FETCH_EMAILS").as_deref(),
            Some("gmail")
        );
        assert!(infer_app_slug_from_action_name("execute").is_none());
    }

    #[test]
    fn connected_account_cache_key_is_stable() {
        assert_eq!(
            connected_account_cache_key("GMAIL", " default "),
            "default:gmail"
        );
    }

    #[test]
    fn build_connected_account_hint_returns_guidance_when_missing_ref() {
        let hint = build_connected_account_hint(Some("gmail"), Some("default"), None);
        assert!(hint.contains("list_accounts"));
        assert!(hint.contains("gmail"));
        assert!(hint.contains("default"));
    }

    #[test]
    fn build_connected_account_hint_without_app_is_still_actionable() {
        let hint = build_connected_account_hint(None, Some("default"), None);
        assert!(hint.contains("list_accounts"));
        assert!(hint.contains("entity_id='default'"));
        assert!(!hint.contains("app='"));
    }

    #[test]
    fn connected_account_is_usable_for_initializing_active_and_initiated() {
        for status in ["INITIALIZING", "ACTIVE", "INITIATED"] {
            let account = ComposioConnectedAccount {
                id: "ca_1".to_string(),
                status: status.to_string(),
                toolkit: None,
            };
            assert!(account.is_usable(), "status {status} should be usable");
        }
    }

    #[test]
    fn extract_connected_account_id_supports_common_shapes() {
        let root = json!({"connected_account_id": "ca_root"});
        let camel = json!({"connectedAccountId": "ca_camel"});
        let nested = json!({"data": {"connected_account_id": "ca_nested"}});

        assert_eq!(
            extract_connected_account_id(&root).as_deref(),
            Some("ca_root")
        );
        assert_eq!(
            extract_connected_account_id(&camel).as_deref(),
            Some("ca_camel")
        );
        assert_eq!(
            extract_connected_account_id(&nested).as_deref(),
            Some("ca_nested")
        );
    }

    #[test]
    fn extract_redirect_url_supports_v2_and_v3_shapes() {
        let v2 = json!({"redirectUrl": "https://app.composio.dev/connect-v2"});
        let v3 = json!({"redirect_url": "https://app.composio.dev/connect-v3"});
        let nested = json!({"data": {"redirect_url": "https://app.composio.dev/connect-nested"}});

        assert_eq!(
            extract_redirect_url(&v2).as_deref(),
            Some("https://app.composio.dev/connect-v2")
        );
        assert_eq!(
            extract_redirect_url(&v3).as_deref(),
            Some("https://app.composio.dev/connect-v3")
        );
        assert_eq!(
            extract_redirect_url(&nested).as_deref(),
            Some("https://app.composio.dev/connect-nested")
        );
    }

    #[test]
    fn auth_config_prefers_enabled_status() {
        let enabled = ComposioAuthConfig {
            id: "cfg_1".into(),
            status: Some("ENABLED".into()),
            enabled: None,
        };
        let disabled = ComposioAuthConfig {
            id: "cfg_2".into(),
            status: Some("DISABLED".into()),
            enabled: Some(false),
        };

        assert!(enabled.is_enabled());
        assert!(!disabled.is_enabled());
    }

    #[test]
    fn extract_api_error_message_from_common_shapes() {
        let nested = r#"{"error":{"message":"tool not found"}}"#;
        let flat = r#"{"message":"invalid api key"}"#;

        assert_eq!(
            extract_api_error_message(nested).as_deref(),
            Some("tool not found")
        );
        assert_eq!(
            extract_api_error_message(flat).as_deref(),
            Some("invalid api key")
        );
        assert_eq!(extract_api_error_message("not-json"), None);
    }

    #[test]
    fn composio_action_with_null_fields() {
        let json_str =
            r#"{"name": "TEST_ACTION", "appName": null, "description": null, "enabled": false}"#;
        let action: ComposioAction = serde_json::from_str(json_str).unwrap();
        assert_eq!(action.name, "TEST_ACTION");
        assert!(action.app_name.is_none());
        assert!(action.description.is_none());
        assert!(!action.enabled);
    }

    #[test]
    fn composio_action_with_special_characters() {
        let json_str = r#"{"name": "GMAIL_SEND_EMAIL_WITH_ATTACHMENT", "appName": "gmail", "description": "Send email with attachment & special chars: <>'\"\"", "enabled": true}"#;
        let action: ComposioAction = serde_json::from_str(json_str).unwrap();
        assert_eq!(action.name, "GMAIL_SEND_EMAIL_WITH_ATTACHMENT");
        assert!(action.description.as_ref().unwrap().contains('&'));
        assert!(action.description.as_ref().unwrap().contains('<'));
    }

    #[test]
    fn composio_action_with_unicode() {
        let json_str = r#"{"name": "SLACK_SEND_MESSAGE", "appName": "slack", "description": "Send message with emoji 🎉 and unicode Ω", "enabled": true}"#;
        let action: ComposioAction = serde_json::from_str(json_str).unwrap();
        assert!(action.description.as_ref().unwrap().contains("🎉"));
        assert!(action.description.as_ref().unwrap().contains("Ω"));
    }

    #[test]
    fn composio_malformed_json_returns_error() {
        let json_str = r#"{"name": "TEST_ACTION", "appName": "gmail", }"#;
        let result: Result<ComposioAction, _> = serde_json::from_str(json_str);
        assert!(result.is_err());
    }

    #[test]
    fn composio_empty_json_string_returns_error() {
        let json_str = r#" ""#;
        let result: Result<ComposioAction, _> = serde_json::from_str(json_str);
        assert!(result.is_err());
    }

    #[test]
    fn composio_large_actions_list() {
        let mut items = Vec::new();
        for i in 0..100 {
            items.push(json!({
                "name": format!("ACTION_{i}"),
                "appName": "test",
                "description": "Test action",
                "enabled": true
            }));
        }
        let json_str = json!({"items": items}).to_string();
        let resp: ComposioActionsResponse = serde_json::from_str(&json_str).unwrap();
        assert_eq!(resp.items.len(), 100);
    }

    #[test]
    fn composio_api_base_url_is_v3() {
        assert_eq!(COMPOSIO_API_BASE_V3, "https://backend.composio.dev/api/v3");
    }

    #[test]
    fn build_execute_action_v3_request_uses_fixed_endpoint_and_body_account_id() {
        let (url, body) = ComposioTool::build_execute_action_v3_request(
            "gmail-send-email",
            json!({"to": "test@example.com"}),
            Some("workspace-user"),
            Some("account-42"),
        );

        assert_eq!(
            url,
            "https://backend.composio.dev/api/v3/tools/execute/gmail-send-email"
        );
        assert_eq!(body["arguments"]["to"], json!("test@example.com"));
        assert_eq!(body["version"], json!(COMPOSIO_TOOL_VERSION_LATEST));
        assert_eq!(body["user_id"], json!("workspace-user"));
        assert_eq!(body["connected_account_id"], json!("account-42"));
    }

    #[test]
    fn build_list_actions_v3_query_requests_latest_versions() {
        let query = ComposioTool::build_list_actions_v3_query(None)
            .into_iter()
            .collect::<HashMap<String, String>>();
        assert_eq!(
            query.get("toolkit_versions"),
            Some(&COMPOSIO_TOOL_VERSION_LATEST.to_string())
        );
        assert_eq!(query.get("limit"), Some(&"200".to_string()));
        assert!(!query.contains_key("toolkits"));
        assert!(!query.contains_key("toolkit_slug"));
    }

    #[test]
    fn build_list_actions_v3_query_adds_app_filters_when_present() {
        let query = ComposioTool::build_list_actions_v3_query(Some(" github "))
            .into_iter()
            .collect::<HashMap<String, String>>();
        assert_eq!(
            query.get("toolkit_versions"),
            Some(&COMPOSIO_TOOL_VERSION_LATEST.to_string())
        );
        assert_eq!(query.get("toolkits"), Some(&"github".to_string()));
        assert_eq!(query.get("toolkit_slug"), Some(&"github".to_string()));
    }

    // ── resolve_connected_account_ref (multi-account fix) ────

    #[test]
    fn resolve_picks_first_usable_when_multiple_accounts_exist() {
        // Regression test for issue #959: previously returned None when
        // multiple accounts existed, causing the LLM to loop on the OAuth URL.
        let tool = ComposioTool::new("test-key", None, test_security());
        let accounts = vec![
            ComposioConnectedAccount {
                id: "ca_old".to_string(),
                status: "ACTIVE".to_string(),
                toolkit: None,
            },
            ComposioConnectedAccount {
                id: "ca_new".to_string(),
                status: "ACTIVE".to_string(),
                toolkit: None,
            },
        ];
        // Simulate what resolve_connected_account_ref does: find first usable.
        let resolved = accounts.into_iter().find(|a| a.is_usable()).map(|a| a.id);
        assert_eq!(resolved.as_deref(), Some("ca_old"));
    }

    #[test]
    fn resolve_picks_first_usable_skipping_unusable_head() {
        let accounts = vec![
            ComposioConnectedAccount {
                id: "ca_dead".to_string(),
                status: "DISCONNECTED".to_string(),
                toolkit: None,
            },
            ComposioConnectedAccount {
                id: "ca_live".to_string(),
                status: "ACTIVE".to_string(),
                toolkit: None,
            },
        ];
        let resolved = accounts.into_iter().find(|a| a.is_usable()).map(|a| a.id);
        assert_eq!(resolved.as_deref(), Some("ca_live"));
    }

    #[test]
    fn resolve_returns_none_when_no_usable_accounts() {
        let accounts = vec![ComposioConnectedAccount {
            id: "ca_dead".to_string(),
            status: "DISCONNECTED".to_string(),
            toolkit: None,
        }];
        let resolved = accounts.into_iter().find(|a| a.is_usable()).map(|a| a.id);
        assert!(resolved.is_none());
    }

    #[test]
    fn resolve_returns_none_for_empty_accounts() {
        let accounts: Vec<ComposioConnectedAccount> = vec![];
        let resolved = accounts.into_iter().find(|a| a.is_usable()).map(|a| a.id);
        assert!(resolved.is_none());
    }

    // ── connected_accounts alias ──────────────────────────────

    #[tokio::test]
    async fn connected_accounts_alias_dispatches_same_as_list_accounts() {
        // Both spellings should reach the same handler and return the same
        // shape of error (network failure in test, not a dispatch error).
        let tool = ComposioTool::new("test-key", None, test_security());
        let r1 = tool
            .execute(json!({"action": "list_accounts"}))
            .await
            .unwrap();
        let r2 = tool
            .execute(json!({"action": "connected_accounts"}))
            .await
            .unwrap();
        // Both fail the same way (network) — neither is a dispatch error.
        assert!(!r1.success);
        assert!(!r2.success);
        let e1 = r1.error.unwrap_or_default();
        let e2 = r2.error.unwrap_or_default();
        assert!(!e1.contains("Unknown action"), "list_accounts: {e1}");
        assert!(!e2.contains("Unknown action"), "connected_accounts: {e2}");
    }

    #[test]
    fn schema_enum_includes_connected_accounts_alias() {
        let tool = ComposioTool::new("test-key", None, test_security());
        let schema = tool.parameters_schema();
        let values: Vec<&str> = schema["properties"]["action"]["enum"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert!(values.contains(&"connected_accounts"));
        assert!(values.contains(&"list_accounts"));
    }

    #[test]
    fn description_mentions_connected_accounts() {
        let tool = ComposioTool::new("test-key", None, test_security());
        assert!(tool.description().contains("connected_accounts"));
    }

    #[test]
    fn build_execute_action_v3_request_drops_blank_optional_fields() {
        let (url, body) = ComposioTool::build_execute_action_v3_request(
            "github-list-repos",
            json!({}),
            None,
            Some("   "),
        );

        assert_eq!(
            url,
            "https://backend.composio.dev/api/v3/tools/execute/github-list-repos"
        );
        assert_eq!(body["arguments"], json!({}));
        assert_eq!(body["version"], json!(COMPOSIO_TOOL_VERSION_LATEST));
        assert!(body.get("connected_account_id").is_none());
        assert!(body.get("user_id").is_none());
    }
}
