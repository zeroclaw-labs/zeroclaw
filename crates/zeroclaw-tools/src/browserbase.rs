//! Browserbase managed-browser backend.
//!
//! Browserbase (<https://www.browserbase.com>) hosts Chromium sessions and
//! hands back a Chrome DevTools Protocol (CDP) WebSocket endpoint. This
//! module implements:
//!
//! - [`BrowserbaseClient`]: the HTTP control-plane calls (create/release a
//!   session).
//! - A minimal hand-rolled CDP client ([`CdpConnection`]) over
//!   `tokio-tungstenite` — just enough to drive `Target.*`, `Page.*`,
//!   `Runtime.evaluate`, and `Input.*` for the subset of `BrowserAction`
//!   variants the `browserbase` backend supports (open/navigate,
//!   screenshot, click, fill/type, and text/title/url extraction). No
//!   general-purpose CDP crate (e.g. `chromiumoxide`) is used on purpose —
//!   the wire surface we need is tiny.
//! - [`BrowserbaseSession`]: the live-session handle cached by `BrowserTool`,
//!   with idle-timeout detection and an RAII `Drop` guard that best-effort
//!   releases the remote session if nothing else did.
//!
//! Session lifecycle notes:
//! - Sessions are never intentionally leaked: every fallible step after the
//!   HTTP session is created (CDP connect, `Target.createTarget`,
//!   `Target.attachToTarget`) releases the just-created session on error
//!   before returning.
//! - Idle expiry is checked lazily on next use (no background reaper task):
//!   `BrowserTool` calls [`BrowserbaseSession::is_stale`] before reusing a
//!   cached session and replaces it if stale.
//! - `Drop` spawns a best-effort release on the current Tokio runtime if the
//!   session was not already explicitly released. This is a safety net, not
//!   a substitute for explicit release on the happy path (process exit can
//!   still race the spawned task).

use anyhow::Context;
use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};

const DEFAULT_API_BASE_URL: &str = "https://api.browserbase.com";

/// Runtime configuration for the Browserbase backend.
///
/// Mirrors `zeroclaw_config::schema::BrowserbaseConfig` field-for-field; kept
/// as a separate plain struct (like `ComputerUseConfig` in `browser.rs`) so
/// this crate does not need to depend on the full config-schema type to
/// construct or test a client.
#[derive(Clone)]
pub struct BrowserbaseConfig {
    pub api_key: Option<String>,
    pub project_id: Option<String>,
    pub region: String,
    pub session_ttl_secs: u64,
    pub keep_alive: bool,
    pub context_id: Option<String>,
}

impl std::fmt::Debug for BrowserbaseConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BrowserbaseConfig")
            .field("api_key", &self.api_key.as_ref().map(|_| "***"))
            .field("project_id", &self.project_id)
            .field("region", &self.region)
            .field("session_ttl_secs", &self.session_ttl_secs)
            .field("keep_alive", &self.keep_alive)
            .field("context_id", &self.context_id)
            .finish()
    }
}

impl Default for BrowserbaseConfig {
    fn default() -> Self {
        Self {
            api_key: None,
            project_id: None,
            region: "us-west-2".into(),
            session_ttl_secs: 900,
            keep_alive: false,
            context_id: None,
        }
    }
}

// ── HTTP control plane ──────────────────────────────────────────

#[derive(Debug, Serialize)]
struct ContextSettings {
    id: String,
    persist: bool,
}

#[derive(Debug, Serialize)]
struct BrowserSettingsRequest {
    context: ContextSettings,
}

#[derive(Debug, Serialize)]
struct CreateSessionRequest {
    #[serde(rename = "projectId")]
    project_id: String,
    region: String,
    timeout: u64,
    #[serde(rename = "browserSettings", skip_serializing_if = "Option::is_none")]
    browser_settings: Option<BrowserSettingsRequest>,
}

/// Response body from `POST /v1/sessions`.
#[derive(Debug, Clone, Deserialize)]
pub struct CreateSessionResponse {
    pub id: String,
    #[serde(rename = "connectUrl")]
    pub connect_url: String,
}

#[derive(Debug, Serialize)]
struct UpdateSessionRequest {
    status: &'static str,
}

/// HTTP client for the Browserbase session control-plane
/// (`https://api.browserbase.com/v1/sessions`).
pub struct BrowserbaseClient {
    http: reqwest::Client,
    base_url: String,
    config: BrowserbaseConfig,
}

impl BrowserbaseClient {
    pub fn new(config: BrowserbaseConfig) -> Self {
        Self::with_base_url(config, DEFAULT_API_BASE_URL)
    }

    /// Construct a client pointed at a non-default API base URL. Used by
    /// tests to target a `wiremock` server; production callers should use
    /// [`BrowserbaseClient::new`].
    pub fn with_base_url(config: BrowserbaseConfig, base_url: impl Into<String>) -> Self {
        Self {
            http: zeroclaw_config::schema::build_runtime_proxy_client("tool.browser.browserbase"),
            base_url: base_url.into().trim_end_matches('/').to_string(),
            config,
        }
    }

    fn require_api_key(&self) -> anyhow::Result<&str> {
        let key = self.config.api_key.as_deref().unwrap_or("").trim();
        if key.is_empty() {
            anyhow::bail!(
                "browser.backend='browserbase' requires browser.browserbase.api_key to be set"
            );
        }
        Ok(key)
    }

    fn require_project_id(&self) -> anyhow::Result<&str> {
        let id = self.config.project_id.as_deref().unwrap_or("").trim();
        if id.is_empty() {
            anyhow::bail!(
                "browser.backend='browserbase' requires browser.browserbase.project_id to be set"
            );
        }
        Ok(id)
    }

    /// `POST /v1/sessions` — create a new managed browser session.
    pub async fn create_session(&self) -> anyhow::Result<CreateSessionResponse> {
        let api_key = self.require_api_key()?;
        let project_id = self.require_project_id()?;

        let browser_settings = self.config.context_id.as_ref().map(|id| BrowserSettingsRequest {
            context: ContextSettings {
                id: id.clone(),
                persist: true,
            },
        });

        let body = CreateSessionRequest {
            project_id: project_id.to_string(),
            region: self.config.region.clone(),
            timeout: self.config.session_ttl_secs,
            browser_settings,
        };

        let url = format!("{}/v1/sessions", self.base_url);
        let response = self
            .http
            .post(&url)
            .header("X-BB-API-Key", api_key)
            .json(&body)
            .send()
            .await
            .context("Failed to reach Browserbase session API")?;

        let status = response.status();
        let text = response
            .text()
            .await
            .context("Failed to read Browserbase session-create response body")?;

        if !status.is_success() {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"status": status.as_u16()})),
                "browserbase: session create failed"
            );
            anyhow::bail!("Browserbase session create failed with status {status}: {}", text.trim());
        }

        serde_json::from_str::<CreateSessionResponse>(&text)
            .with_context(|| format!("Failed to parse Browserbase session-create response: {text}"))
    }

    /// `POST /v1/sessions/{id}` with `{"status":"REQUEST_RELEASE"}` — ask
    /// Browserbase to tear the session down. Best-effort by design: callers
    /// (including the `Drop` guard) treat failures here as non-fatal since
    /// the session will also expire server-side via its own TTL.
    pub async fn release_session(&self, session_id: &str) -> anyhow::Result<()> {
        let api_key = self.require_api_key()?;
        let url = format!("{}/v1/sessions/{session_id}", self.base_url);

        let response = self
            .http
            .post(&url)
            .header("X-BB-API-Key", api_key)
            .json(&UpdateSessionRequest {
                status: "REQUEST_RELEASE",
            })
            .send()
            .await
            .context("Failed to reach Browserbase session API for release")?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            anyhow::bail!(
                "Browserbase session release failed with status {status}: {}",
                text.trim()
            );
        }
        Ok(())
    }

    /// Create a session and drive the returned CDP endpoint far enough to
    /// have one attached page target ready for navigation. On any failure
    /// after the HTTP session exists, the session is released before the
    /// error is returned — the caller never has to clean up a half-open
    /// session.
    pub async fn open_session(self: &Arc<Self>) -> anyhow::Result<BrowserbaseSession> {
        let created = self.create_session().await?;

        match self.attach_new_target(&created.connect_url).await {
            Ok((cdp, cdp_session_id, target_id)) => Ok(BrowserbaseSession {
                client: Arc::clone(self),
                session_id: created.id,
                cdp,
                cdp_session_id,
                target_id,
                last_used: Instant::now(),
                released: false,
            }),
            Err(err) => {
                if let Err(release_err) = self.release_session(&created.id).await {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure),
                        &format!("browserbase: failed to release session after setup error: {release_err}")
                    );
                }
                Err(err)
            }
        }
    }

    async fn attach_new_target(
        &self,
        connect_url: &str,
    ) -> anyhow::Result<(CdpConnection, String, String)> {
        let mut cdp = CdpConnection::connect(connect_url).await?;
        let target_id = cdp.create_target("about:blank").await?;
        let cdp_session_id = cdp.attach_to_target(&target_id).await?;
        Ok((cdp, cdp_session_id, target_id))
    }
}

// ── Minimal hand-rolled CDP client ──────────────────────────────

type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;

#[derive(Debug, Serialize)]
struct CdpRequest<'a> {
    id: u64,
    method: &'a str,
    params: Value,
    #[serde(rename = "sessionId", skip_serializing_if = "Option::is_none")]
    session_id: Option<&'a str>,
}

#[derive(Debug, Deserialize)]
struct CdpResponse {
    #[serde(default)]
    id: Option<u64>,
    #[serde(default)]
    result: Option<Value>,
    #[serde(default)]
    error: Option<CdpErrorBody>,
}

#[derive(Debug, Deserialize)]
struct CdpErrorBody {
    code: i64,
    message: String,
}

/// A single WebSocket connection to a Browserbase CDP endpoint, with a
/// synchronous request/response call helper. Frames that are not responses
/// to the in-flight request (CDP events, or responses to other targets) are
/// read and discarded — acceptable because `BrowserTool` only ever drives
/// one action at a time per session (guarded by its own mutex).
pub struct CdpConnection {
    stream: WsStream,
    next_id: u64,
}

impl CdpConnection {
    pub async fn connect(connect_url: &str) -> anyhow::Result<Self> {
        let (stream, _response) = connect_async(connect_url)
            .await
            .with_context(|| format!("Failed to connect to Browserbase CDP endpoint {connect_url}"))?;
        Ok(Self { stream, next_id: 1 })
    }

    async fn call(
        &mut self,
        method: &str,
        params: Value,
        session_id: Option<&str>,
    ) -> anyhow::Result<Value> {
        let id = self.next_id;
        self.next_id += 1;

        let request = CdpRequest {
            id,
            method,
            params,
            session_id,
        };
        let payload = serde_json::to_string(&request)
            .with_context(|| format!("Failed to serialize CDP request for {method}"))?;

        self.stream
            .send(Message::Text(payload.into()))
            .await
            .with_context(|| format!("Failed to send CDP command {method}"))?;

        loop {
            let next = self.stream.next().await.ok_or_else(|| {
                anyhow::Error::msg(format!(
                    "Browserbase CDP connection closed while waiting for response to {method}"
                ))
            })?;
            let message = next.with_context(|| {
                format!("Browserbase CDP connection error while waiting for response to {method}")
            })?;

            let text = match message {
                Message::Text(text) => text,
                Message::Close(_) => anyhow::bail!(
                    "Browserbase CDP connection closed by remote while waiting for {method}"
                ),
                _ => continue,
            };

            let Ok(parsed) = serde_json::from_str::<CdpResponse>(&text) else {
                // Malformed or unrelated frame (e.g. a CDP event) — ignore.
                continue;
            };

            if parsed.id != Some(id) {
                continue;
            }

            if let Some(error) = parsed.error {
                anyhow::bail!("CDP {method} failed: {} (code {})", error.message, error.code);
            }

            return Ok(parsed.result.unwrap_or(Value::Null));
        }
    }

    pub async fn create_target(&mut self, url: &str) -> anyhow::Result<String> {
        let result = self
            .call("Target.createTarget", json!({ "url": url }), None)
            .await?;
        result
            .get("targetId")
            .and_then(Value::as_str)
            .map(String::from)
            .ok_or_else(|| anyhow::Error::msg("Target.createTarget response missing targetId"))
    }

    pub async fn attach_to_target(&mut self, target_id: &str) -> anyhow::Result<String> {
        let result = self
            .call(
                "Target.attachToTarget",
                json!({ "targetId": target_id, "flatten": true }),
                None,
            )
            .await?;
        result
            .get("sessionId")
            .and_then(Value::as_str)
            .map(String::from)
            .ok_or_else(|| anyhow::Error::msg("Target.attachToTarget response missing sessionId"))
    }

    pub async fn navigate(&mut self, session_id: &str, url: &str) -> anyhow::Result<()> {
        let result = self
            .call("Page.navigate", json!({ "url": url }), Some(session_id))
            .await?;
        if let Some(error_text) = result.get("errorText").and_then(Value::as_str) {
            anyhow::bail!("Page.navigate to {url} failed: {error_text}");
        }
        Ok(())
    }

    pub async fn capture_screenshot(&mut self, session_id: &str) -> anyhow::Result<String> {
        let result = self
            .call(
                "Page.captureScreenshot",
                json!({ "format": "png" }),
                Some(session_id),
            )
            .await?;
        result
            .get("data")
            .and_then(Value::as_str)
            .map(String::from)
            .ok_or_else(|| anyhow::Error::msg("Page.captureScreenshot response missing data"))
    }

    /// `Runtime.evaluate` with `returnByValue: true`, returning the JSON
    /// value of the expression (or `Value::Null` for `undefined`/`null`).
    pub async fn evaluate(&mut self, session_id: &str, expression: &str) -> anyhow::Result<Value> {
        let outer = self
            .call(
                "Runtime.evaluate",
                json!({ "expression": expression, "returnByValue": true, "awaitPromise": true }),
                Some(session_id),
            )
            .await?;

        if let Some(exception) = outer.get("exceptionDetails") {
            let description = exception
                .get("exception")
                .and_then(|e| e.get("description"))
                .and_then(Value::as_str)
                .unwrap_or("unknown error");
            anyhow::bail!("JavaScript evaluation failed: {description}");
        }

        Ok(outer
            .get("result")
            .and_then(|r| r.get("value"))
            .cloned()
            .unwrap_or(Value::Null))
    }

    pub async fn dispatch_mouse_event(
        &mut self,
        session_id: &str,
        params: Value,
    ) -> anyhow::Result<()> {
        self.call("Input.dispatchMouseEvent", params, Some(session_id))
            .await?;
        Ok(())
    }

    pub async fn dispatch_key_event(
        &mut self,
        session_id: &str,
        params: Value,
    ) -> anyhow::Result<()> {
        self.call("Input.dispatchKeyEvent", params, Some(session_id))
            .await?;
        Ok(())
    }

    /// Best-effort close of the underlying WebSocket. Errors are swallowed
    /// by callers (the connection is being torn down regardless).
    pub async fn close(&mut self) -> anyhow::Result<()> {
        self.stream
            .close(None)
            .await
            .context("Failed to close Browserbase CDP WebSocket")
    }
}

fn js_string_literal(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_string())
}

// ── Session handle ──────────────────────────────────────────────

/// A live Browserbase session: the HTTP session id (for release) plus an
/// attached CDP target ready to drive `BrowserAction`s.
pub struct BrowserbaseSession {
    client: Arc<BrowserbaseClient>,
    session_id: String,
    cdp: CdpConnection,
    cdp_session_id: String,
    // Not read today; retained for future target-lifecycle use (e.g.
    // `Target.closeTarget` on release) and for diagnostics.
    #[allow(dead_code)]
    target_id: String,
    last_used: Instant,
    released: bool,
}

impl BrowserbaseSession {
    /// Whether this session should be replaced before its next use: either
    /// idle past `ttl` (when not `keep_alive`) — checked lazily by
    /// `BrowserTool` on the next action rather than via a background timer.
    pub fn is_stale(&self, ttl: Duration, keep_alive: bool) -> bool {
        if keep_alive {
            return false;
        }
        self.last_used.elapsed() > ttl
    }

    fn touch(&mut self) {
        self.last_used = Instant::now();
    }

    pub async fn navigate(&mut self, url: &str) -> anyhow::Result<()> {
        let session_id = self.cdp_session_id.clone();
        self.cdp.navigate(&session_id, url).await?;
        self.touch();
        Ok(())
    }

    pub async fn screenshot_base64(&mut self) -> anyhow::Result<String> {
        let session_id = self.cdp_session_id.clone();
        let data = self.cdp.capture_screenshot(&session_id).await?;
        self.touch();
        Ok(data)
    }

    async fn evaluate_value(&mut self, expression: &str) -> anyhow::Result<Value> {
        let session_id = self.cdp_session_id.clone();
        self.cdp.evaluate(&session_id, expression).await
    }

    async fn element_center(&mut self, selector: &str) -> anyhow::Result<(f64, f64)> {
        let expr = format!(
            "(function(){{var el=document.querySelector({sel});if(!el)return null;\
             el.scrollIntoView({{block:'center',inline:'center'}});\
             var r=el.getBoundingClientRect();\
             return {{x:r.left+r.width/2,y:r.top+r.height/2}};}})()",
            sel = js_string_literal(selector)
        );
        let value = self.evaluate_value(&expr).await?;
        if value.is_null() {
            anyhow::bail!("No element matched selector '{selector}'");
        }
        let x = value
            .get("x")
            .and_then(Value::as_f64)
            .ok_or_else(|| anyhow::Error::msg("Element bounding box missing x"))?;
        let y = value
            .get("y")
            .and_then(Value::as_f64)
            .ok_or_else(|| anyhow::Error::msg("Element bounding box missing y"))?;
        Ok((x, y))
    }

    pub async fn click(&mut self, selector: &str) -> anyhow::Result<()> {
        let (x, y) = self.element_center(selector).await?;
        let session_id = self.cdp_session_id.clone();
        self.cdp
            .dispatch_mouse_event(&session_id, json!({"type": "mouseMoved", "x": x, "y": y}))
            .await?;
        self.cdp
            .dispatch_mouse_event(
                &session_id,
                json!({"type": "mousePressed", "x": x, "y": y, "button": "left", "clickCount": 1}),
            )
            .await?;
        self.cdp
            .dispatch_mouse_event(
                &session_id,
                json!({"type": "mouseReleased", "x": x, "y": y, "button": "left", "clickCount": 1}),
            )
            .await?;
        self.touch();
        Ok(())
    }

    async fn type_str(&mut self, text: &str) -> anyhow::Result<()> {
        let session_id = self.cdp_session_id.clone();
        for ch in text.chars() {
            let text = ch.to_string();
            self.cdp
                .dispatch_key_event(
                    &session_id,
                    json!({"type": "keyDown", "text": text, "unmodifiedText": text, "key": text}),
                )
                .await?;
            self.cdp
                .dispatch_key_event(&session_id, json!({"type": "keyUp", "key": text}))
                .await?;
        }
        Ok(())
    }

    pub async fn type_into(&mut self, selector: &str, text: &str) -> anyhow::Result<()> {
        self.click(selector).await?;
        self.type_str(text).await?;
        self.touch();
        Ok(())
    }

    pub async fn fill(&mut self, selector: &str, value: &str) -> anyhow::Result<()> {
        self.click(selector).await?;
        let clear_expr = format!(
            "(function(){{var el=document.querySelector({sel});if(el){{el.value='';\
             el.dispatchEvent(new Event('input',{{bubbles:true}}));}}}})()",
            sel = js_string_literal(selector)
        );
        self.evaluate_value(&clear_expr).await?;
        self.type_str(value).await?;
        self.touch();
        Ok(())
    }

    pub async fn get_text(&mut self, selector: &str) -> anyhow::Result<String> {
        let expr = format!(
            "(function(){{var el=document.querySelector({sel});\
             return el ? (el.innerText||el.textContent||'') : null;}})()",
            sel = js_string_literal(selector)
        );
        let value = self.evaluate_value(&expr).await?;
        self.touch();
        value
            .as_str()
            .map(String::from)
            .ok_or_else(|| anyhow::Error::msg(format!("No element matched selector '{selector}'")))
    }

    pub async fn get_title(&mut self) -> anyhow::Result<String> {
        let value = self.evaluate_value("document.title").await?;
        self.touch();
        Ok(value.as_str().unwrap_or_default().to_string())
    }

    pub async fn get_url(&mut self) -> anyhow::Result<String> {
        let value = self.evaluate_value("window.location.href").await?;
        self.touch();
        Ok(value.as_str().unwrap_or_default().to_string())
    }

    /// Explicit release: asks Browserbase to tear the session down and
    /// marks it released so the `Drop` guard does not attempt a redundant
    /// (and by-then-pointless) background release.
    pub async fn release(mut self) -> anyhow::Result<()> {
        self.released = true;
        let _ = self.cdp.close().await;
        self.client.release_session(&self.session_id).await
    }
}

impl Drop for BrowserbaseSession {
    fn drop(&mut self) {
        if self.released {
            return;
        }
        self.released = true;

        let client = Arc::clone(&self.client);
        let session_id = self.session_id.clone();
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                if let Err(err) = client.release_session(&session_id).await {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure),
                        &format!("browserbase: background session release failed: {err}")
                    );
                }
            });
        }
    }
}

/// Decode a `Page.captureScreenshot` base64 payload into raw PNG bytes.
pub fn decode_screenshot_base64(data: &str) -> anyhow::Result<Vec<u8>> {
    base64::engine::general_purpose::STANDARD
        .decode(data)
        .context("Failed to decode Browserbase screenshot base64 payload")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> BrowserbaseConfig {
        BrowserbaseConfig {
            api_key: Some("bb-test-key".into()),
            project_id: Some("proj-123".into()),
            region: "us-west-2".into(),
            session_ttl_secs: 900,
            keep_alive: false,
            context_id: None,
        }
    }

    #[test]
    fn create_session_request_serializes_camel_case_without_context() {
        let body = CreateSessionRequest {
            project_id: "proj-123".into(),
            region: "us-west-2".into(),
            timeout: 900,
            browser_settings: None,
        };
        let value = serde_json::to_value(&body).unwrap();
        assert_eq!(
            value,
            json!({
                "projectId": "proj-123",
                "region": "us-west-2",
                "timeout": 900,
            })
        );
    }

    #[test]
    fn create_session_request_serializes_persistent_context_when_set() {
        let body = CreateSessionRequest {
            project_id: "proj-123".into(),
            region: "us-west-2".into(),
            timeout: 900,
            browser_settings: Some(BrowserSettingsRequest {
                context: ContextSettings {
                    id: "ctx-abc".into(),
                    persist: true,
                },
            }),
        };
        let value = serde_json::to_value(&body).unwrap();
        assert_eq!(
            value,
            json!({
                "projectId": "proj-123",
                "region": "us-west-2",
                "timeout": 900,
                "browserSettings": {
                    "context": { "id": "ctx-abc", "persist": true }
                }
            })
        );
    }

    #[test]
    fn create_session_response_deserializes() {
        let raw = r#"{"id":"sess-1","connectUrl":"wss://connect.browserbase.com/session-1"}"#;
        let parsed: CreateSessionResponse = serde_json::from_str(raw).unwrap();
        assert_eq!(parsed.id, "sess-1");
        assert_eq!(parsed.connect_url, "wss://connect.browserbase.com/session-1");
    }

    #[test]
    fn create_session_response_ignores_unknown_fields() {
        let raw = r#"{"id":"sess-1","connectUrl":"wss://x","seleniumRemoteUrl":"http://x","status":"RUNNING"}"#;
        let parsed: CreateSessionResponse = serde_json::from_str(raw).unwrap();
        assert_eq!(parsed.id, "sess-1");
    }

    #[test]
    fn update_session_request_serializes_release_status() {
        let body = UpdateSessionRequest {
            status: "REQUEST_RELEASE",
        };
        let value = serde_json::to_value(&body).unwrap();
        assert_eq!(value, json!({ "status": "REQUEST_RELEASE" }));
    }

    #[test]
    fn cdp_request_serializes_without_session_id() {
        let req = CdpRequest {
            id: 1,
            method: "Target.createTarget",
            params: json!({ "url": "about:blank" }),
            session_id: None,
        };
        let value = serde_json::to_value(&req).unwrap();
        assert_eq!(
            value,
            json!({
                "id": 1,
                "method": "Target.createTarget",
                "params": { "url": "about:blank" },
            })
        );
    }

    #[test]
    fn cdp_request_serializes_with_session_id() {
        let req = CdpRequest {
            id: 2,
            method: "Page.navigate",
            params: json!({ "url": "https://example.com" }),
            session_id: Some("cdp-session-1"),
        };
        let value = serde_json::to_value(&req).unwrap();
        assert_eq!(
            value,
            json!({
                "id": 2,
                "method": "Page.navigate",
                "params": { "url": "https://example.com" },
                "sessionId": "cdp-session-1",
            })
        );
    }

    #[test]
    fn cdp_response_deserializes_success_result() {
        let raw = r#"{"id":1,"result":{"targetId":"t1"},"sessionId":"s1"}"#;
        let parsed: CdpResponse = serde_json::from_str(raw).unwrap();
        assert_eq!(parsed.id, Some(1));
        assert_eq!(
            parsed.result.unwrap().get("targetId").unwrap().as_str(),
            Some("t1")
        );
        assert!(parsed.error.is_none());
    }

    #[test]
    fn cdp_response_deserializes_error() {
        let raw = r#"{"id":3,"error":{"code":-32000,"message":"No target with given id found"}}"#;
        let parsed: CdpResponse = serde_json::from_str(raw).unwrap();
        assert_eq!(parsed.id, Some(3));
        let err = parsed.error.unwrap();
        assert_eq!(err.code, -32000);
        assert_eq!(err.message, "No target with given id found");
    }

    #[test]
    fn cdp_response_deserializes_event_without_id() {
        // CDP events (unsolicited notifications) have no top-level "id".
        let raw = r#"{"method":"Target.targetInfoChanged","params":{}}"#;
        let parsed: CdpResponse = serde_json::from_str(raw).unwrap();
        assert_eq!(parsed.id, None);
        assert!(parsed.result.is_none());
    }

    #[test]
    fn js_string_literal_escapes_quotes_and_backslashes() {
        let literal = js_string_literal(r#"a"b\c"#);
        assert_eq!(literal, r#""a\"b\\c""#);
    }

    #[test]
    fn browserbase_config_debug_redacts_api_key() {
        let config = test_config();
        let debug = format!("{config:?}");
        assert!(!debug.contains("bb-test-key"));
        assert!(debug.contains("***"));
    }

    #[test]
    fn require_api_key_and_project_id_fail_clearly_when_unset() {
        let client = BrowserbaseClient::new(BrowserbaseConfig::default());
        let err = client.require_api_key().unwrap_err().to_string();
        assert!(err.contains("browser.browserbase.api_key"), "{err}");

        let config = BrowserbaseConfig {
            api_key: Some("k".into()),
            ..BrowserbaseConfig::default()
        };
        let client = BrowserbaseClient::new(config);
        let err = client.require_project_id().unwrap_err().to_string();
        assert!(err.contains("browser.browserbase.project_id"), "{err}");
    }

    #[tokio::test]
    async fn create_session_hits_configured_endpoint_and_parses_response() {
        use wiremock::matchers::{body_json, header, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/sessions"))
            .and(header("X-BB-API-Key", "bb-test-key"))
            .and(body_json(json!({
                "projectId": "proj-123",
                "region": "us-west-2",
                "timeout": 900,
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "sess-42",
                "connectUrl": "wss://connect.browserbase.com/sess-42",
            })))
            .mount(&server)
            .await;

        let client = BrowserbaseClient::with_base_url(test_config(), server.uri());
        let created = client.create_session().await.unwrap();
        assert_eq!(created.id, "sess-42");
        assert_eq!(created.connect_url, "wss://connect.browserbase.com/sess-42");
    }

    #[tokio::test]
    async fn create_session_surfaces_non_success_status() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/sessions"))
            .respond_with(ResponseTemplate::new(402).set_body_string("payment required"))
            .mount(&server)
            .await;

        let client = BrowserbaseClient::with_base_url(test_config(), server.uri());
        let err = client.create_session().await.unwrap_err().to_string();
        assert!(err.contains("402"), "{err}");
    }

    #[tokio::test]
    async fn release_session_posts_request_release_status() {
        use wiremock::matchers::{body_json, header, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/sessions/sess-42"))
            .and(header("X-BB-API-Key", "bb-test-key"))
            .and(body_json(json!({ "status": "REQUEST_RELEASE" })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "success": true })))
            .mount(&server)
            .await;

        let client = BrowserbaseClient::with_base_url(test_config(), server.uri());
        client.release_session("sess-42").await.unwrap();
    }

    #[tokio::test]
    async fn release_session_surfaces_failure_status() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/sessions/sess-42"))
            .respond_with(ResponseTemplate::new(404).set_body_string("not found"))
            .mount(&server)
            .await;

        let client = BrowserbaseClient::with_base_url(test_config(), server.uri());
        let err = client.release_session("sess-42").await.unwrap_err().to_string();
        assert!(err.contains("404"), "{err}");
    }

    #[tokio::test]
    async fn open_session_releases_http_session_when_cdp_connect_fails() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/sessions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "sess-99",
                // Not a real CDP endpoint — connect_async will fail against
                // this plain HTTP mock server, exercising the cleanup path.
                "connectUrl": format!("{}/not-a-websocket", server.uri()),
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/sessions/sess-99"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "success": true })))
            .mount(&server)
            .await;

        let client = Arc::new(BrowserbaseClient::with_base_url(test_config(), server.uri()));
        let result = client.open_session().await;
        assert!(result.is_err(), "expected CDP connect to fail against a non-WS endpoint");
    }
}
