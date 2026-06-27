//! `zeroclaw self-test` — quick and full diagnostic checks.

use anyhow::Result;
use std::path::Path;
use zeroclaw_runtime::i18n::get_required_cli_string_with_args;

/// Result of a single diagnostic check.
pub struct CheckResult {
    pub name: &'static str,
    pub passed: bool,
    pub detail: String,
}

impl CheckResult {
    fn pass(name: &'static str, detail: impl Into<String>) -> Self {
        Self {
            name,
            passed: true,
            detail: detail.into(),
        }
    }
    fn fail(name: &'static str, detail: impl Into<String>) -> Self {
        Self {
            name,
            passed: false,
            detail: detail.into(),
        }
    }
}

/// Run the quick self-test suite (no network required).
pub async fn run_quick(config: &crate::config::Config) -> Result<Vec<CheckResult>> {
    let mut results = Vec::new();

    // 1. Config file exists and parses
    results.push(check_config(config));

    // 2. Workspace directory is writable
    results.push(check_workspace(&config.data_dir).await);

    // 3. SQLite memory backend opens
    results.push(check_sqlite(&config.data_dir));

    // 4. ModelProvider registry has entries
    results.push(check_model_provider_registry());

    // 5. Tool registry has entries
    results.push(check_tool_registry(config));

    // 6. Channel registry loads
    results.push(check_channel_config(config));

    // 7. Security policy parses
    results.push(check_security_policy(config));

    // 8. Version sanity
    results.push(check_version());

    // 9. gateway.web_dist_dir is a literal path (no shell-style expansion)
    results.push(check_web_dist_dir(config));

    Ok(results)
}

/// Run the full self-test suite (includes network checks).
pub async fn run_full(config: &crate::config::Config) -> Result<Vec<CheckResult>> {
    let mut results = run_quick(config).await?;

    // 9. Gateway health endpoint
    results.push(check_gateway_health(config).await);

    // 10. Memory write/read round-trip
    results.push(check_memory_roundtrip(config).await);

    // 11. WebSocket handshake
    #[cfg(feature = "gateway")]
    results.push(check_websocket_handshake(config).await);

    Ok(results)
}

/// Print results in a formatted table.
pub fn print_results(results: &[CheckResult]) {
    let total = results.len();
    let passed = results.iter().filter(|r| r.passed).count();
    let failed = total - passed;

    println!();
    for (i, r) in results.iter().enumerate() {
        let icon = if r.passed {
            "\x1b[32m✓\x1b[0m"
        } else {
            "\x1b[31m✗\x1b[0m"
        };
        println!("  {} {}/{} {} — {}", icon, i + 1, total, r.name, r.detail);
    }
    println!();
    if failed == 0 {
        println!(
            "  \x1b[32m{}\x1b[0m",
            get_required_cli_string_with_args(
                "cli-selftest-all-passed",
                &[("total", &total.to_string())]
            )
        );
    } else {
        println!(
            "  \x1b[31m{}\x1b[0m",
            get_required_cli_string_with_args(
                "cli-selftest-some-failed",
                &[
                    ("failed", &failed.to_string()),
                    ("total", &total.to_string())
                ],
            )
        );
    }
    println!();
}

fn check_config(config: &crate::config::Config) -> CheckResult {
    if config.config_path.exists() {
        CheckResult::pass(
            "config",
            format!("loaded from {}", config.config_path.display()),
        )
    } else {
        CheckResult::fail("config", "config file not found (using defaults)")
    }
}

async fn check_workspace(workspace_dir: &Path) -> CheckResult {
    match tokio::fs::metadata(workspace_dir).await {
        Ok(meta) if meta.is_dir() => {
            // Try writing a temp file
            let test_file = workspace_dir.join(".selftest_probe");
            match tokio::fs::write(&test_file, b"ok").await {
                Ok(()) => {
                    let _ = tokio::fs::remove_file(&test_file).await;
                    CheckResult::pass(
                        "workspace",
                        format!("{} (writable)", workspace_dir.display()),
                    )
                }
                Err(e) => CheckResult::fail(
                    "workspace",
                    format!("{} (not writable: {e})", workspace_dir.display()),
                ),
            }
        }
        Ok(_) => CheckResult::fail(
            "workspace",
            format!("{} exists but is not a directory", workspace_dir.display()),
        ),
        Err(e) => CheckResult::fail(
            "workspace",
            format!("{} (error: {e})", workspace_dir.display()),
        ),
    }
}

fn check_sqlite(workspace_dir: &Path) -> CheckResult {
    let db_path = workspace_dir.join("memory.db");
    match rusqlite::Connection::open(&db_path) {
        Ok(conn) => match conn.execute_batch("SELECT 1") {
            Ok(()) => CheckResult::pass("sqlite", "memory.db opens and responds"),
            Err(e) => CheckResult::fail("sqlite", format!("query failed: {e}")),
        },
        Err(e) => CheckResult::fail("sqlite", format!("cannot open memory.db: {e}")),
    }
}

fn check_model_provider_registry() -> CheckResult {
    let model_providers = crate::providers::list_model_providers();
    if model_providers.is_empty() {
        CheckResult::fail("model_providers", "no model providers registered")
    } else {
        CheckResult::pass(
            "model_providers",
            format!("{} model providers available", model_providers.len()),
        )
    }
}

fn check_tool_registry(config: &crate::config::Config) -> CheckResult {
    // Probe one tool registry per enabled agent. V3 has no global default —
    // tools are bound to a specific agent's risk profile.
    let enabled_agents: Vec<&String> = config
        .agents
        .iter()
        .filter(|(_, a)| a.enabled)
        .map(|(alias, _)| alias)
        .collect();
    if enabled_agents.is_empty() {
        return CheckResult::fail("tools", "no enabled agents configured");
    }
    let mut total_tools = 0usize;
    for alias in &enabled_agents {
        let security = match crate::security::SecurityPolicy::for_agent(config, alias) {
            Ok(p) => std::sync::Arc::new(p),
            Err(e) => return CheckResult::fail("tools", format!("agent {alias}: {e}")),
        };
        let tools = crate::tools::default_tools(security);
        if tools.is_empty() {
            return CheckResult::fail("tools", format!("agent {alias}: no tools registered"));
        }
        total_tools = tools.len();
    }
    CheckResult::pass(
        "tools",
        format!(
            "{} enabled agent(s); {} core tools per registry",
            enabled_agents.len(),
            total_tools
        ),
    )
}

fn check_channel_config(config: &crate::config::Config) -> CheckResult {
    let channels = zeroclaw_channels::listing::compiled_channels(&config.channels);
    let configured = channels.iter().filter(|e| e.configured).count();
    let uncompiled = zeroclaw_channels::listing::configured_uncompiled_channels(&config.channels);
    if !uncompiled.is_empty() {
        let names = uncompiled
            .iter()
            .map(|entry| entry.name)
            .collect::<Vec<_>>()
            .join(", ");
        return CheckResult::fail(
            "channels",
            channel_config_uncompiled_detail(channels.len(), configured, &names),
        );
    }
    CheckResult::pass(
        "channels",
        format!(
            "{} channel types, {} configured",
            channels.len(),
            configured
        ),
    )
}

fn channel_config_uncompiled_detail(
    compiled: usize,
    compiled_configured: usize,
    names: &str,
) -> String {
    let compiled = compiled.to_string();
    let configured = compiled_configured.to_string();
    get_required_cli_string_with_args(
        "cli-selftest-channel-config-uncompiled",
        &[
            ("compiled", compiled.as_str()),
            ("configured", configured.as_str()),
            ("names", names),
        ],
    )
}

fn check_security_policy(config: &crate::config::Config) -> CheckResult {
    // Probe the security policy of every enabled agent. V3 binds policy
    // to risk_profile per agent; there is no global "active" policy.
    let enabled_agents: Vec<&String> = config
        .agents
        .iter()
        .filter(|(_, a)| a.enabled)
        .map(|(alias, _)| alias)
        .collect();
    if enabled_agents.is_empty() {
        return CheckResult::fail("security", "no enabled agents configured");
    }
    let mut summaries = Vec::new();
    for alias in &enabled_agents {
        let Some(profile) = config.risk_profile_for_agent(alias) else {
            return CheckResult::fail(
                "security",
                format!(
                    "agents.{alias}.risk_profile does not name a configured risk_profiles entry"
                ),
            );
        };
        if let Err(e) = crate::security::SecurityPolicy::for_agent(config, alias) {
            return CheckResult::fail("security", format!("agent {alias}: {e}"));
        }
        summaries.push(format!("{alias}={:?}", profile.level));
    }
    CheckResult::pass("security", summaries.join(", "))
}

fn check_version() -> CheckResult {
    let version = env!("CARGO_PKG_VERSION");
    CheckResult::pass("version", format!("v{version}"))
}

/// Flag `gateway.web_dist_dir` values that rely on shell-style expansion
/// (a leading `~` or any `$VAR` / `${VAR}`). The gateway reads this field
/// verbatim and never invokes a shell, so values like `~/web-dist` or
/// `$HOME/web-dist` resolve to literal on-disk paths and silently fail to
/// find the bundled assets — surface that here at `zeroclaw self-test`
/// time instead of at runtime.
///
/// User-facing strings (check name + detail) go through Fluent
/// (`cli-self-test-web-dist-dir-*` keys) per AGENTS.md § Localization —
/// no bare Rust literals for CLI output. The check `name` field is
/// `&'static str`, so we resolve the Fluent string once into a leaked
/// static at first call. Reason phrases are Fluent keys too
/// (`cli-web-dist-dir-reason-{tilde,dollar}`).
fn check_web_dist_dir(config: &crate::config::Config) -> CheckResult {
    let name = web_dist_dir_check_name();
    match config.gateway.web_dist_dir.as_deref() {
        None => CheckResult::pass(
            name,
            zeroclaw_runtime::i18n::get_required_cli_string(
                "cli-self-test-web-dist-dir-pass-unset",
            ),
        ),
        Some(value) => match web_dist_dir_expansion_reason_key(value) {
            None => CheckResult::pass(
                name,
                zeroclaw_runtime::i18n::get_required_cli_string_with_args(
                    "cli-self-test-web-dist-dir-pass-literal",
                    &[("path", value)],
                ),
            ),
            Some(reason_key) => {
                let reason = zeroclaw_runtime::i18n::get_required_cli_string(reason_key);
                CheckResult::fail(
                    name,
                    zeroclaw_runtime::i18n::get_required_cli_string_with_args(
                        "cli-self-test-web-dist-dir-fail-expansion",
                        &[("path", value), ("reason", reason.as_str())],
                    ),
                )
            }
        },
    }
}

/// Resolve the localized check name once and cache it as a `&'static str`
/// (CheckResult::name is `&'static str` to stay copyable across the table
/// renderer). Falls back to the bare identifier if the Fluent string is
/// missing (mirrors the `missing_cli_string` warn-log behavior).
fn web_dist_dir_check_name() -> &'static str {
    use std::sync::OnceLock;
    static CACHED: OnceLock<&'static str> = OnceLock::new();
    CACHED.get_or_init(|| {
        let resolved =
            zeroclaw_runtime::i18n::get_required_cli_string("cli-self-test-web-dist-dir-name");
        Box::leak(resolved.into_boxed_str())
    })
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

/// Resolve a wildcard bind address (`0.0.0.0`, `[::]`) to a concrete
/// loopback target so the probe can actually connect — and report the
/// configured value alongside so the user isn't confused about why the
/// output says `127.0.0.1` when their `config.toml` says `0.0.0.0`
///. Returns `(probe_host, display_host)` where `display_host`
/// is `Some(_)` only when a rewrite happened.
fn resolve_probe_host(configured: &str) -> (&str, Option<&str>) {
    match configured {
        "0.0.0.0" => ("127.0.0.1", Some("0.0.0.0")),
        // Normalise both shapes to bracketed form for the display URL so the
        // unbracketed `::` doesn't yield `http://:::42617` (three colons,
        // invalid URL). The probe target stays `[::1]`.
        "[::]" | "::" => ("[::1]", Some("[::]")),
        other => (other, None),
    }
}

fn format_probe_url(scheme: &str, configured_host: &str, port: u16, path: &str) -> String {
    let (probe_host, display_host) = resolve_probe_host(configured_host);
    let probed = format!("{scheme}://{probe_host}:{port}{path}");
    match display_host {
        Some(cfg) => {
            format!("{scheme}://{cfg}:{port}{path} (probed via {scheme}://{probe_host}:{port})")
        }
        None => probed,
    }
}

async fn check_gateway_health(config: &crate::config::Config) -> CheckResult {
    let port = config.gateway.port;
    let (probe_host, _) = resolve_probe_host(&config.gateway.host);
    let probe_url = format!("http://{probe_host}:{port}/health");
    let display_url = format_probe_url("http", &config.gateway.host, port, "/health");
    match reqwest::Client::new()
        .get(&probe_url)
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => {
            CheckResult::pass("gateway", format!("health OK at {display_url}"))
        }
        Ok(resp) => CheckResult::fail("gateway", format!("health returned {}", resp.status())),
        Err(e) => CheckResult::fail("gateway", format!("not reachable at {display_url}: {e}")),
    }
}

async fn check_memory_roundtrip(config: &crate::config::Config) -> CheckResult {
    let mem = match crate::memory::create_memory(&config.memory, &config.data_dir, None) {
        Ok(m) => m,
        Err(e) => return CheckResult::fail("memory", format!("cannot create backend: {e}")),
    };

    let test_key = "__selftest_probe__";
    let test_value = "selftest_ok";

    if let Err(e) = mem
        .store(
            test_key,
            test_value,
            crate::memory::MemoryCategory::Core,
            None,
        )
        .await
    {
        return CheckResult::fail("memory", format!("write failed: {e}"));
    }

    match mem.recall(test_key, 1, None, None, None).await {
        Ok(entries) if !entries.is_empty() => {
            let _ = mem.forget(test_key).await;
            CheckResult::pass("memory", "write/read/delete round-trip OK")
        }
        Ok(_) => {
            let _ = mem.forget(test_key).await;
            CheckResult::fail("memory", "no entries returned after round-trip")
        }
        Err(e) => {
            let _ = mem.forget(test_key).await;
            CheckResult::fail("memory", format!("read failed: {e}"))
        }
    }
}

#[cfg(feature = "gateway")]
async fn check_websocket_handshake(config: &crate::config::Config) -> CheckResult {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    use tokio_tungstenite::tungstenite::http::header;

    let port = config.gateway.port;
    let (probe_host, _) = resolve_probe_host(&config.gateway.host);
    let alias = config.agents.keys().next().map(String::as_str);
    let token = resolve_gateway_bearer_token(config);
    let display_url = format_probe_url("ws", &config.gateway.host, port, "/ws/chat");

    if config.gateway.require_pairing && token.is_none() {
        return CheckResult::fail(
            "websocket",
            format!(
                "pairing required but no bearer token available for self-test \
                 (set ZEROCLAW_GATEWAY_TOKEN or keep a plaintext zc_* entry in \
                 gateway.paired_tokens): {display_url}"
            ),
        );
    }

    let probe_url = build_websocket_probe_url(
        probe_host,
        port,
        alias,
        config.gateway.require_pairing,
        token.as_deref(),
    );

    let request = match probe_url.as_str().into_client_request() {
        Ok(mut req) => {
            if let Some(token) = token {
                if let Ok(value) = header::HeaderValue::from_str(&format!("Bearer {token}")) {
                    req.headers_mut().insert(header::AUTHORIZATION, value);
                }
            }
            req
        }
        Err(e) => {
            return CheckResult::fail(
                "websocket",
                format!("failed to build websocket request for {display_url}: {e}"),
            );
        }
    };

    match tokio_tungstenite::connect_async(request).await {
        Ok((_, _)) => CheckResult::pass("websocket", format!("handshake OK at {display_url}")),
        Err(e) => CheckResult::fail(
            "websocket",
            format!("handshake failed at {display_url}: {e}"),
        ),
    }
}

/// Build the websocket probe URL for the self-test handshake.
///
/// When `require_pairing` is true, the resolved plaintext token (if any) is
/// appended as a query parameter so the browser-compatible query-token path
/// is exercised alongside the `Authorization: Bearer` header. The separator
/// is `?` when the URL has no query string yet (no-agent fallback) and `&`
/// when `?agent=` is already present, so the appended segment is always a
/// valid query pair on the `/ws/chat` route.
#[cfg(feature = "gateway")]
fn build_websocket_probe_url(
    probe_host: &str,
    port: u16,
    alias: Option<&str>,
    require_pairing: bool,
    token: Option<&str>,
) -> String {
    let mut url = match alias {
        Some(alias) => format!("ws://{probe_host}:{port}/ws/chat?agent={alias}"),
        None => format!("ws://{probe_host}:{port}/ws/chat"),
    };
    if require_pairing {
        if let Some(token) = token {
            let sep = if url.contains('?') { '&' } else { '?' };
            url.push(sep);
            url.push_str("token=");
            url.push_str(token);
        }
    }
    url
}

/// Resolve a plaintext gateway bearer token for local diagnostics.
///
/// Precedence: `ZEROCLAW_GATEWAY_TOKEN`, then `ZEROCLAW_ACP_BRIDGE_TOKEN`,
/// then the first plaintext (`zc_*`) entry in `gateway.paired_tokens`.
#[cfg(feature = "gateway")]
fn resolve_gateway_bearer_token(config: &crate::config::Config) -> Option<String> {
    for key in ["ZEROCLAW_GATEWAY_TOKEN", "ZEROCLAW_ACP_BRIDGE_TOKEN"] {
        if let Ok(value) = std::env::var(key) {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    config
        .gateway
        .paired_tokens
        .iter()
        .map(|t| t.trim())
        .find(|t| t.starts_with("zc_"))
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "gateway")]
    use super::{build_websocket_probe_url, resolve_gateway_bearer_token};
    use super::{format_probe_url, resolve_probe_host, web_dist_dir_expansion_reason_key};
    #[cfg(feature = "gateway")]
    use zeroclaw_config::schema::Config;

    #[test]
    fn web_dist_dir_with_tilde_resolves_to_tilde_reason_key() {
        // Issue #6079: `~/web-dist` is read verbatim and silently fails.
        // #6961 Round 3: predicate now returns Fluent key, not bare phrase.
        assert_eq!(
            web_dist_dir_expansion_reason_key("~/web-dist"),
            Some("cli-web-dist-dir-reason-tilde")
        );
        assert_eq!(
            web_dist_dir_expansion_reason_key("~"),
            Some("cli-web-dist-dir-reason-tilde")
        );
    }

    #[test]
    fn web_dist_dir_with_env_var_resolves_to_dollar_reason_key() {
        // Issue #6079: `$HOME/web-dist` and `${HOME}/web-dist` are read verbatim.
        assert_eq!(
            web_dist_dir_expansion_reason_key("$HOME/web-dist"),
            Some("cli-web-dist-dir-reason-dollar")
        );
        assert_eq!(
            web_dist_dir_expansion_reason_key("${HOME}/web-dist"),
            Some("cli-web-dist-dir-reason-dollar")
        );
        assert_eq!(
            web_dist_dir_expansion_reason_key("/srv/$USER/dist"),
            Some("cli-web-dist-dir-reason-dollar")
        );
        // Absolute and relative literal paths must NOT be flagged.
        assert!(web_dist_dir_expansion_reason_key("/srv/zeroclaw/web-dist").is_none());
        assert!(web_dist_dir_expansion_reason_key("./dist").is_none());
    }

    #[test]
    fn check_web_dist_dir_emits_localized_fail_for_tilde() {
        // #6961 Round 3: the failure detail goes through Fluent
        // (cli-self-test-web-dist-dir-fail-expansion) — assert the
        // resolved English string contains the inlined path + reason.
        let mut config = crate::config::Config::default();
        config.gateway.web_dist_dir = Some("~/web-dist".to_string());

        let result = super::check_web_dist_dir(&config);
        assert!(!result.passed, "tilde path must fail the check");

        let expected_reason =
            zeroclaw_runtime::i18n::get_required_cli_string("cli-web-dist-dir-reason-tilde");
        let expected_detail = zeroclaw_runtime::i18n::get_required_cli_string_with_args(
            "cli-self-test-web-dist-dir-fail-expansion",
            &[("path", "~/web-dist"), ("reason", expected_reason.as_str())],
        );
        assert_eq!(result.detail, expected_detail);

        let expected_name =
            zeroclaw_runtime::i18n::get_required_cli_string("cli-self-test-web-dist-dir-name");
        assert_eq!(result.name, expected_name.as_str());
    }

    #[test]
    fn check_web_dist_dir_emits_localized_pass_for_literal() {
        let mut config = crate::config::Config::default();
        config.gateway.web_dist_dir = Some("/srv/zeroclaw/web-dist".to_string());

        let result = super::check_web_dist_dir(&config);
        assert!(result.passed);

        let expected_detail = zeroclaw_runtime::i18n::get_required_cli_string_with_args(
            "cli-self-test-web-dist-dir-pass-literal",
            &[("path", "/srv/zeroclaw/web-dist")],
        );
        assert_eq!(result.detail, expected_detail);
    }

    #[test]
    fn check_web_dist_dir_emits_localized_pass_when_unset() {
        let config = crate::config::Config::default();
        let result = super::check_web_dist_dir(&config);
        assert!(result.passed);

        let expected_detail = zeroclaw_runtime::i18n::get_required_cli_string(
            "cli-self-test-web-dist-dir-pass-unset",
        );
        assert_eq!(result.detail, expected_detail);
    }

    #[test]
    fn resolve_probe_host_ipv4_wildcard() {
        assert_eq!(
            resolve_probe_host("0.0.0.0"),
            ("127.0.0.1", Some("0.0.0.0"))
        );
    }

    #[test]
    fn resolve_probe_host_ipv6_wildcard_bracketed() {
        assert_eq!(resolve_probe_host("[::]"), ("[::1]", Some("[::]")));
    }

    #[test]
    fn resolve_probe_host_ipv6_wildcard_unbracketed_normalises_to_brackets() {
        // Regression: previously returned `Some("::")`, which `format_probe_url`
        // would render as `http://:::42617/...` (three colons, invalid URL).
        assert_eq!(resolve_probe_host("::"), ("[::1]", Some("[::]")));
    }

    #[test]
    fn resolve_probe_host_concrete_host_passthrough() {
        assert_eq!(resolve_probe_host("127.0.0.1"), ("127.0.0.1", None));
        assert_eq!(
            resolve_probe_host("example.internal"),
            ("example.internal", None)
        );
    }

    #[test]
    fn format_probe_url_ipv4_wildcard_shows_both() {
        assert_eq!(
            format_probe_url("http", "0.0.0.0", 42617, "/health"),
            "http://0.0.0.0:42617/health (probed via http://127.0.0.1:42617)"
        );
    }

    #[test]
    fn format_probe_url_ipv6_wildcard_unbracketed_shows_valid_url() {
        // Regression: was `http://:::42617/health`, now `http://[::]:42617/health`.
        assert_eq!(
            format_probe_url("http", "::", 42617, "/health"),
            "http://[::]:42617/health (probed via http://[::1]:42617)"
        );
    }

    #[test]
    fn format_probe_url_ipv6_wildcard_bracketed_shows_valid_url() {
        assert_eq!(
            format_probe_url("http", "[::]", 42617, "/health"),
            "http://[::]:42617/health (probed via http://[::1]:42617)"
        );
    }

    #[test]
    fn format_probe_url_concrete_host_no_probe_suffix() {
        assert_eq!(
            format_probe_url("ws", "127.0.0.1", 42617, "/ws/chat"),
            "ws://127.0.0.1:42617/ws/chat"
        );
    }

    #[cfg(feature = "gateway")]
    #[test]
    fn resolve_gateway_bearer_token_reads_plaintext_paired_token() {
        let mut config = Config::default();
        config.gateway.paired_tokens = vec!["zc_test".into()];
        assert_eq!(
            resolve_gateway_bearer_token(&config).as_deref(),
            Some("zc_test")
        );
    }

    #[cfg(feature = "gateway")]
    #[test]
    fn resolve_gateway_bearer_token_ignores_hashed_paired_tokens() {
        let mut config = Config::default();
        config.gateway.paired_tokens = vec!["a".repeat(64)];
        assert!(resolve_gateway_bearer_token(&config).is_none());
    }

    #[cfg(feature = "gateway")]
    #[test]
    fn build_websocket_probe_url_uses_amp_when_agent_alias_present() {
        // Agent-alias branch: URL already has `?agent=`, so the token appends
        // with `&` to keep the query string valid.
        let url = build_websocket_probe_url("127.0.0.1", 42617, Some("dev"), true, Some("zc_test"));
        assert_eq!(url, "ws://127.0.0.1:42617/ws/chat?agent=dev&token=zc_test");
    }

    #[cfg(feature = "gateway")]
    #[test]
    fn build_websocket_probe_url_uses_question_mark_when_no_alias() {
        // Regression for PR #7732: previously the no-alias fallback appended
        // `&token=` to a URL with no `?`, producing
        // `ws://.../ws/chat&token=...` which is not a valid query string and
        // would fail the handshake for the wrong reason on instances that
        // have no configured agents but do require pairing.
        let url = build_websocket_probe_url("127.0.0.1", 42617, None, true, Some("zc_test"));
        assert_eq!(url, "ws://127.0.0.1:42617/ws/chat?token=zc_test");
    }

    #[cfg(feature = "gateway")]
    #[test]
    fn build_websocket_probe_url_omits_token_when_pairing_not_required() {
        let url =
            build_websocket_probe_url("127.0.0.1", 42617, Some("dev"), false, Some("zc_test"));
        assert_eq!(url, "ws://127.0.0.1:42617/ws/chat?agent=dev");
    }

    #[cfg(feature = "gateway")]
    #[test]
    fn build_websocket_probe_url_omits_token_when_none_resolved() {
        let url = build_websocket_probe_url("127.0.0.1", 42617, None, true, None);
        assert_eq!(url, "ws://127.0.0.1:42617/ws/chat");
    }
}
