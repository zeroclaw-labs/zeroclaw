//! A ZeroClaw WIT tool plugin: `mastodon_post`.
//!
//! Posts a status (toot) to a Mastodon instance. Demonstrates host-injected
//! credentials: the plugin requests `secret_exists("MASTODON_TOKEN")` and makes
//! the HTTP call, but the *host* injects `Authorization: Bearer <token>` at the
//! egress boundary — the WASM guest never sees the token value.
//!
//! Build:  rustup target add wasm32-wasip2
//!         cargo build --target wasm32-wasip2 --release
//!         cp target/wasm32-wasip2/release/mastodon_post.wasm mastodon_post.wasm

wit_bindgen::generate!({
    world: "tool-plugin",
    path: "../../wit/v0",
    features: ["plugins-wit-v0"],
});

use exports::zeroclaw::plugin::plugin_info::Guest as PluginInfoGuest;
use exports::zeroclaw::plugin::tool::{Guest as ToolGuest, ToolResult};
use zeroclaw::plugin::host;

struct MastodonPost;

impl PluginInfoGuest for MastodonPost {
    fn plugin_name() -> String {
        "mastodon-post".to_string()
    }
    fn plugin_version() -> String {
        "0.1.0".to_string()
    }
}

impl ToolGuest for MastodonPost {
    fn name() -> String {
        "mastodon_post".to_string()
    }

    fn description() -> String {
        "Post a status (toot) to a Mastodon instance. Requires a Mastodon access \
         token configured host-side (the plugin never sees it)."
            .to_string()
    }

    fn parameters_schema() -> String {
        serde_json::json!({
            "type": "object",
            "properties": {
                "instance": {
                    "type": "string",
                    "description": "Mastodon instance base URL, e.g. \"https://mastodon.social\"."
                },
                "text": {
                    "type": "string",
                    "description": "The status text to post."
                }
            },
            "required": ["instance", "text"]
        })
        .to_string()
    }

    fn execute(args: String) -> Result<ToolResult, String> {
        let parsed: serde_json::Value =
            serde_json::from_str(&args).map_err(|e| format!("invalid arguments JSON: {e}"))?;

        let instance = parsed
            .get("instance")
            .and_then(|v| v.as_str())
            .map(|s| s.trim().trim_end_matches('/'))
            .filter(|s| s.starts_with("https://"))
            .ok_or_else(|| "missing 'instance' (e.g. https://mastodon.social)".to_string())?;
        let text = parsed
            .get("text")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| "missing 'text'".to_string())?;

        // Friendly preflight: the host injects the token, but the guest can
        // confirm it's configured (existence only — never the value).
        if !host::secret_exists("MASTODON_TOKEN") {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(
                    "MASTODON_TOKEN is not configured. Add it to [http_request].secrets \
                     (or the environment) on the host."
                        .to_string(),
                ),
            });
        }

        let url = format!("{instance}/api/v1/statuses");
        let body = serde_json::json!({ "status": text }).to_string().into_bytes();
        // The host adds `Authorization: Bearer <token>`; the guest sends only these.
        let headers = r#"{"Content-Type":"application/json","Accept":"application/json"}"#;

        let response = match host::http_request("POST", &url, headers, Some(&body), None) {
            Ok(response) => response,
            Err(error) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("request failed: {error}")),
                });
            }
        };

        match response.status {
            200 | 201 => {
                let posted: serde_json::Value = serde_json::from_slice(&response.body)
                    .map_err(|e| format!("invalid response JSON: {e}"))?;
                let toot_url = posted
                    .get("url")
                    .and_then(|u| u.as_str())
                    .unwrap_or("(posted)");
                Ok(ToolResult {
                    success: true,
                    output: format!("Posted to Mastodon: {toot_url}"),
                    error: None,
                })
            }
            401 | 403 => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(
                    "Mastodon rejected the token (HTTP 401/403). Check MASTODON_TOKEN and \
                     that it has the write:statuses scope."
                        .to_string(),
                ),
            }),
            other => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Mastodon returned HTTP {other}")),
            }),
        }
    }
}

export!(MastodonPost);
