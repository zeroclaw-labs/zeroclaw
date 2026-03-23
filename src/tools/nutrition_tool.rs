use super::traits::{Tool, ToolResult};
use crate::auth::oauth_common::{random_base64url, url_encode};
use crate::security::{policy::ToolOperation, SecurityPolicy};
use async_trait::async_trait;
use base64::Engine;
use hmac::{Hmac, Mac};
use reqwest::Client;
use serde_json::{json, Value};
use sha1::Sha1;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::{Mutex, RwLock};

const BASE_URL: &str = "https://platform.fatsecret.com/rest";
const TOKEN_URL: &str = "https://oauth.fatsecret.com/connect/token";
const MAX_ERROR_BODY_CHARS: usize = 500;

type HmacSha1 = Hmac<Sha1>;

// ── OAuth2 token cache ──────────────────────────────────────────

struct OAuth2Token {
    access_token: String,
    expires_at: u64, // unix seconds
}

// ── Tool struct ─────────────────────────────────────────────────

/// Tool for querying food/recipe data and managing a food diary via the
/// FatSecret Platform API.
///
/// Supports eight actions gated by `[nutrition].allowed_actions` in config:
/// - `item.search`    — search foods by name (OAuth2)
/// - `item.get`       — get detailed nutrition for a food (OAuth2)
/// - `recipe.search`  — search recipes (OAuth2)
/// - `recipe.get`     — get full recipe details (OAuth2)
/// - `diary.get`      — get food diary entries for a date (OAuth1 3-legged)
/// - `diary.create`   — add a food diary entry (OAuth1 3-legged)
/// - `diary.edit`     — edit a food diary entry (OAuth1 3-legged)
/// - `diary.delete`   — delete a food diary entry (OAuth1 3-legged)
pub struct NutritionTool {
    client_id: String,
    client_secret: String,
    auth_token: Option<String>,
    auth_secret: Option<String>,
    allowed_actions: Vec<String>,
    http: Client,
    security: Arc<SecurityPolicy>,
    timeout_secs: u64,
    oauth2_token: RwLock<Option<OAuth2Token>>,
    oauth2_acquire: Mutex<()>,
}

impl NutritionTool {
    pub fn new(
        client_id: String,
        client_secret: String,
        auth_token: Option<String>,
        auth_secret: Option<String>,
        allowed_actions: Vec<String>,
        security: Arc<SecurityPolicy>,
        timeout_secs: u64,
    ) -> Self {
        let http = Client::builder()
            .timeout(Duration::from_secs(timeout_secs))
            .connect_timeout(Duration::from_secs(10))
            .build()
            .expect("failed to build HTTP client");

        Self {
            client_id,
            client_secret,
            auth_token,
            auth_secret,
            allowed_actions,
            http,
            security,
            timeout_secs,
            oauth2_token: RwLock::new(None),
            oauth2_acquire: Mutex::new(()),
        }
    }

    fn is_action_allowed(&self, action: &str) -> bool {
        self.allowed_actions.iter().any(|a| a == action)
    }

    fn has_diary_credentials(&self) -> bool {
        self.auth_token
            .as_ref()
            .is_some_and(|t| !t.trim().is_empty())
            && self
                .auth_secret
                .as_ref()
                .is_some_and(|s| !s.trim().is_empty())
    }

    // ── OAuth2 ──────────────────────────────────────────────────

    async fn get_oauth2_token(&self) -> anyhow::Result<String> {
        // Fast path: check if we have a valid cached token.
        {
            let guard = self.oauth2_token.read().await;
            if let Some(tok) = guard.as_ref() {
                let now = now_unix_secs();
                if tok.expires_at > now + 60 {
                    return Ok(tok.access_token.clone());
                }
            }
        }

        // Acquire mutex to prevent thundering herd.
        let _lock = self.oauth2_acquire.lock().await;

        // Double-check after acquiring.
        {
            let guard = self.oauth2_token.read().await;
            if let Some(tok) = guard.as_ref() {
                let now = now_unix_secs();
                if tok.expires_at > now + 60 {
                    return Ok(tok.access_token.clone());
                }
            }
        }

        let resp = self
            .http
            .post(TOKEN_URL)
            .basic_auth(&self.client_id, Some(&self.client_secret))
            .form(&[
                ("grant_type", "client_credentials"),
                ("scope", "basic premier barcode"),
            ])
            .send()
            .await?;

        let status = resp.status();
        let body: Value = resp.json().await?;

        if !status.is_success() {
            let detail = body
                .get("error_description")
                .or_else(|| body.get("error"))
                .and_then(Value::as_str)
                .unwrap_or("unknown error");
            anyhow::bail!("OAuth2 token request failed ({status}): {detail}");
        }

        let access_token = body["access_token"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("missing access_token in OAuth2 response"))?
            .to_string();
        let expires_in = body["expires_in"].as_u64().unwrap_or(86400);
        let expires_at = now_unix_secs() + expires_in;

        {
            let mut guard = self.oauth2_token.write().await;
            *guard = Some(OAuth2Token {
                access_token: access_token.clone(),
                expires_at,
            });
        }

        Ok(access_token)
    }

    /// Send a GET request to a public endpoint using OAuth2 Bearer auth.
    async fn oauth2_get(&self, path: &str, params: &[(&str, &str)]) -> anyhow::Result<Value> {
        let token = self.get_oauth2_token().await?;
        let url = format!("{BASE_URL}{path}");

        let mut query: Vec<(&str, &str)> = params.to_vec();
        query.push(("format", "json"));

        let resp = self
            .http
            .get(&url)
            .bearer_auth(&token)
            .query(&query)
            .send()
            .await?;

        parse_response(resp).await
    }

    // ── OAuth1 3-legged ─────────────────────────────────────────

    /// Send an OAuth1 3-legged request for diary endpoints.
    async fn oauth1_request(
        &self,
        method: &str,
        path: &str,
        params: &[(&str, String)],
    ) -> anyhow::Result<Value> {
        let auth_token = self
            .auth_token
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("diary operations require auth_token in config"))?;
        let auth_secret = self
            .auth_secret
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("diary operations require auth_secret in config"))?;

        let url = format!("{BASE_URL}{path}");
        let timestamp = now_unix_secs().to_string();
        let nonce = random_base64url(24);

        // Build the parameter map (request params + OAuth params).
        let mut all_params: BTreeMap<String, String> = BTreeMap::new();
        all_params.insert("format".to_string(), "json".to_string());
        for (k, v) in params {
            all_params.insert((*k).to_string(), v.clone());
        }
        all_params.insert("oauth_consumer_key".to_string(), self.client_id.clone());
        all_params.insert("oauth_nonce".to_string(), nonce.clone());
        all_params.insert(
            "oauth_signature_method".to_string(),
            "HMAC-SHA1".to_string(),
        );
        all_params.insert("oauth_timestamp".to_string(), timestamp.clone());
        all_params.insert("oauth_token".to_string(), auth_token.to_string());
        all_params.insert("oauth_version".to_string(), "1.0".to_string());

        // Build signature base string.
        let signature =
            compute_oauth1_signature(method, &url, &all_params, &self.client_secret, auth_secret);
        all_params.insert("oauth_signature".to_string(), signature);

        // Build Authorization header.
        let auth_header = build_oauth1_auth_header(&all_params);

        // Separate non-OAuth params for query/body.
        let request_params: Vec<(String, String)> = all_params
            .into_iter()
            .filter(|(k, _)| !k.starts_with("oauth_"))
            .collect();

        let resp = match method {
            "GET" => {
                self.http
                    .get(&url)
                    .header("Authorization", &auth_header)
                    .query(&request_params)
                    .send()
                    .await?
            }
            "POST" => {
                self.http
                    .post(&url)
                    .header("Authorization", &auth_header)
                    .form(&request_params)
                    .send()
                    .await?
            }
            "PUT" => {
                self.http
                    .put(&url)
                    .header("Authorization", &auth_header)
                    .form(&request_params)
                    .send()
                    .await?
            }
            "DELETE" => {
                self.http
                    .delete(&url)
                    .header("Authorization", &auth_header)
                    .query(&request_params)
                    .send()
                    .await?
            }
            _ => anyhow::bail!("unsupported HTTP method: {method}"),
        };

        parse_response(resp).await
    }

    // ── Action handlers ─────────────────────────────────────────

    async fn item_search(&self, args: &Value) -> anyhow::Result<ToolResult> {
        let search_expr = args["search_expression"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let page = args["page_number"].as_u64().unwrap_or(0).to_string();
        let max = args["max_results"].as_u64().unwrap_or(20).to_string();

        let mut params: Vec<(&str, &str)> = vec![
            ("search_expression", &search_expr),
            ("page_number", &page),
            ("max_results", &max),
        ];

        let generic_desc_val;
        if let Some(gd) = args.get("generic_description").and_then(Value::as_str) {
            generic_desc_val = gd.to_string();
            params.push(("generic_description", &generic_desc_val));
        }

        let body = self.oauth2_get("/foods/search/v1", &params).await?;
        let shaped = shape_food_search(&body);
        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&shaped)?,
            error: None,
        })
    }

    async fn item_get(&self, args: &Value) -> anyhow::Result<ToolResult> {
        let food_id = require_str(args, "food_id")?;
        validate_numeric_id(&food_id, "food_id")?;

        let mut params: Vec<(&str, &str)> = vec![("food_id", &food_id)];

        let flag_val;
        if args
            .get("flag_default_serving")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            flag_val = "true".to_string();
            params.push(("flag_default_serving", &flag_val));
        }

        let include_attrs_val;
        if args
            .get("include_food_attributes")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            include_attrs_val = "true".to_string();
            params.push(("include_food_attributes", &include_attrs_val));
        }

        let body = self.oauth2_get("/food/v5", &params).await?;
        let shaped = shape_food_detail(&body);
        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&shaped)?,
            error: None,
        })
    }

    async fn recipe_search(&self, args: &Value) -> anyhow::Result<ToolResult> {
        let search_expr = args["search_expression"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let page = args["page_number"].as_u64().unwrap_or(0).to_string();
        let max = args["max_results"].as_u64().unwrap_or(20).to_string();

        let mut params: Vec<(&str, &str)> = vec![
            ("search_expression", &search_expr),
            ("page_number", &page),
            ("max_results", &max),
        ];

        let recipe_types_val;
        if let Some(rt) = args.get("recipe_types").and_then(Value::as_str) {
            recipe_types_val = rt.to_string();
            params.push(("recipe_types", &recipe_types_val));
        }

        let cal_from_val;
        if let Some(v) = args.get("calories_from").and_then(Value::as_u64) {
            cal_from_val = v.to_string();
            params.push(("calories.from", &cal_from_val));
        }

        let cal_to_val;
        if let Some(v) = args.get("calories_to").and_then(Value::as_u64) {
            cal_to_val = v.to_string();
            params.push(("calories.to", &cal_to_val));
        }

        let prep_from_val;
        if let Some(v) = args.get("prep_time_from").and_then(Value::as_u64) {
            prep_from_val = v.to_string();
            params.push(("prep_time.from", &prep_from_val));
        }

        let prep_to_val;
        if let Some(v) = args.get("prep_time_to").and_then(Value::as_u64) {
            prep_to_val = v.to_string();
            params.push(("prep_time.to", &prep_to_val));
        }

        let sort_val;
        if let Some(s) = args.get("sort_by").and_then(Value::as_str) {
            sort_val = s.to_string();
            params.push(("sort_by", &sort_val));
        }

        let body = self.oauth2_get("/recipes/search/v3", &params).await?;
        let shaped = shape_recipe_search(&body);
        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&shaped)?,
            error: None,
        })
    }

    async fn recipe_get(&self, args: &Value) -> anyhow::Result<ToolResult> {
        let recipe_id = require_str(args, "recipe_id")?;
        validate_numeric_id(&recipe_id, "recipe_id")?;

        let body = self
            .oauth2_get("/recipe/v2", &[("recipe_id", &recipe_id)])
            .await?;
        let shaped = shape_recipe_detail(&body);
        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&shaped)?,
            error: None,
        })
    }

    async fn diary_get(&self, args: &Value) -> anyhow::Result<ToolResult> {
        if !self.has_diary_credentials() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(
                    "diary operations require auth_token and auth_secret in [nutrition] config. \
                     Obtain them via FatSecret's profile.create API call."
                        .into(),
                ),
            });
        }

        let mut params: Vec<(&str, String)> = Vec::new();

        if let Some(date_str) = args.get("date").and_then(Value::as_str) {
            let days = date_to_days_since_epoch(date_str)?;
            params.push(("date", days.to_string()));
        }

        if let Some(entry_id) = args.get("food_entry_id").and_then(Value::as_str) {
            validate_numeric_id(entry_id, "food_entry_id")?;
            params.push(("food_entry_id", entry_id.to_string()));
        } else if let Some(entry_id) = args.get("food_entry_id").and_then(Value::as_u64) {
            params.push(("food_entry_id", entry_id.to_string()));
        }

        let body = self
            .oauth1_request("GET", "/food-entries/v2", &params)
            .await?;
        let shaped = shape_diary_entries(&body);
        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&shaped)?,
            error: None,
        })
    }

    async fn diary_create(&self, args: &Value) -> anyhow::Result<ToolResult> {
        if !self.has_diary_credentials() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(
                    "diary operations require auth_token and auth_secret in [nutrition] config."
                        .into(),
                ),
            });
        }

        let food_id = require_str(args, "food_id")?;
        validate_numeric_id(&food_id, "food_id")?;

        let food_entry_name = require_str(args, "food_entry_name")?;

        let serving_id = require_str(args, "serving_id")?;
        validate_numeric_id(&serving_id, "serving_id")?;
        if serving_id == "0" {
            anyhow::bail!(
                "serving_id=0 is a derived serving and cannot be used for diary entries. \
                 Use a specific serving_id from the food's servings list."
            );
        }

        let number_of_units = require_number_str(args, "number_of_units")?;
        let meal = require_str(args, "meal")?;
        validate_meal(&meal)?;

        let mut params: Vec<(&str, String)> = vec![
            ("food_id", food_id),
            ("food_entry_name", food_entry_name),
            ("serving_id", serving_id),
            ("number_of_units", number_of_units),
            ("meal", meal),
        ];

        if let Some(date_str) = args.get("date").and_then(Value::as_str) {
            let days = date_to_days_since_epoch(date_str)?;
            params.push(("date", days.to_string()));
        }

        let body = self
            .oauth1_request("POST", "/food-entries/v1", &params)
            .await?;
        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&body)?,
            error: None,
        })
    }

    async fn diary_edit(&self, args: &Value) -> anyhow::Result<ToolResult> {
        if !self.has_diary_credentials() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(
                    "diary operations require auth_token and auth_secret in [nutrition] config."
                        .into(),
                ),
            });
        }

        let food_entry_id = require_str(args, "food_entry_id")?;
        validate_numeric_id(&food_entry_id, "food_entry_id")?;

        let mut params: Vec<(&str, String)> = vec![("food_entry_id", food_entry_id)];

        if let Some(name) = args.get("food_entry_name").and_then(Value::as_str) {
            params.push(("food_entry_name", name.to_string()));
        }
        if let Some(sid) = args.get("serving_id").and_then(Value::as_str) {
            validate_numeric_id(sid, "serving_id")?;
            if sid == "0" {
                anyhow::bail!("serving_id=0 (derived serving) cannot be used for diary entries");
            }
            params.push(("serving_id", sid.to_string()));
        } else if let Some(sid) = args.get("serving_id").and_then(Value::as_u64) {
            if sid == 0 {
                anyhow::bail!("serving_id=0 (derived serving) cannot be used for diary entries");
            }
            params.push(("serving_id", sid.to_string()));
        }
        if let Some(n) = args.get("number_of_units").and_then(|v| {
            v.as_f64()
                .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        }) {
            params.push(("number_of_units", n.to_string()));
        }
        if let Some(m) = args.get("meal").and_then(Value::as_str) {
            validate_meal(m)?;
            params.push(("meal", m.to_string()));
        }

        let body = self
            .oauth1_request("PUT", "/food-entries/v1", &params)
            .await?;
        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&body)?,
            error: None,
        })
    }

    async fn diary_delete(&self, args: &Value) -> anyhow::Result<ToolResult> {
        if !self.has_diary_credentials() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(
                    "diary operations require auth_token and auth_secret in [nutrition] config."
                        .into(),
                ),
            });
        }

        let food_entry_id = require_str(args, "food_entry_id")?;
        validate_numeric_id(&food_entry_id, "food_entry_id")?;

        let params: Vec<(&str, String)> = vec![("food_entry_id", food_entry_id)];

        let body = self
            .oauth1_request("DELETE", "/food-entries/v1", &params)
            .await?;
        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&body)?,
            error: None,
        })
    }
}

// ── Tool trait impl ─────────────────────────────────────────────

#[async_trait]
impl Tool for NutritionTool {
    fn name(&self) -> &str {
        "nutrition"
    }

    fn description(&self) -> &str {
        "Search foods, get nutritional data, browse recipes, and manage a food diary \
         via the FatSecret API."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["action"],
            "properties": {
                "action": {
                    "type": "string",
                    "enum": [
                        "item.search", "item.get",
                        "recipe.search", "recipe.get",
                        "diary.get", "diary.create", "diary.edit", "diary.delete"
                    ],
                    "description": "The action to perform."
                },
                "search_expression": {
                    "type": "string",
                    "description": "Search term for item.search and recipe.search."
                },
                "food_id": {
                    "type": "string",
                    "description": "Numeric food ID for item.get and diary.create."
                },
                "recipe_id": {
                    "type": "string",
                    "description": "Numeric recipe ID for recipe.get."
                },
                "food_entry_id": {
                    "type": "string",
                    "description": "Numeric food entry ID for diary.get (optional), diary.edit, diary.delete."
                },
                "serving_id": {
                    "type": "string",
                    "description": "Numeric serving ID for diary.create (required) and diary.edit. Must not be 0."
                },
                "number_of_units": {
                    "type": "number",
                    "description": "Number of serving units for diary.create (required) and diary.edit."
                },
                "meal": {
                    "type": "string",
                    "enum": ["breakfast", "lunch", "dinner", "other"],
                    "description": "Meal type for diary.create (required) and diary.edit."
                },
                "food_entry_name": {
                    "type": "string",
                    "description": "Display name for diary.create (required) and diary.edit."
                },
                "date": {
                    "type": "string",
                    "description": "Date in YYYY-MM-DD format for diary.get and diary.create. Defaults to today."
                },
                "page_number": {
                    "type": "integer",
                    "description": "Zero-based page offset for search actions. Default: 0."
                },
                "max_results": {
                    "type": "integer",
                    "description": "Max results per page (1-50) for search actions. Default: 20."
                },
                "generic_description": {
                    "type": "string",
                    "enum": ["weight", "portion"],
                    "description": "Controls nutritional summary display in item.search results."
                },
                "flag_default_serving": {
                    "type": "boolean",
                    "description": "Mark the default serving in item.get results."
                },
                "include_food_attributes": {
                    "type": "boolean",
                    "description": "Include allergen and dietary preference data in item.get results."
                },
                "recipe_types": {
                    "type": "string",
                    "description": "Comma-separated recipe type names to filter recipe.search."
                },
                "calories_from": {
                    "type": "integer",
                    "description": "Min calories per serving for recipe.search."
                },
                "calories_to": {
                    "type": "integer",
                    "description": "Max calories per serving for recipe.search."
                },
                "prep_time_from": {
                    "type": "integer",
                    "description": "Min prep time in minutes for recipe.search."
                },
                "prep_time_to": {
                    "type": "integer",
                    "description": "Max prep time in minutes for recipe.search."
                },
                "sort_by": {
                    "type": "string",
                    "enum": ["newest", "oldest", "caloriesPerServingAscending", "caloriesPerServingDescending"],
                    "description": "Sort order for recipe.search."
                }
            }
        })
    }

    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        let action = args["action"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("missing required parameter: action"))?;

        if !self.is_action_allowed(action) {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "action '{action}' is not in the allowed_actions list. \
                     Update [nutrition].allowed_actions in config to enable it."
                )),
            });
        }

        // Rate limit check.
        if self.security.is_rate_limited() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Rate limit exceeded for nutrition tool.".into()),
            });
        }
        if !self.security.record_action() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Action budget exhausted.".into()),
            });
        }

        // Determine operation type.
        let op = match action {
            "diary.create" | "diary.edit" | "diary.delete" => ToolOperation::Act,
            _ => ToolOperation::Read,
        };
        if let Err(error) = self.security.enforce_tool_operation(op, "nutrition") {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(error),
            });
        }

        let result = match action {
            "item.search" => self.item_search(&args).await,
            "item.get" => self.item_get(&args).await,
            "recipe.search" => self.recipe_search(&args).await,
            "recipe.get" => self.recipe_get(&args).await,
            "diary.get" => self.diary_get(&args).await,
            "diary.create" => self.diary_create(&args).await,
            "diary.edit" => self.diary_edit(&args).await,
            "diary.delete" => self.diary_delete(&args).await,
            _ => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("unknown action: {action}")),
                })
            }
        };

        result.or_else(|e| {
            Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(truncate(&e.to_string(), MAX_ERROR_BODY_CHARS)),
            })
        })
    }
}

// ── OAuth1 helpers ──────────────────────────────────────────────

/// Build the OAuth1 HMAC-SHA1 signature.
fn compute_oauth1_signature(
    method: &str,
    url: &str,
    params: &BTreeMap<String, String>,
    consumer_secret: &str,
    token_secret: &str,
) -> String {
    // 1. Normalize parameters: sort by key then value, join with &.
    let normalized: String = params
        .iter()
        .map(|(k, v)| format!("{}={}", url_encode(k), url_encode(v)))
        .collect::<Vec<_>>()
        .join("&");

    // 2. Build base string: METHOD&url_encode(URL)&url_encode(normalized_params).
    let base_string = format!(
        "{}&{}&{}",
        method.to_uppercase(),
        url_encode(url),
        url_encode(&normalized)
    );

    // 3. Signing key: url_encode(consumer_secret)&url_encode(token_secret).
    let signing_key = format!(
        "{}&{}",
        url_encode(consumer_secret),
        url_encode(token_secret)
    );

    // 4. HMAC-SHA1.
    let mut mac =
        HmacSha1::new_from_slice(signing_key.as_bytes()).expect("HMAC accepts any key length");
    mac.update(base_string.as_bytes());
    let result = mac.finalize().into_bytes();

    base64::engine::general_purpose::STANDARD.encode(result)
}

/// Build an OAuth1 Authorization header from the parameter map.
fn build_oauth1_auth_header(params: &BTreeMap<String, String>) -> String {
    let oauth_parts: Vec<String> = params
        .iter()
        .filter(|(k, _)| k.starts_with("oauth_"))
        .map(|(k, v)| format!("{}=\"{}\"", url_encode(k), url_encode(v)))
        .collect();
    format!("OAuth {}", oauth_parts.join(", "))
}

// ── Response parsing & shaping ──────────────────────────────────

async fn parse_response(resp: reqwest::Response) -> anyhow::Result<Value> {
    let status = resp.status();
    let text = resp.text().await?;

    if !status.is_success() {
        let truncated = truncate(&text, MAX_ERROR_BODY_CHARS);
        anyhow::bail!("FatSecret API error ({status}): {truncated}");
    }

    serde_json::from_str(&text)
        .map_err(|e| anyhow::anyhow!("failed to parse FatSecret response: {e}"))
}

fn shape_food_search(body: &Value) -> Value {
    let foods = &body["foods"];
    let total = foods["total_results"].as_u64().unwrap_or(0);
    let page = foods["page_number"].as_u64().unwrap_or(0);
    let max = foods["max_results"].as_u64().unwrap_or(0);

    // Handle single-item vs array quirk in v1.
    let items = match &foods["food"] {
        Value::Array(arr) => arr.clone(),
        Value::Object(_) => vec![foods["food"].clone()],
        _ => vec![],
    };

    let shaped_items: Vec<Value> = items
        .iter()
        .map(|f| {
            json!({
                "food_id": f["food_id"],
                "food_name": f["food_name"],
                "food_type": f["food_type"],
                "brand_name": f.get("brand_name").unwrap_or(&Value::Null),
                "food_description": f["food_description"],
                "food_url": f["food_url"],
            })
        })
        .collect();

    json!({
        "total_results": total,
        "page_number": page,
        "max_results": max,
        "foods": shaped_items,
    })
}

fn shape_food_detail(body: &Value) -> Value {
    let food = &body["food"];
    let servings_raw = &food["servings"]["serving"];

    // Handle single-item vs array.
    let servings = match servings_raw {
        Value::Array(arr) => arr.clone(),
        Value::Object(_) => vec![servings_raw.clone()],
        _ => vec![],
    };

    let shaped_servings: Vec<Value> = servings
        .iter()
        .map(|s| {
            json!({
                "serving_id": s["serving_id"],
                "serving_description": s["serving_description"],
                "metric_serving_amount": s.get("metric_serving_amount"),
                "metric_serving_unit": s.get("metric_serving_unit"),
                "calories": s["calories"],
                "fat": s["fat"],
                "saturated_fat": s.get("saturated_fat"),
                "carbohydrate": s["carbohydrate"],
                "protein": s["protein"],
                "fiber": s.get("fiber"),
                "sugar": s.get("sugar"),
                "sodium": s.get("sodium"),
                "cholesterol": s.get("cholesterol"),
                "potassium": s.get("potassium"),
                "is_default": s.get("is_default"),
            })
        })
        .collect();

    let mut result = json!({
        "food_id": food["food_id"],
        "food_name": food["food_name"],
        "food_type": food["food_type"],
        "brand_name": food.get("brand_name").unwrap_or(&Value::Null),
        "food_url": food["food_url"],
        "servings": shaped_servings,
    });

    if let Some(attrs) = food.get("food_attributes") {
        result["food_attributes"] = attrs.clone();
    }

    result
}

fn shape_recipe_search(body: &Value) -> Value {
    let recipes = &body["recipes"];
    let total = recipes["total_results"].as_u64().unwrap_or(0);
    let page = recipes["page_number"].as_u64().unwrap_or(0);
    let max = recipes["max_results"].as_u64().unwrap_or(0);

    let items = match &recipes["recipe"] {
        Value::Array(arr) => arr.clone(),
        Value::Object(_) => vec![recipes["recipe"].clone()],
        _ => vec![],
    };

    let shaped: Vec<Value> = items
        .iter()
        .map(|r| {
            json!({
                "recipe_id": r["recipe_id"],
                "recipe_name": r["recipe_name"],
                "recipe_description": r["recipe_description"],
                "recipe_image": r.get("recipe_image"),
                "recipe_nutrition": r.get("recipe_nutrition"),
                "recipe_types": r.get("recipe_types"),
            })
        })
        .collect();

    json!({
        "total_results": total,
        "page_number": page,
        "max_results": max,
        "recipes": shaped,
    })
}

fn shape_recipe_detail(body: &Value) -> Value {
    let r = &body["recipe"];
    json!({
        "recipe_id": r["recipe_id"],
        "recipe_name": r["recipe_name"],
        "recipe_description": r["recipe_description"],
        "recipe_url": r["recipe_url"],
        "number_of_servings": r["number_of_servings"],
        "preparation_time_min": r.get("preparation_time_min"),
        "cooking_time_min": r.get("cooking_time_min"),
        "rating": r.get("rating"),
        "recipe_types": r.get("recipe_types"),
        "serving_sizes": r.get("serving_sizes"),
        "ingredients": r.get("ingredients"),
        "directions": r.get("directions"),
        "recipe_images": r.get("recipe_images"),
    })
}

fn shape_diary_entries(body: &Value) -> Value {
    let entries_raw = &body["food_entries"]["food_entry"];

    let entries = match entries_raw {
        Value::Array(arr) => arr.clone(),
        Value::Object(_) => vec![entries_raw.clone()],
        _ => vec![],
    };

    let shaped: Vec<Value> = entries
        .iter()
        .map(|e| {
            json!({
                "food_entry_id": e["food_entry_id"],
                "food_id": e["food_id"],
                "food_entry_name": e["food_entry_name"],
                "food_entry_description": e.get("food_entry_description"),
                "serving_id": e["serving_id"],
                "number_of_units": e["number_of_units"],
                "meal": e["meal"],
                "date_int": e["date_int"],
                "calories": e.get("calories"),
                "carbohydrate": e.get("carbohydrate"),
                "protein": e.get("protein"),
                "fat": e.get("fat"),
            })
        })
        .collect();

    json!({ "entries": shaped })
}

// ── Validation helpers ──────────────────────────────────────────

fn require_str(args: &Value, field: &str) -> anyhow::Result<String> {
    // Accept both string and number types for ID fields.
    if let Some(s) = args.get(field).and_then(Value::as_str) {
        if s.trim().is_empty() {
            anyhow::bail!("required parameter '{field}' must not be empty");
        }
        return Ok(s.to_string());
    }
    if let Some(n) = args.get(field).and_then(Value::as_u64) {
        return Ok(n.to_string());
    }
    anyhow::bail!("missing required parameter: {field}")
}

fn require_number_str(args: &Value, field: &str) -> anyhow::Result<String> {
    if let Some(n) = args.get(field).and_then(Value::as_f64) {
        return Ok(n.to_string());
    }
    if let Some(s) = args.get(field).and_then(Value::as_str) {
        s.parse::<f64>()
            .map_err(|_| anyhow::anyhow!("'{field}' must be a number"))?;
        return Ok(s.to_string());
    }
    anyhow::bail!("missing required parameter: {field}")
}

fn validate_numeric_id(id: &str, field_name: &str) -> anyhow::Result<()> {
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_digit()) {
        anyhow::bail!("'{field_name}' must be a numeric ID, got: {id}");
    }
    Ok(())
}

fn validate_meal(meal: &str) -> anyhow::Result<()> {
    match meal {
        "breakfast" | "lunch" | "dinner" | "other" => Ok(()),
        _ => anyhow::bail!(
            "invalid meal type: '{meal}'. Must be one of: breakfast, lunch, dinner, other"
        ),
    }
}

/// Convert a YYYY-MM-DD date string to days since Unix epoch.
fn date_to_days_since_epoch(date_str: &str) -> anyhow::Result<i64> {
    let parts: Vec<&str> = date_str.split('-').collect();
    if parts.len() != 3 {
        anyhow::bail!("invalid date format: '{date_str}'. Expected YYYY-MM-DD");
    }
    let year: i32 = parts[0]
        .parse()
        .map_err(|_| anyhow::anyhow!("invalid year in date: {date_str}"))?;
    let month: u32 = parts[1]
        .parse()
        .map_err(|_| anyhow::anyhow!("invalid month in date: {date_str}"))?;
    let day: u32 = parts[2]
        .parse()
        .map_err(|_| anyhow::anyhow!("invalid day in date: {date_str}"))?;

    // Days from civil date: algorithm from Howard Hinnant.
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        anyhow::bail!("date out of range: {date_str}");
    }

    let (y, m) = if month <= 2 {
        (i64::from(year) - 1, i64::from(month) + 9)
    } else {
        (i64::from(year), i64::from(month) - 3)
    };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let doy = (153 * m + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;

    Ok(days)
}

fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs()
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max])
    }
}

// ── Tests ───────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oauth1_base_string_construction() {
        let mut params = BTreeMap::new();
        params.insert("format".to_string(), "json".to_string());
        params.insert("method".to_string(), "foods.search".to_string());
        params.insert("search_expression".to_string(), "chicken".to_string());
        params.insert(
            "oauth_consumer_key".to_string(),
            "test_consumer_key".to_string(),
        );
        params.insert("oauth_nonce".to_string(), "abc123".to_string());
        params.insert(
            "oauth_signature_method".to_string(),
            "HMAC-SHA1".to_string(),
        );
        params.insert("oauth_timestamp".to_string(), "1234567890".to_string());
        params.insert("oauth_version".to_string(), "1.0".to_string());

        let sig = compute_oauth1_signature(
            "GET",
            "https://platform.fatsecret.com/rest/server.api",
            &params,
            "consumer_secret",
            "",
        );

        // Verify it produces a non-empty base64 string.
        assert!(!sig.is_empty());
        // Base64-decoded should be exactly 20 bytes (SHA1 output).
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(&sig)
            .unwrap();
        assert_eq!(decoded.len(), 20);
    }

    #[test]
    fn oauth1_signature_deterministic() {
        let mut params = BTreeMap::new();
        params.insert("a".to_string(), "1".to_string());
        params.insert("oauth_consumer_key".to_string(), "mykey".to_string());
        params.insert("oauth_nonce".to_string(), "nonce1".to_string());
        params.insert(
            "oauth_signature_method".to_string(),
            "HMAC-SHA1".to_string(),
        );
        params.insert("oauth_timestamp".to_string(), "100".to_string());
        params.insert("oauth_version".to_string(), "1.0".to_string());

        let sig1 = compute_oauth1_signature(
            "GET",
            "https://example.com/api",
            &params,
            "secret",
            "token_secret",
        );
        let sig2 = compute_oauth1_signature(
            "GET",
            "https://example.com/api",
            &params,
            "secret",
            "token_secret",
        );
        assert_eq!(sig1, sig2);

        // Different token secret should yield different signature.
        let sig3 = compute_oauth1_signature(
            "GET",
            "https://example.com/api",
            &params,
            "secret",
            "other_secret",
        );
        assert_ne!(sig1, sig3);
    }

    #[test]
    fn auth_header_format() {
        let mut params = BTreeMap::new();
        params.insert("oauth_consumer_key".to_string(), "mykey".to_string());
        params.insert("oauth_nonce".to_string(), "nonce1".to_string());
        params.insert("format".to_string(), "json".to_string());

        let header = build_oauth1_auth_header(&params);
        assert!(header.starts_with("OAuth "));
        assert!(header.contains("oauth_consumer_key=\"mykey\""));
        assert!(header.contains("oauth_nonce=\"nonce1\""));
        // Non-oauth params should not be in the header.
        assert!(!header.contains("format"));
    }

    #[test]
    fn date_conversion() {
        // 1970-01-01 = day 0
        assert_eq!(date_to_days_since_epoch("1970-01-01").unwrap(), 0);
        // 1970-01-02 = day 1
        assert_eq!(date_to_days_since_epoch("1970-01-02").unwrap(), 1);
        // 2024-01-01 = 19723 (verified independently)
        assert_eq!(date_to_days_since_epoch("2024-01-01").unwrap(), 19723);
        // 2026-03-22 (today per system context)
        let days = date_to_days_since_epoch("2026-03-22").unwrap();
        assert!(days > 20000);
    }

    #[test]
    fn date_conversion_invalid() {
        assert!(date_to_days_since_epoch("not-a-date").is_err());
        assert!(date_to_days_since_epoch("2024/01/01").is_err());
        assert!(date_to_days_since_epoch("2024-13-01").is_err());
    }

    #[test]
    fn validate_meal_valid() {
        assert!(validate_meal("breakfast").is_ok());
        assert!(validate_meal("lunch").is_ok());
        assert!(validate_meal("dinner").is_ok());
        assert!(validate_meal("other").is_ok());
    }

    #[test]
    fn validate_meal_invalid() {
        assert!(validate_meal("snack").is_err());
        assert!(validate_meal("").is_err());
    }

    #[test]
    fn validate_numeric_id_valid() {
        assert!(validate_numeric_id("12345", "food_id").is_ok());
        assert!(validate_numeric_id("0", "id").is_ok());
    }

    #[test]
    fn validate_numeric_id_invalid() {
        assert!(validate_numeric_id("abc", "food_id").is_err());
        assert!(validate_numeric_id("12.3", "id").is_err());
        assert!(validate_numeric_id("", "id").is_err());
    }

    #[test]
    fn shape_food_search_array() {
        let body = json!({
            "foods": {
                "total_results": "2",
                "page_number": "0",
                "max_results": "20",
                "food": [
                    {
                        "food_id": "1",
                        "food_name": "Chicken Breast",
                        "food_type": "Generic",
                        "food_description": "Per 100g - Calories: 165kcal | Fat: 3.57g",
                        "food_url": "https://example.com/1"
                    },
                    {
                        "food_id": "2",
                        "food_name": "Chicken Wing",
                        "food_type": "Generic",
                        "food_description": "Per 100g - Calories: 203kcal | Fat: 12.8g",
                        "food_url": "https://example.com/2"
                    }
                ]
            }
        });

        let shaped = shape_food_search(&body);
        let foods = shaped["foods"].as_array().unwrap();
        assert_eq!(foods.len(), 2);
        assert_eq!(foods[0]["food_name"], "Chicken Breast");
    }

    #[test]
    fn shape_food_search_single_item() {
        let body = json!({
            "foods": {
                "total_results": "1",
                "page_number": "0",
                "max_results": "20",
                "food": {
                    "food_id": "1",
                    "food_name": "Rice",
                    "food_type": "Generic",
                    "food_description": "Per 100g - Calories: 130kcal",
                    "food_url": "https://example.com/1"
                }
            }
        });

        let shaped = shape_food_search(&body);
        let foods = shaped["foods"].as_array().unwrap();
        assert_eq!(foods.len(), 1);
        assert_eq!(foods[0]["food_name"], "Rice");
    }

    #[test]
    fn shape_food_detail_servings() {
        let body = json!({
            "food": {
                "food_id": "42",
                "food_name": "Banana",
                "food_type": "Generic",
                "food_url": "https://example.com/42",
                "servings": {
                    "serving": [
                        {
                            "serving_id": "1",
                            "serving_description": "1 medium",
                            "calories": "105",
                            "fat": "0.39",
                            "carbohydrate": "27",
                            "protein": "1.29",
                            "fiber": "3.1",
                            "sugar": "14.4"
                        }
                    ]
                }
            }
        });

        let shaped = shape_food_detail(&body);
        assert_eq!(shaped["food_id"], "42");
        let servings = shaped["servings"].as_array().unwrap();
        assert_eq!(servings.len(), 1);
        assert_eq!(servings[0]["calories"], "105");
    }

    #[test]
    fn action_gating() {
        let security = Arc::new(SecurityPolicy::default());
        let tool = NutritionTool::new(
            "id".into(),
            "secret".into(),
            None,
            None,
            vec!["item.search".into()],
            security,
            30,
        );
        assert!(tool.is_action_allowed("item.search"));
        assert!(!tool.is_action_allowed("item.get"));
        assert!(!tool.is_action_allowed("diary.create"));
    }

    #[test]
    fn diary_credentials_check() {
        let security = Arc::new(SecurityPolicy::default());

        let tool_no_creds = NutritionTool::new(
            "id".into(),
            "secret".into(),
            None,
            None,
            vec![],
            security.clone(),
            30,
        );
        assert!(!tool_no_creds.has_diary_credentials());

        let tool_with_creds = NutritionTool::new(
            "id".into(),
            "secret".into(),
            Some("token".into()),
            Some("secret".into()),
            vec![],
            security,
            30,
        );
        assert!(tool_with_creds.has_diary_credentials());
    }

    #[test]
    fn truncate_works() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("hello world", 5), "hello…");
    }
}
