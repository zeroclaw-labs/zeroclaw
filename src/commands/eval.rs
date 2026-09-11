//! `zeroclaw eval` — run the agent evaluation harness.

use anyhow::Result;
use std::path::PathBuf;
use std::time::Duration;
use zeroclaw_config::schema::Config;
use zeroclaw_eval::{CaseProvider, LlmTrace, Mode, RunDeps, SuiteReport};
use zeroclaw_providers::factory::{ProviderEndpoint, endpoint_for_family};
use zeroclaw_runtime::agent::agent::build_session_model_provider;

/// True when `provider_ref`'s family invokes a CLI instead of an HTTP model
/// endpoint. The family is the segment before the dot; a bare reference is
/// already a family name.
fn is_cli_backed(provider_ref: &str) -> bool {
    let family = provider_ref
        .split_once('.')
        .map_or(provider_ref, |(family, _)| family);
    matches!(
        endpoint_for_family(family),
        Some(ProviderEndpoint::CliBacked)
    )
}

/// Reject live eval when any provider the session factory could select is
/// CLI-backed.
///
/// A CLI-backed provider launches its own agent process, which keeps the
/// profile's own tool and permission settings and working directory and ignores
/// the tools carried on the chat request. Live eval's confinement only bounds
/// the native tool surface, so such a provider could read host files a case
/// never placed in its workspace even with an empty `[eval].live_allowed_tools`.
/// Until those providers honor the eval boundary they stay out of it.
///
/// The session factory can reach a provider three ways, so all three are
/// checked here, before any provider is constructed and therefore before any
/// subprocess is launched: the reference itself, the fallback chain reachable
/// from any selected profile, and every `[[model_routes]]` target (the router
/// builds each route's provider up front when routes are configured).
fn ensure_no_cli_backed_provider(config: &Config, provider_ref: &str) -> Result<()> {
    // (reference, remaining fallback depth) pairs. The depth budget mirrors the
    // provider factory's own fallback pruning so the guard rejects exactly the
    // profiles that factory can still reach.
    let mut pending: Vec<(String, usize)> = vec![(provider_ref.to_string(), 0)];
    // Routes are only materialized when at least one is configured; that is the
    // same condition the routed factory uses before building every target.
    for route in &config.model_routes {
        pending.push((route.model_provider.clone(), 0));
    }

    let mut seen: Vec<String> = Vec::new();
    while let Some((name, depth)) = pending.pop() {
        let name = name.trim().to_string();
        if name.is_empty() || seen.iter().any(|s| s == &name) {
            continue;
        }
        seen.push(name.clone());

        if is_cli_backed(&name) {
            anyhow::bail!(
                "live eval refuses the CLI-backed provider `{name}` (reached from \
                 [eval].live_provider `{provider_ref}`). CLI-backed providers run their own \
                 agent with the profile's tools, permissions, and working directory, outside \
                 the per-case workspace and [eval].live_allowed_tools. Point \
                 [eval].live_provider at an HTTP model provider, and remove CLI-backed \
                 profiles from its `fallback` chain and from [[model_routes]]."
            );
        }

        if depth >= zeroclaw_config::providers::MAX_FALLBACK_DEPTH {
            continue;
        }
        if let Some((_, _, entry)) = config.providers.models.find_by_name(&name) {
            for fallback in &entry.fallback {
                pending.push((fallback.as_str().to_string(), depth + 1));
            }
        }
    }
    Ok(())
}

/// Reject a zero per-turn timeout before the runner wraps every turn in it.
///
/// Schema validation already rejects zero, but `Config::load_or_init` demotes
/// validation failures to warnings, so the value still reaches this entry point.
/// A zero duration makes every turn time out, which reads as a provider fault
/// rather than a configuration mistake, so fail closed with the actionable
/// message instead.
fn ensure_case_timeout(case_timeout_secs: u64) -> Result<Duration> {
    if case_timeout_secs == 0 {
        anyhow::bail!(
            "[eval].case_timeout_secs must be greater than zero; a zero per-turn timeout \
             expires every live turn before the provider can answer"
        );
    }
    Ok(Duration::from_secs(case_timeout_secs))
}

/// Build the per-run dependencies for the requested mode, threading the loaded
/// config so live mode can resolve its provider. Replay injects the deterministic
/// trace-replay provider; live resolves `[eval].live_provider` per case.
fn build_run_deps(config: &Config, mode: Mode) -> Result<RunDeps> {
    match mode {
        // Replay's provider wiring is owned by `RunDeps::replay()`; delegate so the
        // trace-replay factory has a single definition. Replay ignores the live-only
        // tool allowlist and timeout.
        Mode::Replay => Ok(RunDeps::replay()),
        Mode::Live => {
            // Trim so validation (which trims) and runtime resolution agree: a
            // whitespace-padded ref must not pass `Config::validate` then miss here.
            let provider_ref = config.eval.live_provider.as_str().trim().to_string();
            zeroclaw_eval::ensure_live_provider(&provider_ref)?;
            ensure_no_cli_backed_provider(config, &provider_ref)?;
            let case_timeout = ensure_case_timeout(config.eval.case_timeout_secs)?;
            // The provider closure must be `'static`, so it owns a config clone and
            // builds a fresh provider per case (isolation).
            let cfg = config.clone();
            Ok(RunDeps {
                mode,
                provider: Box::new(move |_trace: &LlmTrace| {
                    let (provider, provider_type, resolved_model) =
                        build_session_model_provider(&cfg, &provider_ref, None)?;
                    Ok(CaseProvider {
                        provider,
                        provider_name: Some(provider_type),
                        model_name: Some(resolved_model),
                        finish_turn: None,
                    })
                }),
                live_tools: config.eval.live_allowed_tools.clone(),
                case_timeout,
            })
        }
    }
}

/// Run a suite of eval cases and return the aggregated report.
pub async fn run(config: &Config, suite: PathBuf, mode: Mode) -> Result<SuiteReport> {
    let deps = build_run_deps(config, mode)?;
    Box::pin(zeroclaw_eval::run_suite(&suite, &deps)).await
}

/// Output format for the eval report.
#[derive(Copy, Clone, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum OutputFormat {
    /// Human-readable table.
    Table,
    /// Machine-readable JSON, for CI artifacts.
    Json,
}

/// Render a suite report in the requested format.
pub fn print_report(report: &SuiteReport, format: OutputFormat) {
    match format {
        OutputFormat::Json => println!("{}", report.to_json()),
        OutputFormat::Table => println!("{}", report.render_table()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeroclaw_config::providers::ModelProviderRef;
    use zeroclaw_config::schema::{
        CustomModelProviderConfig, GrokCliModelProviderConfig, ModelRouteConfig,
    };

    /// A config whose `[eval].live_provider` points at an HTTP provider profile
    /// that live mode is allowed to use.
    fn http_live_config() -> Config {
        let mut config = Config::default();
        let mut http = CustomModelProviderConfig::default();
        http.base.uri = Some("http://127.0.0.1:9/v1".to_string());
        http.base.model = Some("mock-echo".to_string());
        config
            .providers
            .models
            .custom
            .insert("mock".to_string(), http);
        config.eval.live_provider = ModelProviderRef::from("custom.mock");
        config.eval.case_timeout_secs = 30;
        config
    }

    /// The error text from a live-mode build that must be rejected. `RunDeps`
    /// holds a boxed provider factory and so is not `Debug`, which rules out
    /// `expect_err`.
    fn live_rejection(config: &Config) -> String {
        match build_run_deps(config, Mode::Live) {
            Ok(_) => panic!("live mode must reject this configuration"),
            Err(err) => err.to_string(),
        }
    }

    /// Register a CLI-backed profile so a selection path can reach it.
    fn insert_cli_profile(config: &mut Config, alias: &str) {
        config
            .providers
            .models
            .grok_cli
            .insert(alias.to_string(), GrokCliModelProviderConfig::default());
    }

    #[test]
    fn live_accepts_an_http_provider() {
        let config = http_live_config();
        assert!(build_run_deps(&config, Mode::Live).is_ok());
    }

    #[test]
    fn live_rejects_a_directly_selected_cli_backed_provider() {
        let mut config = http_live_config();
        insert_cli_profile(&mut config, "default");
        config.eval.live_provider = ModelProviderRef::from("grok_cli.default");

        let message = live_rejection(&config);
        assert!(
            message.contains("grok_cli.default"),
            "the error must name the rejected profile: {message}"
        );
    }

    #[test]
    fn live_rejects_a_cli_backed_provider_reachable_through_a_fallback() {
        let mut config = http_live_config();
        insert_cli_profile(&mut config, "fallback");
        config
            .providers
            .models
            .custom
            .get_mut("mock")
            .expect("the HTTP profile was inserted above")
            .base
            .fallback = vec![ModelProviderRef::from("grok_cli.fallback")];

        let message = live_rejection(&config);
        assert!(
            message.contains("grok_cli.fallback"),
            "the error must name the rejected profile: {message}"
        );
    }

    #[test]
    fn live_rejects_a_cli_backed_model_route_target() {
        let mut config = http_live_config();
        insert_cli_profile(&mut config, "routed");
        config.model_routes = vec![ModelRouteConfig {
            hint: "code".to_string(),
            model_provider: "grok_cli.routed".to_string(),
            ..ModelRouteConfig::default()
        }];

        let message = live_rejection(&config);
        assert!(
            message.contains("grok_cli.routed"),
            "the error must name the rejected profile: {message}"
        );
    }

    #[test]
    fn live_rejects_a_zero_case_timeout() {
        let mut config = http_live_config();
        config.eval.case_timeout_secs = 0;

        let message = live_rejection(&config);
        assert!(
            message.contains("case_timeout_secs"),
            "the error must name the offending key: {message}"
        );
    }

    #[test]
    fn replay_ignores_the_live_provider_and_timeout() {
        let mut config = http_live_config();
        insert_cli_profile(&mut config, "default");
        config.eval.live_provider = ModelProviderRef::from("grok_cli.default");
        config.eval.case_timeout_secs = 0;

        assert!(build_run_deps(&config, Mode::Replay).is_ok());
    }
}
