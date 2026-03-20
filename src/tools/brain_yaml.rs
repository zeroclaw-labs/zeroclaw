use super::traits::{Tool, ToolResult};
use crate::security::SecurityPolicy;
use async_trait::async_trait;
use serde_json::json;
use std::fmt::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Tool for reading, listing, and searching YAML files in the ~/.brain/ directory.
///
/// The Brain is a YAML-first cognitive architecture with:
/// - soul/    — identity, judgment, voice (constitutional layer)
/// - cortex/  — session definitions (engineering, business domains)
/// - logic/   — reasoning frameworks
/// - knowledge/ — architecture docs, schema definitions
/// - tools/   — tool registry (index.yaml)
/// - memory/  — episodic memory
/// - principles/ — decision guardrails
pub struct BrainYamlTool {
    security: Arc<SecurityPolicy>,
    brain_dir: PathBuf,
}

impl BrainYamlTool {
    pub fn new(security: Arc<SecurityPolicy>, brain_dir: PathBuf) -> Self {
        Self {
            security,
            brain_dir,
        }
    }

    fn resolve_path(&self, relative: &str) -> PathBuf {
        self.brain_dir.join(relative)
    }

    fn validate_within_brain(&self, path: &Path) -> anyhow::Result<()> {
        let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        let brain_canonical = self
            .brain_dir
            .canonicalize()
            .unwrap_or_else(|_| self.brain_dir.clone());
        if !canonical.starts_with(&brain_canonical) {
            anyhow::bail!("Path escapes brain directory: {}", path.display());
        }
        Ok(())
    }
}

#[async_trait]
impl Tool for BrainYamlTool {
    fn name(&self) -> &str {
        "brain_yaml"
    }

    fn description(&self) -> &str {
        "Read, list, search, and extract from YAML files in the Brain (~/.brain/). \
         Actions: 'read' a file, 'list' files in a subdirectory, 'search' for content \
         across YAML files, 'get' a specific key path from a file."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["read", "list", "search", "get"],
                    "description": "Action to perform"
                },
                "path": {
                    "type": "string",
                    "description": "Relative path within ~/.brain/ (e.g., 'soul/identity.yaml', 'cortex/engineering/')"
                },
                "query": {
                    "type": "string",
                    "description": "Search query (for 'search' action) — matches against file content"
                },
                "key": {
                    "type": "string",
                    "description": "Dot-separated key path (for 'get' action, e.g., 'session.role', 'constraints.non_negotiable')"
                }
            },
            "required": ["action"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        if self.security.is_rate_limited() {
            anyhow::bail!("Rate limited");
        }

        let action = args
            .get("action")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'action' parameter"))?;

        match action {
            "read" => self.action_read(&args).await,
            "list" => self.action_list(&args).await,
            "search" => self.action_search(&args).await,
            "get" => self.action_get(&args).await,
            _ => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Unknown action '{action}'. Use: read, list, search, get"
                )),
            }),
        }
    }
}

impl BrainYamlTool {
    async fn action_read(&self, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        let rel = args
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("'read' requires 'path' parameter"))?;

        let path = self.resolve_path(rel);
        self.validate_within_brain(&path)?;

        if !path.exists() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("File not found: {rel}")),
            });
        }

        let content = tokio::fs::read_to_string(&path).await?;

        // Validate it's parseable YAML
        let parsed: serde_yaml::Value = serde_yaml::from_str(&content)
            .map_err(|e| anyhow::anyhow!("YAML parse error in {rel}: {e}"))?;

        // Return as formatted YAML (normalized)
        let output = serde_yaml::to_string(&parsed)?;
        Ok(ToolResult {
            success: true,
            output,
            error: None,
        })
    }

    async fn action_list(&self, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        let rel = args.get("path").and_then(|v| v.as_str()).unwrap_or("");

        let dir = self.resolve_path(rel);
        self.validate_within_brain(&dir)?;

        if !dir.is_dir() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Not a directory: {rel}")),
            });
        }

        let mut entries = Vec::new();
        let mut rd = tokio::fs::read_dir(&dir).await?;
        while let Some(entry) = rd.next_entry().await? {
            let ft = entry.file_type().await?;
            let name = entry.file_name().to_string_lossy().to_string();

            if ft.is_dir() {
                entries.push(format!("{name}/"));
            } else if name.ends_with(".yaml") || name.ends_with(".yml") || name.ends_with(".md") {
                entries.push(name);
            }
        }

        entries.sort();

        let mut output = String::new();
        if rel.is_empty() {
            writeln!(output, "~/.brain/").ok();
        } else {
            writeln!(output, "~/.brain/{rel}").ok();
        }
        for e in &entries {
            writeln!(output, "  {e}").ok();
        }
        if entries.is_empty() {
            writeln!(output, "  (empty)").ok();
        }

        Ok(ToolResult {
            success: true,
            output,
            error: None,
        })
    }

    async fn action_search(&self, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        let query = args
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("'search' requires 'query' parameter"))?;

        let subdir = args.get("path").and_then(|v| v.as_str()).unwrap_or("");

        let search_root = self.resolve_path(subdir);
        self.validate_within_brain(&search_root)?;

        let query_lower = query.to_lowercase();
        let mut results = Vec::new();

        collect_yaml_matches(&search_root, &self.brain_dir, &query_lower, &mut results).await?;

        if results.is_empty() {
            return Ok(ToolResult {
                success: true,
                output: format!("No matches for '{query}' in ~/.brain/{subdir}"),
                error: None,
            });
        }

        // Cap results
        let total = results.len();
        results.truncate(20);

        let mut output = String::new();
        writeln!(output, "Found {total} match(es) for '{query}':").ok();
        for (rel_path, line_num, line_content) in &results {
            writeln!(output, "  {rel_path}:{line_num}: {line_content}").ok();
        }
        if total > 20 {
            writeln!(output, "  ... ({} more)", total - 20).ok();
        }

        Ok(ToolResult {
            success: true,
            output,
            error: None,
        })
    }

    async fn action_get(&self, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        let rel = args
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("'get' requires 'path' parameter"))?;

        let key = args
            .get("key")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("'get' requires 'key' parameter"))?;

        let path = self.resolve_path(rel);
        self.validate_within_brain(&path)?;

        if !path.exists() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("File not found: {rel}")),
            });
        }

        let content = tokio::fs::read_to_string(&path).await?;
        let parsed: serde_yaml::Value = serde_yaml::from_str(&content)?;

        // Walk dot-separated key path
        let mut current = &parsed;
        for segment in key.split('.') {
            match current {
                serde_yaml::Value::Mapping(map) => {
                    let yaml_key = serde_yaml::Value::String(segment.to_string());
                    match map.get(&yaml_key) {
                        Some(val) => current = val,
                        None => {
                            return Ok(ToolResult {
                                success: false,
                                output: String::new(),
                                error: Some(format!(
                                    "Key '{key}' not found (missing segment '{segment}')"
                                )),
                            });
                        }
                    }
                }
                serde_yaml::Value::Sequence(seq) => {
                    if let Ok(idx) = segment.parse::<usize>() {
                        match seq.get(idx) {
                            Some(val) => current = val,
                            None => {
                                return Ok(ToolResult {
                                    success: false,
                                    output: String::new(),
                                    error: Some(format!("Index {idx} out of bounds at '{key}'")),
                                });
                            }
                        }
                    } else {
                        return Ok(ToolResult {
                            success: false,
                            output: String::new(),
                            error: Some(format!(
                                "Expected numeric index for sequence at '{segment}'"
                            )),
                        });
                    }
                }
                _ => {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!("Cannot index into scalar at '{segment}'")),
                    });
                }
            }
        }

        let output = serde_yaml::to_string(current)?;
        Ok(ToolResult {
            success: true,
            output,
            error: None,
        })
    }
}

/// Recursively find YAML files containing the query string.
async fn collect_yaml_matches(
    dir: &Path,
    brain_root: &Path,
    query: &str,
    results: &mut Vec<(String, usize, String)>,
) -> anyhow::Result<()> {
    if !dir.is_dir() {
        return Ok(());
    }

    let mut rd = tokio::fs::read_dir(dir).await?;
    while let Some(entry) = rd.next_entry().await? {
        let ft = entry.file_type().await?;
        let path = entry.path();

        if ft.is_dir() {
            // Skip hidden dirs and .git
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') {
                continue;
            }
            Box::pin(collect_yaml_matches(&path, brain_root, query, results)).await?;
        } else {
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.ends_with(".yaml") && !name.ends_with(".yml") {
                continue;
            }

            if let Ok(content) = tokio::fs::read_to_string(&path).await {
                let rel = path
                    .strip_prefix(brain_root)
                    .unwrap_or(&path)
                    .display()
                    .to_string();

                for (i, line) in content.lines().enumerate() {
                    if line.to_lowercase().contains(query) {
                        let trimmed = line.trim();
                        let display = if trimmed.len() > 120 {
                            format!("{}...", &trimmed[..117])
                        } else {
                            trimmed.to_string()
                        };
                        results.push((rel.clone(), i + 1, display));
                    }
                }
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::SecurityPolicy;
    use std::sync::Arc;
    use tempfile::TempDir;

    fn setup() -> (TempDir, BrainYamlTool) {
        let dir = TempDir::new().unwrap();
        let security = Arc::new(SecurityPolicy::default());
        let tool = BrainYamlTool::new(security, dir.path().to_path_buf());
        (dir, tool)
    }

    #[test]
    fn schema_has_required_action() {
        let (_, tool) = setup();
        let schema = tool.parameters_schema();
        let required = schema["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v == "action"));
    }

    #[tokio::test]
    async fn read_valid_yaml() {
        let (dir, tool) = setup();
        let yaml = "session:\n  name: Test\n  role: tester\n";
        std::fs::write(dir.path().join("test.yaml"), yaml).unwrap();

        let result = tool
            .execute(json!({"action": "read", "path": "test.yaml"}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("name: Test"));
    }

    #[tokio::test]
    async fn read_missing_file() {
        let (_, tool) = setup();
        let result = tool
            .execute(json!({"action": "read", "path": "nope.yaml"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("not found"));
    }

    #[tokio::test]
    async fn list_directory() {
        let (dir, tool) = setup();
        std::fs::create_dir(dir.path().join("cortex")).unwrap();
        std::fs::write(dir.path().join("cortex/backend.yaml"), "x: 1").unwrap();
        std::fs::write(dir.path().join("cortex/frontend.yaml"), "x: 2").unwrap();
        std::fs::write(dir.path().join("cortex/notes.txt"), "ignore").unwrap();

        let result = tool
            .execute(json!({"action": "list", "path": "cortex"}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("backend.yaml"));
        assert!(result.output.contains("frontend.yaml"));
        assert!(!result.output.contains("notes.txt"));
    }

    #[tokio::test]
    async fn search_finds_matches() {
        let (dir, tool) = setup();
        std::fs::create_dir(dir.path().join("soul")).unwrap();
        std::fs::write(
            dir.path().join("soul/identity.yaml"),
            "_meta:\n  type: soul_identity\nidentity:\n  name: Joel\n",
        )
        .unwrap();

        let result = tool
            .execute(json!({"action": "search", "query": "Joel"}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("soul/identity.yaml"));
    }

    #[tokio::test]
    async fn get_extracts_nested_key() {
        let (dir, tool) = setup();
        let yaml = "session:\n  name: Backend\n  role: backend_engineer\n";
        std::fs::write(dir.path().join("test.yaml"), yaml).unwrap();

        let result = tool
            .execute(json!({"action": "get", "path": "test.yaml", "key": "session.role"}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("backend_engineer"));
    }

    #[tokio::test]
    async fn get_missing_key() {
        let (dir, tool) = setup();
        std::fs::write(dir.path().join("test.yaml"), "x: 1\n").unwrap();

        let result = tool
            .execute(json!({"action": "get", "path": "test.yaml", "key": "x.y.z"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("Cannot index into scalar"));
    }

    #[tokio::test]
    async fn path_escape_blocked() {
        let (_, tool) = setup();
        let result = tool
            .execute(json!({"action": "read", "path": "../../etc/passwd"}))
            .await;
        assert!(result.is_err() || !result.unwrap().success);
    }
}
