use super::linkedin_client::{ImageGenerator, LinkedInClient};
use super::traits::{Tool, ToolResult};
use crate::config::{LinkedInContentConfig, LinkedInImageConfig};
use crate::security::SecurityPolicy;
use async_trait::async_trait;
use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;

pub struct LinkedInTool {
    security: Arc<SecurityPolicy>,
    workspace_dir: PathBuf,
    api_version: String,
    content_config: LinkedInContentConfig,
    image_config: LinkedInImageConfig,
}

impl LinkedInTool {
    pub fn new(
        security: Arc<SecurityPolicy>,
        workspace_dir: PathBuf,
        api_version: String,
        content_config: LinkedInContentConfig,
        image_config: LinkedInImageConfig,
    ) -> Self {
        Self {
            security,
            workspace_dir,
            api_version,
            content_config,
            image_config,
        }
    }

    fn is_write_action(action: &str) -> bool {
        matches!(action, "create_post" | "comment" | "react" | "delete_post")
    }

    fn build_content_strategy_summary(&self) -> String {
        let c = &self.content_config;
        let mut parts = Vec::new();

        if !c.persona.is_empty() {
            parts.push(format!("## Persona\n{}", c.persona));
        }

        if !c.topics.is_empty() {
            parts.push(format!("## Topics\n{}", c.topics.join(", ")));
        }

        if !c.rss_feeds.is_empty() {
            let feeds: Vec<String> = c.rss_feeds.iter().map(|f| format!("- {f}")).collect();
            parts.push(format!(
                "## RSS Feeds (fetch titles only for inspiration)\n{}",
                feeds.join("\n")
            ));
        }

        if !c.github_users.is_empty() {
            parts.push(format!(
                "## GitHub Users (check public activity)\n{}",
                c.github_users.join(", ")
            ));
        }

        if !c.github_repos.is_empty() {
            let repos: Vec<String> = c.github_repos.iter().map(|r| format!("- {r}")).collect();
            parts.push(format!(
                "## GitHub Repos (highlight project work)\n{}",
                repos.join("\n")
            ));
        }

        if !c.instructions.is_empty() {
            parts.push(format!("## Posting Instructions\n{}", c.instructions));
        }

        if parts.is_empty() {
            return "No content strategy configured. Add [linkedin.content] settings to config.toml with rss_feeds, github_repos, persona, topics, and instructions.".to_string();
        }

        parts.join("\n\n")
    }
}

#[async_trait]
impl Tool for LinkedInTool {
    fn name(&self) -> &str {
        "linkedin"
    }

    fn description(&self) -> &str {
        "Manage LinkedIn: create posts, list your posts, comment, react, delete posts, view engagement, get profile info, and read the configured content strategy. Requires LINKEDIN_* credentials in .env file."
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
                        "get_profile",
                        "get_content_strategy"
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
                },
                "generate_image": {
                    "type": "boolean",
                    "description": "Generate an AI image for the post (requires [linkedin.image] config). Falls back to branded SVG card if all providers fail."
                },
                "image_prompt": {
                    "type": "string",
                    "description": "Custom prompt for image generation. If omitted, a prompt is derived from the post text."
                },
                "scheduled_at": {
                    "type": "string",
                    "description": "Schedule the post for future publication. ISO 8601 / RFC 3339 timestamp, e.g. '2026-03-17T08:00:00Z'. The post is saved as a draft with scheduledPublishTime on LinkedIn."
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

        let client = LinkedInClient::new(self.workspace_dir.clone(), self.api_version.clone());

        match action {
            "get_content_strategy" => {
                let strategy = self.build_content_strategy_summary();
                return Ok(ToolResult {
                    success: true,
                    output: strategy,
                    error: None,
                });
            }
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

                let generate_image = args
                    .get("generate_image")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);

                let article_url = args.get("article_url").and_then(|v| v.as_str());
                let article_title = args.get("article_title").and_then(|v| v.as_str());
                let scheduled_at = args.get("scheduled_at").and_then(|v| v.as_str());

                if article_title.is_some() && article_url.is_none() {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some("'article_title' requires 'article_url' to be provided".into()),
                    });
                }

                // Image generation flow
                if generate_image && self.image_config.enabled {
                    let image_prompt =
                        args.get("image_prompt")
                            .and_then(|v| v.as_str())
                            .map(String::from)
                            .unwrap_or_else(|| {
                                format!(
                                "Professional, modern illustration for a LinkedIn post about: {}",
                                if text.len() > 200 { &text[..200] } else { &text }
                            )
                            });

                    let generator =
                        ImageGenerator::new(self.image_config.clone(), self.workspace_dir.clone());

                    match generator.generate(&image_prompt).await {
                        Ok(image_path) => {
                            let image_bytes = tokio::fs::read(&image_path).await?;
                            let creds = client.get_credentials().await?;
                            let image_urn = client
                                .upload_image(&image_bytes, &creds.access_token, &creds.person_id)
                                .await?;

                            let post_id = client
                                .create_post_with_image(&text, visibility, &image_urn, scheduled_at)
                                .await?;

                            // Clean up temp file
                            let _ = ImageGenerator::cleanup(&image_path).await;

                            let action_word = if scheduled_at.is_some() { "scheduled" } else { "published" };
                            return Ok(ToolResult {
                                success: true,
                                output: format!(
                                    "Post {action_word} with image. Post ID: {post_id}, Image: {image_urn}"
                                ),
                                error: None,
                            });
                        }
                        Err(e) => {
                            // Image generation failed entirely — post without image
                            tracing::warn!("Image generation failed, posting without image: {e}");
                        }
                    }
                }

                let post_id = client
                    .create_post(&text, visibility, article_url, article_title, scheduled_at)
                    .await?;

                let action_word = if scheduled_at.is_some() { "scheduled" } else { "published" };
                Ok(ToolResult {
                    success: true,
                    output: format!("Post {action_word} successfully. Post ID: {post_id}"),
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
        LinkedInTool::new(
            test_security(level, max_actions),
            PathBuf::from("/tmp"),
            "202602".to_string(),
            LinkedInContentConfig::default(),
            LinkedInImageConfig::default(),
        )
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
        assert!(props.get("generate_image").is_some());
        assert!(props.get("image_prompt").is_some());
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

    #[tokio::test]
    async fn get_content_strategy_returns_config() {
        let content = LinkedInContentConfig {
            rss_feeds: vec!["https://medium.com/feed/tag/rust".into()],
            github_users: vec!["rareba".into()],
            github_repos: vec!["zeroclaw-labs/zeroclaw".into()],
            topics: vec!["cybersecurity".into(), "Rust".into()],
            persona: "Security engineer and Rust developer".into(),
            instructions: "Write concise posts with hashtags".into(),
        };
        let tool = LinkedInTool::new(
            test_security(AutonomyLevel::Full, 100),
            PathBuf::from("/tmp"),
            "202602".to_string(),
            content,
            LinkedInImageConfig::default(),
        );

        let result = tool
            .execute(json!({"action": "get_content_strategy"}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("Security engineer"));
        assert!(result.output.contains("cybersecurity"));
        assert!(result.output.contains("medium.com"));
        assert!(result.output.contains("zeroclaw-labs/zeroclaw"));
        assert!(result.output.contains("rareba"));
        assert!(result.output.contains("Write concise posts"));
    }

    #[tokio::test]
    async fn get_content_strategy_empty_config_shows_hint() {
        let tool = make_tool(AutonomyLevel::Full, 100);

        let result = tool
            .execute(json!({"action": "get_content_strategy"}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("No content strategy configured"));
    }

    #[tokio::test]
    async fn get_content_strategy_not_rate_limited_as_write() {
        // get_content_strategy is a read action and should work in read-only mode
        let tool = make_tool(AutonomyLevel::ReadOnly, 100);

        let result = tool
            .execute(json!({"action": "get_content_strategy"}))
            .await
            .unwrap();
        assert!(result.success);
    }

    #[test]
    fn parameters_schema_includes_get_content_strategy() {
        let tool = make_tool(AutonomyLevel::Full, 100);
        let schema = tool.parameters_schema();
        let actions = schema["properties"]["action"]["enum"].as_array().unwrap();
        assert!(actions.contains(&json!("get_content_strategy")));
    }
}
