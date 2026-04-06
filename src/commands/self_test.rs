//! `zeroclaw self-test` — quick and full diagnostic checks.

use anyhow::Result;
use std::path::Path;

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
    results.push(check_workspace(&config.workspace_dir).await);

    // 3. SQLite memory backend opens
    results.push(check_sqlite(&config.workspace_dir));

    // 4. Provider registry has entries
    results.push(check_provider_registry());

    // 5. Tool registry has entries
    results.push(check_tool_registry(config));

    // 6. Channel registry loads
    results.push(check_channel_config(config));

    // 7. Security policy parses
    results.push(check_security_policy(config));

    // 8. Version sanity
    results.push(check_version());

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
        println!("  \x1b[32mAll {total} checks passed.\x1b[0m");
    } else {
        println!("  \x1b[31m{failed}/{total} checks failed.\x1b[0m");
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

fn check_provider_registry() -> CheckResult {
    let providers = crate::providers::list_providers();
    if providers.is_empty() {
        CheckResult::fail("providers", "no providers registered")
    } else {
        CheckResult::pass(
            "providers",
            format!("{} providers available", providers.len()),
        )
    }
}

fn check_tool_registry(config: &crate::config::Config) -> CheckResult {
    let security = std::sync::Arc::new(crate::security::SecurityPolicy::from_config(
        &config.autonomy,
        &config.workspace_dir,
        None,
    ));
    let tools = crate::tools::default_tools(security);
    if tools.is_empty() {
        CheckResult::fail("tools", "no tools registered")
    } else {
        CheckResult::pass("tools", format!("{} core tools available", tools.len()))
    }
}

fn check_channel_config(config: &crate::config::Config) -> CheckResult {
    let channels = config.channels_config.channels();
    let configured = channels.iter().filter(|(_, c)| *c).count();
    CheckResult::pass(
        "channels",
        format!(
            "{} channel types, {} configured",
            channels.len(),
            configured
        ),
    )
}

fn check_security_policy(config: &crate::config::Config) -> CheckResult {
    let _policy =
        crate::security::SecurityPolicy::from_config(&config.autonomy, &config.workspace_dir, None);
    CheckResult::pass(
        "security",
        format!("autonomy level: {:?}", config.autonomy.level),
    )
}

fn check_version() -> CheckResult {
    let version = env!("CARGO_PKG_VERSION");
    CheckResult::pass("version", format!("v{version}"))
}

async fn check_gateway_health(config: &crate::config::Config) -> CheckResult {
    let port = config.gateway.port;
    let host = if config.gateway.host == "[::]" || config.gateway.host == "0.0.0.0" {
        "127.0.0.1"
    } else {
        &config.gateway.host
    };
    let url = format!("http://{host}:{port}/health");
    match reqwest::Client::new()
        .get(&url)
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => {
            CheckResult::pass("gateway", format!("health OK at {url}"))
        }
        Ok(resp) => CheckResult::fail("gateway", format!("health returned {}", resp.status())),
        Err(e) => CheckResult::fail("gateway", format!("not reachable at {url}: {e}")),
    }
}

async fn check_memory_roundtrip(config: &crate::config::Config) -> CheckResult {
    let mem = match crate::memory::create_memory(
        &config.memory,
        &config.workspace_dir,
        config.api_key.as_deref(),
    ) {
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

async fn check_websocket_handshake(config: &crate::config::Config) -> CheckResult {
    let port = config.gateway.port;
    let host = if config.gateway.host == "[::]" || config.gateway.host == "0.0.0.0" {
        "127.0.0.1"
    } else {
        &config.gateway.host
    };
    let url = format!("ws://{host}:{port}/ws/chat");

    match tokio_tungstenite::connect_async(&url).await {
        Ok((_, _)) => CheckResult::pass("websocket", format!("handshake OK at {url}")),
        Err(e) => CheckResult::fail("websocket", format!("handshake failed at {url}: {e}")),
    }
}
