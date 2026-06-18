use anyhow::Result;
use chrono::{DateTime, Utc};
use std::io::Write;
use std::path::Path;
use zeroclaw_config::schema::Config;

const DAEMON_STALE_SECONDS: i64 = 30;
const SCHEDULER_STALE_SECONDS: i64 = 120;
const CHANNEL_STALE_SECONDS: i64 = 300;
const COMMAND_VERSION_PREVIEW_CHARS: usize = 60;

// ── Diagnostic item ──────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Ok,
    Warn,
    Error,
}

/// Structured diagnostic result for programmatic consumption (web dashboard, API).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DiagResult {
    pub severity: Severity,
    pub category: String,
    pub message: String,
}

struct DiagItem {
    severity: Severity,
    category: &'static str,
    message: String,
}

impl DiagItem {
    fn ok(category: &'static str, msg: impl Into<String>) -> Self {
        Self {
            severity: Severity::Ok,
            category,
            message: msg.into(),
        }
    }
    fn warn(category: &'static str, msg: impl Into<String>) -> Self {
        Self {
            severity: Severity::Warn,
            category,
            message: msg.into(),
        }
    }
    fn error(category: &'static str, msg: impl Into<String>) -> Self {
        Self {
            severity: Severity::Error,
            category,
            message: msg.into(),
        }
    }

    #[cfg(test)]
    fn icon(&self) -> &'static str {
        match self.severity {
            Severity::Ok => "✅",
            Severity::Warn => "⚠️ ",
            Severity::Error => "❌",
        }
    }

    fn into_result(self) -> DiagResult {
        DiagResult {
            severity: self.severity,
            category: self.category.to_string(),
            message: self.message,
        }
    }
}

// ── Public entry points ──────────────────────────────────────────

/// Run diagnostics and return structured results (for API/web dashboard).
pub fn diagnose(config: &Config) -> Vec<DiagResult> {
    let mut items: Vec<DiagItem> = Vec::new();

    check_config_semantics(config, &mut items);
    check_workspace(config, &mut items);
    check_daemon_state(config, &mut items);
    check_environment(&mut items);
    check_cli_tools(&mut items);

    items.into_iter().map(DiagItem::into_result).collect()
}

/// Outcome of probing one configured provider entry's live catalog.
#[derive(Clone, PartialEq)]
enum ModelProbe {
    /// Catalog fetched — N models advertised.
    Ok(usize),
    /// Probe failed — severity + truncated message.
    Err(Severity, String),
}

/// Render one model-probe row (`<label>: <detail>`) as a `DiagResult`.
fn model_probe_row(label: &str, probe: &ModelProbe) -> DiagResult {
    let (severity, detail) = match probe {
        ModelProbe::Ok(n) => (Severity::Ok, format!("{n} models")),
        ModelProbe::Err(severity, text) => (*severity, text.clone()),
    };
    DiagResult {
        severity,
        category: "providers.models".to_string(),
        message: format!("{label}: {detail}"),
    }
}

/// Collapse per-type model probes: when ≥2 aliases of a provider type return
/// the same result, emit a single `type: …` row; otherwise emit each alias as
/// `type.alias: …` so divergence (or a single configured alias) stays visible.
/// Input is in iteration order, where aliases of a type are contiguous (that's
/// how `iter_entries` yields them). Pure — separated for unit testing.
fn collapse_model_probes(probes: Vec<(String, ModelProbe)>) -> Vec<DiagResult> {
    let mut groups: Vec<(String, Vec<(String, ModelProbe)>)> = Vec::new();
    for (name, probe) in probes {
        let ty = name
            .split_once('.')
            .map(|(t, _)| t.to_string())
            .unwrap_or_else(|| name.clone());
        match groups.last_mut() {
            Some((group_ty, entries)) if *group_ty == ty => entries.push((name, probe)),
            _ => groups.push((ty, vec![(name, probe)])),
        }
    }

    let mut out = Vec::new();
    for (ty, entries) in groups {
        let collapse = entries.len() >= 2 && entries.iter().all(|(_, p)| *p == entries[0].1);
        if collapse {
            out.push(model_probe_row(&ty, &entries[0].1));
        } else {
            for (name, probe) in &entries {
                out.push(model_probe_row(name, probe));
            }
        }
    }
    out
}

async fn probe_models(config: &Config) -> Vec<DiagResult> {
    let targets = doctor_model_targets(config, None);
    let mut probes = Vec::with_capacity(targets.len());

    for provider_name in &targets {
        let probe = match fetch_provider_catalog(config, provider_name).await {
            Ok(models) => ModelProbe::Ok(models.len()),
            Err(e) => {
                let text = format_error_chain(&e);
                let severity = match classify_model_probe_error(&text) {
                    ModelProbeOutcome::Skipped | ModelProbeOutcome::AuthOrAccess => Severity::Warn,
                    ModelProbeOutcome::Ok | ModelProbeOutcome::Error => Severity::Error,
                };
                ModelProbe::Err(severity, truncate_for_display(&text, 120))
            }
        };
        probes.push((provider_name.clone(), probe));
    }

    collapse_model_probes(probes)
}

/// Run the full Doctor suite and return the structured result used by CLI and RPC.
pub async fn run_structured(config: &Config) -> Vec<DiagResult> {
    let mut results = diagnose(config);
    results.extend(probe_models(config).await);
    results
}

/// Run diagnostics and print human-readable report to stdout.
pub async fn run(config: &Config) -> Result<()> {
    let results = run_structured(config).await;

    println!("🩺 ZeroClaw Doctor (enhanced)");
    println!();

    let mut current_cat = String::new();
    for item in &results {
        if item.category != current_cat {
            current_cat = item.category.clone();
            println!("  [{current_cat}]");
        }
        let icon = match item.severity {
            Severity::Ok => "✅",
            Severity::Warn => "⚠️ ",
            Severity::Error => "❌",
        };
        println!("    {} {}", icon, item.message);
    }

    let errors = results
        .iter()
        .filter(|i| i.severity == Severity::Error)
        .count();
    let warns = results
        .iter()
        .filter(|i| i.severity == Severity::Warn)
        .count();
    let oks = results
        .iter()
        .filter(|i| i.severity == Severity::Ok)
        .count();

    println!();
    println!("  Summary: {oks} ok, {warns} warnings, {errors} errors");

    if errors > 0 {
        println!("  💡 Fix the errors above, then run `zeroclaw doctor` again.");
    }

    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ModelProbeOutcome {
    Ok,
    Skipped,
    AuthOrAccess,
    Error,
}

fn model_probe_status_label(outcome: ModelProbeOutcome) -> &'static str {
    match outcome {
        ModelProbeOutcome::Ok => "ok",
        ModelProbeOutcome::Skipped => "skipped",
        ModelProbeOutcome::AuthOrAccess => "auth/access",
        ModelProbeOutcome::Error => "error",
    }
}

fn classify_model_probe_error(err_message: &str) -> ModelProbeOutcome {
    let lower = err_message.to_lowercase();

    if lower.contains("does not support live model discovery") {
        return ModelProbeOutcome::Skipped;
    }

    if [
        "401",
        "403",
        "429",
        "unauthorized",
        "forbidden",
        "api key",
        "token",
        "insufficient balance",
        "insufficient quota",
        "plan does not include",
        "rate limit",
    ]
    .iter()
    .any(|hint| lower.contains(hint))
    {
        return ModelProbeOutcome::AuthOrAccess;
    }

    ModelProbeOutcome::Error
}

fn doctor_model_targets(config: &Config, provider_override: Option<&str>) -> Vec<String> {
    if let Some(model_provider) = provider_override.map(str::trim).filter(|p| !p.is_empty()) {
        return vec![model_provider.to_string()];
    }

    config
        .providers
        .models
        .iter_entries()
        .map(|(type_k, alias_k, _)| format!("{type_k}.{alias_k}"))
        .collect()
}

fn configured_model_provider_api_key<'a>(
    config: &'a Config,
    provider_name: &str,
) -> Option<&'a str> {
    let (family, alias) = provider_name
        .split_once('.')
        .unwrap_or((provider_name, "default"));

    config
        .providers
        .models
        .find(family, alias)
        .and_then(|entry| entry.api_key.as_deref())
}

fn create_doctor_model_provider(
    config: &Config,
    provider_name: &str,
) -> anyhow::Result<Box<dyn zeroclaw_api::model_provider::ModelProvider>> {
    let api_key = configured_model_provider_api_key(config, provider_name);
    let options = zeroclaw_providers::options_for_provider_ref(
        config,
        provider_name,
        &zeroclaw_providers::ModelProviderRuntimeOptions::default(),
    );

    match provider_name.split_once('.') {
        Some((family, alias)) => zeroclaw_providers::create_model_provider_for_alias(
            config, family, alias, api_key, &options,
        ),
        None => {
            zeroclaw_providers::create_model_provider_with_options(provider_name, api_key, &options)
        }
    }
}

pub async fn run_models(
    config: &Config,
    provider_override: Option<&str>,
    _use_cache: bool,
    show_model_names: bool,
) -> Result<()> {
    let targets = doctor_model_targets(config, provider_override);

    if targets.is_empty() {
        anyhow::bail!(
            "No configured model_providers to probe — run `zeroclaw quickstart` to set one up first"
        );
    }

    println!("🩺 ZeroClaw Doctor — Model Catalog Probe");
    println!("  Providers to probe: {}", targets.len());
    println!();

    let mut ok_count = 0usize;
    let mut skipped_count = 0usize;
    let mut auth_count = 0usize;
    let mut error_count = 0usize;
    let mut matrix_rows: Vec<(String, ModelProbeOutcome, Option<usize>, String)> = Vec::new();

    for provider_name in &targets {
        println!("  [{}]", provider_name);

        let outcome = fetch_provider_catalog(config, provider_name).await;

        match outcome {
            Ok(models) => {
                ok_count += 1;
                println!("    ✅ {} models", models.len());
                if show_model_names && !models.is_empty() {
                    for m in &models {
                        println!("      • {}", m);
                    }
                }
                matrix_rows.push((
                    provider_name.clone(),
                    ModelProbeOutcome::Ok,
                    Some(models.len()),
                    "catalog fetched".to_string(),
                ));
            }
            Err(error) => {
                let error_text = format_error_chain(&error);
                match classify_model_probe_error(&error_text) {
                    ModelProbeOutcome::Skipped => {
                        skipped_count += 1;
                        println!("    ⚪ skipped: {}", truncate_for_display(&error_text, 160));
                        matrix_rows.push((
                            provider_name.clone(),
                            ModelProbeOutcome::Skipped,
                            None,
                            truncate_for_display(&error_text, 120),
                        ));
                    }
                    ModelProbeOutcome::AuthOrAccess => {
                        auth_count += 1;
                        println!(
                            "    ⚠️  auth/access: {}",
                            truncate_for_display(&error_text, 160)
                        );
                        matrix_rows.push((
                            provider_name.clone(),
                            ModelProbeOutcome::AuthOrAccess,
                            None,
                            truncate_for_display(&error_text, 120),
                        ));
                    }
                    ModelProbeOutcome::Error | ModelProbeOutcome::Ok => {
                        error_count += 1;
                        println!("    ❌ error: {}", truncate_for_display(&error_text, 160));
                        matrix_rows.push((
                            provider_name.clone(),
                            ModelProbeOutcome::Error,
                            None,
                            truncate_for_display(&error_text, 120),
                        ));
                    }
                }
            }
        }

        println!();
    }

    println!(
        "  Summary: {} ok, {} skipped, {} auth/access, {} errors",
        ok_count, skipped_count, auth_count, error_count
    );

    if !matrix_rows.is_empty() {
        println!();
        println!("  Connectivity matrix:");
        println!(
            "  {:<18} {:<12} {:<8} detail",
            "model_provider", "status", "models"
        );
        println!(
            "  {:<18} {:<12} {:<8} ------",
            "------------------", "------------", "--------"
        );
        for (model_provider, outcome, models_count, detail) in matrix_rows {
            let models_text = models_count
                .map(|count| count.to_string())
                .unwrap_or_else(|| "-".to_string());
            println!(
                "  {:<18} {:<12} {:<8} {}",
                model_provider,
                model_probe_status_label(outcome),
                models_text,
                detail
            );
        }
    }

    if auth_count > 0 {
        println!(
            "  💡 Some model_providers need valid API keys/plan access before `/models` can be fetched."
        );
    }

    if provider_override.is_some() && ok_count == 0 {
        anyhow::bail!("Model probe failed for target model_provider")
    }

    Ok(())
}

/// Update context_window in config.toml from provider /models endpoints.
/// Returns the number of providers updated.
pub async fn update_context_windows(
    config: &mut zeroclaw_config::schema::Config,
    provider_override: Option<&str>,
    dry_run: bool,
) -> anyhow::Result<usize> {
    let mut updated = 0usize;

    // Collect all the data we need first to avoid borrow conflicts
    let targets: Vec<(String, String, Option<String>, Option<usize>)> = if let Some(model_provider) = provider_override {
        // Single provider - use find_by_name to look up by "type.alias" format
        if let Some((_, _, entry)) = config.providers.models.find_by_name(model_provider) {
            vec![(model_provider.to_string(), entry.model.clone().unwrap_or_default(), entry.uri.clone(), entry.context_window)]
        } else {
            anyhow::bail!("Model provider '{model_provider}' not found in config");
        }
    } else {
        // All providers
        config
            .providers
            .models
            .iter_entries()
            .map(|(t, a, e)| (format!("{t}.{a}"), e.model.clone().unwrap_or_default(), e.uri.clone(), e.context_window))
            .collect()
    };

    for (provider_ref, model, uri, existing_context_window) in targets {
        // Skip if already has context_window set
        if existing_context_window.is_some() {
            println!("  {provider_ref}: already has context_window = {}", existing_context_window.unwrap());
            continue;
        }

        // Skip if no model configured
        if model.is_empty() {
            println!("  {provider_ref}: no model configured, skipping");
            continue;
        }

        // Build minimal provider config for fetch
        let provider_type = provider_ref.split('.').next().unwrap_or("");

        // Fetch context window from provider
        match zeroclaw_providers::fetch_context_window(provider_type, &zeroclaw_config::schema::ModelProviderConfig {
            model: Some(model.clone()),
            uri: uri.clone(),
            ..Default::default()
        }).await {
            Some(ctx) => {
                if dry_run {
                    println!("  {provider_ref}: would set context_window = {ctx} (dry run)");
                } else {
                    // Update the config - need to find and mutate
                    let mut found = false;
                    for entry_mut in config.providers.models.iter_entries_mut() {
                        if entry_mut.2.model.as_deref() == Some(model.as_str()) && entry_mut.2.uri == uri {
                            entry_mut.2.context_window = Some(ctx);
                            updated += 1;
                            println!("  {provider_ref}: set context_window = {ctx}");
                            found = true;
                            break;
                        }
                    }
                    if !found {
                        println!("  {provider_ref}: could not find entry to update");
                    }
                }
            }
            None => {
                println!("  {provider_ref}: provider does not expose context window or fetch failed");
            }
        }
    }

    if !dry_run && updated > 0 {
        config.save().await?;
        println!("\nSaved {updated} updates to config.toml");
    } else if dry_run {
        println!("\nDry run complete — no changes written. Run without --dry-run to apply.");
    } else {
        println!("\nNo updates needed.");
    }

    Ok(updated)
}

/// Fetch a provider's live model catalog — the model IDs advertised by its
/// `/models` endpoint. Extracted from the catalog probe so `models list
/// --check` (configured-model verification) and future interactive flows (the
/// `quickstart` model picker, which also wants pricing) share one fetch path.
pub async fn fetch_provider_catalog(config: &Config, provider_ref: &str) -> Result<Vec<String>> {
    create_doctor_model_provider(config, provider_ref)?
        .list_models()
        .await
}

/// Collect the configured `(provider_ref, model)` pairs from config, optionally
/// narrowed to a single target (matched by full `type.alias` ref or by bare
/// family name).
fn configured_model_entries(
    config: &Config,
    provider_override: Option<&str>,
) -> Vec<(String, Option<String>)> {
    let filter = provider_override.map(str::trim).filter(|p| !p.is_empty());
    config
        .providers
        .models
        .iter_entries()
        .map(|(ty, alias, entry)| (format!("{ty}.{alias}"), entry.model.clone()))
        .filter(|(provider_ref, _)| match filter {
            Some(f) => provider_ref == f || provider_ref.split('.').next() == Some(f),
            None => true,
        })
        .collect()
}

/// Whether a configured model id appears verbatim in a provider's live catalog.
/// Pure — separated so the membership rule is explicit and unit-testable
/// without any network probe.
fn model_in_catalog(model: &str, catalog: &[String]) -> bool {
    catalog.iter().any(|id| id == model)
}

/// List the models configured in `config.toml` (one per `[providers.models.*]`
/// entry). Default is an offline readout; `verify = true` (`models list
/// --check`, and the `doctor models` health path) additionally probes each
/// provider's live catalog and flags whether the configured model is actually
/// available.
pub async fn run_configured_models(
    config: &Config,
    provider_override: Option<&str>,
    verify: bool,
) -> Result<()> {
    let entries = configured_model_entries(config, provider_override);

    if entries.is_empty() {
        anyhow::bail!(
            "No configured model_providers — run `zeroclaw quickstart` to set one up first"
        );
    }

    if verify {
        println!("🩺 ZeroClaw — Configured Models (--check)");
    } else {
        println!("🩺 ZeroClaw — Configured Models");
    }
    println!();

    let mut ok = 0usize;
    let mut warn = 0usize;
    let mut error = 0usize;

    for (provider_ref, model) in &entries {
        println!("  [{}]", provider_ref);

        let Some(model) = model.as_deref() else {
            warn += 1;
            println!("    ⚠️  no model configured");
            println!();
            continue;
        };

        if !verify {
            println!("    model: {model}");
            println!();
            continue;
        }

        match fetch_provider_catalog(config, provider_ref).await {
            Ok(catalog) if model_in_catalog(model, &catalog) => {
                ok += 1;
                println!("    model: {model}  ✅ available");
            }
            Ok(catalog) => {
                warn += 1;
                println!(
                    "    model: {model}  ⚠️  not in catalog ({} models advertised)",
                    catalog.len()
                );
            }
            Err(probe_error) => {
                let text = format_error_chain(&probe_error);
                match classify_model_probe_error(&text) {
                    ModelProbeOutcome::Error | ModelProbeOutcome::Ok => {
                        error += 1;
                        println!(
                            "    model: {model}  ❌ {}",
                            truncate_for_display(&text, 140)
                        );
                    }
                    _ => {
                        warn += 1;
                        println!(
                            "    model: {model}  ⚠️  unverified: {}",
                            truncate_for_display(&text, 140)
                        );
                    }
                }
            }
        }
        println!();
    }

    if verify {
        println!("  Connectivity: {ok} ok, {warn} warning, {error} errors");
        if provider_override.is_some() && ok == 0 {
            anyhow::bail!("No configured model verified for target model_provider");
        }
    } else {
        let n = entries.len();
        println!("  {n} provider{} configured", if n == 1 { "" } else { "s" });
    }

    Ok(())
}

pub fn run_traces(
    config: &Config,
    id: Option<&str>,
    event_filter: Option<&str>,
    contains: Option<&str>,
    limit: usize,
) -> Result<()> {
    let path = crate::observability::runtime_trace::resolve_trace_path(
        &config.observability,
        &config.data_dir,
    );

    if let Some(target_id) = id.map(str::trim).filter(|value| !value.is_empty()) {
        match crate::observability::runtime_trace::find_event_by_id(&path, target_id)? {
            Some(event) => {
                println!("{}", serde_json::to_string_pretty(&event)?);
            }
            None => {
                println!(
                    "No runtime trace event found for id '{}' (path: {}).",
                    target_id,
                    path.display()
                );
            }
        }
        return Ok(());
    }

    if !path.exists() {
        println!(
            "Runtime trace file not found: {}.\n\
             Enable [observability] log_persistence = \"rolling\" or \"full\", then reproduce the issue.",
            path.display()
        );
        return Ok(());
    }

    let safe_limit = limit.max(1);
    let events = crate::observability::runtime_trace::load_events(
        &path,
        safe_limit,
        event_filter,
        contains,
    )?;

    if events.is_empty() {
        println!(
            "No runtime trace events matched query (path: {}).",
            path.display()
        );
        return Ok(());
    }

    println!("Runtime traces (newest first)");
    println!("Path: {}", path.display().to_string());
    println!(
        "Filters: event={} contains={} limit={}",
        event_filter.unwrap_or("*"),
        contains.unwrap_or("*"),
        safe_limit
    );
    println!();

    for event in events {
        let outcome = match event.event.outcome.as_str() {
            "success" => "ok",
            "failure" => "fail",
            _ => "-",
        };
        let message = event.message.unwrap_or_default();
        let preview = truncate_for_display(&message, 80);
        println!(
            "- {} | {} | {} | {} | {}",
            event.timestamp, event.id, event.event.action, outcome, preview
        );
    }

    println!();
    println!("Use `zeroclaw doctor traces --id <trace-id>` to inspect a full event payload.");
    Ok(())
}

// ── Config semantic validation ───────────────────────────────────

fn check_config_semantics(config: &Config, items: &mut Vec<DiagItem>) {
    let cat = "config";

    // Config file exists
    if config.config_path.exists() {
        items.push(DiagItem::ok(
            cat,
            format!("config file: {}", config.config_path.display().to_string()),
        ));
    } else {
        items.push(DiagItem::error(
            cat,
            format!(
                "config file not found: {}",
                config.config_path.display().to_string()
            ),
        ));
    }

    // ModelProvider validity — check each configured provider entry
    {
        let mut found_any = false;
        for (family, alias, entry) in config.providers.models.iter_entries() {
            found_any = true;
            let label = format!("{family}.{alias}");
            if let Some(reason) = provider_validation_error(family) {
                items.push(DiagItem::error(
                    cat,
                    format!("model_provider \"{label}\" is invalid: {reason}"),
                ));
            } else {
                items.push(DiagItem::ok(
                    cat,
                    format!("model_provider \"{label}\" is valid"),
                ));
            }

            // API key presence
            if family != "ollama" {
                if entry.api_key.as_deref().is_some() {
                    items.push(DiagItem::ok(cat, format!("{label}: API key configured")));
                } else {
                    items.push(DiagItem::warn(
                        cat,
                        format!("{label}: no api_key set (may rely on env vars or model_provider defaults)"),
                    ));
                }
            }

            // Model configured
            if let Some(model) = entry.model.as_deref() {
                items.push(DiagItem::ok(cat, format!("{label}: model: {model}")));
            } else {
                items.push(DiagItem::warn(cat, format!("{label}: no model configured")));
            }

            // Temperature range
            match entry.temperature {
                Some(temperature) if (0.0..=2.0).contains(&temperature) => {
                    items.push(DiagItem::ok(
                        cat,
                        format!(
                            "{label}: temperature {temperature:.1} (valid range 0.0\u{2013}2.0)"
                        ),
                    ));
                }
                Some(temperature) => {
                    items.push(DiagItem::error(
                        cat,
                        format!(
                            "{label}: temperature {temperature:.1} is out of range (expected 0.0\u{2013}2.0)"
                        ),
                    ));
                }
                None => {
                    items.push(DiagItem::ok(
                        cat,
                        format!("{label}: temperature unset (provider default)"),
                    ));
                }
            }
        }
        if !found_any {
            items.push(DiagItem::error(cat, "no model providers configured"));
        }
    }

    // Gateway port range
    let port = config.gateway.port;
    if port > 0 {
        items.push(DiagItem::ok(cat, format!("gateway port: {port}")));
    } else {
        items.push(DiagItem::error(cat, "gateway port is 0 (invalid)"));
    }

    // Model routes validation
    for route in &config.model_routes {
        if route.hint.is_empty() {
            items.push(DiagItem::warn(cat, "model route with empty hint"));
        }
        if let Some(reason) = provider_validation_error(&route.model_provider) {
            items.push(DiagItem::warn(
                cat,
                format!(
                    "model route \"{}\" uses invalid model_provider \"{}\": {}",
                    route.hint, route.model_provider, reason
                ),
            ));
        }
        if route.model.is_empty() {
            items.push(DiagItem::warn(
                cat,
                format!("model route \"{}\" has empty model", route.hint),
            ));
        }
    }

    // Embedding routes validation
    for route in &config.embedding_routes {
        if route.hint.trim().is_empty() {
            items.push(DiagItem::warn(cat, "embedding route with empty hint"));
        }
        if let Some(reason) = embedding_provider_validation_error(&route.model_provider) {
            items.push(DiagItem::warn(
                cat,
                format!(
                    "embedding route \"{}\" uses invalid model_provider \"{}\": {}",
                    route.hint, route.model_provider, reason
                ),
            ));
        }
        if route.model.trim().is_empty() {
            items.push(DiagItem::warn(
                cat,
                format!("embedding route \"{}\" has empty model", route.hint),
            ));
        }
        if route.dimensions.is_some_and(|value| value == 0) {
            items.push(DiagItem::warn(
                cat,
                format!(
                    "embedding route \"{}\" has invalid dimensions=0",
                    route.hint
                ),
            ));
        }
    }

    if let Some(hint) = config
        .memory
        .embedding_model
        .strip_prefix("hint:")
        .map(str::trim)
        .filter(|value| !value.is_empty())
        && !config
            .embedding_routes
            .iter()
            .any(|route| route.hint.trim() == hint)
    {
        items.push(DiagItem::warn(
                cat,
                format!(
                    "memory.embedding_model uses hint \"{hint}\" but no matching [[embedding_routes]] entry exists"
                ),
            ));
    }

    // gateway.web_dist_dir: flag values that rely on shell expansion the
    // gateway does not perform. Parallel check lives in
    // `src/commands/self_test.rs::check_web_dist_dir`; keep the wording
    // and predicate in sync.
    check_web_dist_dir(config, items);

    // Channel: at least one configured
    let cc = &config.channels;
    let has_channel = cc.channels().iter().any(|info| info.configured);

    if has_channel {
        items.push(DiagItem::ok(cat, "at least one channel configured"));
    } else {
        items.push(DiagItem::warn(
            cat,
            "no channels configured — run `zeroclaw quickstart` to set one up",
        ));
    }

    // Delegate agents: model_provider validity (resolved from model_provider alias)
    let mut agent_names: Vec<_> = config.agents.keys().collect();
    agent_names.sort();
    for name in agent_names {
        let agent = config.agents.get(name).unwrap();
        let provider_type = agent
            .model_provider
            .split_once('.')
            .map_or(agent.model_provider.as_str(), |(t, _)| t);
        if provider_type.is_empty() {
            continue;
        }
        if let Some(reason) = provider_validation_error(provider_type) {
            items.push(DiagItem::warn(
                cat,
                format!(
                    "agent \"{name}\" uses invalid model_provider \"{provider_type}\": {reason}",
                ),
            ));
        }
    }

    // Non-fatal config warnings — dangling fallback refs, wire_api misuse, etc.
    // Source of truth: `Config::collect_warnings()` (same signal as gateway API
    // and `Config::validate()` tracing). Do not duplicate checks here.
    for warning in config.collect_warnings() {
        items.push(DiagItem::warn(
            cat,
            format!("{} (at {})", warning.message, warning.path),
        ));
    }
}

/// Flag `gateway.web_dist_dir` values that rely on shell-style expansion
/// (a leading `~` or any `$VAR` / `${VAR}`). The gateway reads this field
/// verbatim and never invokes a shell, so values like `~/web-dist` or
/// `$HOME/web-dist` resolve to literal on-disk paths and silently fail to
/// find the bundled assets — surface that here at `zeroclaw doctor` time
/// instead of at runtime. Parallel check lives in
/// `src/commands/self_test.rs::check_web_dist_dir`.
///
/// User-facing message goes through Fluent
/// (`cli-doctor-web-dist-dir-expansion-warning`) per AGENTS.md §
/// Localization — no bare Rust literals for CLI output. Reason phrases
/// are Fluent keys too (`cli-web-dist-dir-reason-{tilde,dollar}`).
fn check_web_dist_dir(config: &Config, items: &mut Vec<DiagItem>) {
    let cat = "config";
    match config.gateway.web_dist_dir.as_deref() {
        None => {}
        Some(value) => match web_dist_dir_expansion_reason_key(value) {
            None => {}
            Some(reason_key) => {
                let reason = crate::i18n::get_required_cli_string(reason_key);
                let message = crate::i18n::get_required_cli_string_with_args(
                    "cli-doctor-web-dist-dir-expansion-warning",
                    &[("path", value), ("reason", reason.as_str())],
                );
                items.push(DiagItem::warn(cat, message));
            }
        },
    }
}

/// Return the Fluent reason key when `value` looks like it expects
/// shell expansion the gateway will not perform. `None` means the value
/// is a literal path that the gateway can resolve as-is.
fn web_dist_dir_expansion_reason_key(value: &str) -> Option<&'static str> {
    if value.starts_with('~') {
        Some("cli-web-dist-dir-reason-tilde")
    } else if value.contains('$') {
        Some("cli-web-dist-dir-reason-dollar")
    } else {
        None
    }
}

fn provider_validation_error(name: &str) -> Option<String> {
    match zeroclaw_providers::create_model_provider(name, None) {
        Ok(_) => None,
        Err(err) => Some(
            err.to_string()
                .lines()
                .next()
                .unwrap_or("invalid model_provider")
                .into(),
        ),
    }
}

fn embedding_provider_validation_error(name: &str) -> Option<String> {
    let normalized = name.trim();
    if normalized.eq_ignore_ascii_case("none") || normalized.eq_ignore_ascii_case("openai") {
        return None;
    }

    let Some(url) = normalized.strip_prefix("custom:") else {
        return Some("supported values: none, openai, custom:<url>".into());
    };

    let url = url.trim();
    if url.is_empty() {
        return Some("custom model_provider requires a non-empty URL after 'custom:'".into());
    }

    match reqwest::Url::parse(url) {
        Ok(parsed) if matches!(parsed.scheme(), "http" | "https") => None,
        Ok(parsed) => Some(format!(
            "custom model_provider URL must use http/https, got '{}'",
            parsed.scheme()
        )),
        Err(err) => Some(format!("invalid custom model_provider URL: {err}")),
    }
}

// ── Workspace integrity ──────────────────────────────────────────

fn check_workspace(config: &Config, items: &mut Vec<DiagItem>) {
    let cat = "workspace";
    let ws = &config.data_dir;

    if ws.exists() {
        items.push(DiagItem::ok(
            cat,
            format!("directory exists: {}", ws.display().to_string()),
        ));
    } else {
        items.push(DiagItem::error(
            cat,
            format!("directory missing: {}", ws.display().to_string()),
        ));
        return;
    }

    // Writable check
    let probe = workspace_probe_path(ws);
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
    {
        Ok(mut probe_file) => {
            let write_result = probe_file.write_all(b"probe");
            drop(probe_file);
            let _ = std::fs::remove_file(&probe);
            match write_result {
                Ok(()) => items.push(DiagItem::ok(cat, "directory is writable")),
                Err(e) => items.push(DiagItem::error(
                    cat,
                    format!("directory write probe failed: {e}"),
                )),
            }
        }
        Err(e) => {
            items.push(DiagItem::error(
                cat,
                format!("directory is not writable: {e}"),
            ));
        }
    }

    // Disk space (best-effort via `df`)
    if let Some(avail_mb) = disk_available_mb(ws) {
        if avail_mb >= 100 {
            items.push(DiagItem::ok(
                cat,
                format!("disk space: {avail_mb} MB available"),
            ));
        } else {
            items.push(DiagItem::warn(
                cat,
                format!("low disk space: only {avail_mb} MB available"),
            ));
        }
    }

    // Per-agent personality files. These are resolved per agent from
    // `<install>/agents/<alias>/workspace/` (or an explicit
    // `[agents.<alias>.workspace.path]` override) — never from `data_dir`.
    // Iterate every enabled agent so multi-agent installs each get checked,
    // and name the alias in the result so the report is unambiguous. Sorted
    // for deterministic output (HashMap iteration order is unspecified).
    let mut agent_aliases: Vec<&String> = config.agents.keys().collect();
    agent_aliases.sort();
    for alias in agent_aliases {
        let agent = config.agents.get(alias).expect("alias from keys()");
        if !agent.enabled {
            continue;
        }
        let agent_ws = config.agent_workspace_dir(alias);
        check_agent_file(&agent_ws, "SOUL.md", alias, cat, items);
        check_agent_file(&agent_ws, "AGENTS.md", alias, cat, items);
    }
}

/// Existence check for an optional per-agent workspace file. Prefixes the
/// owning agent alias as `[alias]` so a multi-agent report stays legible and
/// `(optional)` keeps its single, consistent meaning as the severity hint
/// (e.g. `[default] SOUL.md present`, `[default] AGENTS.md not found (optional)`).
fn check_agent_file(
    workspace_dir: &Path,
    name: &str,
    alias: &str,
    cat: &'static str,
    items: &mut Vec<DiagItem>,
) {
    if workspace_dir.join(name).is_file() {
        items.push(DiagItem::ok(cat, format!("[{alias}] {name} present")));
    } else {
        items.push(DiagItem::warn(
            cat,
            format!("[{alias}] {name} not found (optional)"),
        ));
    }
}

fn disk_available_mb(path: &Path) -> Option<u64> {
    let output = std::process::Command::new("df")
        .arg("-m")
        .arg(path)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_df_available_mb(&stdout)
}

fn parse_df_available_mb(stdout: &str) -> Option<u64> {
    let line = stdout.lines().rev().find(|line| !line.trim().is_empty())?;
    let avail = line.split_whitespace().nth(3)?;
    avail.parse::<u64>().ok()
}

fn workspace_probe_path(workspace_dir: &Path) -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    workspace_dir.join(format!(
        ".zeroclaw_doctor_probe_{}_{}",
        std::process::id(),
        nanos
    ))
}

// ── Daemon state (original logic, preserved) ─────────────────────

fn check_daemon_state(config: &Config, items: &mut Vec<DiagItem>) {
    let cat = "daemon";
    let state_file = crate::daemon::state_file_path(config);

    if !state_file.exists() {
        items.push(DiagItem::error(
            cat,
            format!(
                "state file not found: {} — is the daemon running?",
                state_file.display()
            ),
        ));
        return;
    }

    let raw = match std::fs::read_to_string(&state_file) {
        Ok(r) => r,
        Err(e) => {
            items.push(DiagItem::error(cat, format!("cannot read state file: {e}")));
            return;
        }
    };

    let snapshot: serde_json::Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(e) => {
            items.push(DiagItem::error(cat, format!("invalid state JSON: {e}")));
            return;
        }
    };

    // Daemon heartbeat freshness
    let updated_at = snapshot
        .get("updated_at")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");

    if let Ok(ts) = DateTime::parse_from_rfc3339(updated_at) {
        let age = Utc::now()
            .signed_duration_since(ts.with_timezone(&Utc))
            .num_seconds();
        if age <= DAEMON_STALE_SECONDS {
            items.push(DiagItem::ok(cat, format!("heartbeat fresh ({age}s ago)")));
        } else {
            items.push(DiagItem::error(
                cat,
                format!("heartbeat stale ({age}s ago)"),
            ));
        }
    } else {
        items.push(DiagItem::error(
            cat,
            format!("invalid daemon timestamp: {updated_at}"),
        ));
    }

    // Components
    if let Some(components) = snapshot
        .get("components")
        .and_then(serde_json::Value::as_object)
    {
        // Scheduler
        if let Some(scheduler) = components.get("scheduler") {
            let scheduler_ok = scheduler
                .get("status")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|s| s == "ok");
            let scheduler_age = scheduler
                .get("last_ok")
                .and_then(serde_json::Value::as_str)
                .and_then(parse_rfc3339)
                .map_or(i64::MAX, |dt| {
                    Utc::now().signed_duration_since(dt).num_seconds()
                });

            if scheduler_ok && scheduler_age <= SCHEDULER_STALE_SECONDS {
                items.push(DiagItem::ok(
                    cat,
                    format!("scheduler healthy (last ok {scheduler_age}s ago)"),
                ));
            } else {
                items.push(DiagItem::error(
                    cat,
                    format!("scheduler unhealthy (ok={scheduler_ok}, age={scheduler_age}s)"),
                ));
            }
        } else {
            items.push(DiagItem::warn(cat, "scheduler component not tracked yet"));
        }

        // Channels
        let mut channel_count = 0u32;
        let mut stale = 0u32;
        for (name, component) in components {
            if !name.starts_with("channel:") {
                continue;
            }
            channel_count += 1;
            let status_ok = component
                .get("status")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|s| s == "ok");
            let age = component
                .get("last_ok")
                .and_then(serde_json::Value::as_str)
                .and_then(parse_rfc3339)
                .map_or(i64::MAX, |dt| {
                    Utc::now().signed_duration_since(dt).num_seconds()
                });

            if status_ok && age <= CHANNEL_STALE_SECONDS {
                items.push(DiagItem::ok(cat, format!("{name} fresh ({age}s ago)")));
            } else {
                stale += 1;
                items.push(DiagItem::error(
                    cat,
                    format!("{name} stale (ok={status_ok}, age={age}s)"),
                ));
            }
        }

        if channel_count == 0 {
            items.push(DiagItem::warn(cat, "no channel components tracked yet"));
        } else if stale > 0 {
            items.push(DiagItem::warn(
                cat,
                format!("{channel_count} channels, {stale} stale"),
            ));
        }
    }
}

// ── Environment checks ───────────────────────────────────────────

fn check_environment(items: &mut Vec<DiagItem>) {
    let cat = "environment";

    // git
    check_command_available("git", &["--version"], cat, items);

    // Shell — Unix uses $SHELL, Windows uses %ComSpec% (path to cmd.exe).
    let shell = std::env::var("SHELL")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var("ComSpec").ok().filter(|s| !s.is_empty()));
    match shell {
        Some(s) => items.push(DiagItem::ok(cat, format!("shell: {s}"))),
        None => items.push(DiagItem::warn(cat, "neither $SHELL nor %ComSpec% is set")),
    }

    // HOME
    if std::env::var("HOME").is_ok() || std::env::var("USERPROFILE").is_ok() {
        items.push(DiagItem::ok(cat, "home directory env set"));
    } else {
        items.push(DiagItem::error(
            cat,
            "neither $HOME nor $USERPROFILE is set",
        ));
    }

    // Optional tools
    check_command_available("curl", &["--version"], cat, items);
}

fn check_cli_tools(items: &mut Vec<DiagItem>) {
    let cat = "cli-tools";

    let discovered = crate::tools::discover_cli_tools(&[], &[]);

    if discovered.is_empty() {
        items.push(DiagItem::warn(cat, "No CLI tools found in PATH"));
    } else {
        for cli in &discovered {
            let version_info = cli
                .version
                .as_deref()
                .map(|v| truncate_for_display(v, COMMAND_VERSION_PREVIEW_CHARS))
                .unwrap_or_else(|| "unknown version".to_string());
            items.push(DiagItem::ok(
                cat,
                format!("{} ({}) — {}", cli.name, cli.category, version_info),
            ));
        }
        items.push(DiagItem::ok(
            cat,
            format!("{} CLI tools discovered", discovered.len()),
        ));
    }
}

fn check_command_available(cmd: &str, args: &[&str], cat: &'static str, items: &mut Vec<DiagItem>) {
    match std::process::Command::new(cmd)
        .args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
    {
        Ok(output) if output.status.success() => {
            let ver = String::from_utf8_lossy(&output.stdout);
            let first_line = ver.lines().next().unwrap_or("").trim();
            let display = truncate_for_display(first_line, COMMAND_VERSION_PREVIEW_CHARS);
            items.push(DiagItem::ok(cat, format!("{cmd}: {display}")));
        }
        Ok(_) => {
            items.push(DiagItem::warn(
                cat,
                format!("{cmd} found but returned non-zero"),
            ));
        }
        Err(_) => {
            items.push(DiagItem::warn(cat, format!("{cmd} not found in PATH")));
        }
    }
}

fn format_error_chain(error: &anyhow::Error) -> String {
    let mut parts = Vec::new();
    for cause in error.chain() {
        let message = cause.to_string();
        if !message.is_empty() {
            parts.push(message);
        }
    }

    if parts.is_empty() {
        return String::new();
    }

    parts.join(": ")
}

fn truncate_for_display(input: &str, max_chars: usize) -> String {
    let mut chars = input.chars();
    let preview: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{preview}…")
    } else {
        preview
    }
}

// ── Helpers ──────────────────────────────────────────────────────

fn parse_rfc3339(raw: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn collapse_model_probes_groups_identical_and_breaks_divergent() {
        use ModelProbe::{Err as E, Ok as K};
        let probes = vec![
            ("ollama.a".to_string(), K(8)),
            ("ollama.b".to_string(), K(8)),
            ("ollama.c".to_string(), K(8)),
            ("opencode.x".to_string(), K(18)),
            ("opencode.y".to_string(), K(18)),
            ("kilo.solo".to_string(), K(335)),
            ("openai.a".to_string(), K(10)),
            ("openai.b".to_string(), K(12)),
            (
                "kilocli.free".to_string(),
                E(Severity::Error, "not supported".to_string()),
            ),
        ];
        let msgs: Vec<String> = collapse_model_probes(probes)
            .into_iter()
            .map(|r| r.message)
            .collect();
        assert_eq!(
            msgs,
            vec![
                "ollama: 8 models",      // 3 identical aliases → collapsed to type
                "opencode: 18 models",   // 2 identical → collapsed
                "kilo.solo: 335 models", // single alias → kept per-alias
                "openai.a: 10 models",   // divergent counts → broken out
                "openai.b: 12 models",
                "kilocli.free: not supported", // single alias → kept
            ]
        );
    }

    #[test]
    fn model_in_catalog_requires_exact_id_match() {
        let catalog = vec![
            "anthropic/claude-sonnet-4.5".to_string(),
            "openai/gpt-5".to_string(),
        ];
        assert!(model_in_catalog("openai/gpt-5", &catalog));
        // Not present, partial, and empty all fail — no fuzzy/suffix matching.
        assert!(!model_in_catalog("openai/gpt-4", &catalog));
        assert!(!model_in_catalog("gpt-5", &catalog));
        assert!(!model_in_catalog("", &catalog));
        assert!(!model_in_catalog("anthropic/claude-sonnet-4.5", &[]));
    }

    #[test]
    fn provider_validation_checks_custom_url_shape() {
        assert!(provider_validation_error("openrouter").is_none());
        assert!(provider_validation_error("custom:https://example.com").is_none());
        assert!(provider_validation_error("anthropic-custom:https://example.com").is_none());

        let invalid_custom = provider_validation_error("custom:").unwrap_or_default();
        assert!(invalid_custom.contains("requires a URL"));

        let invalid_unknown = provider_validation_error("totally-fake").unwrap_or_default();
        assert!(invalid_unknown.contains("Unknown model_provider"));
    }

    #[test]
    fn diag_item_icons() {
        assert_eq!(DiagItem::ok("t", "m").icon(), "✅");
        assert_eq!(DiagItem::warn("t", "m").icon(), "⚠️ ");
        assert_eq!(DiagItem::error("t", "m").icon(), "❌");
    }

    #[test]
    fn config_validation_catches_bad_temperature() {
        // Single model_provider entry with an out-of-range temperature so the
        // doctor's `iter_entries()` walk deterministically finds it
        // (HashMap iteration order is unspecified — multiple entries
        // produce a coin-flip iteration order).
        let mut config = Config::default();
        config
            .providers
            .models
            .ensure("openrouter", "default")
            .expect("known model_provider type")
            .temperature = Some(5.0);
        let mut items = Vec::new();
        check_config_semantics(&config, &mut items);
        let temp_item = items.iter().find(|i| i.message.contains("temperature"));
        assert!(temp_item.is_some());
        assert_eq!(temp_item.unwrap().severity, Severity::Error);
    }

    #[test]
    fn config_validation_accepts_valid_temperature() {
        let mut config = Config::default();
        config
            .providers
            .models
            .ensure("openrouter", "default")
            .expect("known model_provider type")
            .temperature = Some(0.7);
        let mut items = Vec::new();
        check_config_semantics(&config, &mut items);
        let temp_item = items.iter().find(|i| i.message.contains("temperature"));
        assert!(temp_item.is_some());
        assert_eq!(temp_item.unwrap().severity, Severity::Ok);
    }

    #[test]
    fn config_validation_warns_no_channels() {
        let config = Config::default();
        let mut items = Vec::new();
        check_config_semantics(&config, &mut items);
        let ch_item = items.iter().find(|i| i.message.contains("channel"));
        assert!(ch_item.is_some());
        assert_eq!(ch_item.unwrap().severity, Severity::Warn);
    }

    #[test]
    fn configured_model_provider_api_key_uses_alias_profile() {
        let mut config = Config::default();
        config
            .providers
            .models
            .ensure("custom", "local")
            .expect("known model_provider type")
            .api_key = Some("redacted-test-key".to_string());

        assert_eq!(
            configured_model_provider_api_key(&config, "custom.local"),
            Some("redacted-test-key")
        );
        assert_eq!(configured_model_provider_api_key(&config, "custom"), None);
    }

    #[test]
    fn doctor_model_provider_uses_alias_profile() {
        let mut config = Config::default();
        let profile = config
            .providers
            .models
            .ensure("custom", "local")
            .expect("known model_provider type");
        profile.api_key = Some("redacted-test-key".to_string());
        profile.uri = Some("https://models.example.test/v1".to_string());

        if let Err(error) = create_doctor_model_provider(&config, "custom.local") {
            panic!("doctor model probe should build custom providers from alias config: {error}");
        }
    }

    #[tokio::test]
    async fn structured_run_includes_model_probe_results() {
        let mut config = Config::default();
        let profile = config
            .providers
            .models
            .ensure("custom", "local")
            .expect("known model_provider type");
        profile.api_key = Some("redacted-test-key".to_string());
        profile.uri = Some("http://127.0.0.1:9/v1".to_string());

        let baseline = diagnose(&config);
        assert!(
            !baseline
                .iter()
                .any(|item| item.category == "providers.models")
        );

        let full = run_structured(&config).await;
        assert!(
            full.iter().any(|item| item.category == "providers.models"),
            "shared structured runner should include the same model probe rows as the CLI"
        );
    }

    #[test]
    fn config_validation_catches_unknown_provider() {
        // Typed slots can only hold canonical family names, so an unknown
        // family can no longer reach `iter_entries()`. The
        // remaining reachable path is `agent.model_provider`, which is a
        // free-form `String` an operator can set to any dotted ref.
        let mut config = Config::default();
        config.agents.insert(
            "broken".to_string(),
            zeroclaw_config::schema::AliasedAgentConfig {
                model_provider: "totally-fake.default".into(),
                risk_profile: "default".into(),
                ..Default::default()
            },
        );
        let mut items = Vec::new();
        check_config_semantics(&config, &mut items);
        let prov_item = items.iter().find(|i| {
            i.message
                .contains("agent \"broken\" uses invalid model_provider \"totally-fake\"")
        });
        assert!(
            prov_item.is_some(),
            "doctor should flag unknown agent model_provider"
        );
        assert_eq!(prov_item.unwrap().severity, Severity::Warn);
    }

    // The pre-Phase-6 tests `config_validation_catches_malformed_custom_provider`
    // and `config_validation_accepts_custom_provider` are obsolete: the typed
    // ModelProviders container can't represent malformed `custom:` outer keys at
    // all. Custom-URL model_providers now live under the `custom` typed slot with the
    // operator-supplied URL in `base.uri`. The malformed-custom-key validator
    // path is unreachable.

    #[test]
    fn config_validation_warns_empty_model_route() {
        let config = Config {
            model_routes: vec![zeroclaw_config::schema::ModelRouteConfig {
                hint: "fast".into(),
                model_provider: "groq".into(),
                model: String::new(),
                api_key: None,
            }],
            ..Config::default()
        };
        let mut items = Vec::new();
        check_config_semantics(&config, &mut items);
        let route_item = items.iter().find(|i| i.message.contains("empty model"));
        assert!(route_item.is_some());
        assert_eq!(route_item.unwrap().severity, Severity::Warn);
    }

    #[test]
    fn config_validation_warns_empty_embedding_route_model() {
        let config = Config {
            embedding_routes: vec![zeroclaw_config::schema::EmbeddingRouteConfig {
                hint: "semantic".into(),
                model_provider: "openai".into(),
                model: String::new(),
                dimensions: Some(1536),
                api_key: None,
            }],
            ..Config::default()
        };

        let mut items = Vec::new();
        check_config_semantics(&config, &mut items);
        let route_item = items.iter().find(|item| {
            item.message
                .contains("embedding route \"semantic\" has empty model")
        });
        assert!(route_item.is_some());
        assert_eq!(route_item.unwrap().severity, Severity::Warn);
    }

    #[test]
    fn config_validation_warns_invalid_embedding_route_provider() {
        let config = Config {
            embedding_routes: vec![zeroclaw_config::schema::EmbeddingRouteConfig {
                hint: "semantic".into(),
                model_provider: "groq".into(),
                model: "text-embedding-3-small".into(),
                dimensions: None,
                api_key: None,
            }],
            ..Config::default()
        };

        let mut items = Vec::new();
        check_config_semantics(&config, &mut items);
        let route_item = items.iter().find(|item| {
            item.message
                .contains("uses invalid model_provider \"groq\"")
        });
        assert!(route_item.is_some());
        assert_eq!(route_item.unwrap().severity, Severity::Warn);
    }

    #[test]
    fn config_validation_surfaces_dangling_fallback_ref() {
        use zeroclaw_config::schema::{ModelProviderConfig, NvidiaModelProviderConfig};

        let mut config = Config::default();
        config.providers.models.nvidia.insert(
            "nvidia".to_string(),
            NvidiaModelProviderConfig {
                base: ModelProviderConfig {
                    model: Some("stepfun-ai/step-3.5-flash".into()),
                    fallback: vec![zeroclaw_config::providers::ModelProviderRef::new(
                        "deepseek-ai/deepseek-v4-flash",
                    )],
                    ..Default::default()
                },
            },
        );

        let mut items = Vec::new();
        check_config_semantics(&config, &mut items);
        let fallback_item = items.iter().find(|item| {
            item.message
                .contains("does not resolve to a configured providers.models entry")
                && item
                    .message
                    .contains("providers.models.nvidia.nvidia.fallback[0]")
        });
        assert!(
            fallback_item.is_some(),
            "doctor should surface dangling fallback refs"
        );
        assert_eq!(fallback_item.unwrap().severity, Severity::Warn);
    }

    #[test]
    fn config_validation_warns_missing_embedding_hint_target() {
        let mut config = Config::default();
        config.memory.embedding_model = "hint:semantic".into();

        let mut items = Vec::new();
        check_config_semantics(&config, &mut items);
        let route_item = items.iter().find(|item| {
            item.message
                .contains("no matching [[embedding_routes]] entry exists")
        });
        assert!(route_item.is_some());
        assert_eq!(route_item.unwrap().severity, Severity::Warn);
    }

    #[test]
    fn environment_check_finds_git() {
        let mut items = Vec::new();
        check_environment(&mut items);
        let git_item = items.iter().find(|i| i.message.starts_with("git:"));
        // git should be available in any CI/dev environment
        assert!(git_item.is_some());
        assert_eq!(git_item.unwrap().severity, Severity::Ok);
    }

    #[test]
    fn parse_df_available_mb_uses_last_data_line() {
        let stdout =
            "Filesystem 1M-blocks Used Available Use% Mounted on\n/dev/sda1 1000 500 500 50% /\n";
        assert_eq!(parse_df_available_mb(stdout), Some(500));
    }

    #[test]
    fn truncate_for_display_preserves_utf8_boundaries() {
        let preview = truncate_for_display("🙂example-alpha-build", 3);
        assert_eq!(preview, "🙂ex…");
    }

    #[test]
    fn workspace_probe_path_is_hidden_and_unique() {
        let tmp = TempDir::new().unwrap();
        let first = workspace_probe_path(tmp.path());
        let second = workspace_probe_path(tmp.path());

        assert_ne!(first, second);
        assert!(
            first
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(".zeroclaw_doctor_probe_"))
        );
    }

    /// Build a Config whose install root is `root`, with an existing
    /// `data_dir` (so `check_workspace` doesn't early-return) and no agents.
    /// `config_path` anchors `install_root_dir()` → `agent_workspace_dir()`.
    fn workspace_test_config(root: &Path) -> Config {
        let mut config = Config {
            config_path: root.join("config.toml"),
            data_dir: root.join("data"),
            ..Config::default()
        };
        std::fs::create_dir_all(&config.data_dir).unwrap();
        config.agents.clear();
        config
    }

    fn add_enabled_agent(config: &mut Config, alias: &str) {
        config.agents.insert(
            alias.to_string(),
            zeroclaw_config::schema::AliasedAgentConfig {
                enabled: true,
                ..Default::default()
            },
        );
    }

    #[test]
    fn check_workspace_finds_soul_in_agent_workspace_not_data_dir() {
        let tmp = TempDir::new().unwrap();
        let mut config = workspace_test_config(tmp.path());
        add_enabled_agent(&mut config, "default");

        // SOUL.md lives in the agent workspace — the real load location.
        let ws = config.agent_workspace_dir("default");
        std::fs::create_dir_all(&ws).unwrap();
        std::fs::write(ws.join("SOUL.md"), b"# soul").unwrap();
        // A decoy in data_dir must NOT satisfy the check (proves we don't
        // probe data_dir for personality files).
        std::fs::write(config.data_dir.join("SOUL.md"), b"# decoy").unwrap();

        let mut items = Vec::new();
        check_workspace(&config, &mut items);

        let soul = items
            .iter()
            .find(|i| i.message.contains("SOUL.md"))
            .expect("SOUL.md diagnostic present");
        assert_eq!(soul.severity, Severity::Ok);
        assert_eq!(soul.message, "[default] SOUL.md present");
        // No bare data_dir-style message ever surfaces.
        assert!(
            !items.iter().any(|i| i.message == "SOUL.md present"),
            "doctor must not report SOUL.md from data_dir"
        );
    }

    #[test]
    fn check_workspace_warns_when_agent_soul_missing() {
        let tmp = TempDir::new().unwrap();
        let mut config = workspace_test_config(tmp.path());
        add_enabled_agent(&mut config, "default");
        // Workspace dir need not exist; the file simply isn't there.

        let mut items = Vec::new();
        check_workspace(&config, &mut items);

        let soul = items
            .iter()
            .find(|i| i.message.contains("SOUL.md"))
            .expect("SOUL.md diagnostic present");
        assert_eq!(soul.severity, Severity::Warn);
        assert_eq!(soul.message, "[default] SOUL.md not found (optional)");
    }

    #[test]
    fn check_workspace_skips_disabled_agents() {
        let tmp = TempDir::new().unwrap();
        let mut config = workspace_test_config(tmp.path());
        config.agents.insert(
            "dormant".to_string(),
            zeroclaw_config::schema::AliasedAgentConfig {
                enabled: false,
                ..Default::default()
            },
        );

        let mut items = Vec::new();
        check_workspace(&config, &mut items);

        assert!(
            !items.iter().any(|i| i.message.contains("dormant")),
            "disabled agents must not produce workspace-file diagnostics"
        );
    }

    #[test]
    fn check_workspace_checks_each_enabled_agent() {
        let tmp = TempDir::new().unwrap();
        let mut config = workspace_test_config(tmp.path());
        add_enabled_agent(&mut config, "alpha");
        add_enabled_agent(&mut config, "zeta");

        let mut items = Vec::new();
        check_workspace(&config, &mut items);

        // Each enabled agent gets its own SOUL.md + AGENTS.md probe, named.
        let messages: Vec<&str> = items.iter().map(|i| i.message.as_str()).collect();
        for alias in ["alpha", "zeta"] {
            let expected = format!("[{alias}] SOUL.md not found (optional)");
            assert!(
                messages.contains(&expected.as_str()),
                "expected per-agent SOUL.md diagnostic for {alias}; got {messages:?}"
            );
        }
    }

    #[test]
    fn diagnose_flags_web_dist_dir_with_tilde() {
        // Asserts the localized Fluent message resolves and inlines the path +
        // the tilde reason — the diagnostic now goes through Fluent per
        // AGENTS.md (#6961 Round 3).
        let mut config = Config::default();
        config.gateway.web_dist_dir = Some("~/web-dist".to_string());

        let expected_reason = crate::i18n::get_required_cli_string("cli-web-dist-dir-reason-tilde");
        let expected_message = crate::i18n::get_required_cli_string_with_args(
            "cli-doctor-web-dist-dir-expansion-warning",
            &[("path", "~/web-dist"), ("reason", expected_reason.as_str())],
        );

        let results = diagnose(&config);
        let hit = results
            .iter()
            .find(|item| item.category == "config" && item.message == expected_message);
        assert!(
            hit.is_some(),
            "doctor should flag web_dist_dir = \"~/web-dist\" with the localized warning; \
             expected message: {expected_message:?}; got: {results:?}"
        );
        assert_eq!(hit.unwrap().severity, Severity::Warn);
    }

    #[test]
    fn diagnose_flags_web_dist_dir_with_env_var() {
        let mut config = Config::default();
        config.gateway.web_dist_dir = Some("$HOME/web-dist".to_string());

        let expected_reason =
            crate::i18n::get_required_cli_string("cli-web-dist-dir-reason-dollar");
        let expected_message = crate::i18n::get_required_cli_string_with_args(
            "cli-doctor-web-dist-dir-expansion-warning",
            &[
                ("path", "$HOME/web-dist"),
                ("reason", expected_reason.as_str()),
            ],
        );

        let results = diagnose(&config);
        let hit = results
            .iter()
            .find(|item| item.category == "config" && item.message == expected_message);
        assert!(hit.is_some());
        assert_eq!(hit.unwrap().severity, Severity::Warn);
    }

    #[test]
    fn diagnose_accepts_literal_web_dist_dir() {
        let mut config = Config::default();
        config.gateway.web_dist_dir = Some("/srv/zeroclaw/web-dist".to_string());

        let results = diagnose(&config);
        assert!(
            !results
                .iter()
                .any(|item| item.message.contains("gateway.web_dist_dir")),
            "literal web_dist_dir paths should produce no doctor diagnostic"
        );
    }

    #[test]
    fn web_dist_dir_expansion_reason_key_detects_tilde_and_env() {
        assert_eq!(
            web_dist_dir_expansion_reason_key("~/web-dist"),
            Some("cli-web-dist-dir-reason-tilde")
        );
        assert_eq!(
            web_dist_dir_expansion_reason_key("$HOME/web-dist"),
            Some("cli-web-dist-dir-reason-dollar")
        );
        assert_eq!(
            web_dist_dir_expansion_reason_key("${HOME}/web-dist"),
            Some("cli-web-dist-dir-reason-dollar")
        );
        assert!(web_dist_dir_expansion_reason_key("/srv/zeroclaw/web-dist").is_none());
        assert!(web_dist_dir_expansion_reason_key("./dist").is_none());
    }

    #[test]
    fn config_validation_reports_delegate_agents_in_sorted_order() {
        let mut config = Config::default();
        config.agents.insert(
            "zeta".into(),
            zeroclaw_config::schema::AliasedAgentConfig {
                model_provider: "totally-fake.default".into(),
                ..Default::default()
            },
        );
        config.agents.insert(
            "alpha".into(),
            zeroclaw_config::schema::AliasedAgentConfig {
                model_provider: "totally-fake.default".into(),
                ..Default::default()
            },
        );

        let mut items = Vec::new();
        check_config_semantics(&config, &mut items);

        let agent_messages: Vec<_> = items
            .iter()
            .filter(|item| item.message.starts_with("agent \""))
            .map(|item| item.message.as_str())
            .collect();

        assert_eq!(agent_messages.len(), 2);
        assert!(agent_messages[0].contains("agent \"alpha\""));
        assert!(agent_messages[1].contains("agent \"zeta\""));
    }
}
