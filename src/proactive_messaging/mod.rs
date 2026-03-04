pub mod drain;
pub mod guardrails;
pub mod store;
pub mod types;

#[allow(unused_imports)]
pub use types::{
    GuardrailDecision, GuardrailDenialReason, MessagePriority, MessageStatus, QueuedMessage,
    SendOutcome,
};

use crate::config::ProactiveMessagingConfig;
use std::path::Path;

/// Build a context block describing pending outbound messages for a user.
///
/// This is injected into the LLM conversation so the agent can acknowledge
/// queued messages and optionally clear them via `manage_outbound_queue`.
///
/// Returns an empty string when proactive messaging is disabled or no
/// messages are pending.
pub fn build_queue_context(
    pm_config: &ProactiveMessagingConfig,
    workspace_dir: &Path,
    channel: &str,
    sender: &str,
) -> String {
    if !pm_config.enabled {
        return String::new();
    }

    let pending = match store::pending_for_recipient(workspace_dir, channel, sender) {
        Ok(msgs) => msgs,
        Err(e) => {
            tracing::debug!("proactive_messaging: failed to query pending messages: {e}");
            return String::new();
        }
    };

    if pending.is_empty() {
        return String::new();
    }

    let mut ctx = String::from(
        "[Queued outbound messages]\n\
         The following messages were queued for this user during quiet hours.\n\
         You may address them in your response and then clear them using manage_outbound_queue.\n",
    );

    for msg in &pending {
        let ts = msg.created_at.format("%Y-%m-%d %H:%M UTC");
        ctx.push_str(&format!("- [{}] (queued {ts}): {}\n", msg.id, msg.message));
    }
    ctx.push('\n');

    ctx
}
