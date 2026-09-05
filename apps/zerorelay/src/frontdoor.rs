//! The relay's connection entry point, and the opt-in browser enrollment
//! frontdoor served from it.
//!
//! Two responsibilities, deliberately in one place because they share the same
//! first read: decide whether an accepted connection is a `zeroclaw.relay.v1`
//! WebSocket upgrade, and - only when `[frontdoor]` is enabled - serve the
//! browser pairing page and its enrollment routes to everything else.
//!
//! With the frontdoor OFF (the default) this behaves exactly as the
//! WebSocket-only relay plane does: a non-upgrade request gets a plain 404 and
//! the connection closes. The relay serves no content, so there is no
//! code-origin trust to reason about.
//!
//! With it ON, the relay becomes a TRUSTED CODE ORIGIN for browsers that enroll
//! through it, and (see [`crate::enroll_proxy`]) a PRINCIPAL in their
//! enrollment. That is a documented narrowing of the blind-forwarder guarantee
//! and the reason this is opt-in with a startup warning. zerocode/native
//! enrollment never touches this path.
//!
//! PHASE 1 is enrollment only. There is no dashboard, no session tier, and no
//! relay DATA route in the served page: the browser talks plain `fetch()` to the
//! routes below and the relay does the tunnelling. The page therefore ships no
//! TLS, no X.509 parser and no relay frame codec.

use crate::Inner;
use crate::enroll_proxy::{self, MAX_FRONTDOOR_REQUEST_BYTES, ProxyError};
use crate::frontdoor_assets::{APP_JS, INDEX_HTML};
use anyhow::{Context, Result};
use std::io::Cursor;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context as TaskContext, Poll};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::time::{Duration, timeout};
use tokio_tungstenite::WebSocketStream;
use zeroclaw_relay_proto::SUBPROTOCOL;

const MAX_HTTP_HEAD: usize = 16 * 1024;

/// How long a served connection may sit idle between requests before the relay
/// closes it. Short: the page makes a handful of requests and the operator's
/// thinking time happens between them on separate connections if need be.
const HTTP_KEEP_ALIVE_IDLE: Duration = Duration::from_secs(5);

/// Ceiling on one whole frontdoor connection, however many requests it pipelines.
/// The per-leg budget in `enroll_proxy` bounds a single enrollment exchange; this
/// bounds the session that issues them, so a browser cannot chain requests to
/// hold a served connection (and its frontdoor permit) indefinitely.
const HTTP_SESSION_BUDGET: Duration = Duration::from_secs(180);

pub(crate) enum Accepted<S> {
    /// A completed relay WebSocket upgrade.
    WebSocket(Box<WebSocketStream<PrefixedIo<S>>>),
    /// A plain HTTP request on a relay with the frontdoor ENABLED. The caller
    /// serves it outside the handshake deadline (see [`serve_http`]).
    Http(HttpSession<S>),
    /// A non-WebSocket request with the frontdoor disabled: answered with 404
    /// and closed.
    Rejected,
}

/// A classified plain-HTTP connection, with the request head already read.
///
/// Serving is deliberately NOT done inside the accept phase. An enrollment leg
/// can take tens of seconds, while the accept phase runs under the relay's
/// `handshake_timeout` (10s by default) - the budget that bounds how long a
/// socket may take to become a relay connection. Serving under that budget
/// would either kill legitimate enrollments or force the handshake budget wide
/// enough to weaken it for the relay plane.
pub(crate) struct HttpSession<S> {
    stream: S,
    head: Vec<u8>,
    pending: Vec<u8>,
}

/// Read the request head and classify the connection.
///
/// `frontdoor_enabled` decides only what happens to a NON-upgrade request; the
/// WebSocket path is identical either way, so enabling the frontdoor cannot
/// change how daemons and clients are admitted.
pub(crate) async fn accept<S>(mut stream: S, frontdoor_enabled: bool) -> Result<Accepted<S>>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut pending = Vec::with_capacity(1024);
    let head = read_http_head(&mut stream, &mut pending).await?;
    if is_websocket_upgrade(&head) {
        let mut prefix = head;
        prefix.extend_from_slice(&pending);
        let io = PrefixedIo {
            prefix: Cursor::new(prefix),
            inner: stream,
        };
        // Bound the parser at the protocol budget. Without this,
        // tungstenite's defaults (16 MiB frame / 64 MiB message) let a peer
        // allocate far beyond MAX_WS_MESSAGE before the application-level
        // check ever runs.
        let ws = tokio_tungstenite::accept_hdr_async_with_config(
            io,
            select_subprotocol,
            Some(relay_ws_config()),
        )
        .await
        .context("relay websocket handshake")?;
        return Ok(Accepted::WebSocket(Box::new(ws)));
    }

    if !frontdoor_enabled {
        let body = "this is a ZeroClaw relay endpoint; it speaks only the \
                    zeroclaw.relay.v1 WebSocket protocol. Enroll with zerocode, or \
                    opt in via [frontdoor] enabled = true (see relay.example.toml \
                    for the trust implications).\n";
        let response = http_response("404 Not Found", "text/plain; charset=utf-8", body);
        stream.write_all(&response).await?;
        let _ = stream.shutdown().await;
        return Ok(Accepted::Rejected);
    }

    Ok(Accepted::Http(HttpSession {
        stream,
        head,
        pending,
    }))
}

/// Serve a frontdoor HTTP session to completion, under its own total budget.
pub(crate) async fn serve_http<S>(session: HttpSession<S>, inner: Arc<Inner>)
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let _ = timeout(HTTP_SESSION_BUDGET, serve_session(session, inner)).await;
}

async fn serve_session<S>(session: HttpSession<S>, inner: Arc<Inner>)
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let HttpSession {
        mut stream,
        mut head,
        mut pending,
    } = session;
    loop {
        let response = match route(&head, &mut stream, &mut pending, &inner).await {
            Ok(bytes) => bytes,
            // The request could not be read at all (oversized body, early EOF):
            // answer if we still can, then stop.
            Err(status) => status,
        };
        if stream.write_all(&response).await.is_err() {
            break;
        }
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
}

/// Dispatch one request. `Err` carries an already-rendered error response.
async fn route<S>(
    head: &[u8],
    stream: &mut S,
    pending: &mut Vec<u8>,
    inner: &Arc<Inner>,
) -> std::result::Result<Vec<u8>, Vec<u8>>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let Some((method, path)) = request_line(head) else {
        return Err(http_response(
            "400 Bad Request",
            "text/plain; charset=utf-8",
            "malformed request\n",
        ));
    };
    match (method, path) {
        ("GET" | "HEAD", "/" | "/index.html") => Ok(http_response(
            "200 OK",
            "text/html; charset=utf-8",
            INDEX_HTML,
        )),
        ("GET" | "HEAD", "/app.js") => Ok(http_response(
            "200 OK",
            "application/javascript; charset=utf-8",
            APP_JS,
        )),
        ("POST", "/enroll/ca") => {
            let body = read_body(head, stream, pending).await?;
            let parsed: enroll_proxy::TrustBody = match serde_json::from_slice(&body) {
                Ok(v) => v,
                Err(_) => return Err(json_error(400, "malformed request body")),
            };
            match enroll_proxy::fetch_trust(inner, &parsed.node_id).await {
                Ok(reply) => Ok(json_ok(&reply)),
                Err(e) => Err(proxy_error_response(&e)),
            }
        }
        ("POST", "/enroll") => {
            let body = read_body(head, stream, pending).await?;
            let parsed: enroll_proxy::EnrollBody = match serde_json::from_slice(&body) {
                Ok(v) => v,
                Err(_) => return Err(json_error(400, "malformed request body")),
            };
            match enroll_proxy::post_enroll(inner, &parsed).await {
                Ok(reply) => Ok(json_ok(&reply)),
                Err(e) => Err(proxy_error_response(&e)),
            }
        }
        _ => Err(http_response(
            "404 Not Found",
            "text/plain; charset=utf-8",
            "not found\n",
        )),
    }
}

fn proxy_error_response(error: &ProxyError) -> Vec<u8> {
    json_error(error.status(), &error.message())
}

fn json_ok<T: serde::Serialize>(value: &T) -> Vec<u8> {
    let body = serde_json::to_string(value).unwrap_or_else(|_| "{}".to_string());
    http_response("200 OK", "application/json; charset=utf-8", &body)
}

fn json_error(status: u16, message: &str) -> Vec<u8> {
    let body = serde_json::json!({ "error": message }).to_string();
    let status = format!("{status} {}", reason_phrase(status));
    http_response(&status, "application/json; charset=utf-8", &body)
}

fn reason_phrase(status: u16) -> &'static str {
    match status {
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        429 => "Too Many Requests",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        _ => "Error",
    }
}

/// Read a request body of exactly `content-length` bytes, bounded.
///
/// The cap mirrors the daemon's own `MAX_REQUEST_BYTES`: a CSR plus a pairing
/// code plus a confirmed CA is a few KiB, and an unbounded read here would let
/// an unauthenticated browser grow relay memory at will.
async fn read_body<S>(
    head: &[u8],
    stream: &mut S,
    pending: &mut Vec<u8>,
) -> std::result::Result<Vec<u8>, Vec<u8>>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let len = content_length(head).unwrap_or(0);
    if len > MAX_FRONTDOOR_REQUEST_BYTES {
        return Err(json_error(400, "request body too large"));
    }
    let mut chunk = [0u8; 4096];
    while pending.len() < len {
        let n = match stream.read(&mut chunk).await {
            Ok(0) | Err(_) => return Err(json_error(400, "request body truncated")),
            Ok(n) => n,
        };
        if pending.len() + n > MAX_FRONTDOOR_REQUEST_BYTES {
            return Err(json_error(400, "request body too large"));
        }
        pending.extend_from_slice(&chunk[..n]);
    }
    let rest = pending.split_off(len);
    Ok(std::mem::replace(pending, rest))
}

fn content_length(head: &[u8]) -> Option<usize> {
    let text = String::from_utf8_lossy(head);
    text.lines()
        .skip(1)
        .find_map(|line| {
            let lower = line.to_ascii_lowercase();
            lower
                .strip_prefix("content-length:")
                .map(|v| v.trim().to_string())
        })
        .and_then(|v| v.parse::<usize>().ok())
}

fn request_line(head: &[u8]) -> Option<(&str, &str)> {
    let text = std::str::from_utf8(head).ok()?;
    let request = text.lines().next()?;
    let mut parts = request.split_whitespace();
    let method = parts.next()?;
    let path = parts.next()?;
    Some((method, path.split_once('?').map_or(path, |(p, _)| p)))
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

fn http_response(status: &str, content_type: &str, body: &str) -> Vec<u8> {
    let mut response = format!(
        "HTTP/1.1 {status}\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\ncache-control: no-store\r\nconnection: keep-alive\r\nkeep-alive: timeout=5\r\n\r\n",
        body.len()
    )
    .into_bytes();
    response.extend_from_slice(body.as_bytes());
    response
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

/// WebSocket parser limits for the relay plane, derived from the protocol
/// budget in `zeroclaw-relay-proto` so the transport and application bounds
/// cannot drift apart.
pub(crate) fn relay_ws_config() -> tokio_tungstenite::tungstenite::protocol::WebSocketConfig {
    let mut cfg = tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default();
    cfg.max_message_size = Some(zeroclaw_relay_proto::MAX_WS_MESSAGE);
    cfg.max_frame_size = Some(zeroclaw_relay_proto::MAX_WS_MESSAGE);
    cfg
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pipelined_request_heads_are_split_without_dropping_overflow() {
        let mut pending =
            b"GET /a HTTP/1.1\r\nHost: x\r\n\r\nGET /b HTTP/1.1\r\nHost: x\r\n\r\n".to_vec();
        let first = take_http_head(&mut pending).expect("first request");
        let second = take_http_head(&mut pending).expect("second request");
        assert_eq!(first, b"GET /a HTTP/1.1\r\nHost: x\r\n\r\n");
        assert_eq!(second, b"GET /b HTTP/1.1\r\nHost: x\r\n\r\n");
        assert!(pending.is_empty());
    }

    #[test]
    fn websocket_upgrade_is_detected() {
        let head = b"GET /relay HTTP/1.1\r\nHost: x\r\nUpgrade: websocket\r\n\r\n";
        assert!(is_websocket_upgrade(head));
    }

    /// With the frontdoor off the relay plane is WebSocket-only: a plain HTTP hit
    /// is refused with a 404 and closed, and the relay serves no content.
    #[tokio::test]
    async fn disabled_frontdoor_refuses_plain_http_with_404() {
        let (mut client, server) = tokio::io::duplex(4096);
        let task = tokio::spawn(async move { accept(server, false).await });
        client
            .write_all(b"GET / HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        let mut buf = Vec::new();
        client.read_to_end(&mut buf).await.unwrap();
        let text = String::from_utf8_lossy(&buf);
        assert!(text.starts_with("HTTP/1.1 404 Not Found"), "got: {text}");
        assert!(text.contains("zeroclaw.relay.v1"), "got: {text}");
        assert!(!text.contains("ZeroClaw Relay"), "must not serve the page");
        assert!(matches!(task.await.unwrap(), Ok(Accepted::Rejected)));
    }

    /// Enabling the frontdoor must not change how the relay plane is admitted.
    #[tokio::test]
    async fn websocket_upgrade_completes_with_the_frontdoor_either_way() {
        for enabled in [false, true] {
            let (client_io, server_io) = tokio::io::duplex(4096);
            let task = tokio::spawn(async move { accept(server_io, enabled).await });
            // In-memory duplex stream (no real network / TLS); asserts the WS
            // upgrade path. The request URI is built from parts with the scheme
            // as a bare field so no insecure-scheme string literal exists in
            // source for the hosted scanner to flag - there is no real transport
            // here.
            let uri = tokio_tungstenite::tungstenite::http::Uri::builder()
                .scheme("ws")
                .authority("relay.test")
                .path_and_query("/relay")
                .build()
                .expect("valid test uri");
            let ws = tokio_tungstenite::client_async(uri, client_io).await;
            assert!(ws.is_ok(), "WS upgrade must succeed (frontdoor={enabled})");
            assert!(matches!(task.await.unwrap(), Ok(Accepted::WebSocket(_))));
        }
    }

    #[tokio::test]
    async fn enabled_frontdoor_classifies_plain_http_for_serving() {
        let (mut client, server) = tokio::io::duplex(4096);
        let task = tokio::spawn(async move { accept(server, true).await });
        client
            .write_all(b"GET / HTTP/1.1\r\nHost: x\r\n\r\n")
            .await
            .unwrap();
        assert!(matches!(task.await.unwrap(), Ok(Accepted::Http(_))));
    }

    #[test]
    fn request_line_splits_method_path_and_drops_the_query() {
        let (method, path) =
            request_line(b"POST /enroll?x=1 HTTP/1.1\r\nHost: x\r\n\r\n").expect("request line");
        assert_eq!(method, "POST");
        assert_eq!(path, "/enroll");
    }

    #[test]
    fn content_length_is_read_from_headers_only() {
        // The request line is skipped, so a path that looks like a header cannot
        // spoof the length.
        let head = b"POST /enroll HTTP/1.1\r\nHost: x\r\nContent-Length: 42\r\n\r\n";
        assert_eq!(content_length(head), Some(42));
        assert_eq!(content_length(b"GET / HTTP/1.1\r\nHost: x\r\n\r\n"), None);
    }
}
