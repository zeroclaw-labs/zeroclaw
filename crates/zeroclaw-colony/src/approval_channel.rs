use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use zeroclaw_api::attribution::{Attributable, Role};
use zeroclaw_api::channel::{
    Channel, ChannelApprovalRequest, ChannelApprovalResponse, ChannelMessage, SendMessage,
};
use zeroclaw_runtime::control_plane::{TaskRegistry, TaskStatus};

use crate::{ColonyApproval, store::ColonyStore};

/// A caller-owned approval adapter. It never changes the agent's policy and
/// grants only the exact tool invocation currently awaiting its decision.
pub(super) struct ColonyApprovalChannel {
    pub store: Arc<ColonyStore>,
    pub goal_id: String,
    pub agent: String,
}

impl Attributable for ColonyApprovalChannel {
    fn role(&self) -> Role {
        Role::System
    }
    fn alias(&self) -> &str {
        &self.goal_id
    }
}

#[async_trait::async_trait]
impl Channel for ColonyApprovalChannel {
    fn name(&self) -> &str {
        "colony-approval"
    }
    async fn send(&self, _message: &SendMessage) -> Result<()> {
        anyhow::bail!("colony approval adapter does not provide message delivery")
    }
    async fn listen(&self, _tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> Result<()> {
        anyhow::bail!("colony approval adapter has no external listener")
    }
    async fn request_approval(
        &self,
        _recipient: &str,
        request: &ChannelApprovalRequest,
    ) -> Result<Option<ChannelApprovalResponse>> {
        let approval = ColonyApproval {
            id: uuid::Uuid::new_v4().to_string(),
            goal_id: self.goal_id.clone(),
            agent: self.agent.clone(),
            tool: request.tool_name.clone(),
            arguments: request
                .raw_arguments
                .clone()
                .context("approval requires exact tool arguments")?,
            decision: None,
        };
        self.store.append_approval(&approval)?;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(900);
        loop {
            let task = self
                .store
                .get(&self.goal_id)
                .await?
                .context("approval goal no longer exists")?;
            if task.status.is_terminal() {
                return Ok(Some(ChannelApprovalResponse::Deny));
            }
            let current = self
                .store
                .approvals(&self.goal_id)?
                .into_iter()
                .find(|candidate| candidate.id == approval.id)
                .context("approval record vanished")?;
            if let Some(approved) = current.decision {
                if !approved {
                    return Ok(Some(ChannelApprovalResponse::Deny));
                }
                // Explicit pause holds even an approved tool. Resume may
                // continue the same live turn; restart never reconstructs it.
                if task.status == TaskStatus::Running {
                    return Ok(Some(ChannelApprovalResponse::Approve));
                }
            }
            if tokio::time::Instant::now() >= deadline {
                if current.decision.is_none() {
                    self.store
                        .decide_approval(&self.goal_id, &approval.id, false)?;
                }
                return Ok(Some(ChannelApprovalResponse::Deny));
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }
}
