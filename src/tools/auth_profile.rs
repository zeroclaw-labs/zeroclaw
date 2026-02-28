//! Tool for managing auth profiles (list, switch, refresh).
//!
//! Allows the agent to:
//! - List all configured auth profiles with expiry status
//! - Switch active profile for a provider
//! - Refresh OAuth tokens that are expired or expiring

use crate::auth::{normalize_provider, AuthService};
use crate::config::Config;
use crate::tools::{Tool, ToolResult};
use anyhow::Result;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::fmt::Write as _;
use std::sync::Arc;

pub struct ManageAuthProfileTool {
    config: Arc<Config>,
}

impl ManageAuthProfileTool {
    pub fn new(config: Arc<Config>) -> Self {
        Self { config }
    }

    fn auth_service(&self) -> AuthService {
        AuthService::from_config(&self.config)
    }

    async fn handle_list(&self, provider_filter: Option<&str>) -> Result<ToolResult> {
        let auth = self.auth_service();
        let data = auth.load_profiles().await?;

        let mut output = String::new();
        let _ = writeln!(output, "## Auth Profiles\n");

        let mut count = 0u32;
        for (id, profile) in &data.profiles {
            if let Some(filter) = provider_filter {
                let normalized = normalize_provider(filter).unwrap_or_else(|_| filter.to_string());
                if profile.provider != normalized {
                    continue;
                }
            }

            count += 1;
            let is_active = data
                .active_profiles
                .get(&profile.provider)
                .map_or(false, |active| active == id);

            let active_marker = if is_active { " [ACTIVE]" } else { "" };
            let _ = writeln!(
                output,
                "- **{}** ({}){active_marker}",
                profile.profile_name, profile.provider
            );

            if let Some(ref acct) = profile.account_id {
                let _ = writeln!(output, "  Account: {acct}");
            }

            let _ = writeln!(output, "  Type: {:?}", profile.kind);

            if let Some(ref ts) = profile.token_set {
                if let Some(expires) = ts.expires_at {
                    let now = chrono::Utc::now();
                    if expires < now {
                        let ago = now.signed_duration_since(expires);
                        let _ = writeln!(output, "  Token: EXPIRED ({}h ago)", ago.num_hours());
                    } else {
                        let left = expires.signed_duration_since(now);
                        let _ = writeln!(
                            output,
                            "  Token: valid (expires in {}h {}m)",
                            left.num_hours(),
                            left.num_minutes() % 60
                        );
                    }
                } else {
                    let _ = writeln!(output, "  Token: no expiry set");
                }
                let has_refresh = ts.refresh_token.is_some();
                let _ = writeln!(
                    output,
                    "  Refresh token: {}",
                    if has_refresh { "yes" } else { "no" }
                );
            } else if profile.token.is_some() {
                let _ = writeln!(output, "  Token: API key (no expiry)");
            }
        }

        if count == 0 {
            if provider_filter.is_some() {
                let _ = writeln!(output, "No profiles found for the specified provider.");
            } else {
                let _ = writeln!(output, "No auth profiles configured.");
            }
        } else {
            let _ = writeln!(output, "\nTotal: {count} profile(s)");
        }

        Ok(ToolResult {
            success: true,
            output,
            error: None,
        })
    }

    async fn handle_switch(&self, provider: &str, profile_name: &str) -> Result<ToolResult> {
        let auth = self.auth_service();
        let profile_id = auth.set_active_profile(provider, profile_name).await?;

        Ok(ToolResult {
            success: true,
            output: format!("Switched active profile for {provider} to: {profile_id}"),
            error: None,
        })
    }

    async fn handle_refresh(&self, provider: &str) -> Result<ToolResult> {
        let normalized = normalize_provider(provider)?;
        let auth = self.auth_service();

        let result = match normalized.as_str() {
            "openai-codex" => match auth.get_valid_openai_access_token(None).await {
                Ok(Some(_)) => "OpenAI Codex token refreshed successfully.".to_string(),
                Ok(None) => "No OpenAI Codex profile found to refresh.".to_string(),
                Err(e) => {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!("OpenAI token refresh failed: {e}")),
                    })
                }
            },
            "gemini" => match auth.get_valid_gemini_access_token(None).await {
                Ok(Some(_)) => "Gemini token refreshed successfully.".to_string(),
                Ok(None) => "No Gemini profile found to refresh.".to_string(),
                Err(e) => {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!("Gemini token refresh failed: {e}")),
                    })
                }
            },
            other => {
                // For non-OAuth providers, just verify the token exists
                match auth.get_provider_bearer_token(other, None).await {
                    Ok(Some(_)) => format!("Provider '{other}' uses API key auth (no refresh needed). Token is present."),
                    Ok(None) => format!("No profile found for provider '{other}'."),
                    Err(e) => return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!("Token check failed for '{other}': {e}")),
                    }),
                }
            }
        };

        Ok(ToolResult {
            success: true,
            output: result,
            error: None,
        })
    }
}

#[async_trait]
impl Tool for ManageAuthProfileTool {
    fn name(&self) -> &str {
        "manage_auth_profile"
    }

    fn description(&self) -> &str {
        "Manage auth profiles: list all profiles with token status, switch active profile \
         for a provider, or refresh expired OAuth tokens. Use when user asks about accounts, \
         tokens, or when you encounter expired/rate-limited credentials."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["list", "switch", "refresh"],
                    "description": "Action to perform: 'list' shows all profiles, 'switch' changes active profile, 'refresh' renews OAuth tokens"
                },
                "provider": {
                    "type": "string",
                    "description": "Provider name (e.g., 'gemini', 'openai-codex', 'anthropic'). Required for switch and refresh."
                },
                "profile": {
                    "type": "string",
                    "description": "Profile name to switch to (for 'switch' action). E.g., 'default', 'work', 'personal'."
                }
            },
            "required": ["action"]
        })
    }

    async fn execute(&self, args: Value) -> Result<ToolResult> {
        let action = args
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("list");

        let provider = args.get("provider").and_then(|v| v.as_str());

        let result = match action {
            "list" => self.handle_list(provider).await,
            "switch" => {
                let Some(provider) = provider else {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some("'provider' is required for switch action".into()),
                    });
                };
                let profile = args
                    .get("profile")
                    .and_then(|v| v.as_str())
                    .unwrap_or("default");
                self.handle_switch(provider, profile).await
            }
            "refresh" => {
                let Some(provider) = provider else {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some("'provider' is required for refresh action".into()),
                    });
                };
                self.handle_refresh(provider).await
            }
            other => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Unknown action '{other}'. Valid: list, switch, refresh"
                )),
            }),
        };

        match result {
            Ok(outcome) => Ok(outcome),
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(e.to_string()),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_manage_auth_profile_schema() {
        let tool = ManageAuthProfileTool::new(Arc::new(Config::default()));
        let schema = tool.parameters_schema();
        assert!(schema["properties"]["action"]["enum"].is_array());
        assert_eq!(tool.name(), "manage_auth_profile");
        assert!(tool.description().contains("auth profiles"));
    }

    #[tokio::test]
    async fn test_list_empty_profiles() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config = Config {
            workspace_dir: tmp.path().to_path_buf(),
            config_path: tmp.path().join("config.toml"),
            ..Config::default()
        };
        let tool = ManageAuthProfileTool::new(Arc::new(config));
        let result = tool.execute(json!({"action": "list"})).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("Auth Profiles"));
    }

    #[tokio::test]
    async fn test_switch_missing_provider() {
        let tool = ManageAuthProfileTool::new(Arc::new(Config::default()));
        let result = tool.execute(json!({"action": "switch"})).await.unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("provider"));
    }

    #[tokio::test]
    async fn test_refresh_missing_provider() {
        let tool = ManageAuthProfileTool::new(Arc::new(Config::default()));
        let result = tool.execute(json!({"action": "refresh"})).await.unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("provider"));
    }

    #[tokio::test]
    async fn test_unknown_action() {
        let tool = ManageAuthProfileTool::new(Arc::new(Config::default()));
        let result = tool.execute(json!({"action": "delete"})).await.unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("Unknown action"));
    }
}
