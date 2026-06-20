// Host-side WIT `gateway` implementation for the `channel-plugin` world —
// the heartbeat-timer / stall-detection state machine every WS-gateway-shaped
// channel (Discord, Slack Socket Mode, Lark, WeChat Enterprise WS, DingTalk)
// needs, factored out of the guest entirely. A WASM guest cannot run its own
// background timer independent of host-driven calls, so the host ticks and
// watches; the guest only decides what to send in response via `poll-raw`,
// mirroring the same host-drives/guest-polls model `poll-message` already
// uses for inbound channel messages.
//
// Layered directly on `websocket_host`'s sink type and frame conversion —
// `connect` is still just `ws_connect_with_proxy` plus the same allow-list
// check `send_request`/`websocket.connect` apply.

use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use tokio::sync::{Mutex as AsyncMutex, mpsc};
use tokio_tungstenite::tungstenite::Message;
use wasmtime::component::Resource;
use zeroclaw_infra::stall_watchdog::StallWatchdog;

use super::bindings::channel::zeroclaw::plugin::gateway::{GatewayEvent, Host, HostGatewaySession};
use super::bindings::channel::zeroclaw::plugin::websocket::WsMessage;
use super::plugin_store::PluginStore;
use super::websocket_host::{WsSink, ws_message_to_tungstenite};

/// Host-side state backing one connected `gateway-session` resource.
pub struct HostGatewaySessionState {
    sink: Arc<AsyncMutex<WsSink>>,
    inbound: mpsc::UnboundedReceiver<GatewayEvent>,
    reader: tokio::task::JoinHandle<()>,
    heartbeat: Option<tokio::task::JoinHandle<()>>,
    /// Kept alive so its background poll task keeps running; aborted via
    /// `stop()` on `drop`.
    watchdog: Option<Arc<StallWatchdog>>,
}

impl Host for PluginStore {}

impl HostGatewaySession for PluginStore {
    async fn connect(
        &mut self,
        url: String,
        heartbeat_interval_ms: u32,
        stall_timeout_secs: u32,
    ) -> Result<Resource<HostGatewaySessionState>, String> {
        if !self.is_url_host_allowed(&url) {
            return Err(format!(
                "gateway connect denied: {url} is not in this plugin's allow-list"
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
        .map_err(|e| format!("gateway connect failed: {e}"))?;

        let (sink, mut read) = stream.split();
        let sink = Arc::new(AsyncMutex::new(sink));
        let (tx, rx) = mpsc::unbounded_channel();

        let watchdog = if stall_timeout_secs > 0 {
            let watchdog = Arc::new(StallWatchdog::new(u64::from(stall_timeout_secs)));
            let stall_tx = tx.clone();
            let stall_sink = Arc::clone(&sink);
            watchdog
                .start(move || {
                    let stall_tx = stall_tx.clone();
                    let stall_sink = Arc::clone(&stall_sink);
                    zeroclaw_spawn::spawn!(async move {
                        let _ = stall_tx.send(GatewayEvent::Stalled);
                        let mut sink = stall_sink.lock().await;
                        let _ = sink.send(Message::Close(None)).await;
                    });
                })
                .await;
            Some(watchdog)
        } else {
            None
        };

        let reader_watchdog = watchdog.clone();
        let reader_tx = tx.clone();
        let reader = zeroclaw_spawn::spawn!(async move {
            loop {
                match read.next().await {
                    Some(Ok(Message::Text(text))) => {
                        if let Some(wd) = &reader_watchdog {
                            wd.touch();
                        }
                        if reader_tx
                            .send(GatewayEvent::Frame(WsMessage::Text(text.to_string())))
                            .is_err()
                        {
                            break;
                        }
                    }
                    Some(Ok(Message::Binary(data))) => {
                        if let Some(wd) = &reader_watchdog {
                            wd.touch();
                        }
                        if reader_tx
                            .send(GatewayEvent::Frame(WsMessage::Binary(data.to_vec())))
                            .is_err()
                        {
                            break;
                        }
                    }
                    Some(Ok(Message::Close(frame))) => {
                        let code = frame.map(|f| u16::from(f.code));
                        let _ = reader_tx.send(GatewayEvent::Frame(WsMessage::Closed(code)));
                        break;
                    }
                    Some(Ok(Message::Ping(_) | Message::Pong(_) | Message::Frame(_))) => {}
                    Some(Err(_)) | None => {
                        let _ = reader_tx.send(GatewayEvent::Frame(WsMessage::Closed(None)));
                        break;
                    }
                }
            }
        });

        let heartbeat = if heartbeat_interval_ms > 0 {
            let heartbeat_tx = tx.clone();
            let interval = std::time::Duration::from_millis(u64::from(heartbeat_interval_ms));
            Some(zeroclaw_spawn::spawn!(async move {
                let mut ticker = tokio::time::interval(interval);
                ticker.tick().await; // first tick fires immediately; skip it
                loop {
                    ticker.tick().await;
                    if heartbeat_tx.send(GatewayEvent::HeartbeatTick).is_err() {
                        break;
                    }
                }
            }))
        } else {
            None
        };

        let state = HostGatewaySessionState {
            sink,
            inbound: rx,
            reader,
            heartbeat,
            watchdog,
        };
        self.resource_table_mut()
            .push(state)
            .map_err(|e| format!("failed to register gateway-session resource: {e}"))
    }

    async fn poll_raw(&mut self, self_: Resource<HostGatewaySessionState>) -> Option<GatewayEvent> {
        let state = self.resource_table_mut().get_mut(&self_).ok()?;
        state.inbound.try_recv().ok()
    }

    async fn send(
        &mut self,
        self_: Resource<HostGatewaySessionState>,
        msg: WsMessage,
    ) -> Result<(), String> {
        let state = self
            .resource_table_mut()
            .get(&self_)
            .map_err(|e| format!("invalid gateway-session handle: {e}"))?;
        let sink = Arc::clone(&state.sink);
        let mut sink = sink.lock().await;
        sink.send(ws_message_to_tungstenite(msg))
            .await
            .map_err(|e| format!("gateway send failed: {e}"))
    }

    async fn save_session(
        &mut self,
        _self_: Resource<HostGatewaySessionState>,
        state: String,
    ) -> Result<(), String> {
        self.gateway_resume_state = Some(state);
        Ok(())
    }

    async fn saved_session(&mut self, _self_: Resource<HostGatewaySessionState>) -> Option<String> {
        self.gateway_resume_state.clone()
    }

    async fn close(&mut self, self_: Resource<HostGatewaySessionState>) -> Result<(), String> {
        let state = self
            .resource_table_mut()
            .get(&self_)
            .map_err(|e| format!("invalid gateway-session handle: {e}"))?;
        let sink = Arc::clone(&state.sink);
        let mut sink = sink.lock().await;
        sink.send(Message::Close(None))
            .await
            .map_err(|e| format!("gateway close failed: {e}"))
    }

    async fn drop(&mut self, rep: Resource<HostGatewaySessionState>) -> wasmtime::Result<()> {
        if let Ok(state) = self.resource_table_mut().delete(rep) {
            state.reader.abort();
            if let Some(heartbeat) = state.heartbeat {
                heartbeat.abort();
            }
            if let Some(watchdog) = state.watchdog {
                watchdog.stop().await;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::component::v0::plugin_store::PluginStore;

    async fn allowed_store() -> PluginStore {
        let perms = vec![crate::FineGrainedPermission::Http(
            crate::AddressString::new("127.0.0.1").unwrap(),
        )];
        PluginStore::with_permissions(&perms, &crate::PluginNetworkConfig::default())
            .await
            .unwrap()
    }

    /// A WS server that just holds the connection open without sending
    /// anything, so heartbeat/stall behavior can be observed in isolation
    /// from any real inbound traffic.
    async fn spawn_silent_server() -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = zeroclaw_spawn::spawn!(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            // Hold the connection open until the test drops its side.
            let _ = futures_util::stream::StreamExt::collect::<Vec<_>>(ws).await;
        });
        (format!("ws://127.0.0.1:{port}/"), handle)
    }

    #[tokio::test]
    async fn connect_denied_for_host_outside_allow_list() {
        let mut store = PluginStore::with_permissions(&[], &crate::PluginNetworkConfig::default())
            .await
            .unwrap();
        let err = HostGatewaySession::connect(&mut store, "ws://127.0.0.1:9/".to_string(), 0, 0)
            .await
            .expect_err("host outside allow-list must be denied before connecting");
        assert!(err.contains("not in this plugin's allow-list"));
    }

    #[tokio::test]
    async fn heartbeat_tick_fires_on_schedule_without_any_inbound_traffic() {
        let (url, _server) = spawn_silent_server().await;
        let mut store = allowed_store().await;
        let handle = HostGatewaySession::connect(&mut store, url, 50, 0)
            .await
            .expect("allow-listed host must be permitted");

        let mut saw_tick = false;
        for _ in 0..50 {
            if let Some(GatewayEvent::HeartbeatTick) =
                HostGatewaySession::poll_raw(&mut store, Resource::new_own(handle.rep())).await
            {
                saw_tick = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(
            saw_tick,
            "heartbeat-tick must fire on schedule even with no inbound frames"
        );
    }

    #[tokio::test]
    async fn stall_watchdog_closes_connection_and_surfaces_stalled_event() {
        let (url, _server) = spawn_silent_server().await;
        let mut store = allowed_store().await;
        // stall_timeout_secs is clamped to >=1s internally by the watchdog's
        // poll_interval ((timeout/2).max(1)); use the smallest real value.
        let handle = HostGatewaySession::connect(&mut store, url, 0, 1)
            .await
            .expect("allow-listed host must be permitted");

        let mut saw_stalled = false;
        for _ in 0..100 {
            if let Some(GatewayEvent::Stalled) =
                HostGatewaySession::poll_raw(&mut store, Resource::new_own(handle.rep())).await
            {
                saw_stalled = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        assert!(
            saw_stalled,
            "stall watchdog must surface a Stalled event when no frames arrive"
        );
    }

    #[tokio::test]
    async fn save_and_saved_session_round_trip_across_a_reconnect() {
        let (url, _server) = spawn_silent_server().await;
        let mut store = allowed_store().await;
        let handle = HostGatewaySession::connect(&mut store, url.clone(), 0, 0)
            .await
            .unwrap();

        assert_eq!(
            HostGatewaySession::saved_session(&mut store, Resource::new_own(handle.rep())).await,
            None
        );
        HostGatewaySession::save_session(
            &mut store,
            Resource::new_own(handle.rep()),
            r#"{"session_id":"abc","seq":42}"#.to_string(),
        )
        .await
        .unwrap();
        HostGatewaySession::close(&mut store, Resource::new_own(handle.rep()))
            .await
            .ok();

        // Resume state lives on the store, not the per-connection resource,
        // so a fresh connect() on the same instance still sees it.
        let (url2, _server2) = spawn_silent_server().await;
        let handle2 = HostGatewaySession::connect(&mut store, url2, 0, 0)
            .await
            .unwrap();
        assert_eq!(
            HostGatewaySession::saved_session(&mut store, Resource::new_own(handle2.rep())).await,
            Some(r#"{"session_id":"abc","seq":42}"#.to_string())
        );
        let _ = url;
    }
}
