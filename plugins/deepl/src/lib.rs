//! ZeroClaw WASM plugin: high-quality text translation via the DeepL API.
//!
//! A stateless tool plugin — one request → one response, no stored state. The
//! DeepL v2 translate endpoint accepts JSON and returns JSON, so it works over
//! the standard (text) host HTTP bridge. The endpoint host is selected from the
//! key suffix (`:fx` = free tier). Needs only the `http_client` and `env_read`
//! permissions.
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

const API_KEY_ENV: &str = "DEEPL_API_KEY";
const FREE_URL: &str = "https://api-free.deepl.com/v2/translate";
const PRO_URL: &str = "https://api.deepl.com/v2/translate";

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

// ── Helpers ───────────────────────────────────────────────────────

/// DeepL free-tier keys end with `:fx` and use a different host than Pro keys.
fn endpoint_for(api_key: &str) -> &'static str {
    if api_key.ends_with(":fx") {
        FREE_URL
    } else {
        PRO_URL
    }
}

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
    out.push_str("Data source: DeepL translate API (https://api.deepl.com/v2/translate).\n");
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
            "Translate text into a target language using DeepL (high translation quality). \
             Provide the text and a target language code such as 'EN', 'ES', 'DE', 'FR', 'JA'."
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
                    "description": "Target language code, e.g. 'EN', 'EN-GB', 'ES', 'DE', 'FR', 'JA'."
                },
                "source_lang": {
                    "type": "string",
                    "description": "Optional source language code; omit to auto-detect."
                }
            }
        }),
    };
    Ok(serde_json::to_string(&meta)?)
}

/// Export: execute the DeepL translate tool.
#[plugin_fn]
pub fn execute(input: String) -> FnResult<String> {
    let args: serde_json::Value = serde_json::from_str(&input)?;

    // ── Parse + validate parameters ───────────────────────────────
    let text = match args.get("text").and_then(|v| v.as_str()) {
        Some(t) if !t.trim().is_empty() => t.to_string(),
        _ => return fail("Missing required parameter: 'text'"),
    };
    let target_lang = match args.get("target_lang").and_then(|v| v.as_str()) {
        Some(t) if !t.trim().is_empty() => t.trim().to_uppercase(),
        _ => return fail("Missing required parameter: 'target_lang' (e.g. 'EN', 'ES', 'DE')"),
    };
    let source_lang = args
        .get("source_lang")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_uppercase())
        .filter(|s| !s.is_empty());

    // ── Read API key via host function ────────────────────────────
    let api_key = match env_read(API_KEY_ENV) {
        Ok(k) if !k.trim().is_empty() => k.trim().to_string(),
        Ok(_) => return fail(format!("API key {API_KEY_ENV} is empty")),
        Err(e) => {
            return fail(format!(
                "Missing API key: set the {API_KEY_ENV} environment variable ({e})"
            ));
        }
    };

    // ── Build the JSON body ───────────────────────────────────────
    let mut body = serde_json::Map::new();
    body.insert("text".into(), json!([text]));
    body.insert("target_lang".into(), json!(target_lang));
    if let Some(src) = &source_lang {
        body.insert("source_lang".into(), json!(src));
    }

    let req = HttpRequest {
        method: "POST".into(),
        url: endpoint_for(&api_key).into(),
        headers: [
            ("Authorization".into(), format!("DeepL-Auth-Key {api_key}")),
            ("Content-Type".into(), "application/json".into()),
        ]
        .into_iter()
        .collect(),
        body: Some(serde_json::to_string(&serde_json::Value::Object(body))?),
    };
    let resp = match http_request(&req) {
        Ok(r) => r,
        Err(e) => return fail(format!("DeepL request failed: {e}")),
    };
    if resp.status >= 400 {
        return fail(format!(
            "DeepL API error ({}): {}",
            resp.status,
            &resp.body[..resp.body.len().min(500)]
        ));
    }

    // ── Parse response (translations[0]) ──────────────────────────
    let resp_json: serde_json::Value = serde_json::from_str(&resp.body)
        .map_err(|e| Error::msg(format!("failed to parse DeepL response: {e}")))?;
    let item = match resp_json.pointer("/translations/0") {
        Some(i) => i,
        None => return fail("DeepL returned no translations"),
    };
    let translation = match item.get("text").and_then(|v| v.as_str()) {
        Some(t) => t,
        None => return fail("DeepL translation has no text"),
    };
    let detected = item
        .get("detected_source_language")
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
    fn endpoint_selection_by_key_suffix() {
        assert_eq!(endpoint_for("abcd-1234:fx"), FREE_URL);
        assert_eq!(endpoint_for("abcd-1234"), PRO_URL);
    }

    #[test]
    fn footer_present_last_lists_fields() {
        let out = format_summary("ES", Some("EN"), "Hola");
        let (body, footer) = out.split_once("---").unwrap();
        assert!(footer.contains("Data source: DeepL translate API"));
        assert!(out.trim_end().ends_with("not listed above."));
        let line = footer
            .lines()
            .find(|l| l.starts_with("Fields returned:"))
            .unwrap();
        assert!(line.contains("target_lang"));
        assert!(line.contains("detected_source_language"));
        assert!(line.contains("translation"));
        assert!(body.contains("Translation → ES"));
        assert!(body.contains("Detected source: EN"));
        assert!(body.contains("Hola"));
    }

    #[test]
    fn detected_omitted_when_absent() {
        let out = format_summary("ES", None, "Hola");
        let footer = &out[out.rfind("---").unwrap()..];
        assert!(!footer.contains("detected_source_language"));
        assert!(footer.contains("target_lang"));
        assert!(footer.contains("translation"));
    }
}
