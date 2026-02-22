use crate::clawhub::client::ClawHubClient;
use crate::tools::traits::{Tool, ToolResult};
use async_trait::async_trait;

/// Tool for searching ClawHub skills
pub struct ClawhubSearchTool;

impl ClawhubSearchTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for ClawhubSearchTool {
    fn name(&self) -> &str {
        "clawhub_search"
    }

    fn description(&self) -> &str {
        "Search for skills on ClawHub, the public skill registry for AI agents"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Search query for skills"
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of results",
                    "default": 10
                }
            },
            "required": ["query"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let query = args["query"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing query parameter"))?;

        let limit = args["limit"].as_u64().unwrap_or(10) as usize;

        let client = ClawHubClient::default();

        match client.search_skills(query, limit).await {
            Ok(skills) => {
                let mut output = format!("Found {} skills:\n\n", skills.len());

                for skill in skills {
                    output.push_str(&format!(
                        "- {} ({})\n  {}\n  Stars: {}\n\n",
                        skill.name, skill.slug, skill.description, skill.stars
                    ));
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
                error: Some(format!("Search failed: {}", e)),
            }),
        }
    }
}
