//! ZeroClaw WASM plugin: run AI models via the Replicate predictions API.
//!
//! A stateless tool plugin — one `execute` call submits a prediction and returns
//! its result. It uses Replicate's synchronous `Prefer: wait` mode and a bounded
//! poll fallback so a single call resolves within the host's HTTP budget without
//! persisting any state between calls. Needs only the `http_client` and
//! `env_read` permissions.
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

const PREDICTIONS_URL: &str = "https://api.replicate.com/v1/predictions";
const API_KEY_ENV: &str = "REPLICATE_API_TOKEN";
/// Replicate's `Prefer: wait` server-side hold, in seconds. Kept under the
/// host's 120s HTTP timeout.
const WAIT_SECONDS: u32 = 60;
/// Bounded GET fallback if the prediction is still running after the wait window.
/// There is no sleep host-fn, so spacing comes from each request's round-trip;
/// this is a hard cap, not a busy-spin forever.
const MAX_POLLS: usize = 10;

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

fn auth_headers(api_key: &str) -> std::collections::HashMap<String, String> {
    [
        ("Authorization".to_string(), format!("Bearer {api_key}")),
        ("Content-Type".to_string(), "application/json".to_string()),
    ]
    .into_iter()
    .collect()
}

// ── Output formatting ─────────────────────────────────────────────

/// A field actually present in the Replicate prediction. The fidelity footer is
/// derived from the same set, so it can never list a field the body omits.
struct Field {
    key: &'static str,
    label: &'static str,
    value: String,
}

/// Render Replicate's polymorphic `output` (string, array of strings/URLs, or
/// object) into a single display string.
fn render_output(output: &serde_json::Value) -> Option<String> {
    match output {
        serde_json::Value::Null => None,
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Array(items) => {
            let parts: Vec<String> = items
                .iter()
                .map(|v| match v {
                    serde_json::Value::String(s) => s.clone(),
                    other => other.to_string(),
                })
                .collect();
            if parts.is_empty() {
                None
            } else {
                Some(parts.join("\n"))
            }
        }
        other => Some(other.to_string()),
    }
}

/// Build the model-facing output with the mandatory fidelity footer naming the
/// data source and listing exactly the fields present (footer last).
fn format_summary(version: &str, fields: &[Field]) -> String {
    let mut out = format!("Ran Replicate model version {version}\n\n");
    for f in fields {
        out.push_str(&format!("{}: {}\n", f.label, f.value));
    }

    let mut keys: Vec<&str> = vec!["model_version"];
    keys.extend(fields.iter().map(|f| f.key));
    out.push_str("\n---\n");
    out.push_str(
        "Data source: Replicate predictions API (https://api.replicate.com/v1/predictions).\n",
    );
    out.push_str(&format!("Fields returned: {}.\n", keys.join(", ")));
    out.push_str("Do not infer or fabricate any field not listed above.");
    out
}

fn is_terminal(status: &str) -> bool {
    matches!(status, "succeeded" | "successful" | "failed" | "canceled")
}

// ── Plugin exports ────────────────────────────────────────────────

/// Export: returns tool metadata (name, description, parameters schema).
#[plugin_fn]
pub fn tool_metadata(_input: String) -> FnResult<String> {
    let meta = ToolMetadata {
        name: "replicate_run".into(),
        description:
            "Run an AI model on Replicate by version id with a JSON input object, and return the \
             model's output. Works for image, video, audio, and text models. Requires the exact \
             model 'version' hash and the model-specific 'input' fields."
                .into(),
        parameters_schema: json!({
            "type": "object",
            "required": ["version", "input"],
            "properties": {
                "version": {
                    "type": "string",
                    "description": "The Replicate model version id (the long hash shown on the model's API page)."
                },
                "input": {
                    "type": "object",
                    "description": "The model-specific input parameters object (e.g. {\"prompt\": \"a cat\"})."
                }
            }
        }),
    };
    Ok(serde_json::to_string(&meta)?)
}

/// Export: create a prediction and return its result.
#[plugin_fn]
pub fn execute(input: String) -> FnResult<String> {
    let args: serde_json::Value = serde_json::from_str(&input)?;

    // ── Parse + validate parameters ───────────────────────────────
    let version = match args.get("version").and_then(|v| v.as_str()) {
        Some(v) if !v.trim().is_empty() => v.trim().to_string(),
        _ => return fail("Missing required parameter: 'version' (the model version id)"),
    };
    let model_input = match args.get("input") {
        Some(v) if v.is_object() => v.clone(),
        _ => return fail("Missing required parameter: 'input' (a JSON object of model inputs)"),
    };

    // ── Read API token via host function ──────────────────────────
    let api_key = match env_read(API_KEY_ENV) {
        Ok(k) if !k.trim().is_empty() => k.trim().to_string(),
        Ok(_) => return fail(format!("API token {API_KEY_ENV} is empty")),
        Err(e) => {
            return fail(format!(
                "Missing API token: set the {API_KEY_ENV} environment variable ({e})"
            ));
        }
    };

    // ── Create the prediction (synchronous wait) ──────────────────
    let body = json!({ "version": version, "input": model_input });
    let mut headers = auth_headers(&api_key);
    headers.insert("Prefer".into(), format!("wait={WAIT_SECONDS}"));
    let req = HttpRequest {
        method: "POST".into(),
        url: PREDICTIONS_URL.into(),
        headers,
        body: Some(serde_json::to_string(&body)?),
    };
    let resp = match http_request(&req) {
        Ok(r) => r,
        Err(e) => return fail(format!("Replicate request failed: {e}")),
    };
    if resp.status >= 400 {
        return fail(format!(
            "Replicate API error ({}): {}",
            resp.status,
            &resp.body[..resp.body.len().min(500)]
        ));
    }
    let mut prediction: serde_json::Value = serde_json::from_str(&resp.body)
        .map_err(|e| Error::msg(format!("failed to parse Replicate response: {e}")))?;

    // ── Poll fallback if still running ────────────────────────────
    let get_url = prediction
        .pointer("/urls/get")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let mut polls = 0;
    while !is_terminal(status_of(&prediction)) && polls < MAX_POLLS {
        let Some(ref url) = get_url else { break };
        let poll = HttpRequest {
            method: "GET".into(),
            url: url.clone(),
            headers: auth_headers(&api_key),
            body: None,
        };
        match http_request(&poll) {
            Ok(r) if r.status < 400 => {
                if let Ok(p) = serde_json::from_str::<serde_json::Value>(&r.body) {
                    prediction = p;
                }
            }
            _ => break,
        }
        polls += 1;
    }

    // ── Format result ─────────────────────────────────────────────
    let status = status_of(&prediction).to_string();
    let id = prediction.get("id").and_then(|v| v.as_str()).unwrap_or("");

    if status == "failed" || status == "canceled" {
        let err = prediction
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("no error detail");
        return fail(format!("Replicate prediction {status} (id {id}): {err}"));
    }

    let mut fields: Vec<Field> = vec![Field {
        key: "status",
        label: "Status",
        value: status.clone(),
    }];
    if !id.is_empty() {
        fields.push(Field {
            key: "prediction_id",
            label: "Prediction ID",
            value: id.to_string(),
        });
    }

    if is_terminal(&status) {
        if let Some(out) = prediction.get("output").and_then(render_output) {
            fields.push(Field {
                key: "output",
                label: "Output",
                value: out,
            });
        }
        Ok(serde_json::to_string(&ToolResult::success(
            format_summary(&version, &fields),
        ))?)
    } else {
        // Still running after the wait + poll budget — report honestly rather
        // than block or fabricate an output.
        fields.push(Field {
            key: "note",
            label: "Note",
            value: format!(
                "Prediction is still running after {WAIT_SECONDS}s + {polls} polls. \
                 Check status later via the Replicate dashboard or GET /v1/predictions/{id}."
            ),
        });
        Ok(serde_json::to_string(&ToolResult::success(
            format_summary(&version, &fields),
        ))?)
    }
}

fn status_of(prediction: &serde_json::Value) -> &str {
    prediction
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
}

fn fail(msg: impl Into<String>) -> FnResult<String> {
    Ok(serde_json::to_string(&ToolResult::failure(msg))?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn footer_present_last_and_lists_fields() {
        let fields = vec![
            Field {
                key: "status",
                label: "Status",
                value: "succeeded".into(),
            },
            Field {
                key: "prediction_id",
                label: "Prediction ID",
                value: "abc123".into(),
            },
            Field {
                key: "output",
                label: "Output",
                value: "https://r.example/out.png".into(),
            },
        ];
        let out = format_summary("v1hash", &fields);
        let (body, footer) = out.split_once("---").unwrap();
        assert!(footer.contains("Data source: Replicate predictions API"));
        assert!(out.trim_end().ends_with("not listed above."));
        // model_version is always implied + every field appears in the body.
        assert!(body.contains("Ran Replicate model version v1hash"));
        let line = footer
            .lines()
            .find(|l| l.starts_with("Fields returned:"))
            .unwrap();
        let list = line
            .trim_start_matches("Fields returned:")
            .trim()
            .trim_end_matches('.');
        for field in list.split(", ") {
            let present = match field {
                "model_version" => body.contains("v1hash"),
                "status" => body.contains("Status:"),
                "prediction_id" => body.contains("Prediction ID:"),
                "output" => body.contains("Output:"),
                "note" => body.contains("Note:"),
                other => panic!("unexpected footer field: {other}"),
            };
            assert!(present, "footer field '{field}' missing from body");
        }
    }

    #[test]
    fn running_prediction_does_not_claim_output() {
        let fields = vec![
            Field {
                key: "status",
                label: "Status",
                value: "processing".into(),
            },
            Field {
                key: "prediction_id",
                label: "Prediction ID",
                value: "id9".into(),
            },
            Field {
                key: "note",
                label: "Note",
                value: "still running".into(),
            },
        ];
        let out = format_summary("v2", &fields);
        let footer = &out[out.rfind("---").unwrap()..];
        assert!(!footer.contains("output"));
    }

    #[test]
    fn render_output_variants() {
        assert_eq!(render_output(&json!("hi")).as_deref(), Some("hi"));
        assert_eq!(render_output(&json!(["a", "b"])).as_deref(), Some("a\nb"));
        assert_eq!(render_output(&serde_json::Value::Null), None);
    }

    #[test]
    fn terminal_status_detection() {
        assert!(is_terminal("succeeded"));
        assert!(is_terminal("failed"));
        assert!(!is_terminal("processing"));
        assert!(!is_terminal("starting"));
    }
}
