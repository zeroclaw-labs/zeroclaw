//! Agent-facing JSON Patch over the config file.
//!
//! The third consumer of `zeroclaw_config::patch`, after the gateway's
//! `PATCH /api/config` and the CLI's `zeroclaw config patch`. An agent drafts
//! ops; the approval gate puts them in front of the operator; this tool
//! applies what was approved through the same validated implementation the
//! operator surfaces use.
//!
//! Two deliberate containment properties:
//!
//! - **Disk only, never the live process.** The tool reads `config.toml`
//!   fresh, patches, validates, and saves. The running daemon's in-memory
//!   config — including this agent's own `SecurityPolicy` and tool registry —
//!   is untouched until the operator reloads or restarts. An agent therefore
//!   cannot act under a policy it changed in the turn that changed it; making
//!   a change live is always a second, human act.
//! - **No self-narration.** The arguments carry ops and nothing else — no
//!   free-text "reason" field for the model to argue its case inside the
//!   approval prompt. What the operator sees is computed by the host from
//!   the ops themselves.

use std::path::PathBuf;

use async_trait::async_trait;

use std::collections::BTreeSet;
use std::fmt::Write as _;

use zeroclaw_api::tool::{Tool, ToolOutput, ToolResult};
use zeroclaw_config::api_error::ConfigApiError;
use zeroclaw_config::patch::{
    PatchOp, apply_patch_ops, json_pointer_to_dotted, lookup_prop_field, parse_patch_ops,
};
use zeroclaw_config::policy::SecurityPolicy;
use zeroclaw_config::schema::Config;

pub struct ConfigPatchTool {
    config_path: PathBuf,
}

impl ConfigPatchTool {
    pub fn new(config_path: PathBuf) -> Self {
        Self { config_path }
    }

    /// One human-readable line for a structured patch error. Same rendering
    /// the CLI's human mode uses: path and op index become prose prefixes.
    fn error_text(err: &ConfigApiError) -> String {
        match (err.op_index, err.path.as_deref()) {
            (Some(idx), Some(path)) => format!("op[{idx}] on `{path}`: {}", err.message),
            (Some(idx), None) => format!("op[{idx}]: {}", err.message),
            (None, Some(path)) => format!("`{path}`: {}", err.message),
            (None, None) => err.message.clone(),
        }
    }

    fn refuse(err: &ConfigApiError) -> ToolResult {
        ToolResult {
            success: false,
            output: ToolOutput::default(),
            error: Some(format!(
                "config patch rejected — nothing was saved. {}",
                Self::error_text(err)
            )),
        }
    }

    /// An agent's resolved policy rendered for the operator, or a plain
    /// explanation of why it can't be. Side-effect free: `from_profiles`
    /// only computes — unlike `for_agent`, it creates no directories.
    fn policy_summary_for(config: &Config, alias: &str) -> String {
        match config.risk_profile_for_agent(alias) {
            Some(risk) => SecurityPolicy::from_profiles(
                risk,
                config.runtime_profile_for_agent(alias),
                &config.agent_workspace_dir(alias),
            )
            .prompt_summary(),
            None => "(risk profile does not resolve — the agent cannot boot)".to_string(),
        }
    }

    /// One line per op, with secret values redacted. Secrecy is checked
    /// against both the current and the patched config so a value written
    /// to a *newly created* secret path (dynamic per-alias credentials)
    /// is masked too.
    fn render_op(op: &PatchOp, before: &Config, after: &Config) -> String {
        let dotted = json_pointer_to_dotted(&op.path);
        let sensitive = [before, after].iter().any(|cfg| {
            lookup_prop_field(cfg, &dotted)
                .map(|info| info.is_secret || info.derived_from_secret)
                .unwrap_or(false)
        });
        match op.op.as_str() {
            "add" | "replace" | "test" => {
                let value = if sensitive {
                    "[redacted]".to_string()
                } else {
                    let raw = op
                        .value
                        .as_ref()
                        .map(ToString::to_string)
                        .unwrap_or_else(|| "null".to_string());
                    if raw.chars().count() > 80 {
                        let head: String = raw.chars().take(77).collect();
                        format!("{head}...")
                    } else {
                        raw
                    }
                };
                format!("  {:<8} {dotted} = {value}", op.op)
            }
            _ => format!("  {:<8} {dotted}", op.op),
        }
    }
}

#[async_trait]
impl Tool for ConfigPatchTool {
    fn name(&self) -> &str {
        "config_patch"
    }

    fn description(&self) -> &str {
        "Apply a JSON Patch to the ZeroClaw configuration file. Every call \
         requires operator approval. Changes are written to disk only: the \
         running daemon keeps its current configuration until the operator \
         reloads or restarts it, and this agent's own permissions do not \
         change mid-session. Supported ops: add/replace (require `value`), \
         remove, test (refused on secret paths), comment (requires `comment`). \
         Paths may be JSON Pointer (`/gateway/host`) or dotted (`gateway.host`)."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "ops": {
                    "type": "array",
                    "description": "JSON Patch operations over config properties",
                    "items": {
                        "type": "object",
                        "properties": {
                            "op": {
                                "type": "string",
                                "enum": ["add", "replace", "remove", "test", "comment"]
                            },
                            "path": {
                                "type": "string",
                                "description": "Config property path, JSON Pointer or dotted form"
                            },
                            "value": {
                                "description": "New value for add/replace; expected value for test"
                            },
                            "comment": {
                                "type": "string",
                                "description": "TOML comment preserved alongside the value"
                            }
                        },
                        "required": ["op", "path"]
                    }
                }
            },
            "required": ["ops"]
        })
    }

    /// Rewriting config grants authority; only the operator grants
    /// authority. A chat channel that can answer ordinary approval prompts
    /// is notified about this tool, never asked.
    fn approval_requires_operator(&self) -> bool {
        true
    }

    /// The operator's approval prompt: what the ops are, and — the part the
    /// raw JSON never shows — what they do to each agent's resolved
    /// permissions. Everything here is computed from the ops against the
    /// on-disk config; none of it is model text. Returns `None` when the
    /// patch can't be previewed (unreadable config, ops that don't apply);
    /// the generic argument summary shows instead, and execution will
    /// refuse with the precise error.
    fn approval_summary(&self, args: &serde_json::Value) -> Option<String> {
        let ops = parse_patch_ops(args.get("ops")?.clone()).ok()?;
        let raw = std::fs::read_to_string(&self.config_path).ok()?;
        let mut before: Config = toml::from_str(&raw).ok()?;
        before.config_path = self.config_path.clone();
        let mut after = before.clone();
        apply_patch_ops(&mut after, &ops).ok()?;

        let mut out = String::new();
        let _ = writeln!(out, "apply {} change(s) to config.toml:", ops.len());
        for op in &ops {
            let _ = writeln!(out, "{}", Self::render_op(op, &before, &after));
        }

        // Per-agent resolved-policy delta. Resolving per agent (not per
        // edited path) catches indirect changes too: editing a shared
        // `risk-profiles.*` entry re-renders every agent that references it.
        let aliases: BTreeSet<&String> = before.agents.keys().chain(after.agents.keys()).collect();
        let mut delta = String::new();
        for alias in aliases {
            let was = before
                .agents
                .contains_key(alias.as_str())
                .then(|| Self::policy_summary_for(&before, alias));
            let now = after
                .agents
                .contains_key(alias.as_str())
                .then(|| Self::policy_summary_for(&after, alias));
            if was == now {
                continue;
            }
            let _ = writeln!(delta, "  agent `{alias}`:");
            let _ = writeln!(
                delta,
                "    before:\n      {}",
                was.as_deref()
                    .unwrap_or("(agent does not exist)")
                    .trim_end()
                    .replace('\n', "\n      ")
            );
            let _ = writeln!(
                delta,
                "    after:\n      {}",
                now.as_deref()
                    .unwrap_or("(agent is removed)")
                    .trim_end()
                    .replace('\n', "\n      ")
            );
        }
        if !delta.is_empty() {
            let _ = writeln!(out, "\npermission changes if approved:");
            out.push_str(&delta);
        }
        out.push_str("\nwritten to disk only — live after the daemon reloads or restarts");
        Some(out)
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let Some(ops_value) = args.get("ops").cloned() else {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some("missing required `ops` parameter (a JSON Patch array)".into()),
            });
        };

        let ops = match parse_patch_ops(ops_value) {
            Ok(ops) => ops,
            Err(err) => return Ok(Self::refuse(&err)),
        };

        // Fresh read of the on-disk state, not the boot-time snapshot: the
        // operator may have edited config since this process started, and a
        // stale base would resurrect overwritten values. By the time an agent
        // is running, boot has already migrated the file to the current
        // schema, so a parse failure here means the file is genuinely broken.
        let raw = match tokio::fs::read_to_string(&self.config_path).await {
            Ok(raw) => raw,
            Err(err) => {
                return Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some(format!(
                        "failed to read {}: {err}",
                        self.config_path.display()
                    )),
                });
            }
        };
        let mut working: Config = match toml::from_str(&raw) {
            Ok(config) => config,
            Err(err) => {
                return Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some(format!(
                        "config.toml did not parse ({err}); refusing to patch a broken file"
                    )),
                });
            }
        };
        working.config_path = self.config_path.clone();

        let results = match apply_patch_ops(&mut working, &ops) {
            Ok(results) => results,
            Err(err) => return Ok(Self::refuse(&err)),
        };

        if let Err(err) = working.validate() {
            let api_err = ConfigApiError::from_validation(err);
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(format!(
                    "validation failed after applying patch — nothing was saved. {}",
                    Self::error_text(&api_err)
                )),
            });
        }

        working.save_dirty().await?;

        // Comments go on after save so the comment-preserving sync_table
        // pass doesn't strip them — same order as the gateway and CLI.
        let annotations: Vec<(String, String)> = ops
            .iter()
            .zip(results.iter())
            .filter_map(|(op, res)| op.comment.as_ref().map(|c| (res.path.clone(), c.clone())))
            .collect();
        if !annotations.is_empty()
            && let Err(err) =
                zeroclaw_config::comment_writer::apply_comments(&self.config_path, &annotations)
                    .await
        {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({"error": format!("{}", err)})),
                "config_patch: failed to apply op comments to config.toml"
            );
        }

        Ok(ToolResult::ok(ToolOutput::json(serde_json::json!({
            "saved": true,
            "results": results,
            "note": "written to config.toml; the running daemon keeps its current \
                     configuration until the operator reloads or restarts it"
        }))))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn saved_config(dir: &std::path::Path) -> PathBuf {
        let path = dir.join("config.toml");
        let config = Config {
            config_path: path.clone(),
            ..Config::default()
        };
        config.save().await.expect("save initial config");
        path
    }

    fn read_config(path: &PathBuf) -> Config {
        let raw = std::fs::read_to_string(path).expect("read config back");
        toml::from_str(&raw).expect("saved config parses")
    }

    #[tokio::test]
    async fn applies_a_replace_and_persists_it_to_disk() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = saved_config(dir.path()).await;
        let tool = ConfigPatchTool::new(path.clone());

        let result = tool
            .execute(serde_json::json!({
                "ops": [{"op": "replace", "path": "/gateway/host", "value": "127.0.0.2"}]
            }))
            .await
            .expect("execute");

        assert!(result.success, "patch should succeed: {:?}", result.error);
        assert_eq!(read_config(&path).gateway.host, "127.0.0.2");
        let data = result.output.data().expect("structured output");
        assert_eq!(data["saved"], true);
        assert_eq!(data["results"][0]["path"], "gateway.host");
        assert!(
            data["note"].as_str().expect("note").contains("reloads"),
            "success output must state that nothing is live until reload"
        );
    }

    #[tokio::test]
    async fn an_invalid_op_is_refused_and_the_file_does_not_move() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = saved_config(dir.path()).await;
        let before = std::fs::read_to_string(&path).expect("read before");
        let tool = ConfigPatchTool::new(path.clone());

        let result = tool
            .execute(serde_json::json!({
                "ops": [{"op": "frobnicate", "path": "/gateway/host", "value": "x"}]
            }))
            .await
            .expect("execute");

        assert!(!result.success);
        let error = result.error.expect("error text");
        assert!(
            error.contains("nothing was saved") && error.contains("op[0]"),
            "refusal should carry the op context: {error}"
        );
        assert_eq!(
            std::fs::read_to_string(&path).expect("read after"),
            before,
            "a refused patch must not touch the file"
        );
    }

    #[tokio::test]
    async fn post_apply_validation_failure_saves_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = saved_config(dir.path()).await;
        let before = std::fs::read_to_string(&path).expect("read before");
        let tool = ConfigPatchTool::new(path.clone());

        let result = tool
            .execute(serde_json::json!({
                "ops": [{"op": "replace", "path": "/gateway/host", "value": ""}]
            }))
            .await
            .expect("execute");

        assert!(!result.success);
        assert!(
            result.error.expect("error").contains("validation failed"),
            "the refusal should name validation as the reason"
        );
        assert_eq!(
            std::fs::read_to_string(&path).expect("read after"),
            before,
            "a patch that fails validation must not be saved"
        );
    }

    #[tokio::test]
    async fn missing_ops_parameter_is_a_clean_refusal() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = saved_config(dir.path()).await;
        let tool = ConfigPatchTool::new(path);

        let result = tool.execute(serde_json::json!({})).await.expect("execute");

        assert!(!result.success);
        assert!(result.error.expect("error").contains("`ops`"));
    }

    #[tokio::test]
    async fn a_missing_config_file_is_reported_not_created() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        let tool = ConfigPatchTool::new(path.clone());

        let result = tool
            .execute(serde_json::json!({
                "ops": [{"op": "replace", "path": "/gateway/host", "value": "127.0.0.2"}]
            }))
            .await
            .expect("execute");

        assert!(!result.success);
        assert!(
            !path.exists(),
            "the tool must never bring a config file into existence"
        );
    }

    /// Build a config on disk with one agent on the `balanced` preset and the
    /// `yolo` preset available to move to.
    async fn config_with_balanced_agent(dir: &std::path::Path) -> PathBuf {
        let path = dir.join("config.toml");
        let mut config = Config {
            config_path: path.clone(),
            ..Config::default()
        };
        for preset in ["balanced", "yolo"] {
            config.risk_profiles.insert(
                preset.to_string(),
                (zeroclaw_config::presets::risk_preset(preset)
                    .expect("preset exists")
                    .values)(),
            );
        }
        let ops = parse_patch_ops(serde_json::json!([
            {"op": "add", "path": "/agents/helper/risk_profile", "value": "balanced"}
        ]))
        .expect("fixture ops parse");
        apply_patch_ops(&mut config, &ops).expect("fixture agent applies");
        config.save().await.expect("save initial config");
        path
    }

    #[tokio::test]
    async fn approval_summary_renders_the_permission_delta() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = config_with_balanced_agent(dir.path()).await;
        let tool = ConfigPatchTool::new(path);

        let summary = tool
            .approval_summary(&serde_json::json!({
                "ops": [{"op": "replace", "path": "/agents/helper/risk_profile", "value": "yolo"}]
            }))
            .expect("a previewable patch must produce a host summary");

        assert!(
            summary.contains("agents.helper.risk_profile"),
            "the op itself is listed: {summary}"
        );
        assert!(
            summary.contains("agent `helper`"),
            "the affected agent is named: {summary}"
        );
        assert!(
            summary.contains("Supervised") && summary.contains("Full"),
            "the before/after resolved autonomy must both be visible — this \
             is the line that tells the operator what they are authorizing: {summary}"
        );
        assert!(
            summary.contains("reloads or restarts"),
            "the summary states the change is not live until reload: {summary}"
        );
    }

    #[tokio::test]
    async fn approval_summary_is_quiet_about_unaffected_policies() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = config_with_balanced_agent(dir.path()).await;
        let tool = ConfigPatchTool::new(path);

        let summary = tool
            .approval_summary(&serde_json::json!({
                "ops": [{"op": "replace", "path": "/gateway/host", "value": "127.0.0.2"}]
            }))
            .expect("summary");

        assert!(
            !summary.contains("permission changes"),
            "a patch that moves no policy must not render a policy delta: {summary}"
        );
    }

    #[tokio::test]
    async fn approval_summary_masks_secret_values() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = saved_config(dir.path()).await;
        let tool = ConfigPatchTool::new(path);

        let summary = tool
            .approval_summary(&serde_json::json!({
                "ops": [{"op": "add", "path": "/http_request/secrets/api_token", "value": "tok-123"}]
            }))
            .expect("summary");

        assert!(
            summary.contains("[redacted]"),
            "secret paths render redacted: {summary}"
        );
        assert!(
            !summary.contains("tok-123"),
            "the secret value itself must never reach an approval prompt: {summary}"
        );
    }

    /// The tool's success output goes back to the model and into transcripts
    /// and logs. Setting a secret must report `populated`, never the value —
    /// the same contract the gateway and CLI results follow.
    #[tokio::test]
    async fn setting_a_secret_reports_populated_not_the_value() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = saved_config(dir.path()).await;
        let tool = ConfigPatchTool::new(path);

        let result = tool
            .execute(serde_json::json!({
                "ops": [{"op": "add", "path": "/http_request/secrets/api_token", "value": "tok-456"}]
            }))
            .await
            .expect("execute");

        assert!(
            result.success,
            "secret set should succeed: {:?}",
            result.error
        );
        let data = result.output.data().expect("structured output");
        assert_eq!(data["results"][0]["populated"], true);
        assert!(
            !serde_json::to_string(&data)
                .expect("serialize")
                .contains("tok-456"),
            "the secret value must not appear anywhere in the tool result: {data}"
        );
    }

    #[tokio::test]
    async fn an_unpreviewable_patch_yields_no_summary() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = saved_config(dir.path()).await;
        let tool = ConfigPatchTool::new(path);

        let summary = tool.approval_summary(&serde_json::json!({
            "ops": [{"op": "frobnicate", "path": "/gateway/host", "value": "x"}]
        }));

        assert!(
            summary.is_none(),
            "ops that will be refused fall back to the generic summary"
        );
    }

    #[test]
    fn schema_offers_no_free_text_narration_field() {
        let dir = std::env::temp_dir();
        let tool = ConfigPatchTool::new(dir.join("config.toml"));
        let schema = tool.parameters_schema();
        let props = schema["properties"].as_object().expect("properties");
        assert_eq!(
            props.keys().collect::<Vec<_>>(),
            vec!["ops"],
            "the model gets ops and nothing else — no reason/description field \
             to argue its case inside the approval prompt"
        );
    }
}
