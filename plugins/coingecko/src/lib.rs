//! ZeroClaw WASM plugin: live crypto prices via the (keyless) CoinGecko API.
//!
//! A stateless tool plugin — one request → one response, no stored state. The
//! CoinGecko `simple/price` endpoint is **keyless**; an optional
//! `COINGECKO_API_KEY` (demo key) raises rate limits. JSON response over the
//! standard (text) host HTTP bridge. Needs only the `http_client` and `env_read`
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

const API_BASE: &str = "https://api.coingecko.com/api/v3/simple/price";
/// Optional — a demo key raises rate limits. The plugin works keyless without it.
const API_KEY_ENV: &str = "COINGECKO_API_KEY";

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

/// Build the model-facing output and the mandatory fidelity footer (last,
/// naming the source and listing exactly the fields present).
fn format_summary(vs_currency: &str, prices: &[(String, String)]) -> String {
    let mut out = format!("Crypto prices (vs {}):\n", vs_currency.to_uppercase());
    for (coin, price) in prices {
        out.push_str(&format!("  {coin}: {price}\n"));
    }
    out.push_str("\n---\n");
    out.push_str("Data source: CoinGecko simple/price API (https://api.coingecko.com/api/v3/simple/price).\n");
    out.push_str("Fields returned: vs_currency, prices.\n");
    out.push_str("Do not infer or fabricate any field not listed above.");
    out
}

// ── Plugin exports ────────────────────────────────────────────────

/// Export: returns tool metadata (name, description, parameters schema).
#[plugin_fn]
pub fn tool_metadata(_input: String) -> FnResult<String> {
    let meta = ToolMetadata {
        name: "crypto_price".into(),
        description:
            "Look up the current price of one or more cryptocurrencies via CoinGecko. Provide \
             CoinGecko coin ids (e.g. 'bitcoin', 'ethereum', 'solana') and an optional fiat \
             currency. Returns the price and 24h change."
                .into(),
        parameters_schema: json!({
            "type": "object",
            "required": ["ids"],
            "properties": {
                "ids": {
                    "type": "string",
                    "description": "Comma-separated CoinGecko coin ids, e.g. 'bitcoin,ethereum'."
                },
                "vs_currency": {
                    "type": "string",
                    "description": "Fiat/quote currency code (default 'usd'), e.g. 'usd', 'eur'."
                }
            }
        }),
    };
    Ok(serde_json::to_string(&meta)?)
}

/// Export: execute the CoinGecko price lookup tool.
#[plugin_fn]
pub fn execute(input: String) -> FnResult<String> {
    let args: serde_json::Value = serde_json::from_str(&input)?;

    // ── Parse + validate parameters ───────────────────────────────
    let ids = match args.get("ids").and_then(|v| v.as_str()) {
        Some(i) if !i.trim().is_empty() => i.trim().to_lowercase(),
        _ => return fail("Missing required parameter: 'ids' (e.g. 'bitcoin,ethereum')"),
    };
    let vs_currency = args
        .get("vs_currency")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "usd".to_string());

    // ── Build headers (key is optional — works keyless) ───────────
    let mut headers: std::collections::HashMap<String, String> =
        [("Accept".to_string(), "application/json".to_string())]
            .into_iter()
            .collect();
    if let Ok(key) = env_read(API_KEY_ENV)
        && !key.trim().is_empty()
    {
        headers.insert("x-cg-demo-api-key".into(), key.trim().to_string());
    }

    // ── Call CoinGecko via host HTTP function ─────────────────────
    let url = format!(
        "{API_BASE}?ids={}&vs_currencies={}&include_24hr_change=true",
        percent_encode(&ids),
        percent_encode(&vs_currency)
    );
    let req = HttpRequest {
        method: "GET".into(),
        url,
        headers,
        body: None,
    };
    let resp = match http_request(&req) {
        Ok(r) => r,
        Err(e) => return fail(format!("CoinGecko request failed: {e}")),
    };
    if resp.status >= 400 {
        return fail(format!(
            "CoinGecko API error ({}): {}",
            resp.status,
            &resp.body[..resp.body.len().min(500)]
        ));
    }

    // ── Parse response ({coin: {vs: price, vs_24h_change: ..}}) ───
    let resp_json: serde_json::Value = serde_json::from_str(&resp.body)
        .map_err(|e| Error::msg(format!("failed to parse CoinGecko response: {e}")))?;
    let obj = match resp_json.as_object() {
        Some(o) if !o.is_empty() => o,
        _ => {
            return fail(format!(
                "CoinGecko returned no prices for '{ids}' (check the coin ids)"
            ));
        }
    };

    let change_key = format!("{vs_currency}_24h_change");
    let prices: Vec<(String, String)> = obj
        .iter()
        .map(|(coin, data)| {
            let price = data
                .get(&vs_currency)
                .map(|v| v.to_string())
                .unwrap_or_else(|| "n/a".to_string());
            let change = data
                .get(&change_key)
                .and_then(|v| v.as_f64())
                .map(|c| format!(" ({c:+.2}% 24h)"))
                .unwrap_or_default();
            (coin.clone(), format!("{price}{change}"))
        })
        .collect();

    Ok(serde_json::to_string(&ToolResult::success(
        format_summary(&vs_currency, &prices),
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
        let prices = vec![("bitcoin".to_string(), "67000 (+1.50% 24h)".to_string())];
        let out = format_summary("usd", &prices);
        let (body, footer) = out.split_once("---").unwrap();
        assert!(footer.contains("Data source: CoinGecko simple/price API"));
        assert!(footer.contains("Fields returned: vs_currency, prices."));
        assert!(out.trim_end().ends_with("not listed above."));
        assert!(body.contains("Crypto prices (vs USD)"));
        assert!(body.contains("bitcoin: 67000 (+1.50% 24h)"));
    }

    #[test]
    fn percent_encode_basics() {
        assert_eq!(percent_encode("bitcoin,ethereum"), "bitcoin%2Cethereum");
        assert_eq!(percent_encode("usd"), "usd");
    }
}
