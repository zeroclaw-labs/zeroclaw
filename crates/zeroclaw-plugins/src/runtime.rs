//! Extism-based WASM execution bridge.
//!
//! Creates Extism plugin instances with the permission-gated `zc_http_request`
//! host function and calls plugin-exported functions (`tool_metadata`,
//! `execute`). A plugin's resolved config section is injected into the
//! `execute` input rather than read back through a host call.

use crate::PluginPermission;
use anyhow::{Context, Result};
use extism::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::collections::HashSet;
use std::path::Path;
use zeroclaw_api::tool::ToolResult;

// ── Host function context ─────────────────────────────────────────

/// Permissions available to a single plugin invocation.
#[derive(Debug, Clone)]
struct HostContext {
    permissions: HashSet<PluginPermission>,
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

// ── Plugin creation and invocation ────────────────────────────────

/// Create an Extism plugin from a WASM file with the given permissions.
pub fn create_plugin(wasm_path: &Path, permissions: &[PluginPermission]) -> Result<extism::Plugin> {
    let perm_set: HashSet<PluginPermission> = permissions.iter().cloned().collect();
    let ctx = UserData::new(HostContext {
        permissions: perm_set,
    });

    let http_fn = Function::new(
        "zc_http_request",
        [PTR],
        [PTR],
        ctx.clone(),
        handle_http_request,
    );

    let manifest = Manifest::new([Wasm::file(wasm_path)]);

    Plugin::new(manifest, [http_fn], true)
        .with_context(|| format!("failed to load WASM plugin from {}", wasm_path.display()))
}

/// Call the `tool_metadata` export and parse the result.
pub fn call_tool_metadata(plugin: &mut extism::Plugin) -> Result<ToolMetadata> {
    let output = plugin
        .call::<&str, String>("tool_metadata", "")
        .context("failed to call tool_metadata export")?;

    serde_json::from_str(&output).context("failed to parse tool_metadata JSON")
}

/// Merge the plugin's resolved config section into its `execute` input under the
/// reserved `__config` key, stripping any caller-supplied `__config` first so the
/// section cannot be spoofed through tool args. Kept pure so the injection
/// contract is unit-testable without a live plugin.
fn inject_config(args_json: &[u8], config: &HashMap<String, String>) -> Result<String> {
    let mut args: serde_json::Value =
        serde_json::from_slice(args_json).context("plugin args are not valid JSON")?;

    let obj = args
        .as_object_mut()
        .context("plugin args must be a JSON object")?;
    obj.remove("__config");
    if !config.is_empty() {
        obj.insert(
            "__config".to_string(),
            serde_json::to_value(config).context("failed to serialize plugin config")?,
        );
    }

    serde_json::to_string(&args).context("failed to serialize plugin input")
}

/// Call the `execute` export with the given args JSON plus the plugin's resolved
/// config section, returning a `ToolResult`. The config is injected into the
/// input under the reserved `__config` key so the plugin reads it from its own
/// input rather than calling back into the host.
/// Resolve the config map a plugin actually receives: the configured section
/// only when the manifest grants `ConfigRead`, otherwise empty. Gating here
/// (not at injection) keeps `inject_config`'s caller-`__config` stripping intact
/// for permissionless plugins while honoring the manifest permission contract.
fn effective_config<'a>(
    config: &'a HashMap<String, String>,
    permissions: &[PluginPermission],
) -> &'a HashMap<String, String> {
    if permissions.contains(&PluginPermission::ConfigRead) {
        config
    } else {
        EMPTY_CONFIG.get_or_init(HashMap::new)
    }
}

static EMPTY_CONFIG: std::sync::OnceLock<HashMap<String, String>> = std::sync::OnceLock::new();

pub fn call_execute(
    plugin: &mut extism::Plugin,
    args_json: &[u8],
    config: &HashMap<String, String>,
    permissions: &[PluginPermission],
) -> Result<ToolResult> {
    let input = inject_config(args_json, effective_config(config, permissions))?;

    let output = plugin
        .call::<&str, String>("execute", &input)
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
    fn host_context_permission_check() {
        let ctx = HostContext {
            permissions: HashSet::from([PluginPermission::HttpClient]),
        };
        assert!(ctx.permissions.contains(&PluginPermission::HttpClient));
        assert!(!ctx.permissions.contains(&PluginPermission::ConfigRead));
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

    #[test]
    fn inject_config_adds_config_key() {
        let args = br#"{"prompt":"a sunset"}"#;
        let config = HashMap::from([("api_key".to_string(), "secret".to_string())]);
        let out = inject_config(args, &config).unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["prompt"], "a sunset");
        assert_eq!(v["__config"]["api_key"], "secret");
    }

    #[test]
    fn inject_config_empty_leaves_args_untouched() {
        let args = br#"{"prompt":"x"}"#;
        let out = inject_config(args, &HashMap::new()).unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert!(v.get("__config").is_none());
    }

    #[test]
    fn inject_config_rejects_non_object_args() {
        let args = br#"[1,2,3]"#;
        let config = HashMap::from([("k".to_string(), "v".to_string())]);
        assert!(inject_config(args, &config).is_err());
    }

    #[test]
    fn inject_config_strips_caller_supplied_config_when_section_empty() {
        let args = br#"{"prompt":"x","__config":{"api_key":"forged"}}"#;
        let out = inject_config(args, &HashMap::new()).unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert!(v.get("__config").is_none());
        assert_eq!(v["prompt"], "x");
    }

    #[test]
    fn inject_config_overrides_caller_supplied_config_when_section_present() {
        let args = br#"{"prompt":"x","__config":{"api_key":"forged"}}"#;
        let config = HashMap::from([("api_key".to_string(), "real".to_string())]);
        let out = inject_config(args, &config).unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["__config"]["api_key"], "real");
    }

    #[test]
    fn effective_config_withholds_section_without_config_read_permission() {
        let config = HashMap::from([("api_key".to_string(), "secret".to_string())]);
        let resolved = effective_config(&config, &[PluginPermission::HttpClient]);
        assert!(
            resolved.is_empty(),
            "a plugin without ConfigRead must not receive its configured section"
        );
        // And the resulting injected args carry no __config, even with a caller forging it.
        let args = br#"{"prompt":"x","__config":{"api_key":"forged"}}"#;
        let out = inject_config(args, resolved).unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert!(
            v.get("__config").is_none(),
            "no __config injected for a permissionless plugin; caller-supplied value is stripped"
        );
    }

    #[test]
    fn effective_config_passes_section_with_config_read_permission() {
        let config = HashMap::from([("api_key".to_string(), "secret".to_string())]);
        let resolved = effective_config(&config, &[PluginPermission::ConfigRead]);
        assert_eq!(
            resolved.get("api_key").map(String::as_str),
            Some("secret"),
            "a plugin with ConfigRead must receive its configured section"
        );
    }
}
