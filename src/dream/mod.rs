//! CLI handler for the `zeroclaw dream` command.
//!
//! Dream mode is opt-in per agent and scoped per agent: each cycle runs over a
//! single agent's own memory, through that agent's own `model_provider`, with
//! pending/report state in that agent's workspace dir. The manual CLI therefore
//! always targets one agent — selected with `--agent <alias>`, or inferred when
//! the install has a single (or single dream-enabled) agent.

use anyhow::{Context, Result};
use zeroclaw_config::schema::Config;
use zeroclaw_runtime::dream::pending::DreamPending;
use zeroclaw_runtime::dream::report::DreamReport;
use zeroclaw_runtime::i18n::{get_cli_string_with_args, get_required_cli_string};

/// Resolve the single agent a manual `dream` invocation targets.
///
/// With `--agent`, use it (manual override — runs even if the agent hasn't
/// opted in). Otherwise infer: prefer the unique dream-enabled agent, else the
/// unique configured agent. Ambiguity (multiple candidates) is an error asking
/// for `--agent`.
fn resolve_target_agent(config: &Config, agent: Option<&str>) -> Result<String> {
    if let Some(a) = agent {
        let cfg = config
            .agents
            .get(a)
            .with_context(|| format!("dream: no agent '{a}' configured"))?;
        anyhow::ensure!(cfg.enabled, "dream: agent '{a}' is disabled");
        return Ok(a.to_string());
    }

    // Inference: prefer the unique dream-enabled agent, else the unique
    // *enabled* configured agent. Disabled agents are never inferred — this
    // matches the daemon, which only dreams enabled, opted-in agents.
    let enabled = config.agents_with_dream_enabled();
    let candidates: Vec<&str> = if enabled.is_empty() {
        config
            .agents
            .iter()
            .filter(|(_, a)| a.enabled)
            .map(|(alias, _)| alias.as_str())
            .collect()
    } else {
        enabled
    };

    match candidates.as_slice() {
        [only] => Ok((*only).to_string()),
        [] => anyhow::bail!("dream: no enabled agents configured"),
        _ => anyhow::bail!(
            "dream: multiple agents configured — specify which to run with --agent <alias>"
        ),
    }
}

/// Run a manual dream cycle for one agent from the CLI.
pub async fn run_dream(
    config: &Config,
    agent: Option<&str>,
    dry_run: bool,
    verbose: bool,
) -> Result<()> {
    use zeroclaw_runtime::dream::engine::DreamEngine;

    let agent_alias = resolve_target_agent(config, agent)?;
    let dream_cfg = config.effective_dream_config(&agent_alias);
    let resolved = config.resolved_model_provider_for_agent(&agent_alias);

    // Opt-in LLM: build the agent's own provider only when a model is set.
    let (provider, model): (
        Option<Box<dyn zeroclaw_api::model_provider::ModelProvider>>,
        Option<String>,
    ) = if dream_cfg.model.is_some() {
        let (family, alias, entry) = resolved.with_context(|| {
            format!(
                "dream: agent '{agent_alias}' has dream_mode.model set but no resolvable model_provider"
            )
        })?;
        let provider_ref = format!("{family}.{alias}");
        let model_name = dream_cfg
            .model
            .clone()
            .or_else(|| entry.model.clone())
            .unwrap_or_else(|| "claude-haiku-4-5-20251001".to_string());

        let provider_runtime_options =
            zeroclaw_providers::provider_runtime_options_for_agent(config, &agent_alias);
        let p = zeroclaw_providers::create_routed_model_provider_with_options(
            config,
            &provider_ref,
            entry.api_key.as_deref(),
            entry.uri.as_deref(),
            &config.reliability,
            &config.model_routes,
            &model_name,
            &provider_runtime_options,
        )?;
        (Some(p), Some(model_name))
    } else {
        (None, None)
    };

    // Agent-scoped memory backend — gather/prune/consolidate stay within this
    // agent's own memory.
    let api_key = resolved.and_then(|(_, _, e)| e.api_key.as_deref());
    let memory = zeroclaw_memory::create_memory_for_agent(config, &agent_alias, api_key)
        .await
        .context("dream: failed to create scoped memory backend")?;

    // Pending/report files live in this agent's own workspace dir.
    let workspace = config.agent_workspace_dir(&agent_alias);
    std::fs::create_dir_all(&workspace).ok();

    let audit_mode = dream_cfg.audit_mode;
    let engine = DreamEngine::new(dream_cfg, workspace);

    if verbose {
        let mode_str = if model.is_some() {
            "LLM-assisted"
        } else {
            "local-only"
        };
        let model_display = model.as_deref().unwrap_or("(none)");
        println!(
            "{}",
            get_cli_string_with_args("cli-dream-agent", &[("agent", agent_alias.as_str())])
                .unwrap_or_else(|| format!("Agent: {agent_alias}"))
        );
        println!(
            "{}",
            get_cli_string_with_args(
                "cli-dream-starting",
                &[
                    ("provider", mode_str),
                    ("model", model_display),
                    ("backend", memory.name()),
                ],
            )
            .unwrap_or_else(|| format!(
                "Dream cycle starting...\n  Mode: {mode_str}\n  Model: {model_display}\n  Memory backend: {}",
                memory.name()
            ))
        );
        if dry_run {
            println!("{}", get_required_cli_string("cli-dream-dry-run-mode"));
        }
    }

    let result = engine
        .run_cycle_with_options(
            memory.as_ref(),
            provider.as_ref().map(|p| p.as_ref()),
            model.as_deref(),
            dry_run,
        )
        .await?;

    println!(
        "{}",
        get_cli_string_with_args(
            "cli-dream-complete",
            &[
                ("gathered", &result.gathered_count.to_string()),
                ("consolidated", &result.consolidated_count.to_string()),
                ("pruned", &result.pruned_count.to_string()),
            ],
        )
        .unwrap_or_else(|| format!(
            "Dream cycle complete: {} memories gathered, {} insights consolidated, {} pruned",
            result.gathered_count, result.consolidated_count, result.pruned_count
        ))
    );

    if !result.insights.is_empty() {
        println!("\n{}", get_required_cli_string("cli-dream-insights-header"));
        for (i, insight) in result.insights.iter().enumerate() {
            println!("  {}. {insight}", i + 1);
        }
    }

    if let Some(ref summary) = result.report_summary {
        println!(
            "\n{}",
            get_cli_string_with_args("cli-dream-summary", &[("summary", summary.as_str())])
                .unwrap_or_else(|| format!("Summary: {summary}"))
        );
    }

    if dry_run {
        println!("\n{}", get_required_cli_string("cli-dream-dry-run-notice"));
    } else if audit_mode {
        println!("\n{}", get_required_cli_string("cli-dream-staged-notice"));
    }

    Ok(())
}

/// Show the pending dream report for an agent, if any.
pub fn show_report(config: &Config, agent: Option<&str>) -> Result<()> {
    let agent_alias = resolve_target_agent(config, agent)?;
    let dir = config.agent_workspace_dir(&agent_alias);
    match DreamReport::load_pending(&dir)? {
        Some(report) => {
            println!("{}", report.format_message());
            DreamReport::mark_delivered(&dir)?;
        }
        None => {
            println!("{}", get_required_cli_string("cli-dream-no-report"));
        }
    }
    Ok(())
}

/// Promote an agent's staged dream mutations from its `dream_pending.json`.
///
/// Delegates to `zeroclaw_runtime::dream::pending::promote_pending`, which
/// preserves the pending file on partial backend failures so the user can
/// retry without losing staged work.
pub async fn promote(config: &Config, agent: Option<&str>) -> Result<()> {
    use zeroclaw_runtime::dream::pending::promote_pending;

    let agent_alias = resolve_target_agent(config, agent)?;
    let dir = config.agent_workspace_dir(&agent_alias);

    // Snapshot pending counts up front for the "Promoting N insights..." banner.
    let Some(pending_view) = DreamPending::load(&dir)? else {
        println!("{}", get_required_cli_string("cli-dream-no-pending"));
        return Ok(());
    };

    println!(
        "{}",
        get_cli_string_with_args(
            "cli-dream-promote-summary",
            &[
                ("insights", &pending_view.insights.len().to_string()),
                ("prunes", &pending_view.proposed_prunes.len().to_string()),
            ],
        )
        .unwrap_or_else(|| format!(
            "Promoting {} insights, pruning {} stale keys...",
            pending_view.insights.len(),
            pending_view.proposed_prunes.len()
        ))
    );

    // Apply against the agent's own scoped memory backend.
    let api_key = config
        .resolved_model_provider_for_agent(&agent_alias)
        .and_then(|(_, _, e)| e.api_key.as_deref());
    let memory = zeroclaw_memory::create_memory_for_agent(config, &agent_alias, api_key)
        .await
        .context("dream promote: failed to create scoped memory backend")?;

    let Some(result) = promote_pending(&dir, memory.as_ref(), config.dream_mode.hard_prune).await?
    else {
        // The pending file was removed between the snapshot above and now
        // (e.g. a concurrent promote, or a manual delete). Nothing to apply —
        // don't panic on the missing file.
        println!("{}", get_required_cli_string("cli-dream-no-pending"));
        return Ok(());
    };

    println!(
        "{}",
        get_cli_string_with_args(
            "cli-dream-promote-done",
            &[
                ("stored", &result.stored.to_string()),
                ("pruned", &result.pruned.to_string()),
            ],
        )
        .unwrap_or_else(|| format!(
            "Done: {} insights stored, {} memories pruned.",
            result.stored, result.pruned
        ))
    );

    if result.pending_retained {
        let failed_total = result.failed_insights.len() + result.failed_prunes.len();
        let failed_str = failed_total.to_string();
        println!(
            "{}",
            get_cli_string_with_args(
                "cli-dream-promote-partial",
                &[("failed", failed_str.as_str())],
            )
            .unwrap_or_else(|| format!(
                "{failed_total} item(s) failed; dream_pending.json retained for retry."
            ))
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeroclaw_config::schema::AliasedAgentConfig;

    fn cfg_with(agents: &[(&str, bool)]) -> Config {
        let mut config = Config::default();
        for (alias, enabled) in agents {
            config.agents.insert(
                (*alias).to_string(),
                AliasedAgentConfig {
                    enabled: *enabled,
                    ..AliasedAgentConfig::default()
                },
            );
        }
        config
    }

    #[test]
    fn explicit_disabled_agent_is_rejected() {
        let config = cfg_with(&[("alpha", false)]);
        let err = resolve_target_agent(&config, Some("alpha")).unwrap_err();
        assert!(err.to_string().contains("disabled"), "{err}");
    }

    #[test]
    fn explicit_enabled_agent_resolves() {
        let config = cfg_with(&[("alpha", true)]);
        assert_eq!(
            resolve_target_agent(&config, Some("alpha")).unwrap(),
            "alpha"
        );
    }

    #[test]
    fn unknown_agent_is_rejected() {
        let config = cfg_with(&[("alpha", true)]);
        let err = resolve_target_agent(&config, Some("ghost")).unwrap_err();
        assert!(err.to_string().contains("no agent 'ghost'"), "{err}");
    }

    #[test]
    fn inference_never_picks_a_disabled_agent() {
        // A sole disabled agent, none dream-enabled → no enabled candidates,
        // so a no-arg `zeroclaw dream` refuses rather than dreaming a disabled
        // agent (matches the daemon, which skips disabled agents).
        let config = cfg_with(&[("alpha", false)]);
        let err = resolve_target_agent(&config, None).unwrap_err();
        assert!(err.to_string().contains("no enabled agents"), "{err}");
    }

    #[test]
    fn inference_picks_sole_enabled_agent() {
        let config = cfg_with(&[("alpha", true)]);
        assert_eq!(resolve_target_agent(&config, None).unwrap(), "alpha");
    }
}
