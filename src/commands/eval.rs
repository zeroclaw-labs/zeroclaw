//! `zeroclaw eval` — run the agent evaluation harness.

use anyhow::Result;
use std::path::PathBuf;
use std::time::Duration;
use zeroclaw_config::providers::ModelProviderRef;
use zeroclaw_config::schema::Config;
use zeroclaw_eval::{CaseProvider, LlmTrace, Mode, RunDeps, SuiteReport};
use zeroclaw_providers::factory::{ProviderEndpoint, endpoint_for_family};
use zeroclaw_runtime::agent::agent::build_session_model_provider;

/// How the provider factory resolves one reference: the family whose endpoint
/// decides CLI-backed versus HTTP, the canonical `family.alias` name used for
/// cycle detection and operator messages, and the `fallback` list the factory
/// would walk from there.
struct ResolvedRef<'a> {
    family: String,
    name: String,
    fallback: &'a [ModelProviderRef],
}

/// Resolve a reference the factory selects directly: `[eval].live_provider` and
/// every `[[model_routes]]` target.
///
/// `create_resilient_model_provider_from_ref_with_model_override` splits such a
/// reference on the first dot and builds `<family>.<alias>` from the typed slot;
/// a bare reference goes to the family-only constructor, which never reads a
/// profile and so carries no fallbacks.
fn resolve_selected_ref<'a>(config: &'a Config, raw: &str) -> ResolvedRef<'a> {
    match raw.split_once('.') {
        Some((family, alias)) => ResolvedRef {
            family: family.to_string(),
            name: format!("{family}.{alias}"),
            fallback: config
                .providers
                .models
                .find(family, alias)
                .map_or(&[][..], |entry| entry.fallback.as_slice()),
        },
        None => ResolvedRef {
            family: raw.to_string(),
            name: raw.to_string(),
            fallback: &[][..],
        },
    }
}

/// Resolve a `fallback` entry the way `append_fallback_chain` does.
///
/// Fallback entries are resolved with `ProviderRefs::find_by_name`, which
/// accepts a bare alias and searches every family's alias map for it. A bare
/// entry therefore selects whatever family owns that alias, so the family must
/// come from the lookup and not from the written text. An entry that does not
/// resolve is classified from its written family anyway, so a CLI-backed
/// reference is refused rather than admitted on the strength of a missing
/// profile.
fn resolve_fallback_ref<'a>(config: &'a Config, raw: &str) -> ResolvedRef<'a> {
    match config.providers.models.find_by_name(raw) {
        Some((family, alias, entry)) => ResolvedRef {
            family: family.to_string(),
            name: format!("{family}.{alias}"),
            fallback: entry.fallback.as_slice(),
        },
        None => ResolvedRef {
            family: raw
                .split_once('.')
                .map_or(raw, |(family, _)| family)
                .to_string(),
            name: raw.to_string(),
            fallback: &[][..],
        },
    }
}

/// Refuse one resolved reference when its family invokes a CLI instead of an
/// HTTP model endpoint.
fn reject_cli_backed(resolved: &ResolvedRef<'_>, provider_ref: &str) -> Result<()> {
    if matches!(
        endpoint_for_family(&resolved.family),
        Some(ProviderEndpoint::CliBacked)
    ) {
        let name = &resolved.name;
        anyhow::bail!(
            "live eval refuses the CLI-backed provider `{name}` (reached from \
             [eval].live_provider `{provider_ref}`). CLI-backed providers run their own \
             agent with the profile's tools, permissions, and working directory, outside \
             the per-case workspace and [eval].live_allowed_tools. Point \
             [eval].live_provider at an HTTP model provider, and remove CLI-backed \
             profiles from its `fallback` chain and from [[model_routes]]."
        );
    }
    Ok(())
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
/// subprocess is launched: the reference itself, every `[[model_routes]]` target
/// (the router builds each route's provider up front when routes are
/// configured), and the `fallback` chain of each of those.
///
/// The chain walk covers every profile reachable through `fallback`, with no
/// depth limit of its own. The factory prunes a chain past
/// `MAX_FALLBACK_DEPTH`, so the guard refuses a superset of what the factory can
/// build: an unreachable CLI-backed profile deep in a chain costs an operator an
/// actionable refusal, whereas mirroring the pruning would have to reproduce the
/// factory's traversal order exactly to stay sound.
fn ensure_no_cli_backed_provider(config: &Config, provider_ref: &str) -> Result<()> {
    let routes = config
        .model_routes
        .iter()
        .map(|route| route.model_provider.as_str());
    let mut pending: Vec<ResolvedRef<'_>> = std::iter::once(provider_ref)
        .chain(routes)
        .map(str::trim)
        .filter(|raw| !raw.is_empty())
        .map(|raw| resolve_selected_ref(config, raw))
        .collect();

    let mut seen: Vec<String> = Vec::new();
    while let Some(resolved) = pending.pop() {
        reject_cli_backed(&resolved, provider_ref)?;
        if seen.iter().any(|name| name == &resolved.name) {
            continue;
        }
        seen.push(resolved.name.clone());
        for entry in resolved.fallback {
            let raw = entry.as_str().trim();
            if raw.is_empty() {
                continue;
            }
            pending.push(resolve_fallback_ref(config, raw));
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

    /// Register an HTTP profile with the given `fallback` chain, so a test can
    /// build the profile graph the provider factory would walk.
    fn insert_http_profile(config: &mut Config, alias: &str, fallback: &[&str]) {
        let mut http = CustomModelProviderConfig::default();
        http.base.uri = Some("http://127.0.0.1:9/v1".to_string());
        http.base.model = Some("mock-echo".to_string());
        http.base.fallback = fallback
            .iter()
            .copied()
            .map(ModelProviderRef::from)
            .collect();
        config
            .providers
            .models
            .custom
            .insert(alias.to_string(), http);
    }

    /// Replace the `fallback` chain of an already-registered custom profile.
    fn set_fallback(config: &mut Config, alias: &str, fallback: &[&str]) {
        config
            .providers
            .models
            .custom
            .get_mut(alias)
            .expect("the profile must be registered first")
            .base
            .fallback = fallback
            .iter()
            .copied()
            .map(ModelProviderRef::from)
            .collect();
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
    fn live_rejects_a_bare_fallback_ref_that_resolves_to_a_cli_backed_profile() {
        let mut config = http_live_config();
        insert_cli_profile(&mut config, "sentinel");
        // A fallback entry is stored as written and resolved by alias lookup
        // across every family, so a dotless entry still selects the CLI-backed
        // profile that owns the alias. Classifying such an entry by its written
        // text would read `sentinel` as a family name and admit it.
        set_fallback(&mut config, "mock", &["sentinel"]);

        let message = live_rejection(&config);
        assert!(
            message.contains("grok_cli.sentinel"),
            "the error must name the profile the bare reference resolves to: {message}"
        );
    }

    #[test]
    fn live_rejects_a_cli_backed_profile_reachable_only_through_a_longer_fallback_path() {
        let mut config = http_live_config();
        insert_cli_profile(&mut config, "evil");
        // A diamond: `custom.x` is reachable both directly from the selected
        // profile and through `custom.y`. Whichever arm the walk takes first, the
        // CLI-backed profile under `custom.x` must still be found.
        set_fallback(&mut config, "mock", &["custom.x", "custom.y"]);
        insert_http_profile(&mut config, "x", &["custom.w"]);
        insert_http_profile(&mut config, "y", &["custom.x"]);
        insert_http_profile(&mut config, "w", &["grok_cli.evil"]);

        let message = live_rejection(&config);
        assert!(
            message.contains("grok_cli.evil"),
            "the error must name the rejected profile: {message}"
        );
    }

    #[test]
    fn live_rejects_a_cli_backed_provider_reachable_through_a_model_route_fallback() {
        let mut config = http_live_config();
        insert_cli_profile(&mut config, "routed_fallback");
        insert_http_profile(&mut config, "routed", &["grok_cli.routed_fallback"]);
        config.model_routes = vec![ModelRouteConfig {
            hint: "code".to_string(),
            model_provider: "custom.routed".to_string(),
            ..ModelRouteConfig::default()
        }];

        let message = live_rejection(&config);
        assert!(
            message.contains("grok_cli.routed_fallback"),
            "the error must name the rejected profile: {message}"
        );
    }

    #[test]
    fn live_accepts_a_fallback_chain_of_http_profiles() {
        let mut config = http_live_config();
        set_fallback(&mut config, "mock", &["custom.second"]);
        insert_http_profile(&mut config, "second", &["custom.third"]);
        insert_http_profile(&mut config, "third", &["custom.mock"]);

        assert!(
            build_run_deps(&config, Mode::Live).is_ok(),
            "an HTTP-only chain, cycle included, must still be admitted"
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
