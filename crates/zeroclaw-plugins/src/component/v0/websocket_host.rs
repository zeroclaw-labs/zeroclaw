// Host-side WIT `websocket` implementation for the `channel-plugin` world —
// the one genuinely new networking surface channel plugins need beyond the
// now-proxy-aware `wasi:http` (WASI Preview 2 has no WebSocket primitive at
// all). `connect` wraps `zeroclaw_config::schema::ws_connect_with_proxy`
// directly: no new proxy/tunnel logic, just the same allow-list check
// `send_request` already applies plus a thin resource wrapper.
//
// I/O model mirrors `poll-message`: a background task owns the actual read
// loop and feeds an unbounded channel; `poll` drains it non-blockingly. The
// guest never needs its own thread or timer.

use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use tokio::sync::{Mutex as AsyncMutex, mpsc};
use tokio_tungstenite::tungstenite::Message;
use wasmtime::component::Resource;

use super::bindings::channel::zeroclaw::plugin::websocket::{Host, HostWebsocket, WsMessage};
use super::plugin_store::PluginStore;

pub(super) type WsSink =
    futures_util::stream::SplitSink<zeroclaw_config::schema::ProxiedWsStream, Message>;

/// Convert a WIT `ws-message` into the wire-level tungstenite frame to send.
/// Shared with `gateway_host`, which sends frames through the same sink type.
pub(super) fn ws_message_to_tungstenite(msg: WsMessage) -> Message {
    match msg {
        WsMessage::Text(text) => Message::Text(text.into()),
        WsMessage::Binary(data) => Message::Binary(data.into()),
        WsMessage::Closed(code) => {
            let frame = code.map(|c| tokio_tungstenite::tungstenite::protocol::CloseFrame {
                code: tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode::from(c),
                reason: tokio_tungstenite::tungstenite::Utf8Bytes::from_static(""),
            });
            Message::Close(frame)
        }
    }
}

/// Host-side state backing one connected `websocket` resource.
pub struct HostWebsocketState {
    sink: Arc<AsyncMutex<WsSink>>,
    inbound: mpsc::UnboundedReceiver<WsMessage>,
    reader: tokio::task::JoinHandle<()>,
}

impl Host for PluginStore {}

impl HostWebsocket for PluginStore {
    async fn connect(&mut self, url: String) -> Result<Resource<HostWebsocketState>, String> {
        if !self.is_url_host_allowed(&url) {
            return Err(format!(
                "websocket connect denied: {url} is not in this plugin's allow-list"
            ));
        }

        let service_key = self.network_config.service_key.clone();
        let proxy_url = self.network_config.proxy_url.clone();
        let (stream, _response) = zeroclaw_config::schema::ws_connect_with_proxy(
            &url,
            &service_key,
            proxy_url.as_deref(),
        )
        .await
        .map_err(|e| format!("websocket connect failed: {e}"))?;

        let (sink, mut read) = stream.split();
        let sink = Arc::new(AsyncMutex::new(sink));
        let (tx, rx) = mpsc::unbounded_channel();

        let reader_sink = Arc::clone(&sink);
        let reader = zeroclaw_spawn::spawn!(async move {
            loop {
                match read.next().await {
                    Some(Ok(Message::Text(text))) => {
                        if tx.send(WsMessage::Text(text.to_string())).is_err() {
                            break;
                        }
                    }
                    Some(Ok(Message::Binary(data))) => {
                        if tx.send(WsMessage::Binary(data.to_vec())).is_err() {
                            break;
                        }
                    }
                    Some(Ok(Message::Ping(payload))) => {
                        // Tungstenite's read half doesn't auto-reply; do it
                        // ourselves through the shared sink so the connection
                        // stays alive without guest involvement.
                        let mut sink = reader_sink.lock().await;
                        if sink.send(Message::Pong(payload)).await.is_err() {
                            break;
                        }
                    }
                    Some(Ok(Message::Close(frame))) => {
                        let code = frame.map(|f| u16::from(f.code));
                        let _ = tx.send(WsMessage::Closed(code));
                        break;
                    }
                    Some(Ok(Message::Pong(_) | Message::Frame(_))) => {}
                    Some(Err(_)) | None => {
                        let _ = tx.send(WsMessage::Closed(None));
                        break;
                    }
                }
            }
        });

        let state = HostWebsocketState {
            sink,
            inbound: rx,
            reader,
        };
        self.resource_table_mut()
            .push(state)
            .map_err(|e| format!("failed to register websocket resource: {e}"))
    }

    async fn send_text(
        &mut self,
        self_: Resource<HostWebsocketState>,
        text: String,
    ) -> Result<(), String> {
        let state = self
            .resource_table_mut()
            .get(&self_)
            .map_err(|e| format!("invalid websocket handle: {e}"))?;
        let sink = Arc::clone(&state.sink);
        let mut sink = sink.lock().await;
        sink.send(Message::Text(text.into()))
            .await
            .map_err(|e| format!("websocket send failed: {e}"))
    }

    async fn send_binary(
        &mut self,
        self_: Resource<HostWebsocketState>,
        data: Vec<u8>,
    ) -> Result<(), String> {
        let state = self
            .resource_table_mut()
            .get(&self_)
            .map_err(|e| format!("invalid websocket handle: {e}"))?;
        let sink = Arc::clone(&state.sink);
        let mut sink = sink.lock().await;
        sink.send(Message::Binary(data.into()))
            .await
            .map_err(|e| format!("websocket send failed: {e}"))
    }

    async fn poll(&mut self, self_: Resource<HostWebsocketState>) -> Option<WsMessage> {
        let state = self.resource_table_mut().get_mut(&self_).ok()?;
        state.inbound.try_recv().ok()
    }

    async fn close(
        &mut self,
        self_: Resource<HostWebsocketState>,
        code: Option<u16>,
    ) -> Result<(), String> {
        let state = self
            .resource_table_mut()
            .get(&self_)
            .map_err(|e| format!("invalid websocket handle: {e}"))?;
        let sink = Arc::clone(&state.sink);
        let mut sink = sink.lock().await;
        let frame = code.map(|c| tokio_tungstenite::tungstenite::protocol::CloseFrame {
            code: tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode::from(c),
            reason: tokio_tungstenite::tungstenite::Utf8Bytes::from_static(""),
        });
        sink.send(Message::Close(frame))
            .await
            .map_err(|e| format!("websocket close failed: {e}"))
    }

    async fn drop(&mut self, rep: Resource<HostWebsocketState>) -> wasmtime::Result<()> {
        if let Ok(state) = self.resource_table_mut().delete(rep) {
            state.reader.abort();
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::component::v0::plugin_store::PluginStore;

    /// Spin up a local WS server that echoes the first text frame it
    /// receives, then closes. Returns the `ws://` URL to connect to.
    async fn spawn_echo_server() -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = zeroclaw_spawn::spawn!(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            if let Some(Ok(Message::Text(text))) = ws.next().await {
                let _ = ws.send(Message::Text(text)).await;
            }
            let _ = ws.close(None).await;
        });
        (format!("ws://127.0.0.1:{port}/"), handle)
    }

    async fn allowed_store() -> PluginStore {
        let perms = vec![crate::FineGrainedPermission::Http(
            crate::AddressString::new("127.0.0.1").unwrap(),
        )];
        PluginStore::with_permissions(&perms, &crate::PluginNetworkConfig::default())
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn connect_denied_for_host_outside_allow_list() {
        let mut store = PluginStore::with_permissions(&[], &crate::PluginNetworkConfig::default())
            .await
            .unwrap();
        let err = HostWebsocket::connect(&mut store, "ws://127.0.0.1:9/".to_string())
            .await
            .expect_err("host outside allow-list must be denied before connecting");
        assert!(err.contains("not in this plugin's allow-list"));
    }

    #[tokio::test]
    async fn connect_send_and_poll_round_trip_through_real_server() {
        let (url, server) = spawn_echo_server().await;
        let mut store = allowed_store().await;

        let handle = HostWebsocket::connect(&mut store, url)
            .await
            .expect("allow-listed host must be permitted");

        HostWebsocket::send_text(
            &mut store,
            Resource::new_own(handle.rep()),
            "hi".to_string(),
        )
        .await
        .expect("send must succeed");

        let mut received = None;
        for _ in 0..50 {
            if let Some(msg) =
                HostWebsocket::poll(&mut store, Resource::new_own(handle.rep())).await
            {
                received = Some(msg);
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }

        match received.expect("must receive the echoed frame") {
            WsMessage::Text(text) => assert_eq!(text, "hi"),
            other => panic!("expected a text frame, got {other:?}"),
        }

        HostWebsocket::close(&mut store, Resource::new_own(handle.rep()), None)
            .await
            .ok();
        server.await.unwrap();
    }
}
