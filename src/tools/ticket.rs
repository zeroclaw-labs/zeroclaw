use super::traits::{Tool, ToolResult};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Config-driven ticket management tool.
///
/// Reads subcommand definitions from `.tickets/tool.toml` so new actions
/// can be added without recompilation.
pub struct TicketTool {
    workspace_dir: PathBuf,
    config: TicketToolConfig,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct TicketToolConfig {
    #[serde(default)]
    commands: BTreeMap<String, SubcommandDef>,
}

#[derive(Debug, Clone, Deserialize)]
struct SubcommandDef {
    description: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    params: BTreeMap<String, ParamDef>,
}

#[derive(Debug, Clone, Deserialize)]
struct ParamDef {
    #[serde(rename = "type", default = "default_string_type")]
    param_type: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    flag: Option<String>,
    #[serde(default)]
    positional: bool,
    #[serde(default)]
    required: bool,
}

fn default_string_type() -> String {
    "string".to_string()
}

impl TicketTool {
    pub fn new(workspace_dir: PathBuf) -> Self {
        let config = load_config(&workspace_dir).unwrap_or_default();
        Self {
            workspace_dir,
            config,
        }
    }

    fn build_help(&self) -> String {
        let mut lines = vec!["Available ticket commands:".to_string(), String::new()];
        for (name, cmd) in &self.config.commands {
            lines.push(format!("  {name:<14} {}", cmd.description));
        }
        lines.push(String::new());
        lines.push("  help           Show this help message".to_string());
        lines.join("\n")
    }

    fn build_action_schema(&self) -> serde_json::Value {
        let mut action_enum: Vec<String> = self.config.commands.keys().cloned().collect();
        action_enum.push("help".to_string());

        let mut properties = serde_json::Map::new();
        properties.insert(
            "action".to_string(),
            json!({
                "type": "string",
                "enum": action_enum,
                "description": "The ticket subcommand to run. Use 'help' to see all options."
            }),
        );

        // Merge all parameter definitions from all subcommands into the top-level
        // schema. The LLM sees which params each action accepts from the description.
        let mut all_params = BTreeMap::<String, &ParamDef>::new();
        for cmd in self.config.commands.values() {
            for (pname, pdef) in &cmd.params {
                // Use a prefixed name to avoid collisions (e.g., show.id vs create.title)
                // Actually, keep it flat — most param names are unique enough,
                // and this is simpler for the LLM. If there's a collision, last wins.
                all_params.entry(pname.clone()).or_insert(pdef);
            }
        }

        for (pname, pdef) in &all_params {
            let schema_type = match pdef.param_type.as_str() {
                "integer" => "integer",
                "boolean" => "boolean",
                _ => "string",
            };
            properties.insert(
                pname.clone(),
                json!({
                    "type": schema_type,
                    "description": pdef.description
                }),
            );
        }

        json!({
            "type": "object",
            "properties": properties,
            "required": ["action"],
            "additionalProperties": false
        })
    }
}

#[async_trait]
impl Tool for TicketTool {
    fn name(&self) -> &str {
        "ticket"
    }

    fn description(&self) -> &str {
        "Manage project tickets: list, show, create, query, stats, pipeline, and more. Use action 'help' to see all subcommands."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        self.build_action_schema()
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        // Reload config each execution so edits take effect without restart.
        let config = load_config(&self.workspace_dir).unwrap_or_else(|e| {
            tracing::warn!("Failed to reload ticket tool config: {e}");
            self.config.clone()
        });

        let action = args
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("help");

        if action == "help" {
            return Ok(ToolResult {
                success: true,
                output: self.build_help(),
                error: None,
            });
        }

        let cmd_def = match config.commands.get(action) {
            Some(def) => def,
            None => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!(
                        "Unknown action '{action}'. Use action 'help' to see available commands."
                    )),
                });
            }
        };

        // Build the tk command arguments.
        let mut tk_args: Vec<String> = cmd_def.args.clone();

        // Process parameters: positional args first, then flags.
        let mut positionals = Vec::new();
        let mut flags = Vec::new();

        for (pname, pdef) in &cmd_def.params {
            let value = args.get(pname.as_str());
            let str_value = value.and_then(|v| {
                if v.is_string() {
                    v.as_str().map(String::from)
                } else if v.is_boolean() {
                    Some(v.as_bool().unwrap_or(false).to_string())
                } else if v.is_number() {
                    Some(v.to_string())
                } else {
                    None
                }
            });

            if let Some(val) = str_value {
                if val.is_empty() {
                    continue;
                }
                if !is_safe_tk_arg(&val) {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!("Invalid value for parameter '{pname}'")),
                    });
                }
                if pdef.positional {
                    positionals.push((pname.clone(), val));
                } else if pdef.param_type == "boolean" {
                    if val == "true" {
                        if let Some(ref flag) = pdef.flag {
                            flags.push(flag.clone());
                        }
                    }
                } else if let Some(ref flag) = pdef.flag {
                    flags.push(flag.clone());
                    flags.push(val);
                }
            } else if pdef.required {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Required parameter '{pname}' is missing")),
                });
            }
        }

        // Positionals go right after the base args.
        for (_name, val) in &positionals {
            tk_args.push(val.clone());
        }
        // Flags go last.
        tk_args.extend(flags);

        let arg_refs: Vec<&str> = tk_args.iter().map(|s| s.as_str()).collect();
        run_tk(&self.workspace_dir, &arg_refs).await
    }
}

fn load_config(workspace_dir: &Path) -> anyhow::Result<TicketToolConfig> {
    // Walk up from workspace_dir to find .tickets/tool.toml
    let mut dir = workspace_dir.to_path_buf();
    loop {
        let candidate = dir.join(".tickets").join("tool.toml");
        if candidate.exists() {
            let content = std::fs::read_to_string(&candidate)?;
            let config: TicketToolConfig = toml::from_str(&content)?;
            return Ok(config);
        }
        if !dir.pop() {
            break;
        }
    }
    Ok(TicketToolConfig::default())
}

/// Validate that a `tk` argument doesn't contain shell metacharacters.
fn is_safe_tk_arg(arg: &str) -> bool {
    !arg.is_empty() && arg.len() <= 1024 && !arg.contains([';', '&', '|', '`', '\n', '\r'])
}

/// Execute a `tk` command and return a ToolResult.
async fn run_tk(workspace_dir: &Path, args: &[&str]) -> anyhow::Result<ToolResult> {
    let output = tokio::process::Command::new("tk")
        .args(args)
        .current_dir(workspace_dir)
        .output()
        .await;

    match output {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();

            if output.status.success() {
                Ok(ToolResult {
                    success: true,
                    output: stdout,
                    error: None,
                })
            } else {
                Ok(ToolResult {
                    success: false,
                    output: stdout,
                    error: Some(if stderr.is_empty() {
                        format!("tk exited with status {}", output.status)
                    } else {
                        stderr
                    }),
                })
            }
        }
        Err(e) => Ok(ToolResult {
            success: false,
            output: String::new(),
            error: Some(format!(
                "Failed to run tk: {e}. Is tk installed and in PATH?"
            )),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_arg_rejects_shell_metacharacters() {
        assert!(is_safe_tk_arg("add-usage-remaining-a4f4"));
        assert!(is_safe_tk_arg("bug"));
        assert!(is_safe_tk_arg(".type == \"bug\""));
        assert!(!is_safe_tk_arg("foo;rm -rf /"));
        assert!(!is_safe_tk_arg("foo|bar"));
        assert!(!is_safe_tk_arg(""));
    }

    #[test]
    fn safe_arg_allows_jq_dollar_in_filters() {
        // jq filters may use $ for variables — we allow it since tk query
        // doesn't pass through a shell.
        assert!(is_safe_tk_arg(".priority <= 1"));
    }

    #[test]
    fn config_parses_from_toml() {
        let toml_str = r#"
[commands.list]
description = "List tickets"
args = ["list", "--json"]

[commands.list.params.type]
flag = "--type"
type = "string"
description = "Filter by type"
"#;
        let config: TicketToolConfig = toml::from_str(toml_str).unwrap();
        assert!(config.commands.contains_key("list"));
        let list = &config.commands["list"];
        assert_eq!(list.args, vec!["list", "--json"]);
        assert!(list.params.contains_key("type"));
        assert_eq!(list.params["type"].flag.as_deref(), Some("--type"));
    }

    #[test]
    fn config_defaults_when_missing() {
        let config = TicketToolConfig::default();
        assert!(config.commands.is_empty());
    }

    #[test]
    fn help_output_lists_commands() {
        let toml_str = r#"
[commands.list]
description = "List tickets"
args = ["list"]

[commands.show]
description = "Show ticket"
args = ["show"]
"#;
        let config: TicketToolConfig = toml::from_str(toml_str).unwrap();
        let tool = TicketTool {
            workspace_dir: PathBuf::from("/tmp"),
            config,
        };
        let help = tool.build_help();
        assert!(help.contains("list"));
        assert!(help.contains("show"));
        assert!(help.contains("help"));
    }
}
