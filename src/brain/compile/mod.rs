//! `augusta brain compile` — render per-agent AGENTS.md/SOUL.md/TOOLS.md
//! from `~/.brain/` into Paperclip's managed instructions directory.
//!
//! Replaces the old plugin-side compiler. Output is narrative prose, not raw YAML.
//! Writes atomically into `~/.paperclip/instances/default/companies/<co>/agents/<id>/instructions/`.

mod agents_api;
mod render;
mod sources;
mod writer;

use anyhow::{Context, Result};
use std::path::PathBuf;

pub struct CompileOptions {
    pub brain_dir: PathBuf,
    pub agent_id: Option<String>,
    pub force: bool,
    pub dry_run: bool,
    pub paperclip_host: String,
}

pub struct CompileReport {
    pub total_agents: usize,
    pub written: usize,
    pub skipped_unchanged: usize,
    pub failed: usize,
    pub dry_run: bool,
    pub errors: Vec<String>,
}

pub async fn run(opts: CompileOptions) -> Result<CompileReport> {
    let brain = sources::load(&opts.brain_dir)
        .with_context(|| format!("loading brain from {}", opts.brain_dir.display()))?;

    let companies = agents_api::list_companies(&opts.paperclip_host).await?;
    let mut report = CompileReport {
        total_agents: 0,
        written: 0,
        skipped_unchanged: 0,
        failed: 0,
        dry_run: opts.dry_run,
        errors: Vec::new(),
    };

    let paperclip_root = paperclip_instances_root();

    for company in companies {
        if company.status != "active" {
            continue;
        }
        let agents = agents_api::list_agents(&opts.paperclip_host, &company.id).await?;
        for agent in agents {
            if let Some(target) = &opts.agent_id {
                if &agent.id != target {
                    continue;
                }
            }
            report.total_agents += 1;

            let bundle = render::render_agent(&brain, &agent);
            let agent_dir = paperclip_root
                .join("companies")
                .join(&company.id)
                .join("agents")
                .join(&agent.id)
                .join("instructions");

            match writer::write_bundle(&agent_dir, &bundle, opts.force, opts.dry_run) {
                Ok(writer::WriteOutcome::Wrote) => report.written += 1,
                Ok(writer::WriteOutcome::Unchanged) => report.skipped_unchanged += 1,
                Ok(writer::WriteOutcome::WouldWrite) => report.written += 1,
                Err(err) => {
                    report.failed += 1;
                    report
                        .errors
                        .push(format!("{} ({}): {err}", agent.name, agent.id));
                }
            }
        }
    }

    Ok(report)
}

fn paperclip_instances_root() -> PathBuf {
    if let Ok(custom) = std::env::var("PAPERCLIP_INSTANCES_ROOT") {
        return PathBuf::from(custom);
    }
    directories::UserDirs::new()
        .map(|u| u.home_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join(".paperclip/instances/default")
}
