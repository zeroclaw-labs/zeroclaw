use super::traits::{Tool, ToolResult};
use async_trait::async_trait;
use serde_json::json;

/// Tool for providing progress updates and status reports to the user.
pub struct StatusUpdateTool;

impl StatusUpdateTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for StatusUpdateTool {
    fn name(&self) -> &str {
        "status_update"
    }

    fn description(&self) -> &str {
        "Provide a progress update or status report to the user during long-running tasks."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "message": {
                    "type": "string",
                    "description": "The status message to display to the user."
                },
                "phase": {
                    "type": "string",
                    "description": "Optional phase identifier (e.g., 'researching', 'auditing', 'applying').",
                    "enum": ["starting", "researching", "auditing", "applying", "verifying", "finished"]
                },
                "progress_percent": {
                    "type": "integer",
                    "description": "Optional progress percentage (0-100).",
                    "minimum": 0,
                    "maximum": 100
                }
            },
            "required": ["message"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let message = args.get("message").and_then(|v| v.as_str()).unwrap_or("");
        let phase = args.get("phase").and_then(|v| v.as_str());
        let progress = args.get("progress_percent").and_then(|v| v.as_u64());

        let mut output = String::new();
        
        // Print to stdout for immediate user feedback
        println!("\n\u{1f4e2} **Status Update**:");
        if let Some(p) = phase {
            print!("[{}] ", p.to_uppercase());
        }
        println!("{}", message);
        
        if let Some(pct) = progress {
            println!("Progress: {}%", pct);
        }
        println!();

        Ok(ToolResult {
            success: true,
            output: "Status update displayed to user.".to_string(),
            error: None,
        })
    }
}
