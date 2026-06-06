//! Extism-based WASM execution bridge.
//!
//! Creates Extism plugin instances with permission-gated host functions
//! (`zc_http_request`, `zc_env_read`) and calls plugin-exported functions
//! (`tool_metadata`, `execute`).

use crate::PluginPermission;
use anyhow::{Context, Result};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use extism::*;
use serde::{Deserialize, Serialize};
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
    /// Textual request body (JSON, form, etc.).
    #[serde(default)]
    body: Option<String>,
    /// Standard-base64-encoded **binary** request body. When present it takes
    /// precedence over `body` and is decoded to raw bytes before sending — this
    /// lets a plugin upload binary payloads (e.g. audio for speech-to-text)
    /// across the text-only host↔guest boundary.
    #[serde(default)]
    body_base64: Option<String>,
}

/// HTTP response returned from host to plugin.
#[derive(Debug, Serialize, Deserialize)]
struct HttpResponse {
    status: u16,
    /// Response body as text, for textual content types (`text/*`, JSON, XML…).
    /// Empty when the response is binary — see `body_base64`.
    body: String,
    /// Standard-base64-encoded response body, populated **only** when the
    /// response is binary (a non-textual content type). Omitted entirely for
    /// textual responses, so existing text plugins are unaffected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    body_base64: Option<String>,
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

// ── HTTP body codec helpers ───────────────────────────────────────

/// Whether a `Content-Type` denotes a textual body that is safe to return as a
/// UTF-8 string. The parameter set (`charset`, etc.) after `;` is ignored.
fn is_textual_content_type(content_type: &str) -> bool {
    let ct = content_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    ct.starts_with("text/")
        || ct == "application/json"
        || ct == "application/xml"
        || ct == "application/javascript"
        || ct == "application/x-www-form-urlencoded"
        || ct.ends_with("+json")
        || ct.ends_with("+xml")
}

/// Split raw response bytes into `(body, body_base64)` based on the content
/// type. Textual types are returned as a string (the pre-existing behavior);
/// everything else is base64-encoded so binary survives the text-only
/// host→guest boundary. An empty/unknown content type that is valid UTF-8 is
/// treated as text to preserve prior behavior.
fn encode_response_body(content_type: &str, bytes: &[u8]) -> (String, Option<String>) {
    let textual = if content_type.trim().is_empty() {
        std::str::from_utf8(bytes).is_ok()
    } else {
        is_textual_content_type(content_type)
    };
    if textual {
        (String::from_utf8_lossy(bytes).into_owned(), None)
    } else {
        (String::new(), Some(STANDARD.encode(bytes)))
    }
}

/// Resolve the outgoing request body bytes: a `body_base64` (decoded) takes
/// precedence over the textual `body`. Returns `None` when neither is set.
fn resolve_request_body(
    body: &Option<String>,
    body_base64: &Option<String>,
) -> Result<Option<Vec<u8>>, Error> {
    if let Some(b64) = body_base64 {
        let bytes = STANDARD
            .decode(b64.as_bytes())
            .map_err(|e| Error::msg(format!("invalid body_base64 in request: {e}")))?;
        Ok(Some(bytes))
    } else if let Some(text) = body {
        Ok(Some(text.clone().into_bytes()))
    } else {
        Ok(None)
    }
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

    if let Some(bytes) = resolve_request_body(&req.body, &req.body_base64)? {
        builder = builder.body(bytes);
    }

    let resp = builder
        .send()
        .map_err(|e| Error::msg(format!("HTTP request failed: {e}")))?;

    let status = resp.status().as_u16();
    // Capture headers + content type before `bytes()` consumes the response.
    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let headers: std::collections::HashMap<String, String> = resp
        .headers()
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
        .collect();
    let bytes = resp
        .bytes()
        .map_err(|e| Error::msg(format!("failed to read response body: {e}")))?;
    let (body, body_base64) = encode_response_body(&content_type, &bytes);

    let response = HttpResponse {
        status,
        body,
        body_base64,
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

    let value = std::env::var(&var_name)
        .map_err(|_| Error::msg(format!("environment variable '{var_name}' not set")))?;

    plugin.memory_set_val(&mut outputs[0], value)?;

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

    let env_fn = Function::new("zc_env_read", [PTR], [PTR], ctx, handle_env_read);

    let manifest = Manifest::new([Wasm::file(wasm_path)]);

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
    fn host_context_permission_check() {
        let ctx = HostContext {
            permissions: HashSet::from([PluginPermission::HttpClient]),
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
            body_base64: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        let parsed: HttpRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.method, "POST");
        assert_eq!(parsed.url, "https://example.com/api");
        assert_eq!(parsed.body.as_deref(), Some(r#"{"key":"value"}"#));
        assert_eq!(parsed.body_base64, None);
    }

    #[test]
    fn http_request_back_compat_without_base64_field() {
        // A guest built before this change omits `body_base64` entirely.
        let json = r#"{"method":"GET","url":"https://e.test","body":null}"#;
        let parsed: HttpRequest = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.body_base64, None);
        assert!(parsed.headers.is_empty());
    }

    #[test]
    fn text_response_omits_base64_field() {
        // Textual responses must serialize WITHOUT a `body_base64` key so old
        // plugins see the exact JSON shape they did before.
        let resp = HttpResponse {
            status: 200,
            body: "hello".into(),
            body_base64: None,
            headers: std::collections::HashMap::new(),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(!json.contains("body_base64"), "got: {json}");
    }

    #[test]
    fn resolve_request_body_prefers_base64() {
        // "hi" == base64 "aGk=".
        let bytes = resolve_request_body(&Some("text".into()), &Some("aGk=".into()))
            .unwrap()
            .unwrap();
        assert_eq!(bytes, b"hi");
    }

    #[test]
    fn resolve_request_body_text_then_none() {
        assert_eq!(
            resolve_request_body(&Some("abc".into()), &None).unwrap(),
            Some(b"abc".to_vec())
        );
        assert_eq!(resolve_request_body(&None, &None).unwrap(), None);
    }

    #[test]
    fn resolve_request_body_rejects_bad_base64() {
        assert!(resolve_request_body(&None, &Some("not!base64!".into())).is_err());
    }

    #[test]
    fn encode_response_text_vs_binary() {
        // JSON content type → text body, no base64.
        let (body, b64) = encode_response_body("application/json; charset=utf-8", b"{\"ok\":1}");
        assert_eq!(body, "{\"ok\":1}");
        assert!(b64.is_none());

        // Binary content type → empty body, base64 set, round-trips.
        let raw = [0u8, 159, 146, 150];
        let (body, b64) = encode_response_body("image/png", &raw);
        assert!(body.is_empty());
        let decoded = STANDARD.decode(b64.unwrap()).unwrap();
        assert_eq!(decoded, raw);
    }

    #[test]
    fn encode_response_empty_content_type_falls_back_to_utf8_check() {
        // No content type + valid UTF-8 → text.
        let (body, b64) = encode_response_body("", b"plain");
        assert_eq!(body, "plain");
        assert!(b64.is_none());
        // No content type + invalid UTF-8 → base64.
        let (body, b64) = encode_response_body("", &[0xff, 0xfe]);
        assert!(body.is_empty());
        assert!(b64.is_some());
    }

    #[test]
    fn textual_content_type_classification() {
        assert!(is_textual_content_type("text/html"));
        assert!(is_textual_content_type("application/json"));
        assert!(is_textual_content_type("application/ld+json"));
        assert!(is_textual_content_type("image/svg+xml"));
        assert!(!is_textual_content_type("image/png"));
        assert!(!is_textual_content_type("audio/mpeg"));
        assert!(!is_textual_content_type("application/octet-stream"));
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
