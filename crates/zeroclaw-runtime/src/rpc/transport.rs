//! Transport trait for RPC connections.

use async_trait::async_trait;
use tokio::sync::mpsc;

#[async_trait]
pub trait RpcTransport: Send + 'static {
    fn writer(&self) -> mpsc::Sender<String>;
    async fn next_frame(&mut self) -> Option<String>;
    fn peer_label(&self) -> String;
}
