//! The agent's approval bridge: a synthetic [`Channel`] that fans a single
//! tool-approval request out to every registered `ask_user` back-channel (ACP
//! editor, WebSocket dashboard, …) and returns the first decisive answer.

use std::sync::Arc;

use crate::agent::agent::{RoutedApproval, resolve_routed_approval};
use crate::tools::PerToolChannelHandle;
use zeroclaw_api::channel::{
    AttributedApprovalResponse, Channel, ChannelApprovalRequest, ChannelMessage, SendMessage,
};

pub(crate) struct AskUserApprovalBridge {
    handles: PerToolChannelHandle,
    route: Option<zeroclaw_config::autonomy::ApprovalRoute>,
}

impl AskUserApprovalBridge {
    pub(crate) fn new(
        handles: PerToolChannelHandle,
        route: Option<zeroclaw_config::autonomy::ApprovalRoute>,
    ) -> Self {
        Self { handles, route }
    }
}

impl ::zeroclaw_api::attribution::Attributable for AskUserApprovalBridge {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Channel(::zeroclaw_api::attribution::ChannelKind::Cli)
    }
    fn alias(&self) -> &str {
        "agent-approval-bridge"
    }
}

#[async_trait::async_trait]
impl Channel for AskUserApprovalBridge {
    fn name(&self) -> &str {
        "agent-approval-bridge"
    }

    async fn send(&self, _message: &SendMessage) -> anyhow::Result<()> {
        Ok(())
    }

    async fn listen(&self, _tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        Ok(())
    }

    /// Non-attributed entry point, kept for trait completeness: delegates to
    /// [`Self::request_approval_attributed`] and drops the attribution so the
    /// fan-out logic lives in exactly one place.
    async fn request_approval(
        &self,
        recipient: &str,
        request: &ChannelApprovalRequest,
    ) -> anyhow::Result<Option<zeroclaw_api::channel::ChannelApprovalResponse>> {
        Ok(self
            .request_approval_attributed(recipient, request)
            .await?
            .map(|attributed| attributed.response))
    }

    async fn request_approval_attributed(
        &self,
        recipient: &str,
        request: &ChannelApprovalRequest,
    ) -> anyhow::Result<Option<AttributedApprovalResponse>> {
        if let Some(route) = &self.route {
            match resolve_routed_approval(&self.handles, route, recipient, request).await {
                RoutedApproval::Decided { response, decider } => {
                    return Ok(Some(AttributedApprovalResponse {
                        response,
                        decided_by: decider,
                    }));
                }
                RoutedApproval::Fallthrough => {
                    // explicit InheritOriginator → originating fan-out below
                }
            }
        }

        let channels: Vec<(String, Arc<dyn Channel>)> = self
            .handles
            .read()
            .iter()
            .map(|(name, channel)| (name.clone(), Arc::clone(channel)))
            .collect();
        for (channel_name, channel) in &channels {
            match channel.request_approval(recipient, request).await {
                // The deciding back-channel's name travels back on the response
                // itself, so a concurrent fan-out on the same bridge instance
                // cannot overwrite this call's attribution.
                Ok(Some(response)) => {
                    return Ok(Some(AttributedApprovalResponse {
                        response,
                        decided_by: Some(channel_name.clone()),
                    }));
                }
                Ok(None) => continue,
                Err(e) => {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                            .with_attrs(::serde_json::json!({
                                "tool": request.tool_name,
                                "channel": channel_name,
                                "error": format!("{}", e),
                            })),
                        "channel approval request failed"
                    );
                }
            }
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use parking_lot::RwLock;
    use std::collections::HashMap;
    use zeroclaw_api::channel::ChannelApprovalResponse;

    /// A back-channel that approves only when the recipient matches its
    /// configured target, abstaining (`Ok(None)`) for anything else. Lets a
    /// test make a specific back-channel "win" a fan-out deterministically.
    struct RecipientScopedApprover {
        name: String,
        approves_recipient: String,
    }

    impl ::zeroclaw_api::attribution::Attributable for RecipientScopedApprover {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Channel(
                ::zeroclaw_api::attribution::ChannelKind::Cli,
            )
        }
        fn alias(&self) -> &str {
            &self.name
        }
    }

    #[async_trait::async_trait]
    impl Channel for RecipientScopedApprover {
        fn name(&self) -> &str {
            &self.name
        }
        async fn send(&self, _m: &SendMessage) -> anyhow::Result<()> {
            Ok(())
        }
        async fn listen(
            &self,
            _tx: tokio::sync::mpsc::Sender<ChannelMessage>,
        ) -> anyhow::Result<()> {
            Ok(())
        }
        async fn request_approval(
            &self,
            recipient: &str,
            _r: &ChannelApprovalRequest,
        ) -> anyhow::Result<Option<ChannelApprovalResponse>> {
            if recipient == self.approves_recipient {
                Ok(Some(ChannelApprovalResponse::Approve))
            } else {
                Ok(None)
            }
        }
    }

    fn approver(name: &str, approves_recipient: &str) -> Arc<dyn Channel> {
        Arc::new(RecipientScopedApprover {
            name: name.to_string(),
            approves_recipient: approves_recipient.to_string(),
        })
    }

    fn handles_with(channels: Vec<Arc<dyn Channel>>) -> PerToolChannelHandle {
        let map: HashMap<String, Arc<dyn Channel>> = channels
            .into_iter()
            .map(|c| (c.name().to_string(), c))
            .collect();
        Arc::new(RwLock::new(map))
    }

    fn req() -> ChannelApprovalRequest {
        ChannelApprovalRequest {
            tool_name: "shell".to_string(),
            arguments_summary: "ls".to_string(),
            raw_arguments: None,
        }
    }

    #[tokio::test]
    async fn attributes_decision_to_the_backchannel_that_answered() {
        // "ws" abstains for this recipient and "acp" approves it, so the
        // attribution must name "acp" — proving the deciding surface travels
        // with the response rather than defaulting to the bridge's own name.
        let bridge = AskUserApprovalBridge::new(
            handles_with(vec![approver("ws", "nobody"), approver("acp", "user-A")]),
            None,
        );

        let decided = bridge
            .request_approval_attributed("user-A", &req())
            .await
            .unwrap()
            .expect("a back-channel approved");

        assert_eq!(decided.response, ChannelApprovalResponse::Approve);
        assert_eq!(decided.decided_by.as_deref(), Some("acp"));
    }

    #[tokio::test]
    async fn concurrent_fanouts_keep_their_own_attribution() {
        let bridge = Arc::new(AskUserApprovalBridge::new(
            handles_with(vec![
                approver("chan-A", "user-A"),
                approver("chan-B", "user-B"),
            ]),
            None,
        ));

        let bridge_a = Arc::clone(&bridge);
        let bridge_b = Arc::clone(&bridge);
        let (ra, rb) = tokio::join!(
            zeroclaw_spawn::spawn!(async move {
                bridge_a
                    .request_approval_attributed("user-A", &req())
                    .await
                    .unwrap()
            }),
            zeroclaw_spawn::spawn!(async move {
                bridge_b
                    .request_approval_attributed("user-B", &req())
                    .await
                    .unwrap()
            }),
        );

        let ra = ra.unwrap().expect("chan-A approved user-A");
        let rb = rb.unwrap().expect("chan-B approved user-B");
        assert_eq!(ra.decided_by.as_deref(), Some("chan-A"));
        assert_eq!(rb.decided_by.as_deref(), Some("chan-B"));
    }

    #[tokio::test]
    async fn all_backchannels_abstain_yields_no_decision() {
        let bridge = AskUserApprovalBridge::new(handles_with(vec![approver("ws", "nobody")]), None);
        assert!(
            bridge
                .request_approval_attributed("user-A", &req())
                .await
                .unwrap()
                .is_none()
        );
    }
}
