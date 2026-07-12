use crate::frontdoor_assets::{APP_JS, INDEX_HTML, SERVICE_WORKER_JS, TUNNEL_WORKER_JS};
use crate::frontdoor_tls_assets::TLS_ENGINE_JS;
use anyhow::{Context, Result};
use std::io::Cursor;
use std::pin::Pin;
use std::task::{Context as TaskContext, Poll};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio_tungstenite::WebSocketStream;
use zeroclaw_relay_proto::SUBPROTOCOL;

const MAX_HTTP_HEAD: usize = 16 * 1024;

pub(crate) enum Frontdoor<S> {
    WebSocket(Box<WebSocketStream<PrefixedIo<S>>>),
    ServedHttp,
}

pub(crate) async fn accept_or_serve<S>(mut stream: S) -> Result<Frontdoor<S>>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let head = read_http_head(&mut stream).await?;
    if is_websocket_upgrade(&head) {
        let io = PrefixedIo {
            prefix: Cursor::new(head),
            inner: stream,
        };
        let ws = tokio_tungstenite::accept_hdr_async(io, select_subprotocol)
            .await
            .context("relay websocket handshake")?;
        return Ok(Frontdoor::WebSocket(Box::new(ws)));
    }

    let response = response_for(&head);
    stream.write_all(&response).await?;
    let _ = stream.shutdown().await;
    Ok(Frontdoor::ServedHttp)
}

async fn read_http_head<S>(stream: &mut S) -> Result<Vec<u8>>
where
    S: AsyncRead + Unpin,
{
    let mut head = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    loop {
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            anyhow::bail!("connection closed before request headers");
        }
        head.extend_from_slice(&chunk[..n]);
        if header_end(&head).is_some() {
            return Ok(head);
        }
        if head.len() > MAX_HTTP_HEAD {
            anyhow::bail!("request headers too large");
        }
    }
}

fn header_end(head: &[u8]) -> Option<usize> {
    head.windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|i| i + 4)
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

fn response_for(head: &[u8]) -> Vec<u8> {
    let path = request_path(head).unwrap_or("/");
    match path {
        "/" | "/index.html" => http_response("200 OK", "text/html; charset=utf-8", INDEX_HTML),
        "/app.js" => http_response("200 OK", "application/javascript; charset=utf-8", APP_JS),
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
        _ => http_response("404 Not Found", "text/plain; charset=utf-8", "not found\n"),
    }
}

fn request_path(head: &[u8]) -> Option<&str> {
    let text = std::str::from_utf8(head).ok()?;
    let request = text.lines().next()?;
    let mut parts = request.split_whitespace();
    match (parts.next(), parts.next()) {
        (Some("GET"), Some(path)) => Some(path),
        _ => None,
    }
}

fn http_response(status: &str, content_type: &str, body: &str) -> Vec<u8> {
    format!(
        "HTTP/1.1 {status}\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\ncache-control: no-store\r\nconnection: close\r\n\r\n{body}",
        body.len()
    )
    .into_bytes()
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
        assert!(text.contains("sas-panel"));
        assert!(text.contains("chat-panel"));
        assert!(text.contains("messages"));
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
        assert!(text.contains("confirmEnrollment"));
        assert!(text.contains("abortEnrollment"));
        assert!(text.contains("rpc-ready"));
        assert!(text.contains("session/new"));
        assert!(text.contains("session/prompt"));
        assert!(text.contains("session/approve"));
        assert!(text.contains("session/update"));
    }

    #[test]
    fn service_worker_route_serves_javascript() {
        let resp = response_for(b"GET /sw.js HTTP/1.1\r\nHost: x\r\n\r\n");
        let text = String::from_utf8(resp).unwrap();
        assert!(text.starts_with("HTTP/1.1 200 OK"));
        assert!(text.contains("zeroclaw-relay-worker-ready"));
        assert!(text.contains("proxyRpc(event)"));
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
        assert!(text.contains("ZeroClawEnrollmentTls.enroll"));
        assert!(text.contains("enrollment-sas"));
        assert!(text.contains("material = { ...material, relayUrl, nodeId, pairingCode }"));
        assert!(text.contains("connectRpcTunnel"));
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
        assert!(text.contains("verifyServerCertificateVerify"));
        assert!(text.contains("verifyServerCertificateChain"));
        assert!(text.contains("ecdsaDerSignatureToRaw"));
        assert!(text.contains("_internals"));
        assert!(text.contains("onNotification"));
        assert!(text.contains("Unsupported browser RPC request"));
        assert!(text.contains("pairing_code"));
    }
}
