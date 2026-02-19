use crate::config::Config;
use anyhow::Result;
use chrono::{DateTime, Utc};
use std::io::Write;
use std::path::Path;

const DAEMON_STALE_SECONDS: i64 = 30;
const SCHEDULER_STALE_SECONDS: i64 = 120;
const CHANNEL_STALE_SECONDS: i64 = 300;
const COMMAND_VERSION_PREVIEW_CHARS: usize = 60;

// ── Diagnostic item ──────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Severity {
    Ok,
    Warn,
    Error,
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

    fn icon(&self) -> &'static str {
        match self.severity {
            Severity::Ok => "✅",
            Severity::Warn => "⚠️ ",
            Severity::Error => "❌",
        }
    }
}

// ── Public entry point ───────────────────────────────────────────

pub fn run(config: &Config) -> Result<()> {
    let mut items: Vec<DiagItem> = Vec::new();

    check_config_semantics(config, &mut items);
    check_workspace(config, &mut items);
    check_daemon_state(config, &mut items);
    check_environment(&mut items);

    // Print report
    println!("🩺 ZeroClaw Doctor (enhanced)");
    println!();

    let mut current_cat = "";
    for item in &items {
        if item.category != current_cat {
            current_cat = item.category;
            println!("  [{current_cat}]");
        }
        println!("    {} {}", item.icon(), item.message);
    }

    let errors = items
        .iter()
        .filter(|i| i.severity == Severity::Error)
        .count();
    let warns = items
        .iter()
        .filter(|i| i.severity == Severity::Warn)
        .count();
    let oks = items.iter().filter(|i| i.severity == Severity::Ok).count();

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

fn doctor_model_targets(provider_override: Option<&str>) -> Vec<String> {
    if let Some(provider) = provider_override.map(str::trim).filter(|p| !p.is_empty()) {
        return vec![provider.to_string()];
    }

    crate::providers::list_providers()
        .into_iter()
        .map(|provider| provider.name.to_string())
        .collect()
}

pub fn run_models(config: &Config, provider_override: Option<&str>, use_cache: bool) -> Result<()> {
    let targets = doctor_model_targets(provider_override);

    if targets.is_empty() {
        anyhow::bail!("No providers available for model probing");
    }

    println!("🩺 ZeroClaw Doctor — Model Catalog Probe");
    println!("  Providers to probe: {}", targets.len());
    println!(
        "  Mode: {}",
        if use_cache {
            "cache-first"
        } else {
            "force live refresh"
        }
    );
    println!();

    let mut ok_count = 0usize;
    let mut skipped_count = 0usize;
    let mut auth_count = 0usize;
    let mut error_count = 0usize;

    for provider_name in &targets {
        println!("  [{}]", provider_name);

        match crate::onboard::run_models_refresh(config, Some(provider_name), !use_cache) {
            Ok(()) => {
                ok_count += 1;
                println!("    ✅ model catalog check passed");
            }
            Err(error) => {
                let error_text = format_error_chain(&error);
                match classify_model_probe_error(&error_text) {
                    ModelProbeOutcome::Skipped => {
                        skipped_count += 1;
                        println!("    ⚪ skipped: {}", truncate_for_display(&error_text, 160));
                    }
                    ModelProbeOutcome::AuthOrAccess => {
                        auth_count += 1;
                        println!(
                            "    ⚠️  auth/access: {}",
                            truncate_for_display(&error_text, 160)
                        );
                    }
                    ModelProbeOutcome::Error => {
                        error_count += 1;
                        println!("    ❌ error: {}", truncate_for_display(&error_text, 160));
                    }
                    ModelProbeOutcome::Ok => {
                        ok_count += 1;
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

    if auth_count > 0 {
        println!(
            "  💡 Some providers need valid API keys/plan access before `/models` can be fetched."
        );
    }

    if provider_override.is_some() && ok_count == 0 {
        anyhow::bail!("Model probe failed for target provider")
    }

    Ok(())
}

// ── Config semantic validation ───────────────────────────────────

fn check_config_semantics(config: &Config, items: &mut Vec<DiagItem>) {
    let cat = "config";

    // Config file exists
    if config.config_path.exists() {
        items.push(DiagItem::ok(
            cat,
            format!("config file: {}", config.config_path.display()),
        ));
    } else {
        items.push(DiagItem::error(
            cat,
            format!("config file not found: {}", config.config_path.display()),
        ));
    }

    // Provider validity
    if let Some(ref provider) = config.default_provider {
        if let Some(reason) = provider_validation_error(provider) {
            items.push(DiagItem::error(
                cat,
                format!("default provider \"{provider}\" is invalid: {reason}"),
            ));
        } else {
            items.push(DiagItem::ok(
                cat,
                format!("provider \"{provider}\" is valid"),
            ));
        }
    } else {
        items.push(DiagItem::error(cat, "no default_provider configured"));
    }

    // API key presence
    if config.default_provider.as_deref() != Some("ollama") {
        if config.api_key.is_some() {
            items.push(DiagItem::ok(cat, "API key configured"));
        } else {
            items.push(DiagItem::warn(
                cat,
                "no api_key set (may rely on env vars or provider defaults)",
            ));
        }
    }

    // Model configured
    if config.default_model.is_some() {
        items.push(DiagItem::ok(
            cat,
            format!(
                "default model: {}",
                config.default_model.as_deref().unwrap_or("?")
            ),
        ));
    } else {
        items.push(DiagItem::warn(cat, "no default_model configured"));
    }

    // Temperature range
    if config.default_temperature >= 0.0 && config.default_temperature <= 2.0 {
        items.push(DiagItem::ok(
            cat,
            format!(
                "temperature {:.1} (valid range 0.0–2.0)",
                config.default_temperature
            ),
        ));
    } else {
        items.push(DiagItem::error(
            cat,
            format!(
                "temperature {:.1} is out of range (expected 0.0–2.0)",
                config.default_temperature
            ),
        ));
    }

    // Gateway port range
    let port = config.gateway.port;
    if port > 0 {
        items.push(DiagItem::ok(cat, format!("gateway port: {port}")));
    } else {
        items.push(DiagItem::error(cat, "gateway port is 0 (invalid)"));
    }

    // Reliability: fallback providers
    for fb in &config.reliability.fallback_providers {
        if let Some(reason) = provider_validation_error(fb) {
            items.push(DiagItem::warn(
                cat,
                format!("fallback provider \"{fb}\" is invalid: {reason}"),
            ));
        }
    }

    // Model routes validation
    for route in &config.model_routes {
        if route.hint.is_empty() {
            items.push(DiagItem::warn(cat, "model route with empty hint"));
        }
        if let Some(reason) = provider_validation_error(&route.provider) {
            items.push(DiagItem::warn(
                cat,
                format!(
                    "model route \"{}\" uses invalid provider \"{}\": {}",
                    route.hint, route.provider, reason
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
        if let Some(reason) = embedding_provider_validation_error(&route.provider) {
            items.push(DiagItem::warn(
                cat,
                format!(
                    "embedding route \"{}\" uses invalid provider \"{}\": {}",
                    route.hint, route.provider, reason
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
    {
        if !config
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
    }

    // Channel: at least one configured
    let cc = &config.channels_config;
    let has_channel = cc.telegram.is_some()
        || cc.discord.is_some()
        || cc.slack.is_some()
        || cc.imessage.is_some()
        || cc.matrix.is_some()
        || cc.whatsapp.is_some()
        || cc.email.is_some()
        || cc.irc.is_some()
        || cc.lark.is_some()
        || cc.webhook.is_some();

    if has_channel {
        items.push(DiagItem::ok(cat, "at least one channel configured"));
    } else {
        items.push(DiagItem::warn(
            cat,
            "no channels configured — run `zeroclaw onboard` to set one up",
        ));
    }

    // Delegate agents: provider validity
    let mut agent_names: Vec<_> = config.agents.keys().collect();
    agent_names.sort();
    for name in agent_names {
        let agent = config.agents.get(name).unwrap();
        if let Some(reason) = provider_validation_error(&agent.provider) {
            items.push(DiagItem::warn(
                cat,
                format!(
                    "agent \"{name}\" uses invalid provider \"{}\": {}",
                    agent.provider, reason
                ),
            ));
        }
    }
}

fn provider_validation_error(name: &str) -> Option<String> {
    match crate::providers::create_provider(name, None) {
        Ok(_) => None,
        Err(err) => Some(
            err.to_string()
                .lines()
                .next()
                .unwrap_or("invalid provider")
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
        return Some("custom provider requires a non-empty URL after 'custom:'".into());
    }

    match reqwest::Url::parse(url) {
        Ok(parsed) if matches!(parsed.scheme(), "http" | "https") => None,
        Ok(parsed) => Some(format!(
            "custom provider URL must use http/https, got '{}'",
            parsed.scheme()
        )),
        Err(err) => Some(format!("invalid custom provider URL: {err}")),
    }
}

// ── Workspace integrity ──────────────────────────────────────────

fn check_workspace(config: &Config, items: &mut Vec<DiagItem>) {
    let cat = "workspace";
    let ws = &config.workspace_dir;

    if ws.exists() {
        items.push(DiagItem::ok(
            cat,
            format!("directory exists: {}", ws.display()),
        ));
    } else {
        items.push(DiagItem::error(
            cat,
            format!("directory missing: {}", ws.display()),
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

    // Key workspace files
    check_file_exists(ws, "SOUL.md", false, cat, items);
    check_file_exists(ws, "AGENTS.md", false, cat, items);
}

fn check_file_exists(
    base: &Path,
    name: &str,
    required: bool,
    cat: &'static str,
    items: &mut Vec<DiagItem>,
) {
    let path = base.join(name);
    if path.is_file() {
        items.push(DiagItem::ok(cat, format!("{name} present")));
    } else if required {
        items.push(DiagItem::error(cat, format!("{name} missing")));
    } else {
        items.push(DiagItem::warn(cat, format!("{name} not found (optional)")));
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

    // Shell
    let shell = std::env::var("SHELL").unwrap_or_default();
    if shell.is_empty() {
        items.push(DiagItem::warn(cat, "$SHELL not set"));
    } else {
        items.push(DiagItem::ok(cat, format!("shell: {shell}")));
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
    fn provider_validation_checks_custom_url_shape() {
        assert!(provider_validation_error("openrouter").is_none());
        assert!(provider_validation_error("custom:https://example.com").is_none());
        assert!(provider_validation_error("anthropic-custom:https://example.com").is_none());

        let invalid_custom = provider_validation_error("custom:").unwrap_or_default();
        assert!(invalid_custom.contains("requires a URL"));

        let invalid_unknown = provider_validation_error("totally-fake").unwrap_or_default();
        assert!(invalid_unknown.contains("Unknown provider"));
    }

    #[test]
    fn diag_item_icons() {
        assert_eq!(DiagItem::ok("t", "m").icon(), "✅");
        assert_eq!(DiagItem::warn("t", "m").icon(), "⚠️ ");
        assert_eq!(DiagItem::error("t", "m").icon(), "❌");
    }

    #[test]
    fn classify_model_probe_error_marks_unsupported_as_skipped() {
        let outcome = classify_model_probe_error(
            "Provider 'copilot' does not support live model discovery yet",
        );
        assert_eq!(outcome, ModelProbeOutcome::Skipped);
    }

    #[test]
    fn classify_model_probe_error_marks_auth_and_plan_issues() {
        let auth_outcome = classify_model_probe_error("OpenAI API error (401): unauthorized");
        assert_eq!(auth_outcome, ModelProbeOutcome::AuthOrAccess);

        let plan_outcome = classify_model_probe_error(
            "Z.AI API error (429): plan does not include requested model",
        );
        assert_eq!(plan_outcome, ModelProbeOutcome::AuthOrAccess);
    }

    #[test]
    fn config_validation_catches_bad_temperature() {
        let mut config = Config::default();
        config.default_temperature = 5.0;
        let mut items = Vec::new();
        check_config_semantics(&config, &mut items);
        let temp_item = items.iter().find(|i| i.message.contains("temperature"));
        assert!(temp_item.is_some());
        assert_eq!(temp_item.unwrap().severity, Severity::Error);
    }

    #[test]
    fn config_validation_accepts_valid_temperature() {
        let mut config = Config::default();
        config.default_temperature = 0.7;
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
    fn config_validation_catches_unknown_provider() {
        let mut config = Config::default();
        config.default_provider = Some("totally-fake".into());
        let mut items = Vec::new();
        check_config_semantics(&config, &mut items);
        let prov_item = items
            .iter()
            .find(|i| i.message.contains("default provider"));
        assert!(prov_item.is_some());
        assert_eq!(prov_item.unwrap().severity, Severity::Error);
    }

    #[test]
    fn config_validation_catches_malformed_custom_provider() {
        let mut config = Config::default();
        config.default_provider = Some("custom:".into());
        let mut items = Vec::new();
        check_config_semantics(&config, &mut items);

        let prov_item = items.iter().find(|item| {
            item.message
                .contains("default provider \"custom:\" is invalid")
        });
        assert!(prov_item.is_some());
        assert_eq!(prov_item.unwrap().severity, Severity::Error);
    }

    #[test]
    fn config_validation_accepts_custom_provider() {
        let mut config = Config::default();
        config.default_provider = Some("custom:https://my-api.com".into());
        let mut items = Vec::new();
        check_config_semantics(&config, &mut items);
        let prov_item = items.iter().find(|i| i.message.contains("is valid"));
        assert!(prov_item.is_some());
        assert_eq!(prov_item.unwrap().severity, Severity::Ok);
    }

    #[test]
    fn config_validation_warns_bad_fallback() {
        let mut config = Config::default();
        config.reliability.fallback_providers = vec!["fake-provider".into()];
        let mut items = Vec::new();
        check_config_semantics(&config, &mut items);
        let fb_item = items
            .iter()
            .find(|i| i.message.contains("fallback provider"));
        assert!(fb_item.is_some());
        assert_eq!(fb_item.unwrap().severity, Severity::Warn);
    }

    #[test]
    fn config_validation_warns_bad_custom_fallback() {
        let mut config = Config::default();
        config.reliability.fallback_providers = vec!["custom:".into()];
        let mut items = Vec::new();
        check_config_semantics(&config, &mut items);

        let fb_item = items.iter().find(|item| {
            item.message
                .contains("fallback provider \"custom:\" is invalid")
        });
        assert!(fb_item.is_some());
        assert_eq!(fb_item.unwrap().severity, Severity::Warn);
    }

    #[test]
    fn config_validation_warns_empty_model_route() {
        let mut config = Config::default();
        config.model_routes = vec![crate::config::ModelRouteConfig {
            hint: "fast".into(),
            provider: "groq".into(),
            model: String::new(),
            api_key: None,
        }];
        let mut items = Vec::new();
        check_config_semantics(&config, &mut items);
        let route_item = items.iter().find(|i| i.message.contains("empty model"));
        assert!(route_item.is_some());
        assert_eq!(route_item.unwrap().severity, Severity::Warn);
    }

    #[test]
    fn config_validation_warns_empty_embedding_route_model() {
        let mut config = Config::default();
        config.embedding_routes = vec![crate::config::EmbeddingRouteConfig {
            hint: "semantic".into(),
            provider: "openai".into(),
            model: String::new(),
            dimensions: Some(1536),
            api_key: None,
        }];

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
        let mut config = Config::default();
        config.embedding_routes = vec![crate::config::EmbeddingRouteConfig {
            hint: "semantic".into(),
            provider: "groq".into(),
            model: "text-embedding-3-small".into(),
            dimensions: None,
            api_key: None,
        }];

        let mut items = Vec::new();
        check_config_semantics(&config, &mut items);
        let route_item = items
            .iter()
            .find(|item| item.message.contains("uses invalid provider \"groq\""));
        assert!(route_item.is_some());
        assert_eq!(route_item.unwrap().severity, Severity::Warn);
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
        assert!(first
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with(".zeroclaw_doctor_probe_")));
    }

    #[test]
    fn config_validation_reports_delegate_agents_in_sorted_order() {
        let mut config = Config::default();
        config.agents.insert(
            "zeta".into(),
            crate::config::DelegateAgentConfig {
                provider: "totally-fake".into(),
                model: "model-z".into(),
                system_prompt: None,
                api_key: None,
                temperature: None,
                max_depth: 3,
            },
        );
        config.agents.insert(
            "alpha".into(),
            crate::config::DelegateAgentConfig {
                provider: "totally-fake".into(),
                model: "model-a".into(),
                system_prompt: None,
                api_key: None,
                temperature: None,
                max_depth: 3,
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
