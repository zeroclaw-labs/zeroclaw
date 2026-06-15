//! RpcApprovalChannel — bridges Channel::request_approval() to the
//! daemon Unix socket RPC stream.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::json;
use uuid::Uuid;

use zeroclaw_api::attribution::{Attributable, ChannelKind, Role};
use zeroclaw_api::channel::{
    Channel, ChannelApprovalRequest, ChannelApprovalResponse, ChannelMessage, SendMessage,
};
use zeroclaw_api::jsonrpc::RpcOutbound;

use super::context::ApprovalPendingMap;

const DEFAULT_APPROVAL_TIMEOUT: Duration = Duration::from_secs(120);

pub struct RpcApprovalChannel {
    name: String,
    session_id: String,
    rpc: Arc<RpcOutbound>,
    pending: Arc<ApprovalPendingMap>,
    approval_timeout: Duration,
}

impl RpcApprovalChannel {
    pub fn new(
        name: impl Into<String>,
        session_id: impl Into<String>,
        rpc: Arc<RpcOutbound>,
        pending: Arc<ApprovalPendingMap>,
    ) -> Self {
        Self {
            name: name.into(),
            session_id: session_id.into(),
            rpc,
            pending,
            approval_timeout: DEFAULT_APPROVAL_TIMEOUT,
        }
    }
}

impl Attributable for RpcApprovalChannel {
    fn role(&self) -> Role {
        Role::Channel(ChannelKind::AcpChannel)
    }

    fn alias(&self) -> &str {
        &self.name
    }
}

#[async_trait]
impl Channel for RpcApprovalChannel {
    fn name(&self) -> &str {
        &self.name
    }

    async fn send(&self, _message: &SendMessage) -> anyhow::Result<()> {
        Ok(())
    }

    async fn listen(&self, _tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        anyhow::bail!("RpcApprovalChannel.listen is not supported")
    }

    async fn request_approval(
        &self,
        recipient: &str,
        request: &ChannelApprovalRequest,
    ) -> anyhow::Result<Option<ChannelApprovalResponse>> {
        self.request_approval_with_timeout(recipient, request, self.approval_timeout)
            .await
    }
}

impl RpcApprovalChannel {
    pub async fn request_approval_with_timeout(
        &self,
        _recipient: &str,
        request: &ChannelApprovalRequest,
        timeout: Duration,
    ) -> anyhow::Result<Option<ChannelApprovalResponse>> {
        let request_id = Uuid::new_v4().to_string();
        let (tx, rx) = tokio::sync::oneshot::channel::<ChannelApprovalResponse>();
        self.pending.insert(request_id.clone(), tx);

        self.rpc
            .notify(
                "session/update",
                json!({
                    "type": "approval_request",
                    "session_id": self.session_id,
                    "request_id": request_id,
                    "tool_name": request.tool_name,
                    "arguments_summary": request.arguments_summary,
                    "timeout_secs": timeout.as_secs(),
                }),
            )
            .await;

        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(response)) => Ok(Some(response)),
            Ok(Err(_)) | Err(_) => Ok(Some(ChannelApprovalResponse::Deny)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tokio::sync::mpsc;
    use zeroclaw_api::channel::{ChannelApprovalRequest, ChannelApprovalResponse};
    use zeroclaw_api::jsonrpc::RpcOutbound;

    fn make_rpc() -> (Arc<RpcOutbound>, mpsc::Receiver<String>) {
        let (tx, rx) = mpsc::channel::<String>(16);
        (Arc::new(RpcOutbound::new(tx)), rx)
    }

    fn make_pending() -> Arc<crate::rpc::context::ApprovalPendingMap> {
        Arc::new(crate::rpc::context::ApprovalPendingMap::default())
    }

    #[tokio::test]
    async fn sends_approval_request_notification_and_awaits_response() {
        let (rpc, mut write_rx) = make_rpc();
        let pending = make_pending();
        let ch = RpcApprovalChannel::new("rpc", "sess-1", Arc::clone(&rpc), Arc::clone(&pending));

        let request = ChannelApprovalRequest {
            tool_name: "shell".to_string(),
            arguments_summary: "ls /tmp".to_string(),
            raw_arguments: None,
        };

        let pending_for_resolve = Arc::clone(&pending);
        let task = zeroclaw_spawn::spawn!(async move { ch.request_approval("", &request).await });

        let line = write_rx.recv().await.unwrap();
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["method"], "session/update");
        assert_eq!(v["params"]["type"], "approval_request");
        assert_eq!(v["params"]["session_id"], "sess-1");
        assert_eq!(v["params"]["tool_name"], "shell");

        let request_id = v["params"]["request_id"].as_str().unwrap().to_string();
        pending_for_resolve.resolve(&request_id, ChannelApprovalResponse::Approve);

        let result = task.await.unwrap().unwrap();
        assert_eq!(result, Some(ChannelApprovalResponse::Approve));
    }

    #[tokio::test]
    async fn times_out_and_auto_denies() {
        let (rpc, _write_rx) = make_rpc();
        let pending = make_pending();
        let ch = RpcApprovalChannel::new("rpc", "sess-1", Arc::clone(&rpc), Arc::clone(&pending));
        let request = ChannelApprovalRequest {
            tool_name: "shell".to_string(),
            arguments_summary: "rm -rf /".to_string(),
            raw_arguments: None,
        };
        let result = ch
            .request_approval_with_timeout("", &request, std::time::Duration::from_millis(50))
            .await
            .unwrap();
        assert_eq!(result, Some(ChannelApprovalResponse::Deny));
    }
}
