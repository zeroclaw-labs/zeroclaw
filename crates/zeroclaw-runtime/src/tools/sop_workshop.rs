use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::json;

use crate::sop::procedural_memory::{
    ProposalDraft, apply_proposal, capture_successful_run, create_proposal, set_proposal_status,
};
use crate::sop::{ProposalStatus, SopEngine};
use zeroclaw_api::tool::{Tool, ToolOutput, ToolResult};

macro_rules! sop_workshop_actions {
    ($($variant:ident => $wire:literal),+ $(,)?) => {
        #[derive(Clone, Copy)]
        enum SopWorkshopAction {
            $($variant),+
        }

        impl SopWorkshopAction {
            const ALL: [Self; sop_workshop_actions!(@count $($variant),+)] = [
                $(Self::$variant),+
            ];

            fn parse(value: &str) -> anyhow::Result<Self> {
                Self::ALL
                    .into_iter()
                    .find(|action| action.wire_name() == value)
                    .ok_or_else(|| anyhow::Error::msg(format!("Unsupported sop_workshop action: {value}")))
            }

            fn wire_name(self) -> &'static str {
                match self {
                    $(Self::$variant => $wire),+
                }
            }

            fn wire_names() -> Vec<&'static str> {
                Self::ALL
                    .into_iter()
                    .map(Self::wire_name)
                    .collect()
            }
        }
    };
    (@count $($variant:ident),+) => {
        <[()]>::len(&[$(sop_workshop_actions!(@unit $variant)),+])
    };
    (@unit $variant:ident) => {
        ()
    };
}

sop_workshop_actions! {
    Propose => "propose",
    CaptureRun => "capture_run",
    List => "list",
    Inspect => "inspect",
    Apply => "apply",
    Reject => "reject",
    Quarantine => "quarantine",
}

/// Agent-facing SOP proposal lifecycle tool.
pub struct SopWorkshopTool {
    engine: Arc<Mutex<SopEngine>>,
    /// Install root (`config.install_root_dir()`) that anchors SOP-definition
    /// writes. `apply` resolves `resolve_sops_dir(install_root, sops_dir)` (the
    /// documented `shared/sops` yields `<install>/shared/sops`) and reloads the
    /// engine from the same root, so it must match the root the engine was built
    /// from — otherwise apply would write to one tree and reload another.
    install_root: std::path::PathBuf,
}

impl SopWorkshopTool {
    pub fn new(engine: Arc<Mutex<SopEngine>>, install_root: std::path::PathBuf) -> Self {
        Self {
            engine,
            install_root,
        }
    }
}

impl ::zeroclaw_api::attribution::Attributable for SopWorkshopTool {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Sop
    }

    fn alias(&self) -> &str {
        "sop_workshop"
    }
}

#[async_trait]
impl Tool for SopWorkshopTool {
    fn name(&self) -> &str {
        "sop_workshop"
    }

    fn description(&self) -> &str {
        "Manage SOP procedural-memory proposals: propose, capture_run, list, inspect, apply, reject, or quarantine. Apply writes SOP.toml/SOP.md only after an explicit action."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        let action_names = SopWorkshopAction::wire_names();
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": action_names,
                    "description": "Proposal lifecycle action"
                },
                "id": {
                    "type": "string",
                    "description": "Proposal id for inspect/apply/reject/quarantine"
                },
                "status": {
                    "type": "string",
                    "description": "Optional list status filter"
                },
                "sop_name": {
                    "type": "string",
                    "description": "Target SOP name for propose"
                },
                "description": {
                    "type": "string",
                    "description": "SOP description for propose when manifest_toml is omitted"
                },
                "manifest_toml": {
                    "type": "string",
                    "description": "Proposed SOP.toml content; if omitted, a manual-trigger manifest is generated"
                },
                "procedure_markdown": {
                    "type": "string",
                    "description": "Proposed SOP.md content"
                },
                "source_run_id": {
                    "type": "string",
                    "description": "Source SOP run id for propose/capture_run"
                },
                "actor": {
                    "type": "string",
                    "description": "Operator or agent label recorded in proposal provenance"
                },
                "reason": {
                    "type": "string",
                    "description": "Reason for reject/quarantine"
                }
            },
            "required": ["action"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let action = args
            .get("action")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::Error::msg("Missing 'action' parameter"))?;
        let result = match SopWorkshopAction::parse(action)? {
            SopWorkshopAction::Propose => self.propose(&args),
            SopWorkshopAction::CaptureRun => self.capture_run(&args),
            SopWorkshopAction::List => self.list(&args),
            SopWorkshopAction::Inspect => self.inspect(&args),
            SopWorkshopAction::Apply => self.apply(&args),
            SopWorkshopAction::Reject => self.set_status(&args, ProposalStatus::Rejected),
            SopWorkshopAction::Quarantine => self.set_status(&args, ProposalStatus::Quarantined),
        };

        match result {
            Ok(output) => Ok(ToolResult {
                success: true,
                output: output.into(),
                error: None,
            }),
            Err(e) => Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(e.to_string()),
            }),
        }
    }
}

impl SopWorkshopTool {
    fn lock_engine(&self) -> anyhow::Result<std::sync::MutexGuard<'_, SopEngine>> {
        self.engine
            .lock()
            .map_err(|e| anyhow::Error::msg(format!("Engine lock poisoned: {e}")))
    }

    fn propose(&self, args: &serde_json::Value) -> anyhow::Result<String> {
        let sop_name = required_str(args, "sop_name")?;
        let description = required_str(args, "description")?;
        let procedure_markdown = required_str(args, "procedure_markdown")?;
        let manifest_toml = args
            .get("manifest_toml")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let source_run_id = args
            .get("source_run_id")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let actor = args
            .get("actor")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let engine = self.lock_engine()?;
        let proposal = create_proposal(
            &engine,
            ProposalDraft {
                sop_name: sop_name.to_string(),
                description: description.to_string(),
                manifest_toml,
                procedure_markdown: procedure_markdown.to_string(),
                source_run_id,
                requested_by: actor,
            },
        )?;
        Ok(serde_json::to_string_pretty(&proposal)?)
    }

    fn capture_run(&self, args: &serde_json::Value) -> anyhow::Result<String> {
        let run_id = required_str(args, "source_run_id")?;
        let actor = args
            .get("actor")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let engine = self.lock_engine()?;
        let proposal = capture_successful_run(&engine, run_id, actor)?;
        Ok(serde_json::to_string_pretty(&proposal)?)
    }

    fn list(&self, args: &serde_json::Value) -> anyhow::Result<String> {
        let status = args
            .get("status")
            .and_then(|v| v.as_str())
            .map(parse_status)
            .transpose()?;
        let engine = self.lock_engine()?;
        let mut proposals = engine.list_proposals(status)?;
        proposals.sort_by(|a, b| a.created_at.cmp(&b.created_at).then(a.id.cmp(&b.id)));
        let rows: Vec<_> = proposals
            .into_iter()
            .map(|p| {
                json!({
                    "id": p.id,
                    "status": p.status,
                    "kind": p.kind,
                    "sop_name": p.sop_name,
                    "source_run_id": p.source_run_id,
                    "created_at": p.created_at,
                    "updated_at": p.updated_at,
                    "status_reason": p.status_reason,
                })
            })
            .collect();
        Ok(serde_json::to_string_pretty(&rows)?)
    }

    fn inspect(&self, args: &serde_json::Value) -> anyhow::Result<String> {
        let id = required_str(args, "id")?;
        let engine = self.lock_engine()?;
        let proposal = engine
            .load_proposal(id)?
            .ok_or_else(|| anyhow::Error::msg(format!("proposal not found: {id}")))?;
        Ok(serde_json::to_string_pretty(&proposal)?)
    }

    fn apply(&self, args: &serde_json::Value) -> anyhow::Result<String> {
        let id = required_str(args, "id")?;
        let actor = args
            .get("actor")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let mut engine = self.lock_engine()?;
        let outcome = apply_proposal(&mut engine, &self.install_root, id, actor)?;
        Ok(serde_json::to_string_pretty(&json!({
            "id": outcome.proposal.id,
            "status": outcome.proposal.status,
            "sop_name": outcome.proposal.sop_name,
            "target_dir": outcome.target_dir,
            "rollback_path": outcome.proposal.rollback_path,
        }))?)
    }

    fn set_status(
        &self,
        args: &serde_json::Value,
        status: ProposalStatus,
    ) -> anyhow::Result<String> {
        let id = required_str(args, "id")?;
        let reason = args
            .get("reason")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let engine = self.lock_engine()?;
        let proposal = set_proposal_status(&engine, id, status, reason)?;
        Ok(serde_json::to_string_pretty(&proposal)?)
    }
}

fn required_str<'a>(args: &'a serde_json::Value, field: &str) -> anyhow::Result<&'a str> {
    args.get(field)
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| anyhow::Error::msg(format!("Missing '{field}' parameter")))
}

fn parse_status(status: &str) -> anyhow::Result<ProposalStatus> {
    serde_json::from_value(serde_json::Value::String(status.to_string()))
        .map_err(|e| anyhow::Error::msg(format!("invalid proposal status '{status}': {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sop::SopEngine;
    use zeroclaw_config::schema::SopConfig;

    #[tokio::test]
    async fn propose_and_list_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let engine = Arc::new(Mutex::new(SopEngine::new(SopConfig {
            sops_dir: Some(tmp.path().join("sops").display().to_string()),
            ..SopConfig::default()
        })));
        let tool = SopWorkshopTool::new(Arc::clone(&engine), tmp.path().to_path_buf());

        let proposed = tool
            .execute(json!({
                "action": "propose",
                "sop_name": "daily-check",
                "description": "Daily check",
                "procedure_markdown": "## Steps\n\n1. **Check** - Do it.\n",
                "actor": "test"
            }))
            .await
            .unwrap();
        assert!(proposed.success, "{:?}", proposed.error);
        assert!(proposed.output.contains("daily-check"));

        let listed = tool.execute(json!({"action": "list"})).await.unwrap();
        assert!(listed.success);
        assert!(listed.output.contains("daily-check"));
    }

    // Regression: apply resolves the configured `sops_dir` against the install
    // root the workshop was constructed with, so the documented `shared/sops`
    // lands at `<install>/shared/sops` (not doubled, not the data dir), and
    // reload must not drop SOPs that already live there. Mirrors build_sop_engine
    // wiring, where the workshop base is `config.install_root_dir()`.
    #[tokio::test]
    async fn apply_resolves_shared_sops_under_install_root_and_preserves_existing() {
        let tmp = tempfile::tempdir().unwrap();
        let install_root = tmp.path();
        // Canonical resolved directory for `sops_dir = "shared/sops"`.
        let sops_root = install_root.join("shared").join("sops");
        let data_dir = install_root.join("data");
        let agent_dir = install_root.join("agent-ws");
        for d in [&sops_root, &data_dir, &agent_dir] {
            std::fs::create_dir_all(d).unwrap();
        }

        // Pre-seed an existing SOP in the canonical `<install>/shared/sops` dir.
        let existing = sops_root.join("existing-sop");
        std::fs::create_dir_all(&existing).unwrap();
        std::fs::write(
            existing.join("SOP.toml"),
            "[sop]\nname = \"existing-sop\"\ndescription = \"pre-seeded\"\nversion = \"0.1.0\"\n\n[[triggers]]\ntype = \"manual\"\n",
        )
        .unwrap();
        std::fs::write(existing.join("SOP.md"), "## Steps\n\n1. **Do** - it.\n").unwrap();

        // Engine + workshop resolve against the INSTALL ROOT with the documented
        // relative value (mirrors build_sop_engine wiring).
        let engine = Arc::new(Mutex::new(SopEngine::new(SopConfig {
            sops_dir: Some("shared/sops".to_string()),
            ..SopConfig::default()
        })));
        engine.lock().unwrap().reload(install_root);
        assert!(
            engine.lock().unwrap().get_sop("existing-sop").is_some(),
            "pre-seeded shared SOP should load"
        );

        let tool = SopWorkshopTool::new(Arc::clone(&engine), install_root.to_path_buf());

        let proposed = tool
            .execute(json!({
                "action": "propose",
                "sop_name": "new-sop",
                "description": "Freshly proposed",
                "procedure_markdown": "## Steps\n\n1. **Check** - Do it.\n",
                "actor": "test"
            }))
            .await
            .unwrap();
        assert!(proposed.success, "{:?}", proposed.error);
        let proposal: serde_json::Value = serde_json::from_str(&proposed.output).unwrap();
        let id = proposal["id"].as_str().unwrap();

        let applied = tool
            .execute(json!({"action": "apply", "id": id, "actor": "test"}))
            .await
            .unwrap();
        assert!(applied.success, "{:?}", applied.error);

        // apply wrote to the canonical `<install>/shared/sops`, not a doubled
        // `shared/shared/sops`, the data dir, or the per-agent workspace.
        assert!(
            sops_root.join("new-sop").join("SOP.md").exists(),
            "new SOP must land under <install>/shared/sops"
        );
        assert!(
            !install_root
                .join("shared")
                .join("shared")
                .join("sops")
                .join("new-sop")
                .exists(),
            "apply must not double the shared segment"
        );
        assert!(
            !data_dir.join("sops").join("new-sop").exists(),
            "apply must not write under the data dir"
        );
        assert!(
            !agent_dir.join("sops").join("new-sop").exists(),
            "apply must not write under the per-agent workspace"
        );

        // reload after apply must keep both the pre-seeded and the new SOP.
        engine.lock().unwrap().reload(install_root);
        let guard = engine.lock().unwrap();
        assert!(
            guard.get_sop("existing-sop").is_some(),
            "reload must not drop the pre-existing shared SOP"
        );
        assert!(
            guard.get_sop("new-sop").is_some(),
            "reload must surface the newly applied SOP"
        );
    }
}
