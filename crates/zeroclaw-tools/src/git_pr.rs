//! Pull-request authoring tool routed through a forge-backed channel.
//!
//! Exposes `open_pull_request` / `update_pull_request` from the [`Channel`]
//! trait as an agent-callable tool. Holds the same late-bound channel map as
//! [`ReactionTool`](super::reaction::ReactionTool); the git channel is the only
//! channel that supports the operations, and non-forge channels return a clear
//! unsupported error.

use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;
use zeroclaw_api::channel::{OpenPullRequest, UpdatePullRequest};
use zeroclaw_api::tool::{Tool, ToolOutput, ToolResult};
use zeroclaw_config::policy::{SecurityPolicy, ToolOperation};

use crate::reaction::ChannelMapHandle;

/// Agent-callable tool for opening and updating pull requests through the git
/// channel's configured forge provider.
pub struct GitPrTool {
    channels: ChannelMapHandle,
    security: Arc<SecurityPolicy>,
}

impl GitPrTool {
    /// Create the tool over the shared late-bound channel map.
    pub fn new(security: Arc<SecurityPolicy>, channels: ChannelMapHandle) -> Self {
        Self { channels, security }
    }

    fn resolve_channel(
        &self,
        name: &str,
    ) -> Result<Arc<dyn zeroclaw_api::channel::Channel>, String> {
        let map = self.channels.read();
        if map.is_empty() {
            return Err("No channels available yet (channels not initialized)".to_string());
        }
        match map.get(name) {
            Some(ch) => Ok(Arc::clone(ch)),
            None => {
                let available: Vec<String> = map.keys().cloned().collect();
                Err(format!(
                    "Channel '{name}' not found. Available channels: {}",
                    available.join(", ")
                ))
            }
        }
    }
}

#[async_trait]
impl Tool for GitPrTool {
    fn name(&self) -> &str {
        "git_pr"
    }

    fn description(&self) -> &str {
        "Open or update a pull request through the git forge channel. \
         action 'open' creates a PR (title, body, head, base, optional draft); \
         action 'update' edits an existing PR by number (mark ready with \
         draft=false, edit body/title, or close). Names the git channel by its \
         channel key (default 'git')."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["open", "update"],
                    "description": "Whether to open a new PR or update an existing one"
                },
                "channel": {
                    "type": "string",
                    "description": "Git channel key to route through (default 'git')"
                },
                "repo": {
                    "type": "string",
                    "description": "Target repository as 'owner/repo'"
                },
                "title": {
                    "type": "string",
                    "description": "PR title (required for open; optional for update)"
                },
                "body": {
                    "type": "string",
                    "description": "PR body/description"
                },
                "head": {
                    "type": "string",
                    "description": "Source branch, or 'owner:branch' for a cross-fork head (open only)"
                },
                "base": {
                    "type": "string",
                    "description": "Target branch to merge into (open only)"
                },
                "draft": {
                    "type": "boolean",
                    "description": "Open as draft; on update, draft=false marks a draft ready for review"
                },
                "number": {
                    "type": "integer",
                    "description": "PR number to update (required for update)"
                },
                "close": {
                    "type": "boolean",
                    "description": "Close/supersede the PR (update only)"
                }
            },
            "required": ["action", "repo"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        if let Err(error) = self
            .security
            .enforce_tool_operation(ToolOperation::Act, "git_pr")
        {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(error),
            });
        }

        let action = args.get("action").and_then(|v| v.as_str()).unwrap_or("");
        let channel_name = args
            .get("channel")
            .and_then(|v| v.as_str())
            .unwrap_or("git");
        let repo = match args.get("repo").and_then(|v| v.as_str()) {
            Some(r) if !r.trim().is_empty() => r.to_string(),
            _ => {
                return Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some("Missing 'repo' parameter (expected 'owner/repo')".to_string()),
                });
            }
        };

        let channel = match self.resolve_channel(channel_name) {
            Ok(ch) => ch,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some(e),
                });
            }
        };

        match action {
            "open" => {
                let title = args
                    .get("title")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                let head = args
                    .get("head")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                let base = args
                    .get("base")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                if title.is_empty() || head.is_empty() || base.is_empty() {
                    return Ok(ToolResult {
                        success: false,
                        output: ToolOutput::default(),
                        error: Some("'open' requires 'title', 'head', and 'base'".to_string()),
                    });
                }
                let request = OpenPullRequest {
                    repo,
                    title,
                    body: args
                        .get("body")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string(),
                    head: head.to_string(),
                    base: base.to_string(),
                    draft: args.get("draft").and_then(|v| v.as_bool()).unwrap_or(false),
                };
                match channel.open_pull_request(request).await {
                    Ok(pr) => Ok(ToolResult {
                        success: true,
                        output: format!("Opened PR #{}: {}", pr.number, pr.url).into(),
                        error: None,
                    }),
                    Err(e) => Ok(ToolResult {
                        success: false,
                        output: ToolOutput::default(),
                        error: Some(format!("Failed to open pull request: {e}")),
                    }),
                }
            }
            "update" => {
                let number = match args.get("number").and_then(serde_json::Value::as_u64) {
                    Some(n) => n,
                    None => {
                        return Ok(ToolResult {
                            success: false,
                            output: ToolOutput::default(),
                            error: Some("'update' requires 'number'".to_string()),
                        });
                    }
                };
                let request = UpdatePullRequest {
                    repo,
                    number,
                    title: args
                        .get("title")
                        .and_then(|v| v.as_str())
                        .map(ToString::to_string),
                    body: args
                        .get("body")
                        .and_then(|v| v.as_str())
                        .map(ToString::to_string),
                    draft: args.get("draft").and_then(serde_json::Value::as_bool),
                    close: args
                        .get("close")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false),
                };
                match channel.update_pull_request(request).await {
                    Ok(()) => Ok(ToolResult {
                        success: true,
                        output: format!("Updated PR #{number}").into(),
                        error: None,
                    }),
                    Err(e) => Ok(ToolResult {
                        success: false,
                        output: ToolOutput::default(),
                        error: Some(format!("Failed to update pull request: {e}")),
                    }),
                }
            }
            other => Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(format!(
                    "Invalid action '{other}': must be 'open' or 'update'"
                )),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use parking_lot::{Mutex, RwLock};
    use std::collections::HashMap;
    use zeroclaw_api::channel::{
        Channel, ChannelMessage, OpenPullRequest, PullRequestRef, SendMessage, UpdatePullRequest,
    };

    struct ForgeMock {
        opened: Mutex<Option<OpenPullRequest>>,
        updated: Mutex<Option<UpdatePullRequest>>,
    }

    impl ForgeMock {
        fn new() -> Self {
            Self {
                opened: Mutex::new(None),
                updated: Mutex::new(None),
            }
        }
    }

    impl ::zeroclaw_api::attribution::Attributable for ForgeMock {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Channel(
                ::zeroclaw_api::attribution::ChannelKind::Git,
            )
        }
        fn alias(&self) -> &str {
            "test"
        }
    }

    #[async_trait]
    impl Channel for ForgeMock {
        fn name(&self) -> &str {
            "git"
        }
        async fn send(&self, _message: &SendMessage) -> anyhow::Result<()> {
            Ok(())
        }
        async fn listen(
            &self,
            _tx: tokio::sync::mpsc::Sender<ChannelMessage>,
        ) -> anyhow::Result<()> {
            Ok(())
        }
        async fn open_pull_request(
            &self,
            request: OpenPullRequest,
        ) -> anyhow::Result<PullRequestRef> {
            *self.opened.lock() = Some(request);
            Ok(PullRequestRef {
                number: 7,
                url: "https://forge.example/octo/repo/pulls/7".to_string(),
            })
        }
        async fn update_pull_request(&self, request: UpdatePullRequest) -> anyhow::Result<()> {
            *self.updated.lock() = Some(request);
            Ok(())
        }
    }

    fn tool_with(channel: Arc<dyn Channel>) -> GitPrTool {
        let handle = Arc::new(RwLock::new(HashMap::new()));
        handle.write().insert("git".to_string(), channel);
        GitPrTool::new(Arc::new(SecurityPolicy::default()), handle)
    }

    #[tokio::test]
    async fn open_routes_to_forge_and_returns_url() {
        let mock = Arc::new(ForgeMock::new());
        let tool = tool_with(Arc::clone(&mock) as Arc<dyn Channel>);
        let result = tool
            .execute(json!({
                "action": "open",
                "repo": "octo/repo",
                "title": "Add feature",
                "body": "Details",
                "head": "feat/x",
                "base": "main",
                "draft": true
            }))
            .await
            .unwrap();
        assert!(result.success, "error: {:?}", result.error);
        assert!(result.output.as_str().contains("PR #7"));
        let opened = mock.opened.lock().clone().unwrap();
        assert_eq!(opened.repo, "octo/repo");
        assert_eq!(opened.head, "feat/x");
        assert_eq!(opened.base, "main");
        assert!(opened.draft);
    }

    #[tokio::test]
    async fn update_marks_ready() {
        let mock = Arc::new(ForgeMock::new());
        let tool = tool_with(Arc::clone(&mock) as Arc<dyn Channel>);
        let result = tool
            .execute(json!({
                "action": "update",
                "repo": "octo/repo",
                "number": 7,
                "draft": false
            }))
            .await
            .unwrap();
        assert!(result.success, "error: {:?}", result.error);
        let updated = mock.updated.lock().clone().unwrap();
        assert_eq!(updated.number, 7);
        assert_eq!(updated.draft, Some(false));
    }

    #[tokio::test]
    async fn open_requires_head_and_base() {
        let mock = Arc::new(ForgeMock::new());
        let tool = tool_with(Arc::clone(&mock) as Arc<dyn Channel>);
        let result = tool
            .execute(json!({
                "action": "open",
                "repo": "octo/repo",
                "title": "x"
            }))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("head"));
    }

    #[tokio::test]
    async fn unknown_channel_errors() {
        let tool = tool_with(Arc::new(ForgeMock::new()) as Arc<dyn Channel>);
        let result = tool
            .execute(json!({
                "action": "open",
                "channel": "nope",
                "repo": "octo/repo",
                "title": "x",
                "head": "a",
                "base": "b"
            }))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("not found"));
    }

    #[test]
    fn metadata() {
        let tool = GitPrTool::new(
            Arc::new(SecurityPolicy::default()),
            Arc::new(RwLock::new(HashMap::new())),
        );
        assert_eq!(tool.name(), "git_pr");
        let schema = tool.parameters_schema();
        assert_eq!(schema["type"], "object");
        let required = schema["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v == "action"));
        assert!(required.iter().any(|v| v == "repo"));
    }
}
