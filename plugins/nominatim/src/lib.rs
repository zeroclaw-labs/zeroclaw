//! ZeroClaw WASM plugin: geocoding via OpenStreetMap Nominatim.
//!
//! A stateless tool plugin — one request → one response, no stored state.
//! Forward-geocodes a place name to coordinates, or reverse-geocodes coordinates
//! to an address, via Nominatim (the public OSM endpoint by default, or the
//! user's own **self-hosted, open-source** instance via `NOMINATIM_URL`). Keyless,
//! JSON response over the standard (text) host HTTP bridge. Needs only the
//! `http_client` and `env_read` permissions.
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

/// Base URL of the Nominatim server (public OSM by default; self-hostable).
const API_URL_ENV: &str = "NOMINATIM_URL";
const DEFAULT_BASE: &str = "https://nominatim.openstreetmap.org";
/// Nominatim's usage policy requires an identifying User-Agent.
const USER_AGENT: &str = "ZeroClaw-nominatim-plugin/0.1";
const MAX_RESULTS: usize = 10;

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

/// Percent-encode a query-string value (RFC 3986 unreserved set kept as-is).
fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for &b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Truncate `s` to at most `max` bytes without splitting a UTF-8 character.
/// Slicing on a raw byte index (e.g. for an error body) can land inside a
/// multi-byte character and panic; this walks back to the nearest char boundary.
fn truncate_chars(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// A geocoded place: display name + coordinates.
struct Place {
    name: String,
    lat: String,
    lon: String,
}

/// Build the model-facing output and the mandatory fidelity footer (last,
/// naming the source and listing exactly the fields present).
fn format_summary(header: &str, places: &[Place]) -> String {
    let mut out = format!("{header} ({} result(s)):\n", places.len());
    for (i, p) in places.iter().enumerate() {
        out.push_str(&format!(
            "{}. {}\n   lat={}, lon={}\n",
            i + 1,
            p.name,
            p.lat,
            p.lon
        ));
    }
    out.push_str("\n---\n");
    out.push_str("Data source: OpenStreetMap Nominatim geocoding API.\n");
    out.push_str("Fields returned: query, results.\n");
    out.push_str("Do not infer or fabricate any field not listed above.");
    out
}

// ── Plugin exports ────────────────────────────────────────────────

/// Export: returns tool metadata (name, description, parameters schema).
#[plugin_fn]
pub fn tool_metadata(_input: String) -> FnResult<String> {
    let meta = ToolMetadata {
        name: "geocode".into(),
        description:
            "Geocode a place name to coordinates, or reverse-geocode coordinates to an address, \
             using OpenStreetMap Nominatim. Provide 'query' to search, or 'lat' and 'lon' to \
             reverse-geocode. Works with the public OSM endpoint or your own self-hosted instance \
             (set NOMINATIM_URL)."
                .into(),
        parameters_schema: json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "A place name or address to geocode (e.g. 'Eiffel Tower')."
                },
                "lat": {
                    "type": "number",
                    "description": "Latitude for reverse geocoding (use with 'lon')."
                },
                "lon": {
                    "type": "number",
                    "description": "Longitude for reverse geocoding (use with 'lat')."
                }
            }
        }),
    };
    Ok(serde_json::to_string(&meta)?)
}

/// Export: execute the Nominatim geocoding tool.
#[plugin_fn]
pub fn execute(input: String) -> FnResult<String> {
    let args: serde_json::Value = serde_json::from_str(&input)?;

    // ── Resolve the base URL (public OSM by default; self-hostable) ─
    let base = match env_read(API_URL_ENV) {
        Ok(u) if !u.trim().is_empty() => u.trim().trim_end_matches('/').to_string(),
        _ => DEFAULT_BASE.to_string(),
    };
    if !(base.starts_with("http://") || base.starts_with("https://")) {
        return fail(format!(
            "Invalid {API_URL_ENV} '{base}': must be an http(s) URL to a Nominatim server"
        ));
    }

    // ── Decide forward vs reverse from the provided args ──────────
    let query = args
        .get("query")
        .and_then(|v| v.as_str())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty());
    let lat = args.get("lat").and_then(|v| v.as_f64());
    let lon = args.get("lon").and_then(|v| v.as_f64());

    let (url, header) = if let Some(q) = query {
        (
            format!(
                "{base}/search?q={}&format=json&limit={MAX_RESULTS}",
                percent_encode(q)
            ),
            format!("Geocode: {q}"),
        )
    } else if let (Some(la), Some(lo)) = (lat, lon) {
        (
            format!("{base}/reverse?lat={la}&lon={lo}&format=json"),
            format!("Reverse geocode: {la}, {lo}"),
        )
    } else {
        return fail(
            "Provide either 'query' (to geocode) or both 'lat' and 'lon' (to reverse geocode)",
        );
    };

    // ── Call Nominatim (User-Agent required by policy) ────────────
    let req = HttpRequest {
        method: "GET".into(),
        url,
        headers: [("User-Agent".to_string(), USER_AGENT.to_string())]
            .into_iter()
            .collect(),
        body: None,
    };
    let resp = match http_request(&req) {
        Ok(r) => r,
        Err(e) => return fail(format!("Nominatim request failed: {e}")),
    };
    if resp.status >= 400 {
        return fail(format!(
            "Nominatim error ({}): {}",
            resp.status,
            truncate_chars(&resp.body, 500)
        ));
    }

    // ── Parse response (array for search, object for reverse) ─────
    let resp_json: serde_json::Value = serde_json::from_str(&resp.body)
        .map_err(|e| Error::msg(format!("failed to parse Nominatim response: {e}")))?;
    let to_place = |v: &serde_json::Value| -> Option<Place> {
        let lat = v.get("lat").and_then(|x| x.as_str())?.to_string();
        let lon = v.get("lon").and_then(|x| x.as_str())?.to_string();
        let name = v
            .get("display_name")
            .and_then(|x| x.as_str())
            .unwrap_or("(unnamed)")
            .to_string();
        Some(Place { name, lat, lon })
    };
    let places: Vec<Place> = match &resp_json {
        serde_json::Value::Array(arr) => arr.iter().filter_map(to_place).collect(),
        obj @ serde_json::Value::Object(_) => to_place(obj).into_iter().collect(),
        _ => Vec::new(),
    };

    if places.is_empty() {
        return fail("Nominatim returned no matching places");
    }

    Ok(serde_json::to_string(&ToolResult::success(
        format_summary(&header, &places),
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
        let places = vec![Place {
            name: "Eiffel Tower, Paris".into(),
            lat: "48.85".into(),
            lon: "2.29".into(),
        }];
        let out = format_summary("Geocode: Eiffel Tower", &places);
        let (body, footer) = out.split_once("---").unwrap();
        assert!(footer.contains("Data source: OpenStreetMap Nominatim"));
        assert!(footer.contains("Fields returned: query, results."));
        assert!(out.trim_end().ends_with("not listed above."));
        assert!(body.contains("Eiffel Tower, Paris"));
        assert!(body.contains("lat=48.85, lon=2.29"));
    }

    #[test]
    fn percent_encode_basics() {
        assert_eq!(percent_encode("New York"), "New%20York");
        assert_eq!(percent_encode("Zz09-_.~"), "Zz09-_.~");
    }

    #[test]
    fn truncate_chars_never_splits_multibyte() {
        // A long non-ASCII error body whose 500-byte cutoff falls mid-character
        // must not panic and must stay on a char boundary.
        let body = "é".repeat(400); // 800 bytes; boundary at 500 is mid-char
        let cut = truncate_chars(&body, 500);
        assert!(cut.len() <= 500);
        assert!(body.is_char_boundary(cut.len()));
        let msg = format!("Nominatim error ({}): {}", 500, truncate_chars(&body, 500));
        assert!(msg.starts_with("Nominatim error (500):"));
    }

    #[test]
    fn truncate_chars_short_input_unchanged() {
        assert_eq!(truncate_chars("hello", 500), "hello");
        assert_eq!(truncate_chars("", 500), "");
    }
}
