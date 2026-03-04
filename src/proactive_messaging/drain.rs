use super::guardrails;
use super::store;
use super::types::MessagePriority;
use crate::config::ProactiveMessagingConfig;
use std::path::PathBuf;
use std::sync::Arc;

const DRAIN_POLL_INTERVAL_SECS: u64 = 60;

/// Spawn a background tokio task that periodically drains the outbound queue.
///
/// The task:
/// 1. Expires stale messages past TTL
/// 2. If outside quiet hours, delivers pending messages via live channels or
///    the cron `deliver_announcement()` fallback
/// 3. Marks delivered messages as sent
pub fn spawn_drain_task(
    config: Arc<ProactiveMessagingConfig>,
    full_config: Arc<crate::config::Config>,
    workspace_dir: PathBuf,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval =
            tokio::time::interval(std::time::Duration::from_secs(DRAIN_POLL_INTERVAL_SECS));
        loop {
            interval.tick().await;

            if !config.enabled {
                continue;
            }

            // 1. Expire stale messages
            if let Err(e) = store::expire_stale(&workspace_dir) {
                tracing::warn!("proactive_messaging drain: failed to expire stale messages: {e}");
            }

            // 2. Check if we're outside quiet hours
            if guardrails::is_in_quiet_hours(&config.quiet_hours).is_some() {
                continue;
            }

            // 3. Drain pending messages
            let pending = match store::drain_due(&workspace_dir) {
                Ok(msgs) => msgs,
                Err(e) => {
                    tracing::warn!("proactive_messaging drain: failed to fetch pending: {e}");
                    continue;
                }
            };

            for msg in &pending {
                // Check rate limits before delivering
                let decision = guardrails::evaluate_guardrails(
                    &config,
                    &workspace_dir,
                    MessagePriority::Normal,
                );
                if !matches!(decision, super::types::GuardrailDecision::Allowed) {
                    tracing::debug!(
                        "proactive_messaging drain: skipping delivery due to guardrails"
                    );
                    break;
                }

                if let Err(e) =
                    deliver_queued_message(&full_config, &msg.channel, &msg.recipient, &msg.message)
                        .await
                {
                    tracing::warn!(
                        "proactive_messaging drain: failed to deliver msg {}: {e}",
                        msg.id
                    );
                    continue;
                }

                if let Err(e) = store::mark_sent(&workspace_dir, &[&msg.id]) {
                    tracing::warn!(
                        "proactive_messaging drain: failed to mark msg {} as sent: {e}",
                        msg.id
                    );
                }
            }
        }
    })
}

/// Attempt to deliver a single queued message.
///
/// Tries `get_live_channel()` first (works for Signal, WhatsApp Web, etc.),
/// then falls back to `deliver_announcement()` (Telegram, Discord, Slack, etc.).
async fn deliver_queued_message(
    config: &crate::config::Config,
    channel: &str,
    recipient: &str,
    message: &str,
) -> anyhow::Result<()> {
    // Try the live channel registry first
    if let Some(live_ch) = crate::channels::get_live_channel(channel) {
        use crate::channels::SendMessage;
        live_ch.send(&SendMessage::new(message, recipient)).await?;
        return Ok(());
    }

    // Fall back to cron's deliver_announcement (constructs a fresh channel)
    crate::cron::scheduler::deliver_announcement(config, channel, recipient, message).await
}
