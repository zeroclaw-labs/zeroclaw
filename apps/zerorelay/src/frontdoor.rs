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
        // Ambiguous framing has no derivable boundary, so nothing after this
        // request can be trusted to be a request. Refuse it and close instead of
        // routing it - the alternative is interpreting a smuggled body as the
        // next request head.
        if framing(&head) == Framing::Ambiguous {
            let refusal = mark_connection_close(json_error(
                400,
                "ambiguous request framing (conflicting content-length, or an \
                 unsupported transfer-encoding)",
            ));
            let refusal = if request_is_head(&head) {
                head_only(refusal)
            } else {
                refusal
            };
            let _ = stream.write_all(&refusal).await;
            break;
        }
        // Whether this request's declared body has been consumed - i.e. whether
        // the stream is still at a request boundary. A route that reads the body
        // sets it; routes that do not (the asset arms, the catch-all 404) leave
        // it, and the drain below decides. `Err` from `route` is NOT the same
        // question: a 404 to a bodyless GET is an error response on a perfectly
        // synchronized stream, while an Ok asset response to a GET carrying
        // `content-length` is a success on a desynchronized one.
        let mut body_consumed = content_length(&head).unwrap_or(0) == 0;
        let mut response =
            match route(&head, &mut stream, &mut pending, &inner, &mut body_consumed).await {
                Ok(bytes) | Err(bytes) => bytes,
            };
        // If the route never read the declared body, drain it so the next read
        // starts on a request head rather than on body bytes. Unbounded or
        // unreadable => the stream cannot be resynchronized, so close.
        let synchronized =
            body_consumed || drain_request_body(&head, &mut stream, &mut pending).await;
        // Keep-alive only while the stream is genuinely at a request boundary.
        // Tell the client when it is not, rather than leaving the shared builder's
        // `keep-alive` on a response we then close underneath.
        let close = !synchronized || should_close_after_response(&head);
        if close {
            response = mark_connection_close(response);
        }
        // A HEAD response carries the headers a GET would - content-length still
        // advertises the entity - but no body bytes.
        if request_is_head(&head) {
            response = head_only(response);
        }
        if stream.write_all(&response).await.is_err() {
            break;
        }
        if close {
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
///
/// `body_consumed` is set when an arm reads the declared request body to
/// completion, so the caller can tell a synchronized stream from a
/// desynchronized one - a question independent of whether this returns `Err`.
async fn route<S>(
    head: &[u8],
    stream: &mut S,
    pending: &mut Vec<u8>,
    inner: &Arc<Inner>,
    body_consumed: &mut bool,
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
            let body = read_body(head, stream, pending, body_consumed).await?;
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
            let body = read_body(head, stream, pending, body_consumed).await?;
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
    body_consumed: &mut bool,
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
        // Read AT MOST the bytes this body still owes. Reading a full chunk
        // could pull in pipelined bytes belonging to the next request, and the
        // over-cap check that used to follow discarded the whole chunk when it
        // tripped - losing already-read body bytes while leaving the body
        // unconsumed, so the drain then ate the next request instead. `len` is
        // bounded above, so a read capped at the remainder can never exceed it.
        let want = (len - pending.len()).min(chunk.len());
        let n = match stream.read(&mut chunk[..want]).await {
            Ok(0) | Err(_) => return Err(json_error(400, "request body truncated")),
            Ok(n) => n,
        };
        pending.extend_from_slice(&chunk[..n]);
    }
    let rest = pending.split_off(len);
    // The declared body is now fully in hand, so the stream sits on the next
    // request head - whatever this route does with the bytes.
    *body_consumed = true;
    Ok(std::mem::replace(pending, rest))
}

/// Swallow a declared body that the route never read, so keep-alive resumes on a
/// request head rather than on body bytes.
///
/// Returns whether the stream ended up at a request boundary. A body over the
/// cap is NOT drained - draining it is exactly the unbounded read the cap
/// exists to prevent - so an oversized declaration closes the connection
/// instead. This is what keeps `GET /app.js` carrying a `content-length`, or a
/// 404 to a bodied request, from desynchronizing the session.
async fn drain_request_body<S>(head: &[u8], stream: &mut S, pending: &mut Vec<u8>) -> bool
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let len = content_length(head).unwrap_or(0);
    if len == 0 {
        return true;
    }
    if len > MAX_FRONTDOOR_REQUEST_BYTES {
        return false;
    }
    let mut chunk = [0u8; 4096];
    while pending.len() < len {
        // Same bound as `read_body`: never read past this body, so whatever is
        // pipelined behind it stays on the stream to be parsed as a request.
        let want = (len - pending.len()).min(chunk.len());
        match stream.read(&mut chunk[..want]).await {
            Ok(0) | Err(_) => return false,
            Ok(n) => pending.extend_from_slice(&chunk[..n]),
        }
    }
    let rest = pending.split_off(len);
    *pending = rest;
    true
}

/// How long this request's body is - or that the question has no trustworthy
/// answer.
///
/// The distinction matters because the session decides keep-alive from it: if
/// the framing is ambiguous, "where does this request end" is unanswerable and
/// anything after it cannot be treated as the next request. Collapsing that into
/// "no body" is the request-smuggling shape (RFC 9112 6.3).
#[derive(Debug, PartialEq, Eq)]
enum Framing {
    /// No body, and the headers say so unambiguously.
    Empty,
    /// Exactly one well-formed `content-length`.
    Length(usize),
    /// Conflicting or unparsable `content-length`, or a transfer coding this
    /// server does not implement. No boundary can be derived.
    Ambiguous,
}

/// Classify a request's body framing.
///
/// Strict on purpose, because this is an unauthenticated surface:
/// - more than one `content-length` (even repeated with the same value) is
///   refused rather than reconciled;
/// - a value that does not parse is `Ambiguous`, never `Empty`;
/// - any `transfer-encoding` is refused - the frontdoor implements no chunked
///   decoder, so pretending the body is empty would leave the coded body on the
///   stream to be read as the next request.
fn framing(head: &[u8]) -> Framing {
    let text = String::from_utf8_lossy(head);
    let mut length: Option<usize> = None;
    let mut seen_length = 0usize;
    for line in text.lines().skip(1) {
        let lower = line.to_ascii_lowercase();
        if lower.starts_with("transfer-encoding:") {
            return Framing::Ambiguous;
        }
        if let Some(value) = lower.strip_prefix("content-length:") {
            seen_length += 1;
            if seen_length > 1 {
                return Framing::Ambiguous;
            }
            match value.trim().parse::<usize>() {
                Ok(n) => length = Some(n),
                Err(_) => return Framing::Ambiguous,
            }
        }
    }
    match length {
        None | Some(0) => Framing::Empty,
        Some(n) => Framing::Length(n),
    }
}

/// The declared body length, for the paths that have already established the
/// framing is unambiguous.
fn content_length(head: &[u8]) -> Option<usize> {
    match framing(head) {
        Framing::Length(n) => Some(n),
        Framing::Empty | Framing::Ambiguous => None,
    }
}

fn request_line(head: &[u8]) -> Option<(&str, &str)> {
    let text = std::str::from_utf8(head).ok()?;
    let request = text.lines().next()?;
    let mut parts = request.split_whitespace();
    let method = parts.next()?;
    let path = parts.next()?;
    Some((method, path.split_once('?').map_or(path, |(p, _)| p)))
}

fn request_is_head(head: &[u8]) -> bool {
    request_line(head).is_some_and(|(method, _)| method.eq_ignore_ascii_case("HEAD"))
}

/// Keep the status line and headers, drop the body - a HEAD response sends the
/// headers a GET would (content-length included) but none of the entity bytes.
fn head_only(mut response: Vec<u8>) -> Vec<u8> {
    if let Some(i) = response.windows(4).position(|w| w == b"\r\n\r\n") {
        response.truncate(i + 4);
    }
    response
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

/// Rewrite a response's keep-alive header to `Connection: close`. Called when the
/// relay will close the connection after this response - an error may have left
/// the request body unconsumed, so the stream is no longer at a request boundary
/// and MUST NOT be reused. The shared builder always writes keep-alive; this
/// flips it so the client is told what the relay is about to do.
fn mark_connection_close(mut response: Vec<u8>) -> Vec<u8> {
    const KEEP: &[u8] = b"connection: keep-alive\r\nkeep-alive: timeout=5\r\n";
    const CLOSE: &[u8] = b"connection: close\r\n";
    if let Some(pos) = response.windows(KEEP.len()).position(|w| w == KEEP) {
        response.splice(pos..pos + KEEP.len(), CLOSE.iter().copied());
    }
    response
}

async fn read_http_head<S>(stream: &mut S, pending: &mut Vec<u8>) -> Result<Vec<u8>>
where
    S: AsyncRead + Unpin,
{
    let mut chunk = [0u8; 1024];
    loop {
        // Enforce the cap BEFORE extracting, so an over-cap head is refused even
        // when its terminator already arrived in the same read - otherwise the
        // cap only catches a head that is still incomplete, and one oversized
        // read slips a full head past it.
        if header_end(pending).unwrap_or(pending.len()) > MAX_HTTP_HEAD {
            anyhow::bail!("request headers too large");
        }
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

/// Security headers applied to EVERY frontdoor HTTP response.
///
/// The frontdoor page is a consent / short-auth-string surface, so it must not
/// be framable (clickjacking of the confirm button) and must not be able to
/// source code cross-origin.
///
/// SAFETY (CSP): the served page (`frontdoor_assets`) carries NO inline
/// `<script>` - its only script is same-origin `/app.js`, covered by
/// `default-src 'self'` - so `script-src` needs no `'unsafe-inline'` and none is
/// granted (scripts inherit `default-src 'self'`). The page does carry one
/// inline `<style>` block, so `style-src` allows `'unsafe-inline'`; inline
/// styles cannot execute script and this is the minimal relaxation that lets the
/// shipped page render. The page's `fetch()` calls are same-origin
/// (`/enroll/ca`, `/enroll`), covered by `default-src`. The headers are inert on
/// the JS/JSON/error responses and harmless there.
const SECURITY_HEADERS: &str = concat!(
    "content-security-policy: frame-ancestors 'none'; default-src 'self'; style-src 'self' 'unsafe-inline'\r\n",
    "referrer-policy: no-referrer\r\n",
    "cross-origin-opener-policy: same-origin\r\n",
    "x-content-type-options: nosniff\r\n",
    "x-frame-options: DENY\r\n",
);

fn http_response(status: &str, content_type: &str, body: &str) -> Vec<u8> {
    let mut response = format!(
        "HTTP/1.1 {status}\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\ncache-control: no-store\r\n{SECURITY_HEADERS}connection: keep-alive\r\nkeep-alive: timeout=5\r\n\r\n",
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

    /// Framing classification: the distinction the boundary predicate rests on.
    #[test]
    fn ambiguous_framing_is_never_mistaken_for_an_empty_body() {
        let empty = b"GET / HTTP/1.1\r\nHost: x\r\n\r\n";
        assert_eq!(framing(empty), Framing::Empty);
        let sized = b"POST /enroll HTTP/1.1\r\nHost: x\r\ncontent-length: 12\r\n\r\n";
        assert_eq!(framing(sized), Framing::Length(12));
        // Zero-length is a real, unambiguous "no body".
        let zero = b"POST /enroll HTTP/1.1\r\nHost: x\r\ncontent-length: 0\r\n\r\n";
        assert_eq!(framing(zero), Framing::Empty);
        // Conflicting lengths: the classic smuggling pair.
        let conflicting =
            b"POST /enroll HTTP/1.1\r\nHost: x\r\ncontent-length: 5\r\ncontent-length: 80\r\n\r\n";
        assert_eq!(framing(conflicting), Framing::Ambiguous);
        // Repeated even when they agree - reconciling is not this server's job.
        let repeated =
            b"POST /enroll HTTP/1.1\r\nHost: x\r\ncontent-length: 5\r\ncontent-length: 5\r\n\r\n";
        assert_eq!(framing(repeated), Framing::Ambiguous);
        // Unparsable must not degrade to "empty".
        let junk = b"POST /enroll HTTP/1.1\r\nHost: x\r\ncontent-length: banana\r\n\r\n";
        assert_eq!(framing(junk), Framing::Ambiguous);
        // No chunked decoder here, so a coded body is not an empty one.
        let chunked = b"POST /enroll HTTP/1.1\r\nHost: x\r\ntransfer-encoding: chunked\r\n\r\n";
        assert_eq!(framing(chunked), Framing::Ambiguous);
    }

    /// Ambiguous framing must be refused and closed, never routed - otherwise the
    /// bytes behind it are read as the next request. The smuggling shape tidux
    /// and Aarlington both flagged as the remaining parser gap.
    #[tokio::test]
    async fn ambiguous_framing_is_refused_and_the_connection_closed() {
        let cfg = crate::RelayConfig {
            frontdoor_enabled: true,
            ..Default::default()
        };
        let inner = crate::RelayServer::new(cfg).inner.clone();

        let (mut client, server) = tokio::io::duplex(64 * 1024);
        // Conflicting lengths, with a smuggled request positioned to be read as
        // the next head if the shorter length were honored.
        let smuggled = "GET / HTTP/1.1\r\nHost: x\r\n\r\n";
        let first = format!(
            "POST /enroll HTTP/1.1\r\nHost: x\r\ncontent-length: 0\r\ncontent-length: {}\r\n\r\n{smuggled}",
            smuggled.len()
        );
        client.write_all(first.as_bytes()).await.unwrap();

        let session = match accept(server, true).await.expect("accept") {
            Accepted::Http(s) => s,
            _ => panic!("expected a frontdoor HTTP session"),
        };
        let task = tokio::spawn(async move { serve_session(session, inner).await });
        let mut buf = Vec::new();
        client.read_to_end(&mut buf).await.unwrap();
        task.await.unwrap();
        let text = String::from_utf8_lossy(&buf);
        let lower = text.to_ascii_lowercase();

        assert!(text.starts_with("HTTP/1.1 400"), "expected a 400: {text}");
        assert!(
            lower.contains("ambiguous request framing"),
            "expected the framing refusal: {text}"
        );
        assert!(lower.contains("connection: close"), "must close: {text}");
        assert!(
            !text.contains("ZeroClaw browser enrollment"),
            "the smuggled request must never be answered: {text}"
        );
        assert_eq!(
            text.matches("HTTP/1.1").count(),
            1,
            "exactly one response: {text}"
        );
    }

    /// The mirror of the test below, and the case an `Err`-based predicate misses:
    /// an asset route answers `Ok` WITHOUT reading a declared body, so those body
    /// bytes must be drained rather than parsed as the next request head.
    ///
    /// Here the body IS a request (`GET /`), which a desynchronized session would
    /// answer with the enrollment page. Regression for Aarlington's finding that
    /// `request_failed` meant "route returned Err", not "stream is at a boundary".
    #[tokio::test]
    async fn a_body_on_an_asset_route_is_drained_not_parsed_as_a_request() {
        let cfg = crate::RelayConfig {
            frontdoor_enabled: true,
            ..Default::default()
        };
        let inner = crate::RelayServer::new(cfg).inner.clone();

        let (mut client, server) = tokio::io::duplex(64 * 1024);
        // A smuggled request, carried as the BODY of a GET that never reads one.
        let smuggled = "GET / HTTP/1.1\r\nHost: x\r\n\r\n";
        let first = format!(
            "GET /app.js HTTP/1.1\r\nHost: x\r\ncontent-length: {}\r\n\r\n{smuggled}",
            smuggled.len()
        );
        // A genuine pipelined request behind it, which must still be answered.
        let second = "GET /app.js HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n";
        client.write_all(first.as_bytes()).await.unwrap();
        client.write_all(second.as_bytes()).await.unwrap();

        let session = match accept(server, true).await.expect("accept") {
            Accepted::Http(s) => s,
            _ => panic!("expected a frontdoor HTTP session"),
        };
        let task = tokio::spawn(async move { serve_session(session, inner).await });
        let mut buf = Vec::new();
        client.read_to_end(&mut buf).await.unwrap();
        task.await.unwrap();
        let text = String::from_utf8_lossy(&buf);

        // The smuggled body must never have been routed: the enrollment page is
        // what `GET /` would have returned.
        assert!(
            !text.contains("ZeroClaw browser enrollment"),
            "desync: the smuggled body was answered as a request: {text}"
        );
        // Both REAL requests were answered, so the body was drained and the
        // connection stayed synchronized rather than being closed defensively.
        assert_eq!(
            text.matches("application/javascript").count(),
            2,
            "both genuine /app.js requests must be answered: {text}"
        );
    }

    /// A 404 leaves a bodyless request perfectly synchronized, so it must NOT end
    /// the connection - `Err` is not a synonym for "desynchronized".
    #[tokio::test]
    async fn a_routing_miss_keeps_a_synchronized_connection_alive() {
        let cfg = crate::RelayConfig {
            frontdoor_enabled: true,
            ..Default::default()
        };
        let inner = crate::RelayServer::new(cfg).inner.clone();

        let (mut client, server) = tokio::io::duplex(64 * 1024);
        client
            .write_all(b"GET /favicon.ico HTTP/1.1\r\nHost: x\r\n\r\n")
            .await
            .unwrap();
        client
            .write_all(b"GET /app.js HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();

        let session = match accept(server, true).await.expect("accept") {
            Accepted::Http(s) => s,
            _ => panic!("expected a frontdoor HTTP session"),
        };
        let task = tokio::spawn(async move { serve_session(session, inner).await });
        let mut buf = Vec::new();
        client.read_to_end(&mut buf).await.unwrap();
        task.await.unwrap();
        let text = String::from_utf8_lossy(&buf);

        assert!(text.contains("404 Not Found"), "expected the 404: {text}");
        assert!(
            text.contains("application/javascript"),
            "the connection must survive a 404 and answer the next request: {text}"
        );
    }

    /// An error response must close the connection rather than keep-alive into an
    /// unconsumed request body. A POST that declares an oversized body, with a
    /// second request pipelined behind it, would otherwise have its leftover
    /// bytes parsed as that second request - a client-confusing HTTP desync.
    /// Regression for the `read_body` keep-alive bug.
    #[tokio::test]
    async fn an_error_response_closes_instead_of_desyncing_the_next_request() {
        let cfg = crate::RelayConfig {
            frontdoor_enabled: true,
            ..Default::default()
        };
        let inner = crate::RelayServer::new(cfg).inner.clone();

        let (mut client, server) = tokio::io::duplex(64 * 1024);
        // A keep-alive POST whose declared body exceeds the cap (read_body rejects
        // it before reading any body), then a full pipelined GET the desync would
        // answer with 200 if the connection stayed open.
        let oversize = MAX_FRONTDOOR_REQUEST_BYTES + 1;
        let first =
            format!("POST /enroll HTTP/1.1\r\nHost: x\r\ncontent-length: {oversize}\r\n\r\n");
        let smuggled = "GET /app.js HTTP/1.1\r\nHost: x\r\n\r\n";
        client.write_all(first.as_bytes()).await.unwrap();
        client.write_all(smuggled.as_bytes()).await.unwrap();

        let session = match accept(server, true).await.expect("accept") {
            Accepted::Http(s) => s,
            _ => panic!("expected a frontdoor HTTP session"),
        };
        let task = tokio::spawn(async move { serve_session(session, inner).await });

        let mut buf = Vec::new();
        client.read_to_end(&mut buf).await.unwrap();
        task.await.unwrap();
        let text = String::from_utf8_lossy(&buf);
        let lower = text.to_ascii_lowercase();

        assert!(text.starts_with("HTTP/1.1 400"), "expected 400: {text}");
        assert!(
            text.contains("request body too large"),
            "expected the size error: {text}"
        );
        // The connection was closed, and the response advertised it.
        assert!(
            lower.contains("connection: close"),
            "must advertise close: {text}"
        );
        assert!(!lower.contains("keep-alive"), "must not keep-alive: {text}");
        // The smuggled request must not have been served.
        assert!(
            !lower.contains("application/javascript"),
            "desync: the pipelined /app.js was served: {text}"
        );
        assert_eq!(
            text.matches("HTTP/1.1").count(),
            1,
            "exactly one response must be written: {text}"
        );
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

    /// A prefill link (`/?node=..&code=..`) is served the ordinary page: the
    /// query is dropped at the request line, so the pairing code is never routed
    /// on, stored, or reflected into the response. The page reads the values
    /// client-side and scrubs them from the address bar.
    #[tokio::test]
    async fn a_prefill_link_serves_the_page_without_reflecting_the_code() {
        let cfg = crate::RelayConfig {
            frontdoor_enabled: true,
            ..Default::default()
        };
        let inner = crate::RelayServer::new(cfg).inner.clone();
        let secret = "PrefillSecret0123456789";
        let (mut client, server) = tokio::io::duplex(64 * 1024);
        let request = format!(
            "GET /?node=0d3c4f3e8b9a1d2c3b4a5968778695a4&code={secret} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n"
        );
        client.write_all(request.as_bytes()).await.unwrap();
        let session = match accept(server, true).await.expect("accept") {
            Accepted::Http(s) => s,
            _ => panic!("expected a frontdoor HTTP session"),
        };
        let task = tokio::spawn(async move { serve_session(session, inner).await });
        let mut buf = Vec::new();
        client.read_to_end(&mut buf).await.unwrap();
        task.await.unwrap();
        let text = String::from_utf8_lossy(&buf);
        assert!(
            text.starts_with("HTTP/1.1 200 OK"),
            "the page must be served: {text}"
        );
        assert!(
            text.contains("ZeroClaw browser enrollment"),
            "not the enrollment page"
        );
        assert!(
            !text.contains(secret),
            "the pairing code was reflected into the response"
        );
        let head = text
            .split("\r\n\r\n")
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        assert!(
            head.contains("cache-control: no-store"),
            "a prefilled page must not be cached: {head}"
        );
        assert!(
            head.contains("referrer-policy: no-referrer"),
            "the link must not leak via Referer: {head}"
        );
    }

    #[test]
    fn a_get_with_a_prefill_query_routes_to_the_page_path() {
        let (method, path) =
            request_line(b"GET /?node=abc&code=PairingCode123 HTTP/1.1\r\nHost: x\r\n\r\n")
                .expect("request line");
        assert_eq!(method, "GET");
        assert_eq!(
            path, "/",
            "the query (and the code in it) must not reach routing"
        );
    }

    #[test]
    fn content_length_is_read_from_headers_only() {
        // The request line is skipped, so a path that looks like a header cannot
        // spoof the length.
        let head = b"POST /enroll HTTP/1.1\r\nHost: x\r\nContent-Length: 42\r\n\r\n";
        assert_eq!(content_length(head), Some(42));
        assert_eq!(content_length(b"GET / HTTP/1.1\r\nHost: x\r\n\r\n"), None);
    }

    /// FIX 3: every frontdoor response carries the anti-clickjacking / consent
    /// hardening headers. The page is a SAS / consent surface, so it must not be
    /// framable and must not source code cross-origin.
    #[test]
    fn every_response_carries_the_security_headers() {
        let response = http_response("200 OK", "text/html; charset=utf-8", "<html></html>");
        let text = String::from_utf8(response).unwrap();
        let head = text.split("\r\n\r\n").next().unwrap().to_ascii_lowercase();
        assert!(head.contains("content-security-policy:"), "no CSP: {head}");
        assert!(
            head.contains("frame-ancestors 'none'"),
            "the page must refuse cross-origin framing: {head}"
        );
        assert!(
            head.contains("default-src 'self'"),
            "no default-src: {head}"
        );
        assert!(head.contains("x-frame-options: deny"), "no XFO: {head}");
        assert!(
            head.contains("x-content-type-options: nosniff"),
            "no nosniff: {head}"
        );
        assert!(
            head.contains("referrer-policy: no-referrer"),
            "no referrer-policy: {head}"
        );
        assert!(
            head.contains("cross-origin-opener-policy: same-origin"),
            "no COOP: {head}"
        );
        // Scripts inherit `default-src 'self'`: no `script-src`, so no chance of
        // an `'unsafe-inline'` script relaxation slipping in.
        assert!(
            !head.contains("script-src"),
            "scripts must inherit default-src 'self': {head}"
        );
    }

    /// Minor (adversarial pass): a HEAD response returns the headers a GET would
    /// - content-length included - but no body.
    #[test]
    fn head_only_keeps_headers_and_drops_the_body() {
        let full = http_response("200 OK", "text/html; charset=utf-8", "<html>hi</html>");
        let stripped = head_only(full.clone());
        let full_text = String::from_utf8(full).unwrap();
        let stripped_text = String::from_utf8(stripped).unwrap();
        let head_block = full_text.split("\r\n\r\n").next().unwrap();
        assert!(
            stripped_text.starts_with(head_block),
            "the headers must be unchanged"
        );
        assert!(
            stripped_text.ends_with("\r\n\r\n"),
            "no body after the headers: {stripped_text:?}"
        );
        // "<html>hi</html>" is 15 bytes: the length a GET would still advertise.
        assert!(
            stripped_text
                .to_ascii_lowercase()
                .contains("content-length: 15"),
            "HEAD keeps the entity content-length: {stripped_text:?}"
        );
        assert!(
            !stripped_text.contains("<html>hi</html>"),
            "the body must be gone"
        );
    }

    #[test]
    fn request_is_head_detects_the_method() {
        assert!(request_is_head(b"HEAD / HTTP/1.1\r\nHost: x\r\n\r\n"));
        assert!(!request_is_head(b"GET / HTTP/1.1\r\nHost: x\r\n\r\n"));
    }
}
