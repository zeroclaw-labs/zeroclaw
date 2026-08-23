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

use std::collections::{BTreeSet, HashMap};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};

use async_trait::async_trait;
use base64::Engine as _;
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

use zeroclaw_api::tool::{
    APPROVAL_EXECUTION_BINDING_ARG, Tool, ToolApprovalSummary, ToolOutput, ToolResult,
};
use zeroclaw_config::api_error::ConfigApiError;
use zeroclaw_config::patch::{
    PatchOp, apply_patch_ops, json_pointer_to_dotted, lookup_prop_field, parse_patch_ops,
};
use zeroclaw_config::policy::{SecurityPolicy, ToolOperation};
use zeroclaw_config::schema::Config;

const PREVIEW_BINDING_VERSION: u8 = 1;
const PREVIEW_BINDING_PAYLOAD_LEN: usize = 1 + 16 + 32 + 32;
const PREVIEW_BINDING_LEN: usize = PREVIEW_BINDING_PAYLOAD_LEN + 32;

type HmacSha256 = Hmac<Sha256>;

fn sha256(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

struct ApprovalPreview {
    text: String,
    ops_digest: [u8; 32],
    base_digest: [u8; 32],
}

enum PreviewBindingError {
    Invalid,
    OperationsChanged,
    ConfigChanged,
}

/// Process-wide, per-config-path async locks that serialize `config_patch`'s
/// read-modify-write so two concurrent calls cannot each read the same base
/// and clobber the other's write.
static CONFIG_WRITE_LOCKS: LazyLock<
    parking_lot::Mutex<HashMap<PathBuf, Arc<tokio::sync::Mutex<()>>>>,
> = LazyLock::new(|| parking_lot::Mutex::new(HashMap::new()));

fn write_lock_for(path: &Path) -> Arc<tokio::sync::Mutex<()>> {
    CONFIG_WRITE_LOCKS
        .lock()
        .entry(path.to_path_buf())
        .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
        .clone()
}

pub struct ConfigPatchTool {
    config_path: PathBuf,
    security: Arc<SecurityPolicy>,
    /// Per-tool random key authenticating opaque preview bindings. The key is
    /// process-local and never leaves the tool; each binding also carries a
    /// fresh random nonce, exact ops digest, and exact base-config digest.
    preview_binding_key: [u8; 32],
}

impl ConfigPatchTool {
    pub fn new(config_path: PathBuf, security: Arc<SecurityPolicy>) -> Self {
        Self {
            config_path,
            security,
            preview_binding_key: rand::random(),
        }
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
    /// Render a value for the operator prompt: bounded, but with the
    /// truncation made explicit (character count) rather than a silent `...`
    /// that could hide a security-relevant suffix. `s` is expected to already
    /// be in a display-safe form (JSON text or a `{:?}`-escaped string).
    fn bounded_display(s: &str) -> String {
        const CAP: usize = 160;
        let count = s.chars().count();
        if count > CAP {
            let head: String = s.chars().take(CAP).collect();
            format!("{head} … ({count} chars total, truncated)")
        } else {
            s.to_string()
        }
    }

    fn render_op(op: &PatchOp, before: &Config, after: &Config) -> String {
        let dotted = json_pointer_to_dotted(&op.path);
        let sensitive = [before, after].iter().any(|cfg| {
            lookup_prop_field(cfg, &dotted)
                .map(|info| info.is_secret || info.derived_from_secret)
                .unwrap_or(false)
        });
        let mut line = match op.op.as_str() {
            "add" | "replace" | "test" => {
                let value = if sensitive {
                    "[redacted]".to_string()
                } else {
                    // JSON form is already escaped and unambiguous (strings
                    // quoted, control chars encoded); bound it with an explicit
                    // truncation marker.
                    let raw = op
                        .value
                        .as_ref()
                        .map(ToString::to_string)
                        .unwrap_or_else(|| "null".to_string());
                    Self::bounded_display(&raw)
                };
                format!("  {:<8} {dotted} = {value}", op.op)
            }
            _ => format!("  {:<8} {dotted}", op.op),
        };
        // Comments are model-authored bytes that get written to `config.toml`.
        // The operator must see them or the "no self-narration" guarantee is
        // hollow. Escape via `{:?}` so a newline or control char cannot forge
        // additional prompt lines.
        if let Some(comment) = &op.comment {
            let _ = write!(
                line,
                "  # comment: {}",
                Self::bounded_display(&format!("{comment:?}"))
            );
        }
        line
    }

    fn build_approval_preview(&self, args: &serde_json::Value) -> Option<ApprovalPreview> {
        let ops_value = args.get("ops")?;
        let ops = parse_patch_ops(ops_value.clone()).ok()?;
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

        Some(ApprovalPreview {
            text: out,
            ops_digest: sha256(&serde_json::to_vec(ops_value).ok()?),
            base_digest: sha256(raw.as_bytes()),
        })
    }

    fn sign_preview_binding(&self, preview: &ApprovalPreview) -> String {
        let mut payload = Vec::with_capacity(PREVIEW_BINDING_LEN);
        payload.push(PREVIEW_BINDING_VERSION);
        payload.extend_from_slice(uuid::Uuid::new_v4().as_bytes());
        payload.extend_from_slice(&preview.ops_digest);
        payload.extend_from_slice(&preview.base_digest);

        let mut mac = <HmacSha256 as Mac>::new_from_slice(&self.preview_binding_key)
            .expect("HMAC accepts a 32-byte key");
        mac.update(&payload);
        payload.extend_from_slice(&mac.finalize().into_bytes());
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(payload)
    }

    fn verify_preview_binding(
        &self,
        args: &serde_json::Value,
        raw_config: &str,
    ) -> Result<(), PreviewBindingError> {
        let encoded = args
            .get(APPROVAL_EXECUTION_BINDING_ARG)
            .and_then(serde_json::Value::as_str)
            .ok_or(PreviewBindingError::Invalid)?;
        let binding = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(encoded)
            .map_err(|_| PreviewBindingError::Invalid)?;
        if binding.len() != PREVIEW_BINDING_LEN || binding.first() != Some(&PREVIEW_BINDING_VERSION)
        {
            return Err(PreviewBindingError::Invalid);
        }
        let (payload, tag) = binding.split_at(PREVIEW_BINDING_PAYLOAD_LEN);
        let mut mac = <HmacSha256 as Mac>::new_from_slice(&self.preview_binding_key)
            .expect("HMAC accepts a 32-byte key");
        mac.update(payload);
        mac.verify_slice(tag)
            .map_err(|_| PreviewBindingError::Invalid)?;

        let ops = serde_json::to_vec(
            args.get("ops")
                .ok_or(PreviewBindingError::OperationsChanged)?,
        )
        .map_err(|_| PreviewBindingError::OperationsChanged)?;
        if payload[17..49] != sha256(&ops) {
            return Err(PreviewBindingError::OperationsChanged);
        }
        if payload[49..81] != sha256(raw_config.as_bytes()) {
            return Err(PreviewBindingError::ConfigChanged);
        }
        Ok(())
    }

    fn preview_binding_refusal(error: PreviewBindingError) -> ToolResult {
        let message = match error {
            PreviewBindingError::ConfigChanged => {
                "configuration changed since the approval preview was shown; the \
                 requested change was not applied. Re-run so the operator can review \
                 it against the current configuration."
            }
            PreviewBindingError::Invalid | PreviewBindingError::OperationsChanged => {
                "the host approval preview binding is missing, invalid, or belongs to \
                 different operations; the requested change was not applied. Re-run so \
                 the operator can review it again."
            }
        };
        ToolResult {
            success: false,
            output: ToolOutput::default(),
            error: Some(message.to_string()),
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

    /// The operator prompt is the secret-aware per-call approval summary, and
    /// there is no safe fallback: the raw arguments carry secret values that
    /// the generic summary would leak. If the summary cannot be produced the
    /// gate refuses instead of showing the arguments verbatim.
    fn requires_host_approval_summary(&self) -> bool {
        true
    }

    /// Mask every op `value` (and `comment`) before the arguments reach any
    /// log, audit, observer, or client sink. A patch value may be a bare
    /// secret written to a config secret path — the generic scrubber does not
    /// recognize it because it sits under the innocuous `value` key — so this
    /// redacts at the source. Pure and infallible: it never reads config, so
    /// no sink is left to fall back to the raw arguments. `path` and `op` are
    /// preserved so audit records stay useful; a config path names a setting,
    /// not a secret.
    fn redact_args_for_log(&self, args: &serde_json::Value) -> Option<serde_json::Value> {
        let mut redacted = args.clone();
        if let Some(obj) = redacted.as_object_mut() {
            obj.remove(APPROVAL_EXECUTION_BINDING_ARG);
        }
        if let Some(ops) = redacted.get_mut("ops").and_then(|v| v.as_array_mut()) {
            for op in ops.iter_mut() {
                if let Some(obj) = op.as_object_mut() {
                    if obj.contains_key("value") {
                        obj.insert("value".into(), serde_json::json!("[redacted]"));
                    }
                    if obj.contains_key("comment") {
                        obj.insert("comment".into(), serde_json::json!("[redacted]"));
                    }
                }
            }
        }
        Some(redacted)
    }

    /// The operator's approval prompt: what the ops are, and — the part the
    /// raw JSON never shows — what they do to each agent's resolved
    /// permissions. Everything here is computed from the ops against the
    /// on-disk config; none of it is model text. Returns `None` when the
    /// patch can't be previewed (unreadable config, ops that don't apply);
    /// because [`Self::requires_host_approval_summary`] is `true`, the gate
    /// then refuses rather than showing the raw arguments, and execution would
    /// refuse with the precise error regardless.
    fn approval_summary(&self, args: &serde_json::Value) -> Option<String> {
        self.build_approval_preview(args)
            .map(|preview| preview.text)
    }

    fn approval_summary_for_call(&self, args: &serde_json::Value) -> Option<ToolApprovalSummary> {
        let preview = self.build_approval_preview(args)?;
        let binding = self.sign_preview_binding(&preview);
        Some(ToolApprovalSummary::with_execution_binding(
            preview.text,
            serde_json::Value::String(binding),
        ))
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

        // Approval and operation policy are separate boundaries. In
        // particular, ReadOnly approval managers intentionally do not prompt
        // because mutating tools must refuse at execution. Keep that refusal
        // inside the tool as well as in registry policy so direct and nested
        // dispatch cannot turn config authoring into an unmetered write.
        if let Err(error) = self
            .security
            .enforce_tool_operation(ToolOperation::Act, self.name())
        {
            return Ok(ToolResult::err(error));
        }

        // Serialize this tool's read-modify-write against other `config_patch`
        // calls sharing this file: two concurrent calls must not each read the
        // same base and clobber the other's write. Held across the read, apply,
        // and save below.
        let write_lock = write_lock_for(&self.config_path);
        let _write_guard = write_lock.lock().await;

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
        let base_digest = sha256(raw.as_bytes());
        // Prompted calls carry a host-injected, tool-authenticated binding to
        // the exact ops and config bytes shown to the operator. It is
        // self-contained rather than stored in a shared ops-keyed cache, so
        // identical concurrent calls cannot overwrite each other and pending
        // previews cannot be evicted. Non-prompted standing grants carry no
        // binding and retain their existing execution semantics.
        if args.get(APPROVAL_EXECUTION_BINDING_ARG).is_some()
            && let Err(error) = self.verify_preview_binding(&args, &raw)
        {
            return Ok(Self::preview_binding_refusal(error));
        }
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

        // Version-check against writers outside this lock (the CLI, the
        // gateway, an editor). The write lock blocks other `config_patch`
        // calls; this re-read catches anyone else who changed the file since
        // the base read, so a concurrent update is failed explicitly rather
        // than silently clobbered by `save_dirty` rewriting from our base.
        match tokio::fs::read_to_string(&self.config_path).await {
            Ok(current) if sha256(current.as_bytes()) == base_digest => {}
            Ok(_) => {
                return Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some(
                        "configuration changed on disk while this patch was being applied; \
                         nothing was saved. Re-run to apply against the current configuration."
                            .to_string(),
                    ),
                });
            }
            Err(err) => {
                return Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some(format!(
                        "failed to re-read {} before saving: {err}",
                        self.config_path.display()
                    )),
                });
            }
        }

        if !working.save_dirty_if_source_unchanged(&raw).await? {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(
                    "configuration changed on disk while this patch was being applied; nothing \
                     was saved. Re-run to apply against the current configuration."
                        .to_string(),
                ),
            });
        }

        // Comments go on after save so the comment-preserving sync_table
        // pass doesn't strip them — same order as the gateway and CLI.
        let annotations: Vec<(String, String)> = ops
            .iter()
            .zip(results.iter())
            .filter_map(|(op, res)| op.comment.as_ref().map(|c| (res.path.clone(), c.clone())))
            .collect();
        let mut comment_error: Option<String> = None;
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
            comment_error = Some(err.to_string());
        }

        // Report the comment outcome honestly: the operator approved seeing
        // those comments persisted, so a best-effort failure must surface here
        // rather than the result claiming an unqualified success.
        let mut out = serde_json::json!({
            "saved": true,
            "results": results,
            "note": "written to config.toml; the running daemon keeps its current \
                     configuration until the operator reloads or restarts it"
        });
        if let Some(err) = comment_error {
            out["comments_applied"] = serde_json::Value::Bool(false);
            out["comment_error"] = serde_json::Value::String(format!(
                "the values were saved, but writing the approved comment(s) failed: {err}"
            ));
        }
        Ok(ToolResult::ok(ToolOutput::json(out)))
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

    fn config_patch_tool(path: PathBuf) -> ConfigPatchTool {
        ConfigPatchTool::new(path, Arc::new(SecurityPolicy::default()))
    }

    fn bind_preview(tool: &ConfigPatchTool, args: &mut serde_json::Value) -> String {
        let summary = tool
            .approval_summary_for_call(args)
            .expect("previewable call gets a per-call binding");
        let binding = summary
            .execution_binding
            .expect("config patch previews bind execution");
        args.as_object_mut()
            .expect("tool args are an object")
            .insert(APPROVAL_EXECUTION_BINDING_ARG.to_string(), binding);
        summary.text
    }

    #[tokio::test]
    async fn applies_a_replace_and_persists_it_to_disk() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = saved_config(dir.path()).await;
        let tool = config_patch_tool(path.clone());

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

    /// The required TOCTOU regression: if the configuration changes between the
    /// operator's preview and the apply, the approved ops must NOT be written —
    /// the effect the operator saw no longer matches the current base.
    #[tokio::test]
    async fn drift_between_preview_and_execute_is_refused_and_nothing_is_written() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = saved_config(dir.path()).await;
        let tool = config_patch_tool(path.clone());
        let mut args = serde_json::json!({
            "ops": [{"op": "replace", "path": "/gateway/host", "value": "10.0.0.99"}]
        });

        // Operator previews the change against the current base.
        bind_preview(&tool, &mut args);

        // The config changes underneath — a different writer edits an unrelated
        // field. A separate tool instance has no preview binding of its own.
        config_patch_tool(path.clone())
            .execute(serde_json::json!({
                "ops": [{"op": "replace", "path": "/gateway/port", "value": "4242"}]
            }))
            .await
            .expect("the concurrent edit applies");

        // Applying the previewed ops now must refuse: the base drifted.
        let result = tool.execute(args).await.expect("execute");
        assert!(!result.success, "a drifted apply must be refused");
        assert!(
            result
                .error
                .as_deref()
                .unwrap_or_default()
                .contains("changed since the approval preview"),
            "the refusal names the drift: {:?}",
            result.error
        );

        let saved = read_config(&path);
        assert_ne!(
            saved.gateway.host, "10.0.0.99",
            "the unapproved effect must never be written"
        );
        assert_eq!(
            saved.gateway.port, 4242,
            "the concurrent writer's change is preserved"
        );
    }

    /// Two turns may preview byte-identical operations on different config
    /// revisions. Each approval must remain bound to its own base instead of a
    /// shared ops-keyed slot that the later preview can overwrite.
    #[tokio::test]
    async fn concurrent_identical_previews_keep_distinct_config_bindings() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = saved_config(dir.path()).await;
        let tool = config_patch_tool(path.clone());
        let original_args = serde_json::json!({
            "ops": [{"op": "replace", "path": "/gateway/host", "value": "10.0.0.77"}]
        });
        let mut first = original_args.clone();
        bind_preview(&tool, &mut first);

        config_patch_tool(path.clone())
            .execute(serde_json::json!({
                "ops": [{"op": "replace", "path": "/gateway/port", "value": "4242"}]
            }))
            .await
            .expect("intervening config write");

        let mut second = original_args;
        bind_preview(&tool, &mut second);
        assert_ne!(
            first[APPROVAL_EXECUTION_BINDING_ARG], second[APPROVAL_EXECUTION_BINDING_ARG],
            "each approval call receives a fresh opaque binding"
        );

        let stale = tool.execute(first).await.expect("first execute");
        assert!(!stale.success, "the first preview is stale and must refuse");
        assert!(
            stale
                .error
                .as_deref()
                .is_some_and(|error| error.contains("changed since the approval preview")),
            "the refusal names config drift: {:?}",
            stale.error
        );
        assert_ne!(read_config(&path).gateway.host, "10.0.0.77");

        let current = tool.execute(second).await.expect("second execute");
        assert!(
            current.success,
            "the independently previewed current-base call may apply: {:?}",
            current.error
        );
        assert_eq!(read_config(&path).gateway.host, "10.0.0.77");
    }

    /// More than the old 64-entry map bound must not evict an outstanding
    /// approval. After config drift, the oldest call still has enough binding
    /// information to refuse instead of silently applying an unreviewed effect.
    #[tokio::test]
    async fn oldest_preview_still_refuses_after_more_than_sixty_four_new_previews() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = saved_config(dir.path()).await;
        let tool = config_patch_tool(path.clone());
        let mut oldest = serde_json::json!({
            "ops": [{"op": "replace", "path": "/gateway/host", "value": "10.0.0.88"}]
        });
        bind_preview(&tool, &mut oldest);

        for index in 0..65 {
            let mut filler = serde_json::json!({
                "ops": [{
                    "op": "comment",
                    "path": "/gateway/host",
                    "comment": format!("pending preview {index}")
                }]
            });
            bind_preview(&tool, &mut filler);
        }

        config_patch_tool(path.clone())
            .execute(serde_json::json!({
                "ops": [{"op": "replace", "path": "/gateway/port", "value": "4343"}]
            }))
            .await
            .expect("intervening config write");

        let result = tool.execute(oldest).await.expect("oldest execute");
        assert!(
            !result.success,
            "an old approved call must not lose its binding and apply after drift"
        );
        assert!(
            result
                .error
                .as_deref()
                .is_some_and(|error| error.contains("changed since the approval preview")),
            "the surviving binding detects drift: {:?}",
            result.error
        );
        let saved = read_config(&path);
        assert_ne!(saved.gateway.host, "10.0.0.88");
        assert_eq!(saved.gateway.port, 4343);
    }

    /// The operator prompt must show model-authored comment text (it is
    /// written to config.toml) and must not silently truncate a value; a
    /// truncated value states its full length.
    #[tokio::test]
    async fn prompt_shows_comment_text_and_marks_truncation() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = saved_config(dir.path()).await;
        let tool = config_patch_tool(path);

        let long = "x".repeat(300);
        let summary = tool
            .approval_summary(&serde_json::json!({
                "ops": [{
                    "op": "replace", "path": "/gateway/host",
                    "value": long, "comment": "set by the assistant"
                }]
            }))
            .expect("summary");

        assert!(
            summary.contains("comment: ") && summary.contains("set by the assistant"),
            "the operator must see the persisted comment: {summary}"
        );
        assert!(
            summary.contains("chars total, truncated"),
            "a truncated value must state its full length, not a silent ellipsis: {summary}"
        );
    }

    /// Two concurrent, disjoint patches must not lose an update: the per-path
    /// write lock serializes the read-modify-write, so the second call reads
    /// the first's result as its base. Both changes survive (or, had one raced
    /// past the base check, it would fail explicitly — never silently clobber).
    #[tokio::test]
    async fn concurrent_disjoint_patches_both_survive() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = saved_config(dir.path()).await;

        let host_tool = config_patch_tool(path.clone());
        let port_tool = config_patch_tool(path.clone());
        let (a, b) = tokio::join!(
            host_tool.execute(serde_json::json!({
                "ops": [{"op": "replace", "path": "/gateway/host", "value": "10.1.1.1"}]
            })),
            port_tool.execute(serde_json::json!({
                "ops": [{"op": "replace", "path": "/gateway/port", "value": "4343"}]
            })),
        );
        let a = a.expect("execute a");
        let b = b.expect("execute b");

        // Neither may silently clobber: any non-success must be an explicit
        // drift refusal, not a lost update.
        for r in [&a, &b] {
            if !r.success {
                assert!(
                    r.error.as_deref().unwrap_or_default().contains("changed"),
                    "a non-success must be an explicit drift refusal: {:?}",
                    r.error
                );
            }
        }
        // Under serialization both apply cleanly, so both updates survive.
        assert!(a.success && b.success, "both disjoint patches should apply");
        let saved = read_config(&path);
        assert_eq!(saved.gateway.host, "10.1.1.1", "host update survived");
        assert_eq!(saved.gateway.port, 4343, "port update survived");
    }

    /// Binding must not break the normal path: preview then apply with no drift
    /// still succeeds.
    #[tokio::test]
    async fn preview_then_execute_without_drift_applies() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = saved_config(dir.path()).await;
        let tool = config_patch_tool(path.clone());
        let mut args = serde_json::json!({
            "ops": [{"op": "replace", "path": "/gateway/host", "value": "10.0.0.42"}]
        });

        bind_preview(&tool, &mut args);
        let result = tool.execute(args).await.expect("execute");

        assert!(
            result.success,
            "an undrifted apply succeeds: {:?}",
            result.error
        );
        assert_eq!(read_config(&path).gateway.host, "10.0.0.42");
    }

    #[tokio::test]
    async fn model_supplied_preview_binding_is_rejected() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = saved_config(dir.path()).await;
        let before = std::fs::read_to_string(&path).expect("read before");
        let tool = config_patch_tool(path.clone());
        let mut args = serde_json::json!({
            "ops": [{"op": "replace", "path": "/gateway/host", "value": "10.0.0.66"}]
        });
        args.as_object_mut().expect("object args").insert(
            APPROVAL_EXECUTION_BINDING_ARG.to_string(),
            serde_json::json!("model-forged-binding"),
        );

        let result = tool.execute(args).await.expect("execute");
        assert!(
            !result.success,
            "an unauthenticated binding must fail closed"
        );
        assert!(
            result
                .error
                .as_deref()
                .is_some_and(|error| error.contains("binding is missing, invalid")),
            "the refusal names the binding failure: {:?}",
            result.error
        );
        assert_eq!(
            std::fs::read_to_string(path).expect("read after"),
            before,
            "a forged binding must not change config"
        );
    }

    #[tokio::test]
    async fn preview_binding_rejects_operations_changed_after_review() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = saved_config(dir.path()).await;
        let before = std::fs::read_to_string(&path).expect("read before");
        let tool = config_patch_tool(path.clone());
        let mut args = serde_json::json!({
            "ops": [{"op": "replace", "path": "/gateway/host", "value": "10.0.0.42"}]
        });
        bind_preview(&tool, &mut args);
        args["ops"][0]["value"] = serde_json::json!("10.0.0.43");

        let result = tool.execute(args).await.expect("execute");
        assert!(
            !result.success,
            "post-preview operation changes fail closed"
        );
        assert!(
            result
                .error
                .as_deref()
                .is_some_and(|error| error.contains("different operations")),
            "the refusal names the operation mismatch: {:?}",
            result.error
        );
        assert_eq!(
            std::fs::read_to_string(path).expect("read after"),
            before,
            "operations not shown to the operator must never be written"
        );
    }

    #[tokio::test]
    async fn read_only_policy_refuses_a_valid_patch_without_touching_disk() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = saved_config(dir.path()).await;
        let before = std::fs::read(&path).expect("read before");
        let tool = ConfigPatchTool::new(
            path.clone(),
            Arc::new(SecurityPolicy {
                autonomy: zeroclaw_config::autonomy::AutonomyLevel::ReadOnly,
                ..SecurityPolicy::default()
            }),
        );

        let result = tool
            .execute(serde_json::json!({
                "ops": [{"op": "replace", "path": "/gateway/host", "value": "127.0.0.2"}]
            }))
            .await
            .expect("execute");

        assert!(!result.success, "read-only config patch must fail");
        assert!(
            result
                .error
                .as_deref()
                .is_some_and(|error| error.contains("read-only mode")),
            "the refusal must name the active operation boundary: {:?}",
            result.error
        );
        assert_eq!(
            std::fs::read(&path).expect("read after"),
            before,
            "read-only execution must leave config.toml byte-identical"
        );
    }

    #[tokio::test]
    async fn an_invalid_op_is_refused_and_the_file_does_not_move() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = saved_config(dir.path()).await;
        let before = std::fs::read_to_string(&path).expect("read before");
        let tool = config_patch_tool(path.clone());

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
        let tool = config_patch_tool(path.clone());

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
        let tool = config_patch_tool(path);

        let result = tool.execute(serde_json::json!({})).await.expect("execute");

        assert!(!result.success);
        assert!(result.error.expect("error").contains("`ops`"));
    }

    #[tokio::test]
    async fn a_missing_config_file_is_reported_not_created() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        let tool = config_patch_tool(path.clone());

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
        let tool = config_patch_tool(path);

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
        let tool = config_patch_tool(path);

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
        let tool = config_patch_tool(path);

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
        let tool = config_patch_tool(path);

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
        let tool = config_patch_tool(path);

        let summary = tool.approval_summary(&serde_json::json!({
            "ops": [{"op": "frobnicate", "path": "/gateway/host", "value": "x"}]
        }));

        assert!(
            summary.is_none(),
            "ops that will be refused fall back to the generic summary"
        );
    }

    /// Log-facing redaction masks every op value and comment at the source,
    /// independent of config readability (it never reads config), so no audit,
    /// log, observer, or client sink can receive a raw secret — including the
    /// failed-preview path where `approval_summary` returns `None`.
    #[test]
    fn redact_args_for_log_masks_every_value_and_comment() {
        let tool = config_patch_tool(std::env::temp_dir().join("config.toml"));
        let mut args = serde_json::json!({
            "ops": [
                {"op": "add", "path": "/http_request/secrets/api_token",
                 "value": "sentinel-token-never-logged-0123", "comment": "sentinel-comment"},
                {"op": "replace", "path": "/gateway/host", "value": "10.0.0.1"},
                {"op": "remove", "path": "/gateway/tls"}
            ]
        });
        args.as_object_mut().expect("object args").insert(
            APPROVAL_EXECUTION_BINDING_ARG.to_string(),
            serde_json::json!("opaque-host-binding"),
        );
        let redacted = tool
            .redact_args_for_log(&args)
            .expect("config_patch redacts");
        let text = redacted.to_string();

        assert!(
            !text.contains("sentinel-token-never-logged") && !text.contains("sentinel-comment"),
            "no op value or comment may survive redaction: {text}"
        );
        // Even a non-secret value is masked for logs — the operator saw the
        // real values in the host-computed prompt; sinks do not need them.
        assert!(
            !text.contains("10.0.0.1"),
            "non-secret values are masked too: {text}"
        );
        // Paths and ops stay, so audit records remain useful.
        assert!(text.contains("http_request/secrets/api_token"));
        assert!(text.contains("gateway.host") || text.contains("/gateway/host"));
        assert_eq!(redacted["ops"][0]["value"], "[redacted]");
        assert_eq!(redacted["ops"][0]["comment"], "[redacted]");
        assert!(
            redacted.get(APPROVAL_EXECUTION_BINDING_ARG).is_none(),
            "the opaque binding is runtime-internal and must not reach sinks"
        );
    }

    #[test]
    fn schema_offers_no_free_text_narration_field() {
        let dir = std::env::temp_dir();
        let tool = config_patch_tool(dir.join("config.toml"));
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
