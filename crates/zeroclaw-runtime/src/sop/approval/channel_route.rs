//! Real channel delivery for the approval route adapter (EPIC G follow-up).
//!
//! [`super::broker::NoopRouteAdapter`] only logs; this adapter actually delivers an
//! approval notice to a configured channel (Discord, Slack, ...), so a SOP that
//! parks at a policied gate can reach an approver OUT OF BAND - e.g. a channel-git
//! trigger starts a triage SOP, the SOP parks for approval, and the request lands in
//! a Discord ops channel where a maintainer approves it through the normal HTTP/WS/
//! tool surfaces (which already route back through the broker + chokepoint).
//!
//! Layering: this lives in `zeroclaw-runtime` because it needs only
//! [`zeroclaw_api::channel::Channel`] (a runtime dependency already), never the
//! `zeroclaw-channels` implementations. The DAEMON builds the concrete channel map
//! (via `zeroclaw_channels::orchestrator::build_channel_map`) and injects it here, so
//! there is no runtime -> channels layering inversion.
//!
//! Delivery is best-effort and fire-and-forget: [`ApprovalRouteAdapter::deliver`] is
//! a SYNC call made under the engine `Mutex` (on park, and on the maintenance-tick
//! escalation path), so it cannot `.await`. It spawns the async `Channel::send` onto
//! a tokio [`Handle`] captured at construction and returns immediately. A send
//! failure is logged inside the spawned task; it never blocks or clears the gate -
//! the gate state is the source of truth, the notice is only a courtesy.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::runtime::Handle;
use zeroclaw_api::channel::{Channel, SendMessage};

use super::broker::{ApprovalNoticeKind, ApprovalRouteAdapter};

/// A route adapter that delivers approval notices to configured channels.
///
/// `channels` is keyed by the channel-map key (`<channel>.<alias>` or bare
/// `<channel>`), the same keys `build_channel_map` produces. A route string is
/// `channel_key:recipient` (e.g. `discord.ops:123456789012345678`); the
/// `channel_key` selects the channel, the `recipient` is that channel's addressee.
pub struct ChannelRouteAdapter {
    channels: HashMap<String, Arc<dyn Channel>>,
    handle: Handle,
}

/// A route configuration problem reported at daemon startup before a parked gate
/// would otherwise discover it during delivery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalRouteIssue {
    Malformed {
        policy: String,
        route_kind: &'static str,
        route: String,
    },
    UndeliverableChannel {
        policy: String,
        route_kind: &'static str,
        route: String,
        channel_key: String,
    },
}

impl ChannelRouteAdapter {
    /// Build from a channel map and the tokio runtime handle to spawn sends onto.
    /// The daemon passes `tokio::runtime::Handle::current()` from its async context;
    /// capturing it here (rather than calling `Handle::current()` inside `deliver`)
    /// keeps `deliver` callable from the sync engine without panicking off-runtime.
    pub fn new(channels: HashMap<String, Arc<dyn Channel>>, handle: Handle) -> Self {
        Self { channels, handle }
    }
}

/// Parse a `channel_key:recipient` route into its two non-empty halves. Splits on
/// the FIRST `:` only, so a recipient that itself contains `:` (e.g. a Matrix
/// `@user:server` id) survives intact on the right. Channel-map keys are
/// dot-separated (`discord.ops`), never colon-separated, so the first colon is
/// unambiguously the channel/recipient boundary.
fn parse_route(route: &str) -> Option<(&str, &str)> {
    let (channel_key, recipient) = route.split_once(':')?;
    if channel_key.is_empty() || recipient.is_empty() {
        return None;
    }
    Some((channel_key, recipient))
}

/// The configured approval routes (`request_route` + `escalation_route`) whose channel
/// key is NOT among `channel_keys` - the channels the route adapter can actually deliver
/// to (present in its map AND `supports_outbound_send`). A route names an undeliverable
/// channel when it is absent from the map (e.g. an AMQP SOP-dispatch channel that needs
/// runtime SOP handles the adapter lacks) OR present but inbound-only (its `send` is a
/// no-op). The daemon logs these at STARTUP so the drift between "configured route" and
/// "deliverable channel" surfaces at boot, not silently on the first parked gate.
/// A route must have non-empty `channel:recipient` halves; malformed values are also
/// surfaced at startup. Returns one [`ApprovalRouteIssue`] per invalid route.
pub fn unresolvable_approval_routes(
    approval: &zeroclaw_config::schema::SopApprovalConfig,
    channel_keys: &std::collections::HashSet<String>,
) -> Vec<ApprovalRouteIssue> {
    let mut unresolvable = Vec::new();
    for (policy_name, policy) in &approval.policies {
        for (kind, route) in [
            ("request_route", policy.request_route.as_deref()),
            ("escalation_route", policy.escalation_route.as_deref()),
        ] {
            let Some(route) = route.filter(|r| !r.is_empty()) else {
                continue;
            };
            let Some((channel_key, _)) = parse_route(route) else {
                unresolvable.push(ApprovalRouteIssue::Malformed {
                    policy: policy_name.clone(),
                    route_kind: kind,
                    route: route.to_string(),
                });
                continue;
            };
            if !channel_keys.contains(channel_key) {
                unresolvable.push(ApprovalRouteIssue::UndeliverableChannel {
                    policy: policy_name.clone(),
                    route_kind: kind,
                    route: route.to_string(),
                    channel_key: channel_key.to_string(),
                });
            }
        }
    }
    unresolvable
}

/// Render the approval-request notice body. Identifies the run so an approver can
/// act on it through their normal approval surface; the return path (approve/deny)
/// is the existing HTTP/WS/tool -> broker -> chokepoint flow, not this adapter.
fn render_notice(notice: ApprovalNoticeKind, run_id: &str, sop_name: &str, step: u32) -> String {
    match notice {
        ApprovalNoticeKind::Request => format!(
            "SOP approval needed: '{sop_name}' run `{run_id}` is waiting for approval at step {step}. \
             Approve or deny it through your approval surface (e.g. `zeroclaw sop approve {run_id}` / \
             `zeroclaw sop deny {run_id}`, or the gateway approve/deny route)."
        ),
        ApprovalNoticeKind::Escalation => format!(
            "SOP approval escalation: '{sop_name}' run `{run_id}` is still waiting for approval at \
             step {step}; its approval timeout elapsed. Please approve or deny it through your approval \
             surface (e.g. `zeroclaw sop approve {run_id}` / `zeroclaw sop deny {run_id}`, or the \
             gateway approve/deny route)."
        ),
    }
}

/// Build the (channel_key, message) delivery pair from a route + run identity, or an
/// error describing why it can't be built. PURE (no I/O, no spawn) so the parse +
/// message-shaping is unit-testable without a runtime.
fn build_delivery(
    notice: ApprovalNoticeKind,
    route: &str,
    run_id: &str,
    sop_name: &str,
    step: u32,
) -> anyhow::Result<(String, SendMessage)> {
    let Some((channel_key, recipient)) = parse_route(route) else {
        anyhow::bail!(
            "approval route '{route}' is not 'channel:recipient' (e.g. \
             'discord.ops:123456789') - both halves must be non-empty"
        );
    };
    let msg =
        SendMessage::new(render_notice(notice, run_id, sop_name, step), recipient).suppress_voice();
    Ok((channel_key.to_string(), msg))
}

impl ApprovalRouteAdapter for ChannelRouteAdapter {
    fn deliver(
        &self,
        notice: ApprovalNoticeKind,
        route: &str,
        run_id: &str,
        sop_name: &str,
        step: u32,
    ) -> anyhow::Result<()> {
        let (channel_key, msg) = build_delivery(notice, route, run_id, sop_name, step)?;
        let Some(channel) = self.channels.get(&channel_key).cloned() else {
            // A misconfigured route (names a channel that isn't configured) is a real
            // operator error worth surfacing: return Err so the broker logs it. It
            // still never affects the gate (the broker's deliver_* wrappers only log).
            anyhow::bail!(
                "approval route channel '{channel_key}' is not a configured channel \
                 (route '{route}')"
            );
        };
        // An inbound-only channel's `send` is a no-op that returns `Ok`, so spawning it
        // would report success without delivering anything. Refuse and surface it (the
        // broker logs the Err) rather than silently dropping the notice.
        if !channel.supports_outbound_send() {
            anyhow::bail!(
                "approval route channel '{channel_key}' does not support outbound \
                 delivery (it is inbound-only); its approval notice cannot be sent \
                 (route '{route}')"
            );
        }
        // Fire-and-forget: hand the async send to the runtime and return. The gate is
        // never blocked on channel I/O; a send failure is logged in the task.
        let run_id = run_id.to_string();
        let route = route.to_string();
        self.handle.spawn(async move {
            if let Err(e) = channel.send(&msg).await {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "route": route, "run_id": run_id, "error": e.to_string()
                        })),
                    "approval route channel send failed (gate unaffected)"
                );
            }
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::sync::Mutex;
    use zeroclaw_api::attribution::{Attributable, ChannelKind, Role};
    use zeroclaw_api::channel::ChannelMessage;

    // ── pure build_delivery / parse_route ────────────────────────

    #[test]
    fn parse_route_splits_on_first_colon_and_keeps_colons_in_recipient() {
        assert_eq!(
            parse_route("discord.ops:12345"),
            Some(("discord.ops", "12345"))
        );
        // A Matrix-style recipient with its own ':' survives on the right.
        assert_eq!(
            parse_route("matrix.main:@alice:server.example"),
            Some(("matrix.main", "@alice:server.example"))
        );
    }

    #[test]
    fn parse_route_rejects_missing_or_empty_halves() {
        assert_eq!(parse_route("discord.ops"), None, "no recipient");
        assert_eq!(parse_route("discord.ops:"), None, "empty recipient");
        assert_eq!(parse_route(":12345"), None, "empty channel key");
    }

    #[test]
    fn unresolvable_routes_flag_malformed_and_undeliverable_routes() {
        // Startup validation for the channel-map drift: a route naming a channel the
        // send-only adapter lacks is surfaced; a route naming a present channel is not;
        // a malformed route is surfaced before a gate can discover it at delivery time.
        use std::collections::{HashMap, HashSet};
        use zeroclaw_config::schema::{ApprovalPolicyConfig, SopApprovalConfig};

        let mut policies = HashMap::new();
        policies.insert(
            "prod".to_string(),
            ApprovalPolicyConfig {
                required_group: None,
                quorum: 1,
                // request_route names a channel the adapter cannot construct (an AMQP
                // SOP-dispatch channel); escalation_route names a present channel.
                request_route: Some("amqp.sopq:runs".into()),
                escalation_route: Some("discord.ops:123".into()),
            },
        );
        // A malformed route is also reported at startup, before a gate parks.
        policies.insert(
            "p2".to_string(),
            ApprovalPolicyConfig {
                required_group: None,
                quorum: 1,
                request_route: Some("discord.ops".into()),
                escalation_route: None,
            },
        );
        let approval = SopApprovalConfig {
            groups: HashMap::new(),
            policies,
        };
        let keys: HashSet<String> = ["discord.ops".to_string()].into_iter().collect();

        let un = unresolvable_approval_routes(&approval, &keys);
        assert_eq!(
            un.len(),
            2,
            "the absent channel and malformed route are both flagged, got {un:?}"
        );
        assert!(un.iter().any(|issue| {
            matches!(issue, ApprovalRouteIssue::UndeliverableChannel {
                policy, route_kind: "request_route", route, channel_key
            } if policy == "prod" && route == "amqp.sopq:runs" && channel_key == "amqp.sopq")
        }));
        assert!(un.iter().any(|issue| {
            matches!(issue, ApprovalRouteIssue::Malformed {
                policy, route_kind: "request_route", route
            } if policy == "p2" && route == "discord.ops")
        }));

        // With NO channels at all, every configured route is reported. The daemon runs
        // this before its empty-channel-map early return so the drift is surfaced at boot.
        let empty: HashSet<String> = HashSet::new();
        assert_eq!(
            unresolvable_approval_routes(&approval, &empty).len(),
            3,
            "with no deliverable channels, every configured route is flagged"
        );
    }

    #[test]
    fn build_delivery_shapes_the_message_and_targets_the_recipient() {
        let (key, msg) = build_delivery(
            ApprovalNoticeKind::Request,
            "discord.ops:98765",
            "run-7",
            "triage",
            3,
        )
        .unwrap();
        assert_eq!(key, "discord.ops");
        assert_eq!(msg.recipient, "98765");
        assert!(msg.content.contains("run-7"), "identifies the run");
        assert!(msg.content.contains("triage"), "names the SOP");
        assert!(msg.content.contains("step 3"), "names the step");
        assert!(msg.suppress_voice, "an approval notice must not be voiced");
    }

    #[test]
    fn render_notice_distinguishes_an_escalation_from_the_initial_request() {
        let request = render_notice(ApprovalNoticeKind::Request, "run-7", "triage", 3);
        let escalation = render_notice(ApprovalNoticeKind::Escalation, "run-7", "triage", 3);
        assert!(request.contains("approval needed"));
        assert!(escalation.contains("approval escalation"));
        assert!(escalation.contains("timeout elapsed"));
        assert_ne!(request, escalation);
    }

    #[test]
    fn build_delivery_errors_on_a_route_without_a_recipient() {
        assert!(build_delivery(ApprovalNoticeKind::Request, "discord.ops", "r", "s", 1).is_err());
    }

    // ── a stub Channel that records what it was asked to send ─────

    #[derive(Default)]
    struct RecordingChannel {
        sent: Arc<Mutex<Vec<SendMessage>>>,
    }

    impl Attributable for RecordingChannel {
        fn role(&self) -> Role {
            Role::Channel(ChannelKind::Discord)
        }
        fn alias(&self) -> &str {
            "ops"
        }
    }

    #[async_trait]
    impl Channel for RecordingChannel {
        fn name(&self) -> &str {
            "recording"
        }
        async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
            self.sent.lock().unwrap().push(message.clone());
            Ok(())
        }
        async fn listen(
            &self,
            _tx: tokio::sync::mpsc::Sender<ChannelMessage>,
        ) -> anyhow::Result<()> {
            Ok(())
        }
    }

    /// An inbound-only channel whose `send` no-ops and returns `Ok` (like AMQP): it must
    /// be refused as a route target, not mistaken for a successful delivery.
    struct InboundOnlyChannel;
    impl Attributable for InboundOnlyChannel {
        fn role(&self) -> Role {
            Role::Channel(ChannelKind::Discord)
        }
        fn alias(&self) -> &str {
            "inbound"
        }
    }
    #[async_trait]
    impl Channel for InboundOnlyChannel {
        fn name(&self) -> &str {
            "inbound-only"
        }
        async fn send(&self, _message: &SendMessage) -> anyhow::Result<()> {
            Ok(())
        }
        async fn listen(
            &self,
            _tx: tokio::sync::mpsc::Sender<ChannelMessage>,
        ) -> anyhow::Result<()> {
            Ok(())
        }
        fn supports_outbound_send(&self) -> bool {
            false
        }
    }

    #[tokio::test]
    async fn deliver_errors_for_an_inbound_only_channel() {
        // A present but inbound-only channel (no-op send) must be REFUSED at deliver -
        // returning Err so the broker logs a delivery failure - rather than spawning a
        // send that silently succeeds without delivering the notice.
        let mut channels: HashMap<String, Arc<dyn Channel>> = HashMap::new();
        channels.insert("amqp.q".to_string(), Arc::new(InboundOnlyChannel));
        let adapter = ChannelRouteAdapter::new(channels, Handle::current());
        let err = adapter
            .deliver(
                ApprovalNoticeKind::Request,
                "amqp.q:runs",
                "run-1",
                "triage",
                1,
            )
            .expect_err("an inbound-only channel cannot deliver");
        assert!(
            err.to_string().contains("does not support outbound"),
            "the refusal must name the outbound-delivery gap, got: {err}"
        );
    }

    #[tokio::test]
    async fn deliver_sends_the_notice_to_the_named_channel() {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let channel = Arc::new(RecordingChannel { sent: sent.clone() });
        let mut channels: HashMap<String, Arc<dyn Channel>> = HashMap::new();
        channels.insert("discord.ops".to_string(), channel);
        let adapter = ChannelRouteAdapter::new(channels, Handle::current());

        adapter
            .deliver(
                ApprovalNoticeKind::Request,
                "discord.ops:98765",
                "run-7",
                "triage",
                3,
            )
            .expect("a registered channel delivers");

        // deliver() spawns the send; poll until the recording channel observes it.
        let deadline = std::time::Duration::from_secs(2);
        let got = tokio::time::timeout(deadline, async {
            loop {
                if let Some(m) = sent.lock().unwrap().first().cloned() {
                    break m;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("send task ran within the deadline");
        assert_eq!(got.recipient, "98765");
        assert!(got.content.contains("run-7"));
    }

    #[tokio::test]
    async fn deliver_errors_when_the_route_channel_is_not_configured() {
        let adapter = ChannelRouteAdapter::new(HashMap::new(), Handle::current());
        let err = adapter
            .deliver(
                ApprovalNoticeKind::Request,
                "discord.ops:98765",
                "run-7",
                "triage",
                3,
            )
            .expect_err("an unregistered channel is a real misconfiguration");
        assert!(err.to_string().contains("not a configured channel"));
    }

    #[tokio::test]
    async fn deliver_errors_on_a_malformed_route() {
        let adapter = ChannelRouteAdapter::new(HashMap::new(), Handle::current());
        assert!(
            adapter
                .deliver(
                    ApprovalNoticeKind::Request,
                    "discord.ops",
                    "run-7",
                    "triage",
                    3,
                )
                .is_err(),
            "a route with no ':recipient' cannot be delivered"
        );
    }
}
