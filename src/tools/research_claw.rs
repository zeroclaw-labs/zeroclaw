use async_trait::async_trait;
use serde_json::json;
use std::process::Command;

use super::traits::{Tool, ToolResult};

pub struct ResearchClawTool {
    claw_path: String,
    config_path: String,
}

impl ResearchClawTool {
    pub fn new(claw_path: Option<String>, config_path: Option<String>) -> Self {
        Self {
            claw_path: claw_path.unwrap_or_else(|| {
                std::env::var("RESEARCHCLAW_PATH").unwrap_or_else(|_| {
                    let workspace =
                        std::env::var("ZEROCLAW_WORKSPACE").unwrap_or_else(|_| ".".to_string());
                    let base = std::path::Path::new(&workspace);
                    base.join("AutoResearchClaw").to_string_lossy().into_owned()
                })
            }),
            config_path: config_path.unwrap_or_else(|| {
                std::env::var("RESEARCHCLAW_CONFIG")
                    .unwrap_or_else(|_| "config.researchclaw.example.yaml".to_string())
            }),
        }
    }
}

#[async_trait]
impl Tool for ResearchClawTool {
    fn name(&self) -> &str {
        "research_claw"
    }

    fn description(&self) -> &str {
        "Run AutoResearchClaw autonomous research pipeline. Given a topic, produces a conference-ready paper (LaTeX, BibTeX, experiments, charts) through 23 stages: literature discovery, hypothesis generation, experiment execution, paper writing, and peer review."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["run", "status", "doctor"],
                    "description": "Action to perform: 'run' starts research, 'status' checks a run, 'doctor' checks system health"
                },
                "topic": {
                    "type": "string",
                    "description": "Research topic or idea (required for 'run' action)"
                },
                "auto_approve": {
                    "type": "boolean",
                    "description": "Skip human-in-the-loop approval gates (default: true)",
                    "default": true
                },
                "run_id": {
                    "type": "string",
                    "description": "Run ID for status checks (optional, uses latest if not provided)"
                }
            },
            "required": ["action"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let action = args.get("action").and_then(|v| v.as_str()).unwrap_or("run");

        match action {
            "run" => {
                let topic = match args.get("topic").and_then(|v| v.as_str()) {
                    Some(t) => t,
                    None => {
                        return Ok(ToolResult {
                            success: false,
                            output: String::new(),
                            error: Some("'topic' is required for 'run' action".to_string()),
                        });
                    }
                };

                let auto_approve = args
                    .get("auto_approve")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(true);

                let mut cmd = Command::new("python3");
                cmd.arg("-m")
                    .arg("researchclaw")
                    .arg("run")
                    .arg("--config")
                    .arg(&self.config_path)
                    .arg("--topic")
                    .arg(topic);

                if auto_approve {
                    cmd.arg("--auto-approve");
                }

                cmd.current_dir(&self.claw_path);

                let output = cmd.output();
                match output {
                    Ok(out) => {
                        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
                        let stderr = String::from_utf8_lossy(&out.stderr).to_string();
                        let success = out.status.success();

                        Ok(ToolResult {
                            success,
                            output: if success {
                                format!("Research pipeline completed.\n\nOutput:\n{}", stdout)
                            } else {
                                format!("stdout:\n{}\n\nstderr:\n{}", stdout, stderr)
                            },
                            error: if success { None } else { Some(stderr) },
                        })
                    }
                    Err(e) => Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!("Failed to execute researchclaw: {}", e)),
                    }),
                }
            }

            "doctor" => {
                let output = Command::new("python3")
                    .arg("-m")
                    .arg("researchclaw")
                    .arg("doctor")
                    .current_dir(&self.claw_path)
                    .output();

                match output {
                    Ok(out) => Ok(ToolResult {
                        success: out.status.success(),
                        output: String::from_utf8_lossy(&out.stdout).to_string(),
                        error: if out.status.success() {
                            None
                        } else {
                            Some(String::from_utf8_lossy(&out.stderr).to_string())
                        },
                    }),
                    Err(e) => Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!("Failed to run doctor: {}", e)),
                    }),
                }
            }

            "status" => {
                let artifacts_dir = std::path::Path::new(&self.claw_path).join("artifacts");
                if !artifacts_dir.exists() {
                    return Ok(ToolResult {
                        success: true,
                        output: "No research runs found in artifacts/".to_string(),
                        error: None,
                    });
                }

                let mut runs: Vec<String> = std::fs::read_dir(&artifacts_dir)?
                    .filter_map(|e| e.ok())
                    .filter(|e| e.path().is_dir())
                    .map(|e| e.file_name().to_string_lossy().to_string())
                    .collect();
                runs.sort();
                runs.reverse();

                let summary = runs
                    .iter()
                    .take(5)
                    .map(|r| format!("  - {}", r))
                    .collect::<Vec<_>>()
                    .join("\n");

                Ok(ToolResult {
                    success: true,
                    output: format!("Recent research runs ({} total):\n{}", runs.len(), summary),
                    error: None,
                })
            }

            _ => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Unknown action: '{}'. Use 'run', 'status', or 'doctor'.",
                    action
                )),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tool_spec() {
        let tool = ResearchClawTool::new(None, None);
        assert_eq!(tool.name(), "research_claw");
        let schema = tool.parameters_schema();
        assert!(schema["properties"]["topic"].is_object());
        assert!(schema["properties"]["action"].is_object());
    }
}
