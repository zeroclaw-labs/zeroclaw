//! Extism-based WASM execution bridge.
//!
//! Creates Extism plugin instances with permission-gated host functions
//! (`zc_http_request`, `zc_env_read`) and calls plugin-exported functions
//! (`tool_metadata`, `execute`).
//!
//! The host functions enforce the plugin's `http_allowed_hosts` /
//! `env_read_vars` allowlists (manifest fields) — see issues #5918 and
//! #5919. Deny-by-default: empty allowlists reject every call.

use crate::url_guard;
use crate::{PluginManifest, PluginPermission};
use anyhow::{Context, Result};
use extism::*;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::Path;
use zeroclaw_api::tool::ToolResult;

// ── Host function context ─────────────────────────────────────────

/// Permissions and policy available to a single plugin invocation.
///
/// Carries the manifest's `http_allowed_hosts` / `allow_private_hosts` and
/// `env_read_vars` pre-parsed into typed policy structs so the host
/// functions don't re-parse the manifest on every call.
#[derive(Debug, Clone)]
struct HostContext {
    permissions: HashSet<PluginPermission>,
    http_policy: HttpPolicy,
    env_policy: EnvPolicy,
}

/// HTTP host policy derived from the manifest.
#[derive(Debug, Clone, Default)]
struct HttpPolicy {
    allow_private_hosts: bool,
    allowed_hosts: Vec<String>,
}

/// Env-read host policy derived from the manifest. The allowlist is a
/// `HashSet` so the per-call check is O(1) regardless of size.
#[derive(Debug, Clone, Default)]
struct EnvPolicy {
    allowed_vars: HashSet<String>,
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

    // Issue #5918: SSRF + host allowlist enforcement. See `validate_request_url`
    // for the four-stage check (extract → allowlist → private → DNS-rebinding).
    let url_for_log = req.url.clone();
    validate_request_url(&ctx, &req.url).map_err(|e| {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                .with_attrs(::serde_json::json!({"url": url_for_log})),
            "zc_http_request: request rejected"
        );
        Error::msg(e)
    })?;

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

    // Issue #5919: per-plugin env-var allowlist. See `validate_env_var` —
    // deny-by-default when the manifest declares no `env_read_vars`.
    validate_env_var(&ctx, &var_name).map_err(|e| {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                .with_attrs(::serde_json::json!({"plugin_requested_var": var_name})),
            "zc_env_read: read rejected"
        );
        Error::msg(e)
    })?;

    let value = std::env::var(&var_name)
        .map_err(|_| Error::msg(format!("environment variable '{var_name}' not set")))?;

    plugin.memory_set_val(&mut outputs[0], value)?;

    Ok(())
}

// ── Validation helpers (extracted for unit testing) ───────────────

/// Issue #5918: validate `url` against the plugin's HTTP policy.
///
/// Order matters:
/// 1. `url_guard::extract_host` — reject malformed / non-http(s) / userinfo / unmatched IPv6 brackets.
/// 2. `host_matches_allowlist` — empty allowlist = deny-by-default.
/// 3. `is_private_or_local_host` — reject loopback / RFC-1918 / IMDS unless `allow_private_hosts = true`.
/// 4. `validate_resolved_host_is_public` — DNS-rebinding guard (`#[cfg(test)]`-stubbed).
///
/// Returns the extracted host on success. Error messages match the contract
/// documented in the plan so tests can assert substrings.
fn validate_request_url(ctx: &HostContext, url: &str) -> Result<String, String> {
    let host = url_guard::extract_host(url).map_err(|e| e.to_string())?;

    if !url_guard::host_matches_allowlist(&host, &ctx.http_policy.allowed_hosts) {
        return Err(format!("http: host '{host}' is not in http_allowed_hosts"));
    }

    if url_guard::is_private_or_local_host(&host) && !ctx.http_policy.allow_private_hosts {
        return Err(format!("Blocked local/private host: {host}"));
    }

    url_guard::validate_resolved_host_is_public(&host).map_err(|e| e.to_string())?;

    Ok(host)
}

/// Issue #5919: validate `var_name` against the plugin's env-var allowlist.
///
/// Deny-by-default: when the manifest declares no `env_read_vars`, every
/// read is rejected — the permission token alone is not enough.
fn validate_env_var(ctx: &HostContext, var_name: &str) -> Result<(), String> {
    if ctx.env_policy.allowed_vars.is_empty() {
        return Err(
            "env: plugin manifest declares no env_read_vars; env_read is denied for all variables"
                .into(),
        );
    }
    if !ctx.env_policy.allowed_vars.contains(var_name) {
        return Err(format!(
            "env: variable '{var_name}' is not in env_read_vars allowlist"
        ));
    }
    Ok(())
}

// ── Plugin creation and invocation ────────────────────────────────

/// Create an Extism plugin from a WASM file with the given permissions.
///
/// Retained for tests and external callers that only need the permission
/// bit-set. New HTTP/env policies default to deny-all (empty allowlists),
/// so plugins calling `zc_http_request` or `zc_env_read` through this
/// entry point will be rejected at the host-function boundary.
///
/// Prefer [`create_plugin_with_manifest`] when the full manifest is
/// available — it carries `http_allowed_hosts` / `env_read_vars` into the
/// host functions so allowlisted plugins can actually make calls.
pub fn create_plugin(wasm_path: &Path, permissions: &[PluginPermission]) -> Result<extism::Plugin> {
    // Construct a minimal manifest carrying only the requested permissions.
    // All new fields default to deny-all (serde defaults).
    let minimal = PluginManifest {
        name: String::new(),
        version: String::new(),
        description: None,
        author: None,
        wasm_path: None,
        capabilities: Vec::new(),
        permissions: permissions.to_vec(),
        allow_private_hosts: false,
        http_allowed_hosts: Vec::new(),
        env_read_vars: Vec::new(),
        signature: None,
        publisher_key: None,
    };
    create_plugin_with_manifest(wasm_path, &minimal)
}

/// Create an Extism plugin with the full manifest so the host functions
/// can enforce HTTP allowlists and env-var allowlists.
///
/// `manifest.http_allowed_hosts`, `manifest.allow_private_hosts`, and
/// `manifest.env_read_vars` are parsed once at construction into
/// [`HttpPolicy`] / [`EnvPolicy`]; the hot path is a single `HashSet`
/// membership test or short `Vec<String>` scan.
pub fn create_plugin_with_manifest(
    wasm_path: &Path,
    manifest: &PluginManifest,
) -> Result<extism::Plugin> {
    let perm_set: HashSet<PluginPermission> = manifest.permissions.iter().cloned().collect();
    let ctx = UserData::new(HostContext {
        permissions: perm_set,
        http_policy: HttpPolicy {
            allow_private_hosts: manifest.allow_private_hosts,
            allowed_hosts: manifest.http_allowed_hosts.clone(),
        },
        env_policy: EnvPolicy {
            allowed_vars: manifest.env_read_vars.iter().cloned().collect(),
        },
    });

    let http_fn = Function::new(
        "zc_http_request",
        [PTR],
        [PTR],
        ctx.clone(),
        handle_http_request,
    );

    let env_fn = Function::new("zc_env_read", [PTR], [PTR], ctx, handle_env_read);

    let plugin_manifest = Manifest::new([Wasm::file(wasm_path)]);

    Plugin::new(plugin_manifest, [http_fn, env_fn], true)
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
    use crate::PluginManifest;

    /// Helper: build a manifest carrying the requested permissions and
    /// the new allowlist fields. Mirrors what the loader passes to
    /// `create_plugin_with_manifest` after parsing `manifest.toml`.
    fn manifest_with(
        perms: Vec<PluginPermission>,
        allow_private_hosts: bool,
        http_allowed_hosts: Vec<String>,
        env_read_vars: Vec<String>,
    ) -> PluginManifest {
        PluginManifest {
            name: "test".into(),
            version: "0.0.1".into(),
            description: None,
            author: None,
            wasm_path: None,
            capabilities: Vec::new(),
            permissions: perms,
            allow_private_hosts,
            http_allowed_hosts,
            env_read_vars,
            signature: None,
            publisher_key: None,
        }
    }

    #[test]
    fn host_context_permission_check() {
        let ctx = HostContext {
            permissions: HashSet::from([PluginPermission::HttpClient]),
            http_policy: HttpPolicy::default(),
            env_policy: EnvPolicy::default(),
        };
        assert!(ctx.permissions.contains(&PluginPermission::HttpClient));
        assert!(!ctx.permissions.contains(&PluginPermission::EnvRead));
        // Default policy fields are deny-all.
        assert!(ctx.http_policy.allowed_hosts.is_empty());
        assert!(!ctx.http_policy.allow_private_hosts);
        assert!(ctx.env_policy.allowed_vars.is_empty());
    }

    #[test]
    fn host_context_carries_http_policy() {
        let ctx = HostContext {
            permissions: HashSet::from([PluginPermission::HttpClient]),
            http_policy: HttpPolicy {
                allow_private_hosts: true,
                allowed_hosts: vec!["*.example.com".into(), "localhost".into()],
            },
            env_policy: EnvPolicy::default(),
        };
        assert!(ctx.http_policy.allow_private_hosts);
        assert_eq!(ctx.http_policy.allowed_hosts.len(), 2);
    }

    #[test]
    fn host_context_carries_env_policy() {
        let ctx = HostContext {
            permissions: HashSet::from([PluginPermission::EnvRead]),
            http_policy: HttpPolicy::default(),
            env_policy: EnvPolicy {
                allowed_vars: HashSet::from(["FOO".to_string(), "BAR".to_string()]),
            },
        };
        assert!(ctx.env_policy.allowed_vars.contains("FOO"));
        assert!(ctx.env_policy.allowed_vars.contains("BAR"));
        assert!(!ctx.env_policy.allowed_vars.contains("BAZ"));
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

    // ── PluginManifest deserialization tests (issues #5918 + #5919) ──

    #[test]
    fn plugin_manifest_deserializes_with_new_fields() {
        let toml_str = r#"
name = "demo"
version = "0.1.0"
capabilities = ["tool"]
permissions = ["http_client", "env_read"]
allow_private_hosts = true
http_allowed_hosts = ["*.example.com", "fal.run"]
env_read_vars = ["FAL_API_KEY"]
"#;
        let manifest: PluginManifest = toml::from_str(toml_str).unwrap();
        assert!(manifest.allow_private_hosts);
        assert_eq!(
            manifest.http_allowed_hosts,
            vec!["*.example.com".to_string(), "fal.run".to_string()]
        );
        assert_eq!(manifest.env_read_vars, vec!["FAL_API_KEY".to_string()]);
    }

    #[test]
    fn plugin_manifest_deserializes_without_new_fields_for_backward_compat() {
        // Existing manifest shape from before #5918/#5919 — must still load.
        let toml_str = r#"
name = "legacy"
version = "0.1.0"
capabilities = ["tool"]
permissions = ["http_client", "env_read"]
"#;
        let manifest: PluginManifest = toml::from_str(toml_str).unwrap();
        assert!(!manifest.allow_private_hosts);
        assert!(manifest.http_allowed_hosts.is_empty());
        assert!(manifest.env_read_vars.is_empty());
    }

    #[test]
    fn plugin_manifest_round_trips_with_new_fields() {
        let original = manifest_with(
            vec![PluginPermission::HttpClient, PluginPermission::EnvRead],
            true,
            vec!["fal.run".into(), "*.fal.run".into()],
            vec!["FAL_API_KEY".into()],
        );
        let json = serde_json::to_string(&original).unwrap();
        let parsed: PluginManifest = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.allow_private_hosts, original.allow_private_hosts);
        assert_eq!(parsed.http_allowed_hosts, original.http_allowed_hosts);
        assert_eq!(parsed.env_read_vars, original.env_read_vars);
        assert_eq!(parsed.permissions.len(), 2);
    }

    // ── Host-function validation tests ────────────────────────────
    //
    // These exercise `validate_request_url` and `validate_env_var` (the
    // extracted pure functions) directly so we can pin the SSRF and
    // allowlist semantics without spinning up a real WASM module.

    fn ctx_with(
        perms: Vec<PluginPermission>,
        allow_private_hosts: bool,
        http_allowed_hosts: Vec<&str>,
        env_read_vars: Vec<&str>,
    ) -> HostContext {
        HostContext {
            permissions: perms.into_iter().collect(),
            http_policy: HttpPolicy {
                allow_private_hosts,
                allowed_hosts: http_allowed_hosts.into_iter().map(String::from).collect(),
            },
            env_policy: EnvPolicy {
                allowed_vars: env_read_vars.into_iter().map(String::from).collect(),
            },
        }
    }

    #[test]
    fn validate_request_url_rejects_non_http_scheme() {
        let ctx = ctx_with(
            vec![PluginPermission::HttpClient],
            false,
            vec!["example.com"],
            vec![],
        );
        let err = validate_request_url(&ctx, "ftp://example.com/x").unwrap_err();
        assert!(err.contains("non-http(s) URL rejected"), "got: {err}");
    }

    #[test]
    fn validate_request_url_rejects_empty_allowlist_by_default() {
        let ctx = ctx_with(vec![PluginPermission::HttpClient], false, vec![], vec![]);
        let err = validate_request_url(&ctx, "https://example.com/x").unwrap_err();
        assert!(err.contains("is not in http_allowed_hosts"), "got: {err}");
    }

    #[test]
    fn validate_request_url_rejects_allowlist_miss() {
        let ctx = ctx_with(
            vec![PluginPermission::HttpClient],
            false,
            vec!["example.com"],
            vec![],
        );
        let err = validate_request_url(&ctx, "https://other.com/x").unwrap_err();
        assert!(
            err.contains("'other.com' is not in http_allowed_hosts"),
            "got: {err}"
        );
    }

    #[test]
    fn validate_request_url_accepts_exact_match() {
        let ctx = ctx_with(
            vec![PluginPermission::HttpClient],
            false,
            vec!["example.com"],
            vec![],
        );
        validate_request_url(&ctx, "https://example.com/x").unwrap();
        validate_request_url(&ctx, "https://EXAMPLE.com/x").unwrap(); // case-insensitive
    }

    #[test]
    fn validate_request_url_accepts_subdomain_match() {
        let ctx = ctx_with(
            vec![PluginPermission::HttpClient],
            false,
            vec!["example.com"],
            vec![],
        );
        validate_request_url(&ctx, "https://api.example.com/x").unwrap();
        validate_request_url(&ctx, "https://v2.api.example.com/x").unwrap();
    }

    #[test]
    fn validate_request_url_accepts_wildcard_subdomain() {
        let ctx = ctx_with(
            vec![PluginPermission::HttpClient],
            false,
            vec!["*.fal.run"],
            vec![],
        );
        validate_request_url(&ctx, "https://fal.run/x").unwrap();
        validate_request_url(&ctx, "https://api.fal.run/x").unwrap();
        let err = validate_request_url(&ctx, "https://other.com/x").unwrap_err();
        assert!(err.contains("'other.com' is not in http_allowed_hosts"));
    }

    #[test]
    fn validate_request_url_rejects_localhost_by_default() {
        let ctx = ctx_with(
            vec![PluginPermission::HttpClient],
            false,
            vec!["localhost"],
            vec![],
        );
        let err = validate_request_url(&ctx, "http://localhost:8080/x").unwrap_err();
        assert!(err.contains("Blocked local/private host"), "got: {err}");
    }

    #[test]
    fn validate_request_url_allows_localhost_when_opt_in() {
        let ctx = ctx_with(
            vec![PluginPermission::HttpClient],
            true,
            vec!["localhost"],
            vec![],
        );
        validate_request_url(&ctx, "http://localhost:8080/x").unwrap();
    }

    #[test]
    fn validate_request_url_rejects_rfc1918_by_default() {
        let ctx = ctx_with(
            vec![PluginPermission::HttpClient],
            false,
            vec!["192.168.1.5"],
            vec![],
        );
        let err = validate_request_url(&ctx, "http://192.168.1.5/x").unwrap_err();
        assert!(err.contains("Blocked local/private host"), "got: {err}");
    }

    #[test]
    fn validate_request_url_rejects_imds_link_local() {
        let ctx = ctx_with(
            vec![PluginPermission::HttpClient],
            false,
            vec!["169.254.169.254"],
            vec![],
        );
        let err =
            validate_request_url(&ctx, "http://169.254.169.254/latest/meta-data").unwrap_err();
        assert!(err.contains("Blocked local/private host"), "got: {err}");
    }

    #[test]
    fn validate_request_url_rejects_ipv6_loopback() {
        let ctx = ctx_with(
            vec![PluginPermission::HttpClient],
            false,
            vec!["::1"],
            vec![],
        );
        let err = validate_request_url(&ctx, "http://[::1]/x").unwrap_err();
        assert!(err.contains("Blocked local/private host"), "got: {err}");
    }

    #[test]
    fn validate_request_url_rejects_userinfo() {
        let ctx = ctx_with(
            vec![PluginPermission::HttpClient],
            false,
            vec!["example.com"],
            vec![],
        );
        let err = validate_request_url(&ctx, "https://user:pass@example.com/x").unwrap_err();
        assert!(err.contains("userinfo is not allowed"), "got: {err}");
    }

    #[test]
    fn validate_request_url_rejects_unmatched_ipv6_brackets() {
        let ctx = ctx_with(
            vec![PluginPermission::HttpClient],
            false,
            vec!["example.com"],
            vec![],
        );
        // `reqwest::Url` rejects `[::1/x` at parse time with "invalid IPv6
        // address" before our bracket-checker runs. Accept either rejection
        // — both are correct fail-closed outcomes.
        let err = validate_request_url(&ctx, "https://[::1/x").unwrap_err();
        assert!(
            err.contains("unmatched IPv6 brackets") || err.contains("invalid URL"),
            "got: {err}"
        );
    }

    #[test]
    fn validate_request_url_wildcard_star_still_rejects_private_host() {
        // `["*"]` allows any PUBLIC host but allow_private_hosts still gates
        // loopback / RFC1918 / link-local / IMDS.
        let ctx = ctx_with(vec![PluginPermission::HttpClient], false, vec!["*"], vec![]);
        validate_request_url(&ctx, "https://news.ycombinator.com/").unwrap();
        let err = validate_request_url(&ctx, "http://localhost/x").unwrap_err();
        assert!(err.contains("Blocked local/private host"), "got: {err}");
    }

    #[test]
    fn validate_env_var_denies_when_allowlist_empty() {
        let ctx = ctx_with(vec![PluginPermission::EnvRead], false, vec![], vec![]);
        let err = validate_env_var(&ctx, "FAL_API_KEY").unwrap_err();
        assert!(err.contains("no env_read_vars"), "got: {err}");
    }

    #[test]
    fn validate_env_var_denies_var_not_in_allowlist() {
        let ctx = ctx_with(
            vec![PluginPermission::EnvRead],
            false,
            vec![],
            vec!["OTHER_VAR"],
        );
        let err = validate_env_var(&ctx, "FAL_API_KEY").unwrap_err();
        assert!(
            err.contains("'FAL_API_KEY' is not in env_read_vars"),
            "got: {err}"
        );
    }

    #[test]
    fn validate_env_var_allows_var_in_allowlist() {
        let ctx = ctx_with(
            vec![PluginPermission::EnvRead],
            false,
            vec![],
            vec!["FAL_API_KEY"],
        );
        validate_env_var(&ctx, "FAL_API_KEY").unwrap();
    }

    #[test]
    fn validate_env_var_is_case_sensitive() {
        let ctx = ctx_with(vec![PluginPermission::EnvRead], false, vec![], vec!["foo"]);
        assert!(validate_env_var(&ctx, "foo").is_ok());
        assert!(validate_env_var(&ctx, "FOO").is_err());
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

        /// Helper for integration tests: build the manifest shape that
        /// matches the updated `image-gen-fal/manifest.toml`.
        fn fal_manifest(perms: Vec<PluginPermission>) -> PluginManifest {
            manifest_with(
                perms,
                false,
                vec!["*.fal.run".to_string(), "fal.run".to_string()],
                vec!["FAL_API_KEY".to_string()],
            )
        }

        #[test]
        fn load_and_read_metadata() {
            let Some(path) = wasm_path() else {
                eprintln!("SKIP: image_gen_fal.wasm not found (build the plugin first)");
                return;
            };
            let manifest = fal_manifest(vec![
                PluginPermission::HttpClient,
                PluginPermission::EnvRead,
            ]);
            let mut plugin = create_plugin_with_manifest(&path, &manifest).unwrap();
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
            let manifest = fal_manifest(vec![
                PluginPermission::HttpClient,
                PluginPermission::EnvRead,
            ]);
            let mut plugin = create_plugin_with_manifest(&path, &manifest).unwrap();
            let args = serde_json::to_vec(&serde_json::json!({})).unwrap();
            let result = call_execute(&mut plugin, &args).unwrap();
            assert!(!result.success);
            assert!(result.error.as_deref().unwrap().contains("prompt"));
        }

        #[test]
        fn execute_invalid_size() {
            let Some(path) = wasm_path() else { return };
            let manifest = fal_manifest(vec![
                PluginPermission::HttpClient,
                PluginPermission::EnvRead,
            ]);
            let mut plugin = create_plugin_with_manifest(&path, &manifest).unwrap();
            let args =
                serde_json::to_vec(&serde_json::json!({"prompt": "test", "size": "bad"})).unwrap();
            let result = call_execute(&mut plugin, &args).unwrap();
            assert!(!result.success);
            assert!(result.error.as_deref().unwrap().contains("Invalid size"));
        }

        #[test]
        fn execute_invalid_model_traversal() {
            let Some(path) = wasm_path() else { return };
            let manifest = fal_manifest(vec![
                PluginPermission::HttpClient,
                PluginPermission::EnvRead,
            ]);
            let mut plugin = create_plugin_with_manifest(&path, &manifest).unwrap();
            let args =
                serde_json::to_vec(&serde_json::json!({"prompt": "test", "model": "../../evil"}))
                    .unwrap();
            let result = call_execute(&mut plugin, &args).unwrap();
            assert!(!result.success);
            assert!(result.error.as_deref().unwrap().contains("Invalid model"));
        }

        /// End-to-end: missing `FAL_API_KEY` exercises the `zc_env_read` host
        /// function — the host returns Err (var unset), which Extism propagates
        /// as a plugin-call trap. Proves the env_read path is wired through
        /// the new `env_read_vars` allowlist (issue #5919).
        #[test]
        fn execute_missing_api_key_exercises_env_read_host_fn() {
            let Some(path) = wasm_path() else { return };
            // SAFETY: test-only, single-threaded test runner.
            unsafe { std::env::remove_var("FAL_API_KEY") };
            let manifest = fal_manifest(vec![
                PluginPermission::HttpClient,
                PluginPermission::EnvRead,
            ]);
            let mut plugin = create_plugin_with_manifest(&path, &manifest).unwrap();
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
            // Only HttpClient granted — EnvRead missing.
            let manifest = fal_manifest(vec![PluginPermission::HttpClient]);
            let mut plugin = create_plugin_with_manifest(&path, &manifest).unwrap();
            let args = serde_json::to_vec(&serde_json::json!({"prompt": "a sunset"})).unwrap();
            let err = call_execute(&mut plugin, &args).unwrap_err();
            let msg = format!("{err:#}");
            assert!(
                msg.contains("permission") || msg.contains("env_read"),
                "expected permission-denied error, got: {msg}"
            );
        }

        /// Issue #5919: with `env_read` granted but an empty `env_read_vars`
        /// allowlist, the host function denies the read even though the
        /// permission bit is set.
        #[test]
        fn execute_with_empty_env_read_allowlist_denies_env_read() {
            let Some(path) = wasm_path() else { return };
            unsafe { std::env::remove_var("FAL_API_KEY") };
            // Same permissions, but env_read_vars deliberately empty.
            let manifest = manifest_with(
                vec![PluginPermission::HttpClient, PluginPermission::EnvRead],
                false,
                vec!["*.fal.run".to_string()],
                vec![],
            );
            let mut plugin = create_plugin_with_manifest(&path, &manifest).unwrap();
            let args = serde_json::to_vec(&serde_json::json!({"prompt": "a sunset"})).unwrap();
            let err = call_execute(&mut plugin, &args).unwrap_err();
            let msg = format!("{err:#}");
            assert!(
                msg.contains("no env_read_vars"),
                "expected deny-by-default env_read error, got: {msg}"
            );
        }
    }
}
