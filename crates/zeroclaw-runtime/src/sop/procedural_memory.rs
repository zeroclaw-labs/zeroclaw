//! Procedural-memory proposal lifecycle for SOP definitions.

use std::fs;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde_json::json;
use sha2::{Digest, Sha256};

use super::engine::{SopEngine, now_iso8601};
use super::load_sops_from_directory;
use super::store::{ProposalKind, ProposalRecord, ProposalStatus};
use super::types::{Sop, SopRunStatus};
use crate::security::{LeakDetector, LeakResult};

#[derive(Debug, Clone)]
pub struct ProposalDraft {
    pub sop_name: String,
    pub description: String,
    pub manifest_toml: Option<String>,
    pub procedure_markdown: String,
    pub source_run_id: Option<String>,
    pub requested_by: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ApplyOutcome {
    pub proposal: ProposalRecord,
    pub target_dir: PathBuf,
}

pub fn create_proposal(engine: &SopEngine, draft: ProposalDraft) -> Result<ProposalRecord> {
    let sop_name = require_nonempty("sop_name", &draft.sop_name)?;
    let procedure_markdown = require_nonempty("procedure_markdown", &draft.procedure_markdown)?;
    let description = require_nonempty("description", &draft.description)?;
    let existing = engine.get_sop(sop_name);
    let kind = if existing.is_some() {
        ProposalKind::Update
    } else {
        ProposalKind::Create
    };
    let manifest_toml = match draft.manifest_toml {
        Some(toml) if !toml.trim().is_empty() => toml,
        _ => default_manifest_toml(sop_name, description),
    };
    if let Some(reason) = scan_candidate(&manifest_toml, procedure_markdown) {
        bail!("proposal rejected: {reason}");
    }
    validate_candidate(sop_name, &manifest_toml, procedure_markdown)?;
    let now = now_iso8601();
    let id = format!(
        "prop-{}-{}-{:08x}",
        slugify(sop_name),
        now.replace(':', "_"),
        rand::random::<u32>()
    );
    let target_content_hash = existing
        .and_then(|sop| sop.location.as_deref())
        .map(hash_sop_dir)
        .transpose()?;
    let proposal = ProposalRecord {
        id,
        kind,
        status: ProposalStatus::Pending,
        source_run_id: draft.source_run_id,
        sop_name: sop_name.to_string(),
        target_content_hash,
        manifest_toml,
        procedure_markdown: procedure_markdown.to_string(),
        provenance: json!({
            "producer": "sop_workshop",
            "requested_by": draft.requested_by,
        }),
        created_at: now.clone(),
        updated_at: now,
        status_reason: None,
        applied_at: None,
        applied_by: None,
        rollback_path: None,
    };
    engine.save_proposal(&proposal)?;
    Ok(proposal)
}

pub fn capture_successful_run(
    engine: &SopEngine,
    run_id: &str,
    requested_by: Option<String>,
) -> Result<ProposalRecord> {
    let run = engine
        .get_run(run_id)
        .ok_or_else(|| anyhow::Error::msg(format!("SOP run not found: {run_id}")))?;
    if run.status != SopRunStatus::Completed {
        bail!("only completed SOP runs can be captured");
    }
    if run.step_results.is_empty() {
        bail!("completed run has no step results to distill");
    }
    if run
        .step_results
        .iter()
        .any(|step| matches!(step.status, super::types::SopStepStatus::Failed))
    {
        bail!("failed step output is not captured into procedural memory");
    }

    let sop = engine
        .get_sop(&run.sop_name)
        .ok_or_else(|| anyhow::Error::msg(format!("SOP not loaded: {}", run.sop_name)))?;
    let manifest_toml = read_or_default_manifest(sop)?;
    let procedure_markdown = append_captured_notes(sop, run_id, &run.step_results)?;
    create_proposal(
        engine,
        ProposalDraft {
            sop_name: sop.name.clone(),
            description: sop.description.clone(),
            manifest_toml: Some(manifest_toml),
            procedure_markdown,
            source_run_id: Some(run_id.to_string()),
            requested_by,
        },
    )
}

pub fn apply_proposal(
    engine: &mut SopEngine,
    workspace_dir: &Path,
    proposal_id: &str,
    applied_by: Option<String>,
) -> Result<ApplyOutcome> {
    let mut proposal = load_required(engine, proposal_id)?;
    if proposal.status != ProposalStatus::Pending {
        bail!(
            "proposal {} is {:?}, not pending",
            proposal.id,
            proposal.status
        );
    }

    let sops_root = super::resolve_sops_dir(workspace_dir, engine.config().sops_dir.as_deref());
    // An Update must land on the currently-loaded SOP's actual directory, which
    // the loader sets from on-disk layout and need not match a slug of the name.
    // Create has no loaded SOP, so derive the new directory from the name.
    let target_dir = match proposal.kind {
        ProposalKind::Update => match engine
            .get_sop(&proposal.sop_name)
            .and_then(|sop| sop.location.clone())
        {
            Some(location) => contained_existing_dir(&sops_root, &location)?,
            None => contained_sop_dir(&sops_root, &proposal.sop_name)?,
        },
        ProposalKind::Create => contained_sop_dir(&sops_root, &proposal.sop_name)?,
    };
    let current_target_hash = if target_dir.exists() {
        Some(hash_sop_dir(&target_dir)?)
    } else {
        None
    };
    match (&proposal.kind, &proposal.target_content_hash) {
        (ProposalKind::Create, None) if current_target_hash.is_some() => {
            proposal.status = ProposalStatus::Stale;
            proposal.updated_at = now_iso8601();
            proposal.status_reason = Some("target SOP was created after proposal capture".into());
            engine.save_proposal(&proposal)?;
            bail!("proposal {} is stale; target SOP now exists", proposal.id);
        }
        (_, Some(expected)) if current_target_hash.as_ref() != Some(expected) => {
            proposal.status = ProposalStatus::Stale;
            proposal.updated_at = now_iso8601();
            proposal.status_reason = Some("target SOP changed since proposal capture".into());
            engine.save_proposal(&proposal)?;
            bail!("proposal {} is stale; inspect and re-propose", proposal.id);
        }
        _ => {}
    }

    if let Some(reason) = scan_candidate(&proposal.manifest_toml, &proposal.procedure_markdown) {
        proposal.status = ProposalStatus::Quarantined;
        proposal.updated_at = now_iso8601();
        proposal.status_reason = Some(reason.clone());
        engine.save_proposal(&proposal)?;
        bail!("proposal {} quarantined: {reason}", proposal.id);
    }

    validate_candidate(
        &proposal.sop_name,
        &proposal.manifest_toml,
        &proposal.procedure_markdown,
    )?;
    let rollback = write_rollback(&sops_root, &target_dir, &proposal.id)?;
    atomic_write_sop(
        &target_dir,
        &proposal.manifest_toml,
        &proposal.procedure_markdown,
    )?;

    proposal.status = ProposalStatus::Applied;
    proposal.updated_at = now_iso8601();
    proposal.applied_at = Some(proposal.updated_at.clone());
    proposal.applied_by = applied_by;
    proposal.rollback_path = rollback.as_ref().map(|p| p.display().to_string());
    proposal.status_reason = None;
    engine.save_proposal(&proposal)?;
    engine.reload(workspace_dir);

    Ok(ApplyOutcome {
        proposal,
        target_dir,
    })
}

pub fn set_proposal_status(
    engine: &SopEngine,
    proposal_id: &str,
    status: ProposalStatus,
    reason: Option<String>,
) -> Result<ProposalRecord> {
    let mut proposal = load_required(engine, proposal_id)?;
    proposal.status = status;
    proposal.updated_at = now_iso8601();
    proposal.status_reason = reason;
    engine.save_proposal(&proposal)?;
    Ok(proposal)
}

fn load_required(engine: &SopEngine, proposal_id: &str) -> Result<ProposalRecord> {
    engine
        .load_proposal(proposal_id)
        .map_err(anyhow::Error::new)?
        .ok_or_else(|| anyhow::Error::msg(format!("proposal not found: {proposal_id}")))
}

fn validate_candidate(sop_name: &str, manifest_toml: &str, procedure_markdown: &str) -> Result<()> {
    let tmp = tempfile::tempdir()?;
    let sop_dir = tmp.path().join(slugify(sop_name));
    fs::create_dir_all(&sop_dir)?;
    fs::write(sop_dir.join("SOP.toml"), manifest_toml)?;
    fs::write(sop_dir.join("SOP.md"), procedure_markdown)?;
    let sops = load_sops_from_directory(tmp.path(), super::parse_execution_mode("supervised"));
    if sops.len() != 1 {
        bail!("candidate SOP did not validate as exactly one loadable SOP");
    }
    if sops[0].name != sop_name {
        bail!(
            "candidate manifest name '{}' does not match proposal target '{}'",
            sops[0].name,
            sop_name
        );
    }
    if sops[0].steps.is_empty() {
        bail!("candidate SOP has no parsed steps");
    }
    Ok(())
}

fn scan_candidate(manifest_toml: &str, procedure_markdown: &str) -> Option<String> {
    let detector = LeakDetector::new();
    let content = format!("{manifest_toml}\n{procedure_markdown}");
    match detector.scan(&content) {
        LeakResult::Clean => None,
        LeakResult::Detected { patterns, .. } => Some(format!(
            "credential-like content detected: {}",
            patterns.join(", ")
        )),
    }
}

fn read_or_default_manifest(sop: &Sop) -> Result<String> {
    if let Some(location) = &sop.location {
        let path = location.join("SOP.toml");
        if path.exists() {
            return fs::read_to_string(path).context("read existing SOP.toml");
        }
    }
    Ok(default_manifest_toml(&sop.name, &sop.description))
}

fn append_captured_notes(
    sop: &Sop,
    run_id: &str,
    results: &[super::types::SopStepResult],
) -> Result<String> {
    let mut md = if let Some(location) = &sop.location {
        let path = location.join("SOP.md");
        if path.exists() {
            fs::read_to_string(path).context("read existing SOP.md")?
        } else {
            default_procedure_markdown(sop)
        }
    } else {
        default_procedure_markdown(sop)
    };
    if !md.ends_with('\n') {
        md.push('\n');
    }
    md.push_str("\n## Captured Run Notes\n\n");
    md.push_str(&format!("Source run: `{run_id}`\n\n"));
    for result in results {
        md.push_str(&format!(
            "- Step {} {}: {}\n",
            result.step_number,
            result.status,
            crate::security::scrub(&result.output)
        ));
    }
    Ok(md)
}

fn default_manifest_toml(name: &str, description: &str) -> String {
    format!(
        "[sop]\nname = \"{}\"\ndescription = \"{}\"\nversion = \"0.1.0\"\n\n[[triggers]]\ntype = \"manual\"\n",
        toml_escape(name),
        toml_escape(description)
    )
}

fn default_procedure_markdown(sop: &Sop) -> String {
    let mut md = format!("# {}\n\n## Steps\n\n", sop.name);
    for step in &sop.steps {
        md.push_str(&format!(
            "{}. **{}** - {}\n",
            step.number, step.title, step.body
        ));
        if !step.suggested_tools.is_empty() {
            md.push_str(&format!(
                "   - tools: {}\n",
                step.suggested_tools.join(", ")
            ));
        }
        if step.requires_confirmation {
            md.push_str("   - requires_confirmation: true\n");
        }
        md.push('\n');
    }
    md
}

fn write_rollback(
    sops_root: &Path,
    target_dir: &Path,
    proposal_id: &str,
) -> Result<Option<PathBuf>> {
    if !target_dir.exists() {
        return Ok(None);
    }
    let rollback_dir = sops_root
        .join(".rollback")
        .join(safe_component(proposal_id));
    fs::create_dir_all(&rollback_dir)?;
    for name in ["SOP.toml", "SOP.md"] {
        let src = target_dir.join(name);
        if src.exists() {
            fs::copy(&src, rollback_dir.join(name))?;
        }
    }
    Ok(Some(rollback_dir))
}

fn atomic_write_sop(
    target_dir: &Path,
    manifest_toml: &str,
    procedure_markdown: &str,
) -> Result<()> {
    fs::create_dir_all(target_dir)?;
    atomic_write_file(&target_dir.join("SOP.toml"), manifest_toml)?;
    atomic_write_file(&target_dir.join("SOP.md"), procedure_markdown)?;
    Ok(())
}

fn atomic_write_file(path: &Path, content: &str) -> Result<()> {
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, content)?;
    fs::rename(&tmp, path)?;
    Ok(())
}

fn contained_sop_dir(sops_root: &Path, sop_name: &str) -> Result<PathBuf> {
    let slug = slugify(sop_name);
    if slug.is_empty() {
        bail!("SOP name does not contain a safe path component");
    }
    ensure_relative_component(&slug)?;
    let target = sops_root.join(slug);
    ensure_within_root(sops_root, &target)?;
    Ok(target)
}

/// Validate that an already-existing SOP directory (taken from the loaded
/// `Sop.location`) stays within `sops_root`, rejecting `..` and symlink escapes.
fn contained_existing_dir(sops_root: &Path, location: &Path) -> Result<PathBuf> {
    let target = location.to_path_buf();
    ensure_within_root(sops_root, &target)?;
    Ok(target)
}

fn ensure_within_root(sops_root: &Path, target: &Path) -> Result<()> {
    let root = resolve_existing_ancestor(sops_root)?;
    let resolved_target = resolve_existing_ancestor(target)?;
    if !resolved_target.starts_with(&root) {
        bail!(
            "target SOP directory '{}' escapes SOPs root",
            target.display()
        );
    }
    Ok(())
}

/// Canonicalize the nearest existing ancestor of `path` and re-append the
/// not-yet-created trailing components verbatim. A `..` or symlink that escapes
/// is caught because the existing portion is canonicalized; a trailing
/// component is rejected unless it is a plain name.
fn resolve_existing_ancestor(path: &Path) -> Result<PathBuf> {
    let mut remainder: Vec<&std::ffi::OsStr> = Vec::new();
    let mut current = path;
    loop {
        if current.exists() {
            let mut resolved = fs::canonicalize(current)
                .with_context(|| format!("canonicalize '{}'", current.display()))?;
            for name in remainder.iter().rev() {
                resolved.push(name);
            }
            return Ok(resolved);
        }
        match (current.file_name(), current.parent()) {
            (Some(name), Some(parent)) => {
                let component = Path::new(name);
                if component
                    .components()
                    .any(|c| !matches!(c, Component::Normal(_)))
                {
                    bail!("unsafe SOP path component '{}'", name.to_string_lossy());
                }
                remainder.push(name);
                current = parent;
            }
            _ => bail!("cannot resolve SOP path '{}'", path.display()),
        }
    }
}

fn ensure_relative_component(component: &str) -> Result<()> {
    let path = Path::new(component);
    if path
        .components()
        .any(|c| !matches!(c, Component::Normal(_)))
    {
        bail!("unsafe SOP path component");
    }
    Ok(())
}

fn hash_sop_dir(dir: &Path) -> Result<String> {
    let mut hasher = Sha256::new();
    for name in ["SOP.toml", "SOP.md"] {
        let path = dir.join(name);
        hasher.update(name.as_bytes());
        hasher.update([0]);
        if path.exists() {
            hasher.update(fs::read(path)?);
        }
        hasher.update([0]);
    }
    Ok(format!("sha256:{}", hex::encode(hasher.finalize())))
}

fn require_nonempty<'a>(field: &str, value: &'a str) -> Result<&'a str> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        bail!("{field} cannot be empty");
    }
    Ok(trimmed)
}

fn safe_component(value: &str) -> String {
    let out = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '-'
            }
        })
        .collect::<String>();
    out.trim_matches('-').to_string()
}

fn slugify(value: &str) -> String {
    let mut out = String::new();
    let mut last_dash = false;
    for ch in value.chars().flat_map(char::to_lowercase) {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    out.trim_matches('-').to_string()
}

fn toml_escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sop::types::{
        Sop, SopEvent, SopExecutionMode, SopPriority, SopStep, SopStepResult, SopStepStatus,
        SopTrigger, SopTriggerSource,
    };
    use zeroclaw_config::schema::SopConfig;

    fn engine_with_workspace(tmp: &Path) -> SopEngine {
        SopEngine::new(SopConfig {
            sops_dir: Some(tmp.join("sops").display().to_string()),
            ..SopConfig::default()
        })
    }

    fn test_sop(name: &str) -> Sop {
        Sop {
            name: name.into(),
            description: "Source SOP".into(),
            version: "1.0.0".into(),
            priority: SopPriority::Normal,
            execution_mode: SopExecutionMode::Auto,
            triggers: vec![SopTrigger::Manual],
            steps: vec![SopStep {
                number: 1,
                title: "Do".into(),
                body: "Thing".into(),
                ..SopStep::default()
            }],
            cooldown_secs: 0,
            max_concurrent: 1,
            location: None,
            deterministic: false,
            admission_policy: crate::sop::types::SopAdmissionPolicy::Parallel,
            max_pending_approvals: 0,
            agent: None,
        }
    }

    #[test]
    fn proposal_apply_writes_files_and_reloads() {
        let tmp = tempfile::tempdir().unwrap();
        let mut engine = engine_with_workspace(tmp.path());
        let proposal = create_proposal(
            &engine,
            ProposalDraft {
                sop_name: "learned-check".into(),
                description: "Learned check".into(),
                manifest_toml: None,
                procedure_markdown: "# Learned\n\n## Steps\n\n1. **Check** - Do it.\n".into(),
                source_run_id: None,
                requested_by: Some("test".into()),
            },
        )
        .unwrap();

        let outcome =
            apply_proposal(&mut engine, tmp.path(), &proposal.id, Some("tester".into())).unwrap();

        assert_eq!(outcome.proposal.status, ProposalStatus::Applied);
        assert!(outcome.target_dir.join("SOP.toml").exists());
        assert!(engine.get_sop("learned-check").is_some());
    }

    #[test]
    fn apply_marks_stale_when_target_hash_changes() {
        let tmp = tempfile::tempdir().unwrap();
        let sops = tmp.path().join("sops").join("existing");
        fs::create_dir_all(&sops).unwrap();
        fs::write(
            sops.join("SOP.toml"),
            "[sop]\nname = \"existing\"\ndescription = \"Existing\"\n\n[[triggers]]\ntype = \"manual\"\n",
        )
        .unwrap();
        fs::write(sops.join("SOP.md"), "## Steps\n\n1. **Old** - Do it.\n").unwrap();

        let mut engine = engine_with_workspace(tmp.path());
        engine.reload(tmp.path());
        let proposal = create_proposal(
            &engine,
            ProposalDraft {
                sop_name: "existing".into(),
                description: "Existing".into(),
                manifest_toml: None,
                procedure_markdown: "## Steps\n\n1. **New** - Do it.\n".into(),
                source_run_id: None,
                requested_by: None,
            },
        )
        .unwrap();
        fs::write(sops.join("SOP.md"), "## Steps\n\n1. **Changed** - Do it.\n").unwrap();

        assert!(apply_proposal(&mut engine, tmp.path(), &proposal.id, None).is_err());
        let stored = engine.load_proposal(&proposal.id).unwrap().unwrap();
        assert_eq!(stored.status, ProposalStatus::Stale);
    }

    #[test]
    fn apply_update_targets_loaded_location_not_name_slug() {
        let tmp = tempfile::tempdir().unwrap();
        // On-disk directory name ("foo") intentionally differs from the manifest
        // SOP name ("daily-check"); the loader keys off the manifest name but
        // records the actual directory as the location.
        let sops = tmp.path().join("sops").join("foo");
        fs::create_dir_all(&sops).unwrap();
        fs::write(
            sops.join("SOP.toml"),
            "[sop]\nname = \"daily-check\"\ndescription = \"Daily check\"\n\n[[triggers]]\ntype = \"manual\"\n",
        )
        .unwrap();
        fs::write(sops.join("SOP.md"), "## Steps\n\n1. **Old** - Do it.\n").unwrap();

        let mut engine = engine_with_workspace(tmp.path());
        engine.reload(tmp.path());
        assert_eq!(
            engine
                .get_sop("daily-check")
                .and_then(|sop| sop.location.clone()),
            Some(sops.clone())
        );

        let proposal = create_proposal(
            &engine,
            ProposalDraft {
                sop_name: "daily-check".into(),
                description: "Daily check".into(),
                manifest_toml: None,
                procedure_markdown: "## Steps\n\n1. **New** - Do it.\n".into(),
                source_run_id: None,
                requested_by: None,
            },
        )
        .unwrap();

        let outcome = apply_proposal(&mut engine, tmp.path(), &proposal.id, None).unwrap();

        assert_eq!(outcome.proposal.status, ProposalStatus::Applied);
        assert_eq!(outcome.target_dir, sops);
        // The original directory was updated in place with the new content
        // (create_proposal trims trailing whitespace from the markdown).
        assert_eq!(
            fs::read_to_string(sops.join("SOP.md")).unwrap(),
            "## Steps\n\n1. **New** - Do it."
        );
        // No new slug-named directory was created next to the real one.
        assert!(!tmp.path().join("sops").join("daily-check").exists());
    }

    #[test]
    fn apply_marks_create_stale_when_target_appears() {
        let tmp = tempfile::tempdir().unwrap();
        let mut engine = engine_with_workspace(tmp.path());
        let proposal = create_proposal(
            &engine,
            ProposalDraft {
                sop_name: "new-sop".into(),
                description: "New SOP".into(),
                manifest_toml: None,
                procedure_markdown: "## Steps\n\n1. **New** - Do it.\n".into(),
                source_run_id: None,
                requested_by: None,
            },
        )
        .unwrap();
        let sops = tmp.path().join("sops").join("new-sop");
        fs::create_dir_all(&sops).unwrap();
        fs::write(
            sops.join("SOP.toml"),
            "[sop]\nname = \"new-sop\"\ndescription = \"New SOP\"\n\n[[triggers]]\ntype = \"manual\"\n",
        )
        .unwrap();
        fs::write(sops.join("SOP.md"), "## Steps\n\n1. **Other** - Do it.\n").unwrap();

        assert!(apply_proposal(&mut engine, tmp.path(), &proposal.id, None).is_err());
        let stored = engine.load_proposal(&proposal.id).unwrap().unwrap();
        assert_eq!(stored.status, ProposalStatus::Stale);
        assert_eq!(
            fs::read_to_string(sops.join("SOP.md")).unwrap(),
            "## Steps\n\n1. **Other** - Do it.\n"
        );
    }

    #[test]
    fn capture_successful_run_appends_redacted_notes() {
        let tmp = tempfile::tempdir().unwrap();
        let mut engine = engine_with_workspace(tmp.path());
        engine.set_sops_for_test(vec![test_sop("source")]);
        let event = SopEvent {
            source: SopTriggerSource::Manual,
            topic: None,
            payload: None,
            timestamp: "2026-06-30T00:00:00Z".into(),
        };
        let action = engine.start_run("source", event).unwrap();
        let run_id = match action {
            crate::sop::types::SopRunAction::ExecuteStep { run_id, .. } => run_id,
            other => panic!("expected execute action, got {other:?}"),
        };
        let redaction_fixture = ["tok", "en=", "neutralplaceholder1234567890"].concat();
        engine
            .advance_step(
                &run_id,
                SopStepResult {
                    effective_agent: None,
                    step_number: 1,
                    status: SopStepStatus::Completed,
                    output: format!("used {redaction_fixture}"),
                    started_at: "2026-06-30T00:00:00Z".into(),
                    completed_at: Some("2026-06-30T00:01:00Z".into()),
                    tool_calls: Vec::new(),
                },
            )
            .unwrap();

        let proposal = capture_successful_run(&engine, &run_id, Some("test".into())).unwrap();

        assert_eq!(proposal.source_run_id.as_deref(), Some(run_id.as_str()));
        assert!(proposal.procedure_markdown.contains("Captured Run Notes"));
        assert!(!proposal.procedure_markdown.contains(&redaction_fixture));
    }

    #[test]
    fn create_proposal_rejects_credential_content_before_storing() {
        let tmp = tempfile::tempdir().unwrap();
        let engine = engine_with_workspace(tmp.path());
        // Use the same token= pattern as the redaction test; 20+ char value triggers generic-secret detection.
        let credential = ["tok", "en=", "neutralplaceholder1234567890"].concat();
        let md = format!("## Steps\n\n1. **Auth** - Use {credential} to authenticate.\n");

        let result = create_proposal(
            &engine,
            ProposalDraft {
                sop_name: "cred-test".into(),
                description: "Credential test".into(),
                manifest_toml: None,
                procedure_markdown: md.clone(),
                source_run_id: None,
                requested_by: None,
            },
        );

        assert!(
            result.is_err(),
            "create_proposal must reject credential-like content"
        );
        // No pending or quarantined record should exist in the store.
        let all = engine.list_proposals(None).unwrap_or_default();
        assert!(
            all.iter()
                .all(|p| !p.procedure_markdown.contains(&credential)),
            "raw credential must not be stored in any proposal record"
        );
    }
}
