//! CLI handler for the `zeroclaw dream` command.

use anyhow::{Context, Result};
use zeroclaw_config::schema::Config;
use zeroclaw_runtime::dream::pending::DreamPending;
use zeroclaw_runtime::dream::report::DreamReport;
use zeroclaw_runtime::i18n::{get_cli_string_with_args, get_required_cli_string};

/// Run a manual dream cycle from the CLI.
pub async fn run_dream(config: &Config, dry_run: bool, verbose: bool) -> Result<()> {
    use zeroclaw_runtime::dream::engine::DreamEngine;

    // Dry-run is enforced via run_cycle_with_options below — audit_mode is left
    // as-configured so dry-run doesn't silently flip behaviour in the report.
    let engine = DreamEngine::new(config.dream_mode.clone(), config.data_dir.clone());

    // Resolve provider only if dream_mode.model is configured (opt-in LLM).
    let (provider, model): (
        Option<Box<dyn zeroclaw_api::model_provider::ModelProvider>>,
        Option<String>,
    ) = if config.dream_mode.model.is_some() {
        // Resolve the first configured model_provider as the default, using the
        // same V3 `<family>.<alias>` reference contract as a normal agent turn.
        let (family, alias, fallback) = config
            .providers
            .models
            .iter_entries()
            .next()
            .context("dream: dream_mode.model set but no model_provider configured")?;
        let provider_ref = format!("{family}.{alias}");
        let model_name = config
            .dream_mode
            .model
            .as_deref()
            .or(fallback.model.as_deref())
            .unwrap_or("claude-haiku-4-5-20251001")
            .to_string();

        let provider_runtime_options =
            zeroclaw_providers::provider_runtime_options_for_alias(config, family, alias);
        let p = zeroclaw_providers::create_routed_model_provider_with_options(
            config,
            &provider_ref,
            fallback.api_key.as_deref(),
            fallback.uri.as_deref(),
            &config.reliability,
            &config.model_routes,
            &model_name,
            &provider_runtime_options,
        )?;
        (Some(p), Some(model_name))
    } else {
        (None, None)
    };

    // Create memory backend.
    let memory = zeroclaw_memory::create_memory(
        &config.memory,
        &config.data_dir,
        config
            .providers
            .models
            .iter_entries()
            .next()
            .and_then(|(_, _, e)| e.api_key.as_deref()),
    )
    .context("dream: failed to create memory backend")?;

    if verbose {
        let mode_str = if model.is_some() {
            "LLM-assisted"
        } else {
            "local-only"
        };
        let model_display = model.as_deref().unwrap_or("(none)");
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
    } else if config.dream_mode.audit_mode {
        println!("\n{}", get_required_cli_string("cli-dream-staged-notice"));
    }

    Ok(())
}

/// Show the pending dream report, if any.
pub fn show_report(config: &Config) -> Result<()> {
    match DreamReport::load_pending(&config.data_dir)? {
        Some(report) => {
            println!("{}", report.format_message());
            DreamReport::mark_delivered(&config.data_dir)?;
        }
        None => {
            println!("{}", get_required_cli_string("cli-dream-no-report"));
        }
    }
    Ok(())
}

/// Promote staged dream mutations from `dream_pending.json` into memory.
///
/// Delegates to `zeroclaw_runtime::dream::pending::promote_pending`, which
/// preserves the pending file on partial backend failures so the user can
/// retry without losing staged work.
pub async fn promote(config: &Config) -> Result<()> {
    use zeroclaw_runtime::dream::pending::promote_pending;

    // Snapshot pending counts up front for the "Promoting N insights..." banner.
    let Some(pending_view) = DreamPending::load(&config.data_dir)? else {
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

    let memory = zeroclaw_memory::create_memory(
        &config.memory,
        &config.data_dir,
        config
            .providers
            .models
            .iter_entries()
            .next()
            .and_then(|(_, _, e)| e.api_key.as_deref()),
    )
    .context("dream promote: failed to create memory backend")?;

    let result = promote_pending(
        &config.data_dir,
        memory.as_ref(),
        config.dream_mode.hard_prune,
    )
    .await?
    .expect("pending was just loaded above");

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
