//! WebSocket transport (HTTP CONNECT proxy + TLS + close diagnostics) and a
//! scripted test double.
//!
//! `tokio_tungstenite::connect_async` cannot tunnel through an HTTP proxy,
//! but reaching Gemini Live from a geo-restricted network requires one. When
//! a [`ProxyConfig`] is supplied, [`WsTransport::connect`] establishes a TCP
//! connection to the proxy, issues an `HTTP CONNECT` to the target host
//! (with optional Basic auth), then runs the TLS + WebSocket handshake over
//! that tunnel. Without a proxy it falls back to a plain `connect_async`.
//!
//! Ported from the kutsu prototype's `src/proxy.rs` + the `Transport`/
//! `WsTransport`/`FakeTransport` pieces of `src/gemini_live.rs`.

#![allow(async_fn_in_trait)]

use std::collections::VecDeque;
use std::fmt;
use std::sync::{Arc, Mutex};

use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use crate::types::{CloseReason, Model};

/// The unified WebSocket stream type, identical for the direct and proxied
/// connect paths. Public so a caller (e.g. kutsu's `net_check` preflight) can
/// drive raw ping/pong RTT on it without re-implementing the connect path.
pub type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// Errors from establishing or driving a [`Transport`].
#[derive(Debug)]
pub enum TransportError {
    /// Failed to establish the connection (TCP/proxy CONNECT/TLS/WS handshake).
    Connect(String),
    /// Failed to send a frame on an established connection.
    Send(String),
    /// The server sent something the transport couldn't make sense of.
    Protocol(String),
    /// The server closed the WebSocket. Carries the close frame's code +
    /// reason (when the server sent one) so callers can log/branch on it
    /// instead of it being silently swallowed.
    Closed(CloseReason),
}

impl fmt::Display for TransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TransportError::Connect(s) => write!(f, "connect: {s}"),
            TransportError::Send(s) => write!(f, "send: {s}"),
            TransportError::Protocol(s) => write!(f, "protocol: {s}"),
            TransportError::Closed(c) => write!(f, "closed: code={} reason={}", c.code, c.reason),
        }
    }
}

impl std::error::Error for TransportError {}

/// The Gemini Live `BidiGenerateContent` WebSocket, abstracted so sessions
/// can be driven against either the real socket ([`WsTransport`]) or a
/// scripted double ([`FakeTransport`]) in tests.
///
/// Server frames arrive as JSON bytes (Gemini Live ships them as WS *binary*
/// frames, not text, though this trait accepts either) — `recv` hands back
/// the raw bytes for the caller to parse. A `Close` frame surfaces as
/// `Some(Err(TransportError::Closed(reason)))` exactly once, after which the
/// stream ends (`recv` returns `None` on subsequent calls).
/// `Send` supertrait + `Send` futures: the session driver owns a `Transport`
/// inside a `tokio::spawn`ed task, so both the transport and every I/O future it
/// produces must be `Send`.
pub trait Transport: Send {
    /// Send a text frame (the setup/client-content JSON messages).
    fn send_text(
        &mut self,
        s: String,
    ) -> impl std::future::Future<Output = Result<(), TransportError>> + Send;
    /// Receive the next server frame's raw bytes, or `None` when the stream
    /// has ended (after a close, or the underlying connection dropped).
    fn recv(
        &mut self,
    ) -> impl std::future::Future<Output = Option<Result<Vec<u8>, TransportError>>> + Send;
}

/// HTTP CONNECT proxy credentials, as needed to reach Gemini Live from a
/// geo-restricted network. The crate's own type — callers map their own
/// proxy config into this.
#[derive(Clone, Debug)]
pub struct ProxyConfig {
    /// The proxy's `http://host:port` (or bare `host:port`) address.
    pub url: String,
    pub user: Option<String>,
    pub password: Option<String>,
}

/// rustls 0.23 does not pick a crypto provider automatically. Install `ring`
/// exactly once, process-wide, before the first TLS handshake. Idempotent
/// and safe to call from every connection attempt.
fn ensure_crypto_provider() {
    use std::sync::Once;
    static TLS_INIT: Once = Once::new();
    TLS_INIT.call_once(|| {
        // Ignore the error if another provider was already installed.
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

/// Build the `BidiGenerateContent` WebSocket endpoint URL for `model`,
/// pinned to its API version, carrying `api_key` as the `key` query param.
/// Public so callers probe the exact URL the session will use (single source
/// of truth for the endpoint + per-model API version).
pub fn endpoint_url(model: Model, api_key: &str) -> String {
    format!(
        "wss://generativelanguage.googleapis.com/ws/\
         google.ai.generativelanguage.{ver}.GenerativeService.BidiGenerateContent?key={key}",
        ver = model.api_version(),
        key = api_key,
    )
}

/// Open a WebSocket connection to `url`, tunneling through `proxy` when present.
/// Public so a caller (kutsu's `net_check` preflight) can obtain the raw stream
/// for ping/pong RTT without re-implementing the proxy/TLS connect path.
pub async fn connect_ws(
    proxy: Option<&ProxyConfig>,
    url: &str,
) -> Result<WsStream, TransportError> {
    ensure_crypto_provider();
    match proxy {
        None => {
            let (ws, _) = tokio_tungstenite::connect_async(url)
                .await
                .map_err(|e| TransportError::Connect(format!("connect: {e}")))?;
            Ok(ws)
        }
        Some(p) => connect_via_proxy(p, url).await,
    }
}

async fn connect_via_proxy(proxy: &ProxyConfig, url: &str) -> Result<WsStream, TransportError> {
    let (target_host, target_port) = target_host_port(url)?;
    let (proxy_host, proxy_port) = parse_host_port(&proxy.url, 80)?;

    let mut stream = TcpStream::connect((proxy_host.as_str(), proxy_port))
        .await
        .map_err(|e| {
            TransportError::Connect(format!(
                "proxy TCP connect to {proxy_host}:{proxy_port}: {e}"
            ))
        })?;

    match (proxy.user.as_deref(), proxy.password.as_deref()) {
        (Some(user), Some(password)) => {
            async_http_proxy::http_connect_tokio_with_basic_auth(
                &mut stream,
                &target_host,
                target_port,
                user,
                password,
            )
            .await
            .map_err(|e| TransportError::Connect(format!("proxy CONNECT: {e}")))?;
        }
        _ => {
            async_http_proxy::http_connect_tokio(&mut stream, &target_host, target_port)
                .await
                .map_err(|e| TransportError::Connect(format!("proxy CONNECT: {e}")))?;
        }
    }

    let request = url
        .into_client_request()
        .map_err(|e| TransportError::Connect(format!("bad ws url: {e}")))?;
    let (ws, _) = tokio_tungstenite::client_async_tls(request, stream)
        .await
        .map_err(|e| TransportError::Connect(format!("ws handshake over proxy: {e}")))?;
    Ok(ws)
}

/// Extract `(host, port)` from a `wss://` URL, defaulting to port 443 when no
/// explicit port is present. Only secure WebSocket URLs are accepted — Gemini
/// Live is always reached over TLS.
fn target_host_port(url: &str) -> Result<(String, u16), TransportError> {
    let rest = url
        .strip_prefix("wss://")
        .ok_or_else(|| TransportError::Connect(format!("not a wss url: {url}")))?;
    // Authority is everything up to the first '/', '?' or '#'.
    let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    parse_host_port(authority, 443u16)
}

/// Parse `host` or `host:port`, applying `default_port` when the port is
/// absent. Accepts an optional `http://` prefix (proxy URLs carry one).
fn parse_host_port(s: &str, default_port: u16) -> Result<(String, u16), TransportError> {
    let s = s.strip_prefix("http://").unwrap_or(s);
    let s = s.strip_suffix('/').unwrap_or(s);
    match s.rsplit_once(':') {
        Some((host, port)) => {
            let port = port
                .parse::<u16>()
                .map_err(|_| TransportError::Connect(format!("bad port in '{s}'")))?;
            if host.is_empty() {
                return Err(TransportError::Connect(format!("empty host in '{s}'")));
            }
            Ok((host.to_string(), port))
        }
        None => {
            if s.is_empty() {
                return Err(TransportError::Connect("empty host".into()));
            }
            Ok((s.to_string(), default_port))
        }
    }
}

/// The real `tokio-tungstenite` transport: a live WebSocket to Gemini Live,
/// optionally tunneled through an HTTP CONNECT proxy.
pub struct WsTransport {
    ws: WsStream,
}

impl WsTransport {
    /// Connect to the Gemini Live `BidiGenerateContent` endpoint for `model`,
    /// authenticating with `api_key`, optionally tunneling through `proxy`.
    pub async fn connect(
        model: Model,
        api_key: &str,
        proxy: Option<&ProxyConfig>,
    ) -> Result<Self, TransportError> {
        let url = endpoint_url(model, api_key);
        let ws = connect_ws(proxy, &url).await?;
        Ok(Self { ws })
    }
}

impl Transport for WsTransport {
    fn send_text(
        &mut self,
        s: String,
    ) -> impl std::future::Future<Output = Result<(), TransportError>> + Send {
        use futures_util::SinkExt;
        async move {
            self.ws
                .send(Message::Text(s.into()))
                .await
                .map_err(|e| TransportError::Send(e.to_string()))
        }
    }

    fn recv(
        &mut self,
    ) -> impl std::future::Future<Output = Option<Result<Vec<u8>, TransportError>>> + Send {
        use futures_util::StreamExt;
        async move {
            loop {
                match self.ws.next().await? {
                    // Gemini Live sends server messages as binary WS frames (JSON
                    // bytes), not text — accept both.
                    Ok(Message::Text(t)) => return Some(Ok(t.as_bytes().to_vec())),
                    Ok(Message::Binary(b)) => return Some(Ok(b.to_vec())),
                    Ok(Message::Close(frame)) => {
                        let reason = match frame {
                            Some(cf) => {
                                tracing::warn!(code = %cf.code, reason = %cf.reason, "gemini: server closed WS");
                                CloseReason {
                                    code: cf.code.into(),
                                    reason: cf.reason.to_string(),
                                }
                            }
                            None => {
                                tracing::warn!("gemini: server closed WS (no close frame)");
                                CloseReason {
                                    code: 0,
                                    reason: String::new(),
                                }
                            }
                        };
                        return Some(Err(TransportError::Closed(reason)));
                    }
                    Ok(_) => continue, // ignore ping/pong
                    Err(e) => return Some(Err(TransportError::Protocol(e.to_string()))),
                }
            }
        }
    }
}

/// One scripted incoming item for [`FakeTransport`]: either a server frame's
/// raw bytes, or a close event carrying the `code`/`reason` to surface.
#[derive(Clone, Debug)]
pub enum FakeFrame {
    Data(Vec<u8>),
    Close(CloseReason),
}

/// A scripted [`Transport`] test double: `recv()` yields the queued
/// `incoming` frames in order, and every `send_text` call is captured (in
/// send order) into `sent` for assertions. Always public (not
/// `#[cfg(test)]`-gated) so both this crate's tests and kutsu's can reuse it.
pub struct FakeTransport {
    pub incoming: VecDeque<FakeFrame>,
    pub sent: Arc<Mutex<Vec<String>>>,
    /// When the queue is drained: `false` (default) mirrors a closed
    /// connection (`recv()` returns `None`); `true` mirrors a live
    /// connection with nothing new yet (`recv()` pends forever) — needed by
    /// tests that must keep a session alive past an empty script.
    pub pending_when_empty: bool,
}

impl FakeTransport {
    /// A transport with no scripted frames; `recv()` ends the stream
    /// immediately (or pends forever, per `pending_when_empty`).
    pub fn new(pending_when_empty: bool) -> Self {
        Self {
            incoming: VecDeque::new(),
            sent: Arc::new(Mutex::new(Vec::new())),
            pending_when_empty,
        }
    }

    /// Queue a data frame to be returned by a future `recv()`.
    pub fn push_data(&mut self, bytes: impl Into<Vec<u8>>) {
        self.incoming.push_back(FakeFrame::Data(bytes.into()));
    }

    /// Queue a close event to be returned by a future `recv()`.
    pub fn push_close(&mut self, code: u16, reason: impl Into<String>) {
        self.incoming.push_back(FakeFrame::Close(CloseReason {
            code,
            reason: reason.into(),
        }));
    }
}

impl Transport for FakeTransport {
    fn send_text(
        &mut self,
        s: String,
    ) -> impl std::future::Future<Output = Result<(), TransportError>> + Send {
        self.sent.lock().unwrap().push(s);
        async { Ok(()) }
    }

    fn recv(
        &mut self,
    ) -> impl std::future::Future<Output = Option<Result<Vec<u8>, TransportError>>> + Send {
        let next = self.incoming.pop_front();
        let pending = self.pending_when_empty;
        async move {
            match next {
                Some(FakeFrame::Data(b)) => Some(Ok(b)),
                Some(FakeFrame::Close(reason)) => {
                    tracing::warn!(code = %reason.code, reason = %reason.reason, "fake: scripted WS close");
                    Some(Err(TransportError::Closed(reason)))
                }
                None if pending => std::future::pending().await,
                None => None,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_defaults_to_443_for_wss() {
        let (h, p) = target_host_port(
            "wss://generativelanguage.googleapis.com/ws/foo.BidiGenerateContent?key=K",
        )
        .unwrap();
        assert_eq!(h, "generativelanguage.googleapis.com");
        assert_eq!(p, 443);
    }

    #[test]
    fn proxy_url_parses_host_and_port() {
        let (h, p) = parse_host_port("http://46.202.204.37:46250", 80).unwrap();
        assert_eq!(h, "46.202.204.37");
        assert_eq!(p, 46250);
    }

    #[test]
    fn host_without_port_uses_default() {
        let (h, p) = parse_host_port("proxy.example.com", 8080).unwrap();
        assert_eq!(h, "proxy.example.com");
        assert_eq!(p, 8080);
    }

    #[test]
    fn non_ws_url_is_rejected() {
        assert!(target_host_port("https://example.com").is_err());
    }

    #[test]
    fn endpoint_url_pins_api_version_and_key() {
        let half = endpoint_url(Model::HalfCascade, "KEY");
        assert!(half.contains(".v1beta."));
        assert!(half.contains("key=KEY"));
        let native = endpoint_url(Model::NativeAudio, "KEY");
        assert!(native.contains(".v1alpha."));
    }

    #[tokio::test]
    async fn fake_transport_round_trips_scripted_frames_in_order() {
        let mut t = FakeTransport::new(false);
        t.push_data(b"first".to_vec());
        t.push_data(b"second".to_vec());

        let a = t.recv().await.unwrap().unwrap();
        assert_eq!(a, b"first");
        let b = t.recv().await.unwrap().unwrap();
        assert_eq!(b, b"second");
        assert!(t.recv().await.is_none(), "queue drained -> stream ends");
    }

    #[tokio::test]
    async fn fake_transport_captures_outgoing_send_text() {
        let mut t = FakeTransport::new(false);
        t.send_text("hello".into()).await.unwrap();
        t.send_text("world".into()).await.unwrap();
        assert_eq!(
            &*t.sent.lock().unwrap(),
            &["hello".to_string(), "world".to_string()]
        );
    }

    #[tokio::test]
    async fn scripted_close_frame_surfaces_code_and_reason() {
        let mut t = FakeTransport::new(false);
        t.push_data(b"before-close".to_vec());
        t.push_close(1011, "server error");

        // Data frames before the close still come through untouched.
        assert_eq!(t.recv().await.unwrap().unwrap(), b"before-close");

        match t.recv().await {
            Some(Err(TransportError::Closed(reason))) => {
                assert_eq!(reason.code, 1011);
                assert_eq!(reason.reason, "server error");
            }
            other => panic!("expected Closed error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn pending_when_empty_keeps_recv_from_ending_the_stream() {
        let mut t = FakeTransport::new(true);
        t.push_data(b"only".to_vec());
        assert_eq!(t.recv().await.unwrap().unwrap(), b"only");
        // Queue is now empty; recv() must pend rather than return None.
        let pend = tokio::time::timeout(std::time::Duration::from_millis(20), t.recv()).await;
        assert!(
            pend.is_err(),
            "recv() should still be pending, not resolved"
        );
    }
}
