// src/connection/mod.rs
pub mod protocol;

pub use protocol::{
    ConnectParams, Header, JsonRpcError, JsonRpcNotification, JsonRpcRequest, JsonRpcResponse,
    RecvAckParams, RecvNotificationParams, SendParams, WUKONGIM_RPC_VERSION, WkChannelType,
    WkMessageType,
};

use std::time::Duration;
use tokio_tungstenite::tungstenite::Message as WsMsg;

pub const PING_INTERVAL: Duration = Duration::from_secs(30);
pub const HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(90);

pub type WsSink = futures_util::stream::SplitSink<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    WsMsg,
>;
