//! Extism-based WASM execution bridge.
//!
//! Creates Extism plugin instances with permission-gated host functions
//! (`zc_http_request`, `zc_env_read`) and calls plugin-exported functions
//! (`tool_metadata`, `execute`).

use crate::PluginPermission;
use anyhow::{Context, Result};
use extism::*;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, ToSocketAddrs};
use std::path::Path;
use zeroclaw_api::tool::ToolResult;

/// Maximum linear memory for a plugin instance: 4096 WebAssembly pages ×
/// 64 KiB = 256 MiB. Generous for the API-wrapper plugins we ship (which
/// encode one key + a small body and read a JSON response) while bounding a
/// runaway allocation so a buggy/malicious plugin can't OOM the host.
const MAX_WASM_PAGES: u32 = 4096;

/// Extism epoch-interruption timeout for a single plugin call, in milliseconds.
/// Must exceed the 120 s outbound HTTP ceiling in [`handle_http_request`] so a
/// legitimate slow call (e.g. cold model inference) is not trapped mid-flight.
/// Epoch interruption only fires at wasm instruction boundaries, so this bounds
/// runaway *wasm* (tight loops); a stall inside the host's blocking HTTP call is
/// bounded by the reqwest client timeout instead.
pub(crate) const EXEC_TIMEOUT_MS: u64 = 180_000;

/// Compile-time invariant: the epoch timeout must outlast the 120 s HTTP ceiling.
const _: () = assert!(EXEC_TIMEOUT_MS > 120_000);

// ── Host function context ─────────────────────────────────────────

/// Permissions and scoping available to a single plugin invocation.
#[derive(Debug, Clone)]
struct HostContext {
    permissions: HashSet<PluginPermission>,
    /// Optional per-plugin egress allowlist. `None` = no host restriction
    /// (the always-on SSRF guard still applies).
    allowed_hosts: Option<Vec<String>>,
    /// Optional per-plugin env-var allowlist. `None` = permissive (back-compat).
    env_allowlist: Option<Vec<String>>,
}

// ── Data types exchanged with plugins ─────────────────────────────

/// HTTP request sent from plugin to host via `zc_http_request`.
#[derive(Debug, Serialize, Deserialize)]
struct HttpRequest {
    method: String,
    url: String,
    #[serde(default)]
    headers: std::collections::HashMap<String, String>,
    #[serde(default)]
    body: Option<String>,
}

/// HTTP response returned from host to plugin.
#[derive(Debug, Serialize, Deserialize)]
struct HttpResponse {
    status: u16,
    body: String,
    #[serde(default)]
    headers: std::collections::HashMap<String, String>,
}

/// Tool metadata returned by the `tool_metadata` export.
#[derive(Debug, Serialize, Deserialize)]
pub struct ToolMetadata {
    pub name: String,
    pub description: String,
    pub parameters_schema: serde_json::Value,
}

/// Result returned by the `execute` export.
#[derive(Debug, Serialize, Deserialize)]
struct PluginToolResult {
    success: bool,
    output: String,
    #[serde(default)]
    error: Option<String>,
}

// ── Host function implementations ─────────────────────────────────

fn handle_http_request(
    plugin: &mut CurrentPlugin,
    inputs: &[Val],
    outputs: &mut [Val],
    user_data: UserData<HostContext>,
) -> Result<(), Error> {
    let ctx = user_data.get()?;
    let ctx = ctx.lock().unwrap();

    if !ctx.permissions.contains(&PluginPermission::HttpClient) {
        return Err(Error::msg(
            "permission denied: plugin does not have 'http_client' permission",
        ));
    }

    // Read input string from WASM memory
    let request_json: String = plugin.memory_get_val(&inputs[0])?;

    let req: HttpRequest = serde_json::from_str(&request_json)
        .map_err(|e| Error::msg(format!("invalid HTTP request JSON: {e}")))?;

    // SSRF guard: always block non-public destinations; additionally enforce the
    // per-plugin host allowlist when one is configured.
    validate_egress(&req.url, ctx.allowed_hosts.as_deref())?;

    // 120s ceiling covers legitimate slow cases: large file downloads and slow
    // model-inference endpoints (fal.ai image generation routinely takes 20-60s
    // on cold models). A per-plugin override or tighter default is a candidate
    // follow-up — see ADR-003 §"Known gaps". Note: this runs inside
    // spawn_blocking, so a stalled request holds a blocking-pool thread for
    // the full duration.
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .map_err(|e| Error::msg(format!("failed to create HTTP client: {e}")))?;

    let mut builder = match req.method.to_uppercase().as_str() {
        "GET" => client.get(&req.url),
        "POST" => client.post(&req.url),
        "PUT" => client.put(&req.url),
        "DELETE" => client.delete(&req.url),
        "PATCH" => client.patch(&req.url),
        "HEAD" => client.head(&req.url),
        other => {
            return Err(Error::msg(format!("unsupported HTTP method: {other}")));
        }
    };

    for (k, v) in &req.headers {
        builder = builder.header(k.as_str(), v.as_str());
    }

    if let Some(body) = req.body {
        builder = builder.body(body);
    }

    let resp = builder
        .send()
        .map_err(|e| Error::msg(format!("HTTP request failed: {e}")))?;

    let status = resp.status().as_u16();
    let headers: std::collections::HashMap<String, String> = resp
        .headers()
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
        .collect();
    let body = resp
        .text()
        .map_err(|e| Error::msg(format!("failed to read response body: {e}")))?;

    let response = HttpResponse {
        status,
        body,
        headers,
    };

    let response_json = serde_json::to_string(&response)
        .map_err(|e| Error::msg(format!("failed to serialize response: {e}")))?;

    plugin.memory_set_val(&mut outputs[0], response_json)?;

    Ok(())
}

fn handle_env_read(
    plugin: &mut CurrentPlugin,
    inputs: &[Val],
    outputs: &mut [Val],
    user_data: UserData<HostContext>,
) -> Result<(), Error> {
    let ctx = user_data.get()?;
    let ctx = ctx.lock().unwrap();

    if !ctx.permissions.contains(&PluginPermission::EnvRead) {
        return Err(Error::msg(
            "permission denied: plugin does not have 'env_read' permission",
        ));
    }

    let var_name: String = plugin.memory_get_val(&inputs[0])?;

    // Per-plugin env scoping. When an allowlist is declared, deny anything
    // outside it; when absent, permit (back-compat) but warn once per var so
    // operators can author an allowlist.
    if !env_var_allowed(ctx.env_allowlist.as_deref(), &var_name) {
        return Err(Error::msg(format!(
            "env access denied: '{var_name}' is not in the plugin's env_allowlist"
        )));
    }
    if ctx.env_allowlist.is_none() {
        warn_unscoped_env_read(&var_name);
    }

    let value = std::env::var(&var_name)
        .map_err(|_| Error::msg(format!("environment variable '{var_name}' not set")))?;

    plugin.memory_set_val(&mut outputs[0], value)?;

    Ok(())
}

// ── Egress (SSRF) guard ───────────────────────────────────────────

/// Validate that a plugin may issue an HTTP request to `url_str`. Always blocks
/// non-public destinations (SSRF protection); additionally enforces a per-plugin
/// host allowlist when one is configured.
fn validate_egress(url_str: &str, allowed_hosts: Option<&[String]>) -> Result<(), Error> {
    let url = url::Url::parse(url_str)
        .map_err(|e| Error::msg(format!("invalid URL '{url_str}': {e}")))?;

    match url.scheme() {
        "http" | "https" => {}
        other => {
            return Err(Error::msg(format!(
                "URL scheme '{other}' not allowed (http/https only)"
            )));
        }
    }

    let host = url
        .host_str()
        .ok_or_else(|| Error::msg("URL has no host"))?;

    if let Some(allow) = allowed_hosts
        && !allow.iter().any(|p| host_matches(p, host))
    {
        return Err(Error::msg(format!(
            "host '{host}' is not in the plugin's allowed_hosts"
        )));
    }

    // DNS-rebinding defense: resolve and validate every address (not just IP
    // literals). Note: reqwest re-resolves on connect, leaving a small TOCTOU
    // window — full closure requires pinning the validated IP into the
    // connection (documented follow-up).
    let port = url.port_or_known_default().unwrap_or(443);
    let mut resolved_any = false;
    for sa in (host, port)
        .to_socket_addrs()
        .map_err(|e| Error::msg(format!("DNS resolution failed for '{host}': {e}")))?
    {
        resolved_any = true;
        if is_blocked_ip(&sa.ip()) {
            return Err(Error::msg(format!(
                "blocked egress to non-public address {} (resolved from '{host}')",
                sa.ip()
            )));
        }
    }
    if !resolved_any {
        return Err(Error::msg(format!("no addresses resolved for '{host}'")));
    }
    Ok(())
}

/// Whether `ip` is a non-public address plugins must not reach.
fn is_blocked_ip(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_blocked_ipv4(v4),
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(mapped) => is_blocked_ipv4(&mapped),
            None => is_blocked_ipv6(v6),
        },
    }
}

fn is_blocked_ipv4(ip: &Ipv4Addr) -> bool {
    let o = ip.octets();
    ip.is_loopback()            // 127.0.0.0/8
        || ip.is_private()      // 10/8, 172.16/12, 192.168/16
        || ip.is_link_local()   // 169.254.0.0/16 (incl. cloud metadata 169.254.169.254)
        || ip.is_unspecified()  // 0.0.0.0
        || ip.is_broadcast()    // 255.255.255.255
        || ip.is_documentation()
        || (o[0] == 100 && (64..=127).contains(&o[1])) // CGNAT 100.64.0.0/10
}

fn is_blocked_ipv6(ip: &Ipv6Addr) -> bool {
    let seg = ip.segments();
    ip.is_loopback()                    // ::1
        || ip.is_unspecified()          // ::
        || (seg[0] & 0xfe00) == 0xfc00  // unique-local fc00::/7
        || (seg[0] & 0xffc0) == 0xfe80 // link-local fe80::/10
}

/// Match a host against an allowlist pattern: exact, or leading `*.` wildcard.
fn host_matches(pattern: &str, host: &str) -> bool {
    let p = pattern.trim().to_ascii_lowercase();
    let h = host.trim().to_ascii_lowercase();
    match p.strip_prefix("*.") {
        Some(suffix) => h == suffix || h.ends_with(&format!(".{suffix}")),
        None => h == p,
    }
}

/// Whether `var_name` may be read given an optional allowlist. `None` = permit
/// (back-compat); `Some` = only names in the list.
fn env_var_allowed(allowlist: Option<&[String]>, var_name: &str) -> bool {
    match allowlist {
        Some(allow) => allow.iter().any(|a| a == var_name),
        None => true,
    }
}

/// Warn once per env-var name when a plugin reads it without an `env_allowlist`.
fn warn_unscoped_env_read(var_name: &str) {
    use std::sync::{Mutex, OnceLock};
    static WARNED: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    let warned = WARNED.get_or_init(|| Mutex::new(HashSet::new()));
    let is_new = warned
        .lock()
        .map(|mut s| s.insert(var_name.to_string()))
        .unwrap_or(false);
    if is_new {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
            &format!(
                "plugin read env var '{var_name}' with no env_allowlist declared; \
                 add `env_allowlist` to the manifest to scope access"
            )
        );
    }
}

// ── Plugin creation and invocation ────────────────────────────────

/// Create an Extism plugin from a WASM file with the given permissions and no
/// per-plugin egress/env scoping (the always-on SSRF guard still applies).
pub fn create_plugin(wasm_path: &Path, permissions: &[PluginPermission]) -> Result<extism::Plugin> {
    create_plugin_with(wasm_path, permissions, None, None)
}

/// Create an Extism plugin with permissions plus optional per-plugin egress and
/// env allowlists.
pub fn create_plugin_with(
    wasm_path: &Path,
    permissions: &[PluginPermission],
    allowed_hosts: Option<&[String]>,
    env_allowlist: Option<&[String]>,
) -> Result<extism::Plugin> {
    let perm_set: HashSet<PluginPermission> = permissions.iter().cloned().collect();
    let ctx = UserData::new(HostContext {
        permissions: perm_set,
        allowed_hosts: allowed_hosts.map(|s| s.to_vec()),
        env_allowlist: env_allowlist.map(|s| s.to_vec()),
    });

    let http_fn = Function::new(
        "zc_http_request",
        [PTR],
        [PTR],
        ctx.clone(),
        handle_http_request,
    );

    let env_fn = Function::new("zc_env_read", [PTR], [PTR], ctx, handle_env_read);

    // Bound resources so a buggy/malicious plugin can't OOM or wedge the host:
    // a 256 MiB memory cap and an epoch-interruption timeout. (Extism enables
    // epoch interruption automatically when a manifest timeout is set.)
    let manifest = Manifest::new([Wasm::file(wasm_path)])
        .with_memory_max(MAX_WASM_PAGES)
        .with_timeout(std::time::Duration::from_millis(EXEC_TIMEOUT_MS));

    Plugin::new(manifest, [http_fn, env_fn], true)
        .with_context(|| format!("failed to load WASM plugin from {}", wasm_path.display()))
}

/// Call the `tool_metadata` export and parse the result.
pub fn call_tool_metadata(plugin: &mut extism::Plugin) -> Result<ToolMetadata> {
    let output = plugin
        .call::<&str, String>("tool_metadata", "")
        .context("failed to call tool_metadata export")?;

    serde_json::from_str(&output).context("failed to parse tool_metadata JSON")
}

/// Call the `execute` export with the given args JSON and return a `ToolResult`.
pub fn call_execute(plugin: &mut extism::Plugin, args_json: &[u8]) -> Result<ToolResult> {
    let input = std::str::from_utf8(args_json).context("plugin args are not valid UTF-8")?;

    let output = plugin
        .call::<&str, String>("execute", input)
        .context("failed to call plugin execute export")?;

    let result: PluginToolResult =
        serde_json::from_str(&output).context("failed to parse plugin execute result")?;

    Ok(ToolResult {
        success: result.success,
        output: result.output,
        error: result.error,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn egress_blocks_loopback_metadata_and_private() {
        for url in [
            "http://127.0.0.1/",
            "http://169.254.169.254/latest/meta-data/",
            "http://10.0.0.5/",
            "http://192.168.1.1/",
            "http://172.16.0.1/",
            "http://[::1]/",
        ] {
            assert!(validate_egress(url, None).is_err(), "should block {url}");
        }
    }

    #[test]
    fn egress_blocks_ipv4_mapped_v6() {
        // ::ffff:10.0.0.1 maps to the private 10.0.0.1.
        assert!(validate_egress("http://[::ffff:10.0.0.1]/", None).is_err());
    }

    #[test]
    fn egress_allows_public_ip_literal() {
        // IP literal: resolved locally, no network needed.
        assert!(validate_egress("https://8.8.8.8/", None).is_ok());
    }

    #[test]
    fn egress_rejects_non_http_scheme() {
        assert!(validate_egress("file:///etc/passwd", None).is_err());
        assert!(validate_egress("ftp://8.8.8.8/", None).is_err());
    }

    #[test]
    fn egress_allowlist_enforced() {
        // Public IP, but not in the allowlist → blocked before DNS.
        let allow = vec!["*.fal.run".to_string()];
        assert!(validate_egress("https://8.8.8.8/", Some(&allow)).is_err());
    }

    #[test]
    fn host_matches_exact_and_wildcard() {
        assert!(host_matches("api.tavily.com", "api.tavily.com"));
        assert!(!host_matches("api.tavily.com", "evil.com"));
        assert!(host_matches("*.fal.run", "queue.fal.run"));
        assert!(host_matches("*.fal.run", "fal.run"));
        assert!(!host_matches("*.fal.run", "fal.run.evil.com"));
        assert!(!host_matches("*.fal.run", "notfal.run"));
    }

    #[test]
    fn blocked_ip_classification() {
        use std::net::Ipv4Addr;
        assert!(is_blocked_ipv4(&Ipv4Addr::new(169, 254, 169, 254)));
        assert!(is_blocked_ipv4(&Ipv4Addr::new(10, 0, 0, 1)));
        assert!(is_blocked_ipv4(&Ipv4Addr::new(100, 64, 0, 1))); // CGNAT
        assert!(!is_blocked_ipv4(&Ipv4Addr::new(8, 8, 8, 8)));
        assert!(!is_blocked_ipv4(&Ipv4Addr::new(1, 1, 1, 1)));
    }

    #[test]
    fn env_allowlist_logic() {
        let allow = vec!["FAL_API_KEY".to_string()];
        assert!(env_var_allowed(Some(&allow), "FAL_API_KEY"));
        assert!(!env_var_allowed(Some(&allow), "AWS_SECRET_ACCESS_KEY"));
        // No allowlist → permissive (back-compat).
        assert!(env_var_allowed(None, "ANYTHING"));
    }

    #[test]
    fn host_context_permission_check() {
        let ctx = HostContext {
            permissions: HashSet::from([PluginPermission::HttpClient]),
            allowed_hosts: None,
            env_allowlist: None,
        };
        assert!(ctx.permissions.contains(&PluginPermission::HttpClient));
        assert!(!ctx.permissions.contains(&PluginPermission::EnvRead));
    }

    #[test]
    fn http_request_serde_roundtrip() {
        let req = HttpRequest {
            method: "POST".into(),
            url: "https://example.com/api".into(),
            headers: [("Authorization".into(), "Bearer tok".into())]
                .into_iter()
                .collect(),
            body: Some(r#"{"key":"value"}"#.into()),
        };
        let json = serde_json::to_string(&req).unwrap();
        let parsed: HttpRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.method, "POST");
        assert_eq!(parsed.url, "https://example.com/api");
        assert_eq!(parsed.body.as_deref(), Some(r#"{"key":"value"}"#));
    }

    #[test]
    fn tool_metadata_serde() {
        let meta = ToolMetadata {
            name: "test_tool".into(),
            description: "A test tool".into(),
            parameters_schema: serde_json::json!({"type": "object"}),
        };
        let json = serde_json::to_string(&meta).unwrap();
        let parsed: ToolMetadata = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.name, "test_tool");
    }

    #[test]
    fn plugin_tool_result_serde() {
        let result = PluginToolResult {
            success: true,
            output: "hello".into(),
            error: None,
        };
        let json = serde_json::to_string(&result).unwrap();
        let parsed: PluginToolResult = serde_json::from_str(&json).unwrap();
        assert!(parsed.success);
        assert_eq!(parsed.output, "hello");
    }

    #[test]
    fn missing_wasm_file_returns_error() {
        let result = create_plugin(Path::new("/nonexistent/plugin.wasm"), &[]);
        assert!(result.is_err());
    }

    /// Integration tests that load the actual image-gen WASM plugin.
    /// These require the plugin to be built first:
    ///   cd plugins/image-gen-fal && cargo build --target wasm32-wasip1 --release
    mod integration {
        use super::*;

        fn wasm_path() -> Option<std::path::PathBuf> {
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../plugins/image-gen-fal/image_gen_fal.wasm");
            if path.exists() { Some(path) } else { None }
        }

        #[test]
        fn load_and_read_metadata() {
            let Some(path) = wasm_path() else {
                eprintln!("SKIP: image_gen_fal.wasm not found (build the plugin first)");
                return;
            };
            let perms = vec![PluginPermission::HttpClient, PluginPermission::EnvRead];
            let mut plugin = create_plugin(&path, &perms).unwrap();
            let meta = call_tool_metadata(&mut plugin).unwrap();
            assert_eq!(meta.name, "image_gen_fal");
            assert!(meta.description.contains("image"));
            assert!(
                meta.parameters_schema["required"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|v| v == "prompt")
            );
        }

        #[test]
        fn execute_missing_prompt() {
            let Some(path) = wasm_path() else { return };
            let perms = vec![PluginPermission::HttpClient, PluginPermission::EnvRead];
            let mut plugin = create_plugin(&path, &perms).unwrap();
            let args = serde_json::to_vec(&serde_json::json!({})).unwrap();
            let result = call_execute(&mut plugin, &args).unwrap();
            assert!(!result.success);
            assert!(result.error.as_deref().unwrap().contains("prompt"));
        }

        #[test]
        fn execute_invalid_size() {
            let Some(path) = wasm_path() else { return };
            let perms = vec![PluginPermission::HttpClient, PluginPermission::EnvRead];
            let mut plugin = create_plugin(&path, &perms).unwrap();
            let args =
                serde_json::to_vec(&serde_json::json!({"prompt": "test", "size": "bad"})).unwrap();
            let result = call_execute(&mut plugin, &args).unwrap();
            assert!(!result.success);
            assert!(result.error.as_deref().unwrap().contains("Invalid size"));
        }

        #[test]
        fn execute_invalid_model_traversal() {
            let Some(path) = wasm_path() else { return };
            let perms = vec![PluginPermission::HttpClient, PluginPermission::EnvRead];
            let mut plugin = create_plugin(&path, &perms).unwrap();
            let args =
                serde_json::to_vec(&serde_json::json!({"prompt": "test", "model": "../../evil"}))
                    .unwrap();
            let result = call_execute(&mut plugin, &args).unwrap();
            assert!(!result.success);
            assert!(result.error.as_deref().unwrap().contains("Invalid model"));
        }

        /// End-to-end: missing `FAL_API_KEY` exercises the `zc_env_read` host
        /// function — the host returns Err (var unset), which Extism propagates
        /// as a plugin-call trap. Proves the env_read path is wired.
        #[test]
        fn execute_missing_api_key_exercises_env_read_host_fn() {
            let Some(path) = wasm_path() else { return };
            // SAFETY: test-only, single-threaded test runner.
            unsafe { std::env::remove_var("FAL_API_KEY") };
            let perms = vec![PluginPermission::HttpClient, PluginPermission::EnvRead];
            let mut plugin = create_plugin(&path, &perms).unwrap();
            let args = serde_json::to_vec(&serde_json::json!({"prompt": "a sunset"})).unwrap();
            let err = call_execute(&mut plugin, &args).unwrap_err();
            let msg = format!("{err:#}");
            assert!(
                msg.contains("FAL_API_KEY") || msg.contains("not set"),
                "expected env-var error, got: {msg}"
            );
        }

        /// End-to-end permission enforcement: without `EnvRead`, the host
        /// function returns permission-denied and Extism propagates it as a trap.
        #[test]
        fn execute_without_env_read_permission_fails() {
            let Some(path) = wasm_path() else { return };
            // Only HttpClient granted — EnvRead missing
            let perms = vec![PluginPermission::HttpClient];
            let mut plugin = create_plugin(&path, &perms).unwrap();
            let args = serde_json::to_vec(&serde_json::json!({"prompt": "a sunset"})).unwrap();
            let err = call_execute(&mut plugin, &args).unwrap_err();
            let msg = format!("{err:#}");
            assert!(
                msg.contains("permission") || msg.contains("env_read"),
                "expected permission-denied error, got: {msg}"
            );
        }
    }
}
