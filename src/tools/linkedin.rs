use super::linkedin_client::LinkedInClient;
use super::traits::{Tool, ToolResult};
use crate::security::SecurityPolicy;
use async_trait::async_trait;
use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;

pub struct LinkedInTool {
    security: Arc<SecurityPolicy>,
    workspace_dir: PathBuf,
}

impl LinkedInTool {
    pub fn new(security: Arc<SecurityPolicy>, workspace_dir: PathBuf) -> Self {
        Self {
            security,
            workspace_dir,
        }
    }

    fn is_write_action(action: &str) -> bool {
        matches!(action, "create_post" | "comment" | "react" | "delete_post")
    }
}

#[async_trait]
impl Tool for LinkedInTool {
    fn name(&self) -> &str {
        "linkedin"
    }

    fn description(&self) -> &str {
        "Manage LinkedIn: create posts, list your posts, comment, react, delete posts, view engagement, and get profile info. Requires LINKEDIN_* credentials in .env file."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": [
                        "create_post",
                        "list_posts",
                        "comment",
                        "react",
                        "delete_post",
                        "get_engagement",
                        "get_profile"
                    ],
                    "description": "The LinkedIn action to perform"
                },
                "text": {
                    "type": "string",
                    "description": "Post or comment text content"
                },
                "visibility": {
                    "type": "string",
                    "enum": ["PUBLIC", "CONNECTIONS"],
                    "description": "Post visibility (default: PUBLIC)"
                },
                "article_url": {
                    "type": "string",
                    "description": "URL for link preview in a post"
                },
                "article_title": {
                    "type": "string",
                    "description": "Title for the article (requires article_url)"
                },
                "post_id": {
                    "type": "string",
                    "description": "LinkedIn post URN identifier"
                },
                "reaction_type": {
                    "type": "string",
                    "enum": ["LIKE", "CELEBRATE", "SUPPORT", "LOVE", "INSIGHTFUL", "FUNNY"],
                    "description": "Type of reaction to add to a post"
                },
                "count": {
                    "type": "integer",
                    "description": "Number of posts to retrieve (default 10, max 50)"
                }
            },
            "required": ["action"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let action = args
            .get("action")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing required 'action' parameter"))?;

        // Write actions require autonomy check
        if Self::is_write_action(action) && !self.security.can_act() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Action blocked: autonomy is read-only".into()),
            });
        }

        // All actions are rate-limited
        if !self.security.record_action() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Action blocked: rate limit exceeded".into()),
            });
        }

        let client = LinkedInClient::new(self.workspace_dir.clone());

        match action {
            "create_post" => {
                let text = match args.get("text").and_then(|v| v.as_str()).map(str::trim) {
                    Some(t) if !t.is_empty() => t.to_string(),
                    _ => {
                        return Ok(ToolResult {
                            success: false,
                            output: String::new(),
                            error: Some("Missing required 'text' parameter for create_post".into()),
                        });
                    }
                };

                let visibility = args
                    .get("visibility")
                    .and_then(|v| v.as_str())
                    .unwrap_or("PUBLIC");

                let article_url = args.get("article_url").and_then(|v| v.as_str());
                let article_title = args.get("article_title").and_then(|v| v.as_str());

                if article_title.is_some() && article_url.is_none() {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some("'article_title' requires 'article_url' to be provided".into()),
                    });
                }

                let post_id = client
                    .create_post(&text, visibility, article_url, article_title)
                    .await?;

                Ok(ToolResult {
                    success: true,
                    output: format!("Post created successfully. Post ID: {post_id}"),
                    error: None,
                })
            }

            "list_posts" => {
                let count = args
                    .get("count")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(10)
                    .clamp(1, 50) as usize;

                let posts = client.list_posts(count).await?;

                Ok(ToolResult {
                    success: true,
                    output: serde_json::to_string(&posts)?,
                    error: None,
                })
            }

            "comment" => {
                let post_id = match args.get("post_id").and_then(|v| v.as_str()) {
                    Some(id) if !id.is_empty() => id,
                    _ => {
                        return Ok(ToolResult {
                            success: false,
                            output: String::new(),
                            error: Some("Missing required 'post_id' parameter for comment".into()),
                        });
                    }
                };

                let text = match args.get("text").and_then(|v| v.as_str()).map(str::trim) {
                    Some(t) if !t.is_empty() => t.to_string(),
                    _ => {
                        return Ok(ToolResult {
                            success: false,
                            output: String::new(),
                            error: Some("Missing required 'text' parameter for comment".into()),
                        });
                    }
                };

                let comment_id = client.add_comment(post_id, &text).await?;

                Ok(ToolResult {
                    success: true,
                    output: format!("Comment posted successfully. Comment ID: {comment_id}"),
                    error: None,
                })
            }

            "react" => {
                let post_id = match args.get("post_id").and_then(|v| v.as_str()) {
                    Some(id) if !id.is_empty() => id,
                    _ => {
                        return Ok(ToolResult {
                            success: false,
                            output: String::new(),
                            error: Some("Missing required 'post_id' parameter for react".into()),
                        });
                    }
                };

                let reaction_type = match args.get("reaction_type").and_then(|v| v.as_str()) {
                    Some(rt) if !rt.is_empty() => rt,
                    _ => {
                        return Ok(ToolResult {
                            success: false,
                            output: String::new(),
                            error: Some(
                                "Missing required 'reaction_type' parameter for react".into(),
                            ),
                        });
                    }
                };

                client.add_reaction(post_id, reaction_type).await?;

                Ok(ToolResult {
                    success: true,
                    output: format!("Reaction '{reaction_type}' added to post {post_id}"),
                    error: None,
                })
            }

            "delete_post" => {
                let post_id = match args.get("post_id").and_then(|v| v.as_str()) {
                    Some(id) if !id.is_empty() => id,
                    _ => {
                        return Ok(ToolResult {
                            success: false,
                            output: String::new(),
                            error: Some(
                                "Missing required 'post_id' parameter for delete_post".into(),
                            ),
                        });
                    }
                };

                client.delete_post(post_id).await?;

                Ok(ToolResult {
                    success: true,
                    output: format!("Post {post_id} deleted successfully"),
                    error: None,
                })
            }

            "get_engagement" => {
                let post_id = match args.get("post_id").and_then(|v| v.as_str()) {
                    Some(id) if !id.is_empty() => id,
                    _ => {
                        return Ok(ToolResult {
                            success: false,
                            output: String::new(),
                            error: Some(
                                "Missing required 'post_id' parameter for get_engagement".into(),
                            ),
                        });
                    }
                };

                let engagement = client.get_engagement(post_id).await?;

                Ok(ToolResult {
                    success: true,
                    output: serde_json::to_string(&engagement)?,
                    error: None,
                })
            }

            "get_profile" => {
                let profile = client.get_profile().await?;

                Ok(ToolResult {
                    success: true,
                    output: serde_json::to_string(&profile)?,
                    error: None,
                })
            }

            unknown => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Unknown action: '{unknown}'")),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::AutonomyLevel;

    fn test_security(level: AutonomyLevel, max_actions_per_hour: u32) -> Arc<SecurityPolicy> {
        Arc::new(SecurityPolicy {
            autonomy: level,
            max_actions_per_hour,
            workspace_dir: std::env::temp_dir(),
            ..SecurityPolicy::default()
        })
    }

    fn make_tool(level: AutonomyLevel, max_actions: u32) -> LinkedInTool {
        LinkedInTool::new(test_security(level, max_actions), PathBuf::from("/tmp"))
    }

    #[test]
    fn tool_name() {
        let tool = make_tool(AutonomyLevel::Full, 100);
        assert_eq!(tool.name(), "linkedin");
    }

    #[test]
    fn tool_description() {
        let tool = make_tool(AutonomyLevel::Full, 100);
        assert!(!tool.description().is_empty());
        assert!(tool.description().contains("LinkedIn"));
    }

    #[test]
    fn parameters_schema_has_required_action() {
        let tool = make_tool(AutonomyLevel::Full, 100);
        let schema = tool.parameters_schema();
        assert_eq!(schema["type"], "object");
        let required = schema["required"].as_array().unwrap();
        assert!(required.contains(&json!("action")));
    }

    #[test]
    fn parameters_schema_has_all_properties() {
        let tool = make_tool(AutonomyLevel::Full, 100);
        let schema = tool.parameters_schema();
        let props = &schema["properties"];
        assert!(props.get("action").is_some());
        assert!(props.get("text").is_some());
        assert!(props.get("visibility").is_some());
        assert!(props.get("article_url").is_some());
        assert!(props.get("article_title").is_some());
        assert!(props.get("post_id").is_some());
        assert!(props.get("reaction_type").is_some());
        assert!(props.get("count").is_some());
    }

    #[tokio::test]
    async fn write_actions_blocked_in_readonly_mode() {
        let tool = make_tool(AutonomyLevel::ReadOnly, 100);

        for action in &["create_post", "comment", "react", "delete_post"] {
            let result = tool
                .execute(json!({
                    "action": action,
                    "text": "hello",
                    "post_id": "urn:li:share:123",
                    "reaction_type": "LIKE"
                }))
                .await
                .unwrap();
            assert!(
                !result.success,
                "Action '{action}' should be blocked in read-only mode"
            );
            assert!(
                result.error.as_ref().unwrap().contains("read-only"),
                "Action '{action}' error should mention read-only"
            );
        }
    }

    #[tokio::test]
    async fn write_actions_blocked_by_rate_limit() {
        let tool = make_tool(AutonomyLevel::Full, 0);

        for action in &["create_post", "comment", "react", "delete_post"] {
            let result = tool
                .execute(json!({
                    "action": action,
                    "text": "hello",
                    "post_id": "urn:li:share:123",
                    "reaction_type": "LIKE"
                }))
                .await
                .unwrap();
            assert!(
                !result.success,
                "Action '{action}' should be blocked by rate limit"
            );
            assert!(
                result.error.as_ref().unwrap().contains("rate limit"),
                "Action '{action}' error should mention rate limit"
            );
        }
    }

    #[tokio::test]
    async fn read_actions_not_blocked_in_readonly_mode() {
        // Read actions skip can_act() but still go through record_action().
        // With rate limit > 0, they should pass security checks and only fail
        // at the client level (no .env file).
        let tool = make_tool(AutonomyLevel::ReadOnly, 100);

        for action in &["list_posts", "get_engagement", "get_profile"] {
            let result = tool
                .execute(json!({
                    "action": action,
                    "post_id": "urn:li:share:123"
                }))
                .await;
            // These will fail at the client level (no .env), but they should NOT
            // return a read-only security error.
            match result {
                Ok(r) => {
                    if !r.success {
                        assert!(
                            !r.error.as_ref().unwrap().contains("read-only"),
                            "Read action '{action}' should not be blocked by read-only mode"
                        );
                    }
                }
                Err(e) => {
                    // Client-level error (no .env) is expected and acceptable
                    let msg = e.to_string();
                    assert!(
                        !msg.contains("read-only"),
                        "Read action '{action}' should not be blocked by read-only mode"
                    );
                }
            }
        }
    }

    #[tokio::test]
    async fn read_actions_blocked_by_rate_limit() {
        let tool = make_tool(AutonomyLevel::ReadOnly, 0);

        for action in &["list_posts", "get_engagement", "get_profile"] {
            let result = tool
                .execute(json!({
                    "action": action,
                    "post_id": "urn:li:share:123"
                }))
                .await
                .unwrap();
            assert!(
                !result.success,
                "Read action '{action}' should be rate-limited"
            );
            assert!(
                result.error.as_ref().unwrap().contains("rate limit"),
                "Read action '{action}' error should mention rate limit"
            );
        }
    }

    #[tokio::test]
    async fn create_post_requires_text() {
        let tool = make_tool(AutonomyLevel::Full, 100);

        let result = tool
            .execute(json!({"action": "create_post"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.as_ref().unwrap().contains("text"));
    }

    #[tokio::test]
    async fn create_post_rejects_empty_text() {
        let tool = make_tool(AutonomyLevel::Full, 100);

        let result = tool
            .execute(json!({"action": "create_post", "text": "   "}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.as_ref().unwrap().contains("text"));
    }

    #[tokio::test]
    async fn article_title_without_url_rejected() {
        let tool = make_tool(AutonomyLevel::Full, 100);

        let result = tool
            .execute(json!({
                "action": "create_post",
                "text": "Hello world",
                "article_title": "My Article"
            }))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.as_ref().unwrap().contains("article_url"));
    }

    #[tokio::test]
    async fn comment_requires_post_id() {
        let tool = make_tool(AutonomyLevel::Full, 100);

        let result = tool
            .execute(json!({"action": "comment", "text": "Nice post!"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.as_ref().unwrap().contains("post_id"));
    }

    #[tokio::test]
    async fn comment_requires_text() {
        let tool = make_tool(AutonomyLevel::Full, 100);

        let result = tool
            .execute(json!({"action": "comment", "post_id": "urn:li:share:123"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.as_ref().unwrap().contains("text"));
    }

    #[tokio::test]
    async fn react_requires_post_id() {
        let tool = make_tool(AutonomyLevel::Full, 100);

        let result = tool
            .execute(json!({"action": "react", "reaction_type": "LIKE"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.as_ref().unwrap().contains("post_id"));
    }

    #[tokio::test]
    async fn react_requires_reaction_type() {
        let tool = make_tool(AutonomyLevel::Full, 100);

        let result = tool
            .execute(json!({"action": "react", "post_id": "urn:li:share:123"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.as_ref().unwrap().contains("reaction_type"));
    }

    #[tokio::test]
    async fn delete_post_requires_post_id() {
        let tool = make_tool(AutonomyLevel::Full, 100);

        let result = tool
            .execute(json!({"action": "delete_post"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.as_ref().unwrap().contains("post_id"));
    }

    #[tokio::test]
    async fn get_engagement_requires_post_id() {
        let tool = make_tool(AutonomyLevel::Full, 100);

        let result = tool
            .execute(json!({"action": "get_engagement"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.as_ref().unwrap().contains("post_id"));
    }

    #[tokio::test]
    async fn unknown_action_returns_error() {
        let tool = make_tool(AutonomyLevel::Full, 100);

        let result = tool
            .execute(json!({"action": "send_message"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.as_ref().unwrap().contains("Unknown action"));
        assert!(result.error.as_ref().unwrap().contains("send_message"));
    }
}
