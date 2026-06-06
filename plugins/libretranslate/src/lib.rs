//! ZeroClaw WASM plugin: translation via a self-hosted LibreTranslate instance.
//!
//! A stateless tool plugin — one request → one response, no stored state. Targets
//! the user's own **self-hosted, open-source** LibreTranslate server (configurable
//! base URL), so translations stay on the user's infrastructure — no third-party
//! translation API. JSON in/out over the standard (text) host HTTP bridge. Needs
//! only the `http_client` and `env_read` permissions.
//!
//! ## Plugin protocol
//!
//! **Exports:**
//! - `tool_metadata(_) -> JSON` — returns `{"name", "description", "parameters_schema"}`
//! - `execute(args_json) -> JSON` — returns `{"success", "output", "error?"}`
//!
//! **Host functions (provided by the ZeroClaw runtime):**
//! - `zc_http_request(json) -> json` — make an HTTP request (`http_client` permission)
//! - `zc_env_read(name) -> value` — read an env var (`env_read` permission)

use extism_pdk::*;
use serde::{Deserialize, Serialize};
use serde_json::json;

/// Base URL of the self-hosted LibreTranslate instance.
const API_URL_ENV: &str = "LIBRETRANSLATE_URL";
const DEFAULT_BASE: &str = "http://localhost:5000";
/// Optional — only needed if the instance requires an API key.
const API_KEY_ENV: &str = "LIBRETRANSLATE_API_KEY";

// ── Types matching the host-side protocol ─────────────────────────

#[derive(Serialize, Deserialize)]
struct ToolMetadata {
    name: String,
    description: String,
    parameters_schema: serde_json::Value,
}

#[derive(Serialize, Deserialize)]
struct ToolResult {
    success: bool,
    output: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

impl ToolResult {
    fn success(output: impl Into<String>) -> Self {
        Self {
            success: true,
            output: output.into(),
            error: None,
        }
    }
    fn failure(error: impl Into<String>) -> Self {
        Self {
            success: false,
            output: String::new(),
            error: Some(error.into()),
        }
    }
}

#[derive(Serialize)]
struct HttpRequest {
    method: String,
    url: String,
    headers: std::collections::HashMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    body: Option<String>,
}

#[derive(Deserialize)]
struct HttpResponse {
    status: u16,
    body: String,
}

// ── Host function declarations ────────────────────────────────────

#[host_fn]
extern "ExtismHost" {
    fn zc_http_request(input: String) -> String;
    fn zc_env_read(input: String) -> String;
}

fn http_request(req: &HttpRequest) -> Result<HttpResponse, Error> {
    let input = serde_json::to_string(req)?;
    let output = unsafe { zc_http_request(input)? };
    Ok(serde_json::from_str(&output)?)
}

fn env_read(var_name: &str) -> Result<String, Error> {
    unsafe { zc_env_read(var_name.to_string()) }
}

// ── Output formatting ─────────────────────────────────────────────

/// Build the model-facing output and the mandatory fidelity footer (last,
/// naming the source and listing exactly the fields present).
fn format_summary(target_lang: &str, detected: Option<&str>, translation: &str) -> String {
    let mut out = format!("Translation → {target_lang}\n");
    let mut keys: Vec<&str> = vec!["target_lang"];
    if let Some(src) = detected {
        out.push_str(&format!("Detected source: {src}\n"));
        keys.push("detected_source_language");
    }
    out.push_str(&format!("\n{translation}"));
    keys.push("translation");

    out.push_str("\n\n---\n");
    out.push_str("Data source: self-hosted LibreTranslate instance (/translate).\n");
    out.push_str(&format!("Fields returned: {}.\n", keys.join(", ")));
    out.push_str("Do not infer or fabricate any field not listed above.");
    out
}

// ── Plugin exports ────────────────────────────────────────────────

/// Export: returns tool metadata (name, description, parameters schema).
#[plugin_fn]
pub fn tool_metadata(_input: String) -> FnResult<String> {
    let meta = ToolMetadata {
        name: "translate".into(),
        description:
            "Translate text into a target language using your own self-hosted, open-source \
             LibreTranslate server. Provide the text and a target language code such as 'en', \
             'es', 'de', 'fr', 'ja'. Set LIBRETRANSLATE_URL to your instance."
                .into(),
        parameters_schema: json!({
            "type": "object",
            "required": ["text", "target_lang"],
            "properties": {
                "text": {
                    "type": "string",
                    "description": "The text to translate."
                },
                "target_lang": {
                    "type": "string",
                    "description": "Target language code, e.g. 'en', 'es', 'de', 'fr', 'ja'."
                },
                "source_lang": {
                    "type": "string",
                    "description": "Source language code, or 'auto' to auto-detect (default 'auto')."
                }
            }
        }),
    };
    Ok(serde_json::to_string(&meta)?)
}

/// Export: execute the LibreTranslate translate tool.
#[plugin_fn]
pub fn execute(input: String) -> FnResult<String> {
    let args: serde_json::Value = serde_json::from_str(&input)?;

    // ── Parse + validate parameters ───────────────────────────────
    let text = match args.get("text").and_then(|v| v.as_str()) {
        Some(t) if !t.trim().is_empty() => t.to_string(),
        _ => return fail("Missing required parameter: 'text'"),
    };
    let target_lang = match args.get("target_lang").and_then(|v| v.as_str()) {
        Some(t) if !t.trim().is_empty() => t.trim().to_lowercase(),
        _ => return fail("Missing required parameter: 'target_lang' (e.g. 'en', 'es', 'de')"),
    };
    let source_lang = args
        .get("source_lang")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "auto".to_string());

    // ── Resolve the self-hosted base URL (defaults to localhost) ──
    let base = match env_read(API_URL_ENV) {
        Ok(u) if !u.trim().is_empty() => u.trim().trim_end_matches('/').to_string(),
        _ => DEFAULT_BASE.to_string(),
    };
    if !(base.starts_with("http://") || base.starts_with("https://")) {
        return fail(format!(
            "Invalid {API_URL_ENV} '{base}': must be an http(s) URL to your self-hosted LibreTranslate"
        ));
    }

    // ── Build the JSON body (api_key only if the instance needs it) ─
    let mut body = serde_json::Map::new();
    body.insert("q".into(), json!(text));
    body.insert("source".into(), json!(source_lang));
    body.insert("target".into(), json!(target_lang));
    body.insert("format".into(), json!("text"));
    if let Ok(key) = env_read(API_KEY_ENV)
        && !key.trim().is_empty()
    {
        body.insert("api_key".into(), json!(key.trim()));
    }

    let req = HttpRequest {
        method: "POST".into(),
        url: format!("{base}/translate"),
        headers: [("Content-Type".to_string(), "application/json".to_string())]
            .into_iter()
            .collect(),
        body: Some(serde_json::to_string(&serde_json::Value::Object(body))?),
    };
    let resp = match http_request(&req) {
        Ok(r) => r,
        Err(e) => {
            return fail(format!(
                "LibreTranslate request failed: {e}. Is your instance running at {base}?"
            ));
        }
    };
    if resp.status >= 400 {
        return fail(format!(
            "LibreTranslate error ({}): {}",
            resp.status,
            &resp.body[..resp.body.len().min(500)]
        ));
    }

    // ── Parse response (translatedText + detectedLanguage?) ───────
    let resp_json: serde_json::Value = serde_json::from_str(&resp.body)
        .map_err(|e| Error::msg(format!("failed to parse LibreTranslate response: {e}")))?;
    let translation = match resp_json.get("translatedText").and_then(|v| v.as_str()) {
        Some(t) => t,
        None => return fail("LibreTranslate response has no translatedText"),
    };
    let detected = resp_json
        .pointer("/detectedLanguage/language")
        .and_then(|v| v.as_str());

    Ok(serde_json::to_string(&ToolResult::success(
        format_summary(&target_lang, detected, translation),
    ))?)
}

fn fail(msg: impl Into<String>) -> FnResult<String> {
    Ok(serde_json::to_string(&ToolResult::failure(msg))?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn footer_present_last_lists_fields() {
        let out = format_summary("es", Some("en"), "Hola");
        let (body, footer) = out.split_once("---").unwrap();
        assert!(footer.contains("Data source: self-hosted LibreTranslate"));
        assert!(out.trim_end().ends_with("not listed above."));
        let line = footer
            .lines()
            .find(|l| l.starts_with("Fields returned:"))
            .unwrap();
        assert!(line.contains("target_lang"));
        assert!(line.contains("detected_source_language"));
        assert!(line.contains("translation"));
        assert!(body.contains("Hola"));
    }

    #[test]
    fn detected_omitted_when_absent() {
        let out = format_summary("es", None, "Hola");
        let footer = &out[out.rfind("---").unwrap()..];
        assert!(!footer.contains("detected_source_language"));
        assert!(footer.contains("translation"));
    }
}
