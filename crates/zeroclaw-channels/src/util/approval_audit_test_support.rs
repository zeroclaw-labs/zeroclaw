use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::Value;
use zeroclaw_api::session_events::capture::{
    PreparedToolPort, ToolObservationPort, scope_tool_preparation,
};

#[derive(Default)]
pub(crate) struct ApprovalRecorder {
    pub(crate) records: std::sync::Mutex<Vec<(String, Value)>>,
    pub(crate) fail: AtomicBool,
    pub(crate) replies: std::sync::Mutex<
        Vec<(
            zeroclaw_api::session_events::EventActor,
            zeroclaw_api::channel::ChannelApprovalResponse,
            Value,
        )>,
    >,
}

impl ApprovalRecorder {
    pub(crate) async fn capture(self: &Arc<Self>) -> super::ApprovalAudit {
        let prepared: Arc<dyn PreparedToolPort> = self.clone();
        scope_tool_preparation(prepared, async { super::ApprovalAudit::current() }).await
    }
}

#[async_trait::async_trait]
impl PreparedToolPort for ApprovalRecorder {
    async fn verify_action(&self, _name: &str, _arguments: &Value) -> anyhow::Result<()> {
        anyhow::bail!("test_prepared_action_unsupported")
    }

    async fn request_approval(
        &self,
        _: zeroclaw_api::session_events::capture::ApprovalRequestIntent,
    ) -> anyhow::Result<()> {
        anyhow::bail!("test_approval_request_unsupported")
    }

    async fn resolve_approval(
        &self,
        _response: zeroclaw_api::channel::ChannelApprovalResponse,
        _reason: &str,
        _: Option<zeroclaw_api::session_events::EventId>,
        _evidence: Value,
    ) -> anyhow::Result<()> {
        anyhow::bail!("test_approval_resolution_unsupported")
    }

    async fn record_approval_reply(
        &self,
        actor: zeroclaw_api::session_events::EventActor,
        response: zeroclaw_api::channel::ChannelApprovalResponse,
        evidence: Value,
    ) -> anyhow::Result<zeroclaw_api::session_events::EventId> {
        if self.fail.load(Ordering::SeqCst) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "test_approval_append_failed",
            )
            .into());
        }
        self.replies
            .lock()
            .unwrap()
            .push((actor, response, evidence));
        Ok(zeroclaw_api::session_events::EventId::new())
    }

    async fn record_approval_reply_rejected(
        &self,
        classification: String,
        evidence: Value,
    ) -> anyhow::Result<()> {
        if self.fail.load(Ordering::SeqCst) {
            anyhow::bail!("test_approval_append_failed")
        }
        self.records
            .lock()
            .unwrap()
            .push((classification, evidence));
        Ok(())
    }

    async fn invalidate(&self, _reason: &str) -> anyhow::Result<()> {
        anyhow::bail!("test_approval_invalidation_unsupported")
    }

    async fn start(&self) -> anyhow::Result<Arc<dyn ToolObservationPort>> {
        anyhow::bail!("test_tool_start_unsupported")
    }
}
