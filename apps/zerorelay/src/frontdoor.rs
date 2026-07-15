use crate::frontdoor_assets::{
    APP_JS, INDEX_HTML, SERVICE_WORKER_JS, TUNNEL_WORKER_JS, WEBUI_FETCH_BRIDGE_JS,
};
use crate::frontdoor_tls_assets::TLS_ENGINE_JS;
use anyhow::{Context, Result};
use std::io::Cursor;
use std::path::PathBuf;
use std::pin::Pin;
use std::task::{Context as TaskContext, Poll};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::time::{Duration, timeout};
use tokio_tungstenite::WebSocketStream;
use zeroclaw_relay_proto::SUBPROTOCOL;

const MAX_HTTP_HEAD: usize = 16 * 1024;
const HTTP_KEEP_ALIVE_IDLE: Duration = Duration::from_secs(5);

pub(crate) enum Frontdoor<S> {
    WebSocket(Box<WebSocketStream<PrefixedIo<S>>>),
    ServedHttp,
}

pub(crate) async fn accept_or_serve<S>(mut stream: S) -> Result<Frontdoor<S>>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut pending = Vec::with_capacity(1024);
    let mut head = read_http_head(&mut stream, &mut pending).await?;
    loop {
        if is_websocket_upgrade(&head) {
            let mut prefix = head;
            prefix.extend_from_slice(&pending);
            let io = PrefixedIo {
                prefix: Cursor::new(prefix),
                inner: stream,
            };
            let ws = tokio_tungstenite::accept_hdr_async(io, select_subprotocol)
                .await
                .context("relay websocket handshake")?;
            return Ok(Frontdoor::WebSocket(Box::new(ws)));
        }

        let response = response_for(&head);
        stream.write_all(&response).await?;
        if should_close_after_response(&head) {
            break;
        }
        match timeout(
            HTTP_KEEP_ALIVE_IDLE,
            read_http_head(&mut stream, &mut pending),
        )
        .await
        {
            Ok(Ok(next)) => head = next,
            Ok(Err(_)) | Err(_) => break,
        }
    }
    let _ = stream.shutdown().await;
    Ok(Frontdoor::ServedHttp)
}

async fn read_http_head<S>(stream: &mut S, pending: &mut Vec<u8>) -> Result<Vec<u8>>
where
    S: AsyncRead + Unpin,
{
    let mut chunk = [0u8; 1024];
    loop {
        if let Some(head) = take_http_head(pending) {
            return Ok(head);
        }
        if pending.len() > MAX_HTTP_HEAD {
            anyhow::bail!("request headers too large");
        }
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            anyhow::bail!("connection closed before request headers");
        }
        pending.extend_from_slice(&chunk[..n]);
    }
}

fn header_end(head: &[u8]) -> Option<usize> {
    head.windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|i| i + 4)
}

fn take_http_head(pending: &mut Vec<u8>) -> Option<Vec<u8>> {
    let end = header_end(pending)?;
    let remaining = pending.split_off(end);
    Some(std::mem::replace(pending, remaining))
}

fn is_websocket_upgrade(head: &[u8]) -> bool {
    let text = String::from_utf8_lossy(head);
    let mut lines = text.lines();
    let Some(request) = lines.next() else {
        return false;
    };
    request.starts_with("GET ")
        && lines.any(|line| {
            let lower = line.to_ascii_lowercase();
            lower.starts_with("upgrade:") && lower.contains("websocket")
        })
}

fn should_close_after_response(head: &[u8]) -> bool {
    let text = String::from_utf8_lossy(head);
    let mut lines = text.lines();
    let request = lines.next().unwrap_or_default();
    let mut connection_close = false;
    let mut connection_keep_alive = false;
    for line in lines {
        let lower = line.to_ascii_lowercase();
        if !lower.starts_with("connection:") {
            continue;
        }
        connection_close |= lower.contains("close");
        connection_keep_alive |= lower.contains("keep-alive");
    }
    connection_close || (request.ends_with(" HTTP/1.0") && !connection_keep_alive)
}

fn response_for(head: &[u8]) -> Vec<u8> {
    let path = request_path(head).unwrap_or("/");
    match path {
        "/" | "/index.html" => http_response("200 OK", "text/html; charset=utf-8", INDEX_HTML),
        "/app.js" => http_response("200 OK", "application/javascript; charset=utf-8", APP_JS),
        "/webui" | "/webui/" => webui_index_response(),
        "/sw.js" => http_response(
            "200 OK",
            "application/javascript; charset=utf-8",
            SERVICE_WORKER_JS,
        ),
        "/tunnel-worker.js" => http_response(
            "200 OK",
            "application/javascript; charset=utf-8",
            TUNNEL_WORKER_JS,
        ),
        "/tls-engine.js" => http_response(
            "200 OK",
            "application/javascript; charset=utf-8",
            TLS_ENGINE_JS,
        ),
        path if path.starts_with("/webui/_app/") => {
            webui_asset_response(path.trim_start_matches("/webui/_app/"), true)
        }
        path if path.starts_with("/_app/") => {
            webui_asset_response(path.trim_start_matches("/_app/"), false)
        }
        path if path.starts_with("/webui/") => webui_index_response(),
        _ => http_response("404 Not Found", "text/plain; charset=utf-8", "not found\n"),
    }
}

fn request_path(head: &[u8]) -> Option<&str> {
    let text = std::str::from_utf8(head).ok()?;
    let request = text.lines().next()?;
    let mut parts = request.split_whitespace();
    match (parts.next(), parts.next()) {
        (Some("GET" | "HEAD"), Some(path)) => Some(path.split_once('?').map_or(path, |(p, _)| p)),
        _ => None,
    }
}

fn http_response(status: &str, content_type: &str, body: &str) -> Vec<u8> {
    http_bytes_response(status, content_type, "no-store", body.as_bytes())
}

fn http_bytes_response(
    status: &str,
    content_type: &str,
    cache_control: &str,
    body: &[u8],
) -> Vec<u8> {
    let mut response = format!(
        "HTTP/1.1 {status}\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\ncache-control: {cache_control}\r\nconnection: keep-alive\r\nkeep-alive: timeout=5\r\n\r\n",
        body.len()
    )
    .into_bytes();
    response.extend_from_slice(body);
    response
}

fn webui_index_response() -> Vec<u8> {
    match std::fs::read_to_string(web_dist_dir().join("index.html")) {
        Ok(html) => {
            let script = format!(
                r#"<script>window.__ZEROCLAW_BASE__="/webui";</script><script>{WEBUI_FETCH_BRIDGE_JS}</script>"#
            );
            let html = html
                .replace("/_app/", "/webui/_app/")
                .replace("<head>", &format!("<head>{script}"));
            http_response("200 OK", "text/html; charset=utf-8", &html)
        }
        Err(_) => http_response(
            "503 Service Unavailable",
            "text/plain; charset=utf-8",
            "ZeroClaw WebUI assets are not available; build web/dist first.\n",
        ),
    }
}

fn webui_asset_response(path: &str, rewrite_base: bool) -> Vec<u8> {
    if path.contains("..") || path.starts_with('/') {
        return http_response(
            "400 Bad Request",
            "text/plain; charset=utf-8",
            "invalid path\n",
        );
    }
    match std::fs::read(web_dist_dir().join(path)) {
        Ok(bytes) => {
            let bytes = if rewrite_base && rewritable_webui_asset(path) {
                match String::from_utf8(bytes) {
                    Ok(text) => rewrite_webui_asset_base(&text).into_bytes(),
                    Err(error) => error.into_bytes(),
                }
            } else {
                bytes
            };
            let cache = if path.contains("assets/") {
                "public, max-age=31536000, immutable"
            } else {
                "no-cache"
            };
            http_bytes_response("200 OK", content_type_for(path), cache, &bytes)
        }
        Err(_) => http_response("404 Not Found", "text/plain; charset=utf-8", "not found\n"),
    }
}

fn rewritable_webui_asset(path: &str) -> bool {
    matches!(
        path.rsplit_once('.').map(|(_, ext)| ext),
        Some("css" | "html" | "js" | "json")
    )
}

fn rewrite_webui_asset_base(text: &str) -> String {
    text.replace("return`/_app/`+", "return`/webui/_app/`+")
        .replace("return\"/_app/\"+", "return\"/webui/_app/\"+")
        .replace("return'/_app/'+", "return'/webui/_app/'+")
}

fn web_dist_dir() -> PathBuf {
    std::env::var_os("ZEROCLAW_WEB_DIST_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../web/dist"))
}

fn content_type_for(path: &str) -> &'static str {
    match path.rsplit_once('.').map(|(_, ext)| ext) {
        Some("css") => "text/css; charset=utf-8",
        Some("js") => "application/javascript; charset=utf-8",
        Some("json") => "application/json; charset=utf-8",
        Some("png") => "image/png",
        Some("svg") => "image/svg+xml",
        Some("xml") => "application/xml; charset=utf-8",
        Some("txt") => "text/plain; charset=utf-8",
        Some("html") => "text/html; charset=utf-8",
        _ => "application/octet-stream",
    }
}

#[allow(clippy::result_large_err)]
fn select_subprotocol(
    req: &tokio_tungstenite::tungstenite::handshake::server::Request,
    mut resp: tokio_tungstenite::tungstenite::handshake::server::Response,
) -> std::result::Result<
    tokio_tungstenite::tungstenite::handshake::server::Response,
    tokio_tungstenite::tungstenite::handshake::server::ErrorResponse,
> {
    let offered = req
        .headers()
        .get_all("Sec-WebSocket-Protocol")
        .iter()
        .any(|v| {
            v.to_str()
                .map(|s| s.split(',').any(|p| p.trim() == SUBPROTOCOL))
                .unwrap_or(false)
        });
    if offered {
        resp.headers_mut().insert(
            "Sec-WebSocket-Protocol",
            tokio_tungstenite::tungstenite::http::HeaderValue::from_static(SUBPROTOCOL),
        );
    }
    Ok(resp)
}

pub(crate) struct PrefixedIo<S> {
    prefix: Cursor<Vec<u8>>,
    inner: S,
}

impl<S> AsyncRead for PrefixedIo<S>
where
    S: AsyncRead + Unpin,
{
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let pos = self.prefix.position() as usize;
        let len = self.prefix.get_ref().len();
        if pos < len {
            let available = &self.prefix.get_ref()[pos..];
            let take = available.len().min(buf.remaining());
            buf.put_slice(&available[..take]);
            self.prefix.set_position((pos + take) as u64);
            return Poll::Ready(Ok(()));
        }
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl<S> AsyncWrite for PrefixedIo<S>
where
    S: AsyncWrite + Unpin,
{
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        data: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, data)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pipelined_request_heads_are_split_without_dropping_overflow() {
        let mut pending =
            b"GET /app.js HTTP/1.1\r\nHost: x\r\n\r\nGET /sw.js HTTP/1.1\r\nHost: x\r\n\r\n"
                .to_vec();
        let first = take_http_head(&mut pending).expect("first request");
        let second = take_http_head(&mut pending).expect("second request");
        assert_eq!(first, b"GET /app.js HTTP/1.1\r\nHost: x\r\n\r\n");
        assert_eq!(second, b"GET /sw.js HTTP/1.1\r\nHost: x\r\n\r\n");
        assert!(pending.is_empty());
    }

    #[test]
    fn websocket_upgrade_is_detected() {
        let head = b"GET /relay HTTP/1.1\r\nHost: x\r\nUpgrade: websocket\r\n\r\n";
        assert!(is_websocket_upgrade(head));
    }

    #[test]
    fn index_route_serves_html() {
        let resp = response_for(b"GET / HTTP/1.1\r\nHost: x\r\n\r\n");
        let text = String::from_utf8(resp).unwrap();
        assert!(text.starts_with("HTTP/1.1 200 OK"));
        assert!(text.contains("ZeroClaw Relay"));
        assert!(text.contains("/app.js"));
        assert!(text.contains("[hidden] { display: none !important; }"));
        assert!(text.contains("sas-panel"));
        assert!(text.contains("webui-panel"));
        assert!(text.contains("body.webui-active"));
        assert!(text.contains("webui-frame"));
    }

    #[test]
    fn app_route_serves_page_driver() {
        let resp = response_for(b"GET /app.js HTTP/1.1\r\nHost: x\r\n\r\n");
        let text = String::from_utf8(resp).unwrap();
        assert!(text.starts_with("HTTP/1.1 200 OK"));
        assert!(text.contains("/tunnel-worker.js"));
        assert!(text.contains("enrollment-material-ready"));
        assert!(text.contains("tls-engine-missing"));
        assert!(text.contains("zeroclaw-rpc-request"));
        assert!(text.contains("tunnel.postMessage(msg, transfers)"));
        assert!(text.contains("window.addEventListener('message'"));
        assert!(text.contains("showWebUi"));
        assert!(text.contains("seedRelayWebUiAuth"));
        assert!(text.contains("zeroclaw_relay_last_connection"));
        assert!(text.contains("restoreSavedConnection"));
        assert!(text.contains("resumeConnection"));
        assert!(text.contains("resume-missing"));
        assert!(text.contains("__ZEROCLAW_RELAY_APP_READY"));
        assert!(text.contains("relayWebUiToken"));
        assert!(text.contains("localStorage.setItem('zeroclaw_token'"));
        assert!(text.contains("relay-mtls."));
        assert!(!text.contains("relay-mtls:"));
        assert!(text.contains("zeroclaw-rpc-notification"));
        assert!(text.contains("Secure tunnel ready. Opening WebUI."));
        assert!(text.contains("document.body.classList.add('webui-active')"));
        assert!(text.contains("document.body.classList.remove('webui-active')"));
        assert!(text.contains("webuiFrame.src = '/webui/'"));
        assert!(text.contains("confirmEnrollment"));
        assert!(text.contains("abortEnrollment"));
        assert!(text.contains("rpc-ready"));
        assert!(!text.contains("chat-panel"));
    }

    #[test]
    fn webui_route_serves_remote_dashboard() {
        let resp = response_for(b"GET /webui/ HTTP/1.1\r\nHost: x\r\n\r\n");
        let text = String::from_utf8(resp).unwrap();
        assert!(text.starts_with("HTTP/1.1 200 OK"));
        assert!(text.contains("<title>ZeroClaw</title>"));
        assert!(text.contains(r#"window.__ZEROCLAW_BASE__="/webui""#));
        assert!(text.contains("__ZEROCLAW_RELAY_FETCH_BRIDGE__"));
        assert!(text.contains("window.fetch = async"));
        assert!(text.contains("window.WebSocket = RelayWebSocket"));
        assert!(text.contains("class RelayChatSocket"));
        assert!(text.contains("session/new"));
        assert!(text.contains("session/prompt"));
        assert!(text.contains("session/approve"));
        assert!(text.contains("zeroclaw-rpc-notification"));
        assert!(text.contains("window.parent.postMessage(msg, location.origin"));
        assert!(
            text.contains("relay webui bridge currently supports read-only dashboard requests")
        );
        assert!(text.contains("/webui/_app/assets/"));
        assert!(!text.contains("Relay service worker is not controlling this page"));
    }

    #[test]
    fn webui_asset_route_serves_dashboard_assets() {
        let index = response_for(b"GET /webui/ HTTP/1.1\r\nHost: x\r\n\r\n");
        let text = String::from_utf8(index).unwrap();
        let asset = text
            .split("/webui/_app/")
            .nth(1)
            .and_then(|rest| rest.split('"').next())
            .expect("dashboard index must reference a rewritten asset");
        let request = format!("GET /webui/_app/{asset} HTTP/1.1\r\nHost: x\r\n\r\n");
        let resp = response_for(request.as_bytes());
        assert!(resp.starts_with(b"HTTP/1.1 200 OK\r\n"));
        let head_end = header_end(&resp).expect("response should include an HTTP header");
        let headers = String::from_utf8(resp[..head_end].to_vec()).unwrap();
        assert!(headers.contains("content-type: "));
        assert!(headers.contains("content-length: "));
    }

    #[test]
    fn head_webui_asset_route_uses_requested_path() {
        let index = response_for(b"GET /webui/ HTTP/1.1\r\nHost: x\r\n\r\n");
        let text = String::from_utf8(index).unwrap();
        let asset = text
            .split("/webui/_app/")
            .nth(1)
            .and_then(|rest| rest.split('"').next())
            .expect("dashboard index must reference a rewritten asset");
        let request = format!("HEAD /_app/{asset} HTTP/1.1\r\nHost: x\r\n\r\n");
        let resp = response_for(request.as_bytes());
        let head_end = header_end(&resp).expect("response should include an HTTP header");
        let headers = String::from_utf8(resp[..head_end].to_vec()).unwrap();
        assert!(headers.starts_with("HTTP/1.1 200 OK"));
        assert!(!headers.contains("content-type: text/html"));
    }

    #[test]
    fn webui_javascript_assets_rewrite_absolute_app_base() {
        let index = response_for(b"GET /webui/ HTTP/1.1\r\nHost: x\r\n\r\n");
        let text = String::from_utf8(index).unwrap();
        let asset = text
            .split("src=\"/webui/_app/")
            .nth(1)
            .and_then(|rest| rest.split('"').next())
            .expect("dashboard index must reference the rewritten entry script");
        let request = format!("GET /webui/_app/{asset} HTTP/1.1\r\nHost: x\r\n\r\n");
        let resp = response_for(request.as_bytes());
        let text = String::from_utf8(resp).unwrap();
        assert!(text.starts_with("HTTP/1.1 200 OK"));
        assert!(text.contains("return`/webui/_app/`+"));
        assert!(!text.contains("return`/_app/`+"));
        assert!(text.contains("/_app/logo.png"));
        assert!(!text.contains("/webui/_app/logo.png"));
    }

    #[test]
    fn service_worker_route_serves_javascript() {
        let resp = response_for(b"GET /sw.js HTTP/1.1\r\nHost: x\r\n\r\n");
        let text = String::from_utf8(resp).unwrap();
        assert!(text.starts_with("HTTP/1.1 200 OK"));
        assert!(text.contains("zeroclaw-relay-worker-ready"));
        assert!(text.contains("proxyRpc(event)"));
        assert!(text.contains("proxyGatewayApi(event"));
        assert!(text.contains("normalizedApiPath"));
        assert!(text.contains("/api/events"));
        assert!(text.contains("dashboardStatus"));
        assert!(text.contains("publicHealth"));
        assert!(text.contains("config/map-keys"));
        assert!(text.contains("config/reload-status"));
        assert!(text.contains("config/drift"));
        assert!(text.contains("quickstart/state"));
        assert!(text.contains("new MessageChannel()"));
        assert!(text.contains("zeroclaw-rpc-request"));
        assert!(text.contains("Secure browser TLS enrollment is unavailable in this build"));
        assert!(text.contains("ready: true"));
        assert!(text.contains("enrollmentTls: true"));
        assert!(text.contains("mtlsEngine: true"));
    }

    #[test]
    fn tunnel_worker_route_serves_enrollment_connector() {
        let resp = response_for(b"GET /tunnel-worker.js HTTP/1.1\r\nHost: x\r\n\r\n");
        let text = String::from_utf8(resp).unwrap();
        assert!(text.starts_with("HTTP/1.1 200 OK"));
        assert!(text.contains("connectEnrollment"));
        assert!(text.contains("resumeConnection"));
        assert!(text.contains("loadCompletedEnrollment"));
        assert!(text.contains("isCompletedEnrollment"));
        assert!(text.contains("No saved browser certificate found"));
        assert!(text.contains("importScripts('/tls-engine.js')"));
        assert!(text.contains("ensureEnrollmentMaterial"));
        assert!(text.contains("createCertificationRequest"));
        assert!(text.contains("CERTIFICATE REQUEST"));
        assert!(text.contains("zeroclaw.relay.v1"));
        assert!(text.contains("RelayDataTransport"));
        assert!(text.contains("encodeDataFrame"));
        assert!(text.contains("decodeDataFrame"));
        assert!(text.contains("beginBrowserEnrollment"));
        assert!(text.contains("tls-engine-missing"));
        assert!(text.contains("handleRpcRequest"));
        assert!(text.contains("ensureRpcClient"));
        assert!(text.contains("zeroclaw-rpc-request"));
        assert!(text.contains("ZeroClawEnrollmentTls.fetchEnrollmentTrust"));
        assert!(text.contains("ZeroClawEnrollmentTls.enroll"));
        assert!(text.contains("enrollment-sas"));
        assert!(text.contains("pendingEnrollmentPost"));
        assert!(text.contains("route-opening-confirmed-enrollment"));
        assert!(text.contains("openEnrollmentRoute(pending.material)"));
        assert!(text.contains("normalizeConnId(frame.conn_id)"));
        assert!(text.contains("caChainPem: trust.ca_chain_pem"));
        assert!(text.contains("storeCompletedEnrollment"));
        assert!(text.contains("material = { ...material, relayUrl, nodeId, pairingCode }"));
        assert!(text.contains("connectRpcTunnel"));
        assert!(text.contains("self.postMessage({ type: 'rpc-ready', nodeId })"));
        assert!(text.contains("resolveRelayUrl"));
        assert!(text.contains("profile.relayUrl || profile.relayProfile?.relay_url"));
        assert!(text.contains("normalizeRelayWebSocketUrl"));
        assert!(text.contains("new URL(relayUrl, self.location.origin)"));
        assert!(text.contains("singleCertificateFingerprintHex"));
        assert!(
            text.contains("enrollment response must contain exactly one daemon CA certificate")
        );
        assert!(text.contains("ZeroClawEnrollmentTls.connectRpc"));
        assert!(text.contains("caChainPem: profile.caChainPem"));
        assert!(text.contains("openRelayDataRoute"));
        assert!(text.contains("rpc-notification"));
        assert!(
            text.contains("socket.send(JSON.stringify({ t: 'enroll', node_id: nodeId }))"),
            "relay control frames must only carry route metadata, not pairing material"
        );
    }

    #[test]
    fn tls_engine_route_serves_javascript() {
        let resp = response_for(b"GET /tls-engine.js HTTP/1.1\r\nHost: x\r\n\r\n");
        let text = String::from_utf8(resp).unwrap();
        assert!(text.starts_with("HTTP/1.1 200 OK"));
        assert!(text.contains("BrowserTls13Client"));
        assert!(text.contains("TlsWebSocket"));
        assert!(text.contains("JsonRpcClient"));
        assert!(text.contains("TLS_AES_128_GCM_SHA256"));
        assert!(text.contains("ZeroClawEnrollmentTls"));
        assert!(text.contains("connectRpc"));
        assert!(text.contains("fetchEnrollmentTrust"));
        assert!(text.contains("/enroll/ca"));
        assert!(text.contains("confirmed daemon CA is required before browser enrollment POST"));
        assert!(text.contains("verifyServerCertificateVerify"));
        assert!(text.contains("verifyServerCertificateChain"));
        assert!(text.contains("ecdsaDerSignatureToRaw"));
        assert!(text.contains("assertCertificateAuthority"));
        assert!(text.contains("enrolled profile must contain exactly one daemon CA certificate"));
        assert!(text.contains("assertServerCertificate"));
        assert!(text.contains("server certificate is not authorized for server authentication"));
        assert!(text.contains("_internals"));
        assert!(text.contains("onNotification"));
        assert!(text.contains("Unsupported browser RPC request"));
        assert!(text.contains("pairing_code"));
    }
}
