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

use super::broker::{ApprovalNoticeKind, ApprovalRouteAdapter, GateNotice};

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
pub fn parse_approval_route(route: &str) -> Option<(&str, &str)> {
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
            let Some((channel_key, _)) = parse_approval_route(route) else {
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

/// Resolve `{{path.to.field}}` placeholders against the notice context: pure
/// dotted lookups into the JSON, nothing else (no logic, no filters). A string
/// value substitutes raw; other values substitute as compact JSON; a missing
/// path substitutes empty. Untrusted values are DATA here — they land in a
/// notification body, never in an instruction stream.
fn render_template(template: &str, context: &serde_json::Value) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else {
            out.push_str(&rest[start..]);
            return out;
        };
        let path = after[..end].trim();
        let mut value = context;
        let mut found = true;
        for key in path.split('.') {
            match value.get(key) {
                Some(v) => value = v,
                None => {
                    found = false;
                    break;
                }
            }
        }
        if found {
            match value {
                serde_json::Value::String(v) => out.push_str(v),
                other => out.push_str(&other.to_string()),
            }
        }
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    out
}

/// Automatic context summary when the step has no authored `- prompt:`: the
/// commonly-present fields of a gate context, compactly.
fn summarize_context(context: &serde_json::Value) -> String {
    let mut lines = Vec::new();
    if let (Some(repo), Some(number)) = (
        context.get("repo").and_then(|v| v.as_str()),
        context.get("number").and_then(|v| v.as_u64()),
    ) {
        lines.push(format!("{repo}#{number}"));
    }
    if let Some(title) = context.get("title").and_then(|v| v.as_str()) {
        lines.push(format!("\u{201c}{title}\u{201d}"));
    }
    if let Some(author) = context
        .get("author")
        .and_then(|a| a.get("login"))
        .and_then(|v| v.as_str())
    {
        lines.push(format!("by {author}"));
    }
    if let Some(body) = context.get("body").and_then(|v| v.as_str()) {
        let trimmed: String = body.chars().take(400).collect();
        let suffix = if body.chars().count() > 400 {
            "\u{2026}"
        } else {
            ""
        };
        lines.push(format!("\n{trimmed}{suffix}"));
    }
    lines.join(" ")
}

/// The reference an answer must carry: the run id, revision-qualified once the
/// gate has been re-presented (`<run_id>#<rev>`). Plain `<run_id>` ≡ revision 0,
/// so pre-revision prompts and habits keep working — and a click on a superseded
/// prompt (older revision) can never resolve the current one.
fn gate_reference(notice: &GateNotice<'_>) -> String {
    if notice.revision == 0 {
        notice.run_id.to_string()
    } else {
        format!("{}#{}", notice.run_id, notice.revision)
    }
}

/// How to answer, appended to every notice.
fn reply_instructions(reference: &str, run_id: &str) -> String {
    format!(
        "Reply `approve {reference}` or `deny {reference}` here, or use \
         `zeroclaw sop approve|deny {run_id}`."
    )
}

/// The notice's CONTEXT body — the header plus WHAT is being approved (the
/// step's authored `- prompt:` rendered over the gate context, or an automatic
/// context summary), WITHOUT the how-to-answer instructions. This is also what
/// a finalized prompt keeps showing (the outcome line appended under it), so
/// the record of what was approved survives resolution in place.
fn render_context(kind: ApprovalNoticeKind, notice: &GateNotice<'_>) -> String {
    let what = match notice.gate_prompt {
        // The `- prompt:` bullet is a single line; a literal `\n` in it is the
        // author's line break.
        Some(template) => render_template(template, notice.context).replace("\\n", "\n"),
        None => summarize_context(notice.context),
    };
    let (run_id, sop_name, step) = (notice.run_id, notice.sop_name, notice.step);
    let header = match kind {
        ApprovalNoticeKind::Request if notice.revision == 0 => {
            format!("SOP approval needed: '{sop_name}' run `{run_id}` (step {step}).")
        }
        ApprovalNoticeKind::Request => format!(
            "SOP approval needed: '{sop_name}' run `{run_id}` (step {step}, revision {}).",
            notice.revision
        ),
        ApprovalNoticeKind::Escalation => format!(
            "SOP approval escalation: '{sop_name}' run `{run_id}` is still waiting at step {step}; its approval timeout elapsed."
        ),
    };
    if what.trim().is_empty() {
        header
    } else {
        format!("{header}\n\n{what}")
    }
}

/// Render the approval-request notice body: the context plus how to answer.
/// The `approve <reference>` text reply resolves the gate via the
/// orchestrator's gate intercept; CLI / gateway keep working.
fn render_notice(kind: ApprovalNoticeKind, notice: &GateNotice<'_>) -> String {
    let context = render_context(kind, notice);
    let instructions = reply_instructions(&gate_reference(notice), notice.run_id);
    format!("{context}\n\n{instructions}")
}

/// Build the native gate prompt for channels that render one (buttons /
/// keyboards). The description carries the text-reply form too, so a screenshot
/// or forward of the prompt is still actionable. Edit/Revise are input-bearing
/// choices: channels with a native form (Discord modal) render them; channels
/// without simply omit them, and approve/deny stay universally answerable.
fn build_gate_prompt(
    kind: ApprovalNoticeKind,
    notice: &GateNotice<'_>,
) -> zeroclaw_api::channel::ChannelGatePrompt {
    use zeroclaw_api::channel::{
        ChannelGatePrompt, GateChoice, GateChoiceEmphasis, GateChoiceInput, GateChoiceKind,
    };
    // Discord embeds cap descriptions at 4096 chars; stay comfortably under.
    let mut description = render_notice(kind, notice);
    if description.chars().count() > 3500 {
        description = description.chars().take(3500).collect::<String>() + "\u{2026}";
    }
    let mut choices = vec![GateChoice {
        id: GateChoiceKind::Approve.id().to_string(),
        label: "Approve".to_string(),
        emphasis: GateChoiceEmphasis::Positive,
        input: None,
    }];
    if let Some(field) = notice.edit_field {
        // Pre-fill with the editable field's current value so the operator
        // starts from the draft, not a blank box.
        let prefill = notice
            .context
            .get(field)
            .and_then(|v| v.as_str())
            .map(str::to_string);
        // A value over Discord's 4000-char text-input cap would be silently
        // truncated into the form and the TRUNCATED text posted as approved —
        // withhold Edit instead (Revise/deny remain; the operator can also
        // resolve out-of-band).
        let oversize = prefill.as_ref().is_some_and(|p| p.chars().count() > 4000);
        if !oversize {
            choices.push(GateChoice {
                id: GateChoiceKind::Edit.id().to_string(),
                label: "Edit".to_string(),
                emphasis: GateChoiceEmphasis::Neutral,
                input: Some(GateChoiceInput {
                    label: format!("Edited {field} (posted as approved)"),
                    prefill,
                }),
            });
        }
    }
    if notice.can_revise {
        choices.push(GateChoice {
            id: GateChoiceKind::Revise.id().to_string(),
            label: "Revise".to_string(),
            emphasis: GateChoiceEmphasis::Neutral,
            input: Some(GateChoiceInput {
                label: "Guidance for the re-draft".to_string(),
                prefill: None,
            }),
        });
    }
    choices.push(GateChoice {
        id: GateChoiceKind::Deny.id().to_string(),
        label: "Deny".to_string(),
        emphasis: GateChoiceEmphasis::Negative,
        input: None,
    });
    // What a RESOLVED prompt keeps showing: the context without the (no longer
    // actionable) reply instructions; the channel appends the outcome line.
    // Capped a little tighter than the live description so the appended
    // outcome still fits Discord's 4096-char embed limit.
    let mut resolved_description = render_context(kind, notice);
    if resolved_description.chars().count() > 3400 {
        resolved_description =
            resolved_description.chars().take(3400).collect::<String>() + "\u{2026}";
    }
    ChannelGatePrompt {
        title: match kind {
            ApprovalNoticeKind::Request => {
                format!("SOP approval needed: {}", notice.sop_name)
            }
            ApprovalNoticeKind::Escalation => {
                format!("SOP approval escalation: {}", notice.sop_name)
            }
        },
        description,
        reference: gate_reference(notice),
        choices,
        resolved_description: Some(resolved_description),
    }
}

/// Build the (channel_key, message) delivery pair from a route + run identity, or an
/// error describing why it can't be built. PURE (no I/O, no spawn) so the parse +
/// message-shaping is unit-testable without a runtime.
fn build_delivery(
    kind: ApprovalNoticeKind,
    route: &str,
    notice: &GateNotice<'_>,
) -> anyhow::Result<(String, SendMessage)> {
    let Some((channel_key, recipient)) = parse_approval_route(route) else {
        anyhow::bail!(
            "approval route '{route}' is not 'channel:recipient' (e.g. \
             'discord.ops:123456789') - both halves must be non-empty"
        );
    };
    let msg = SendMessage::new(render_notice(kind, notice), recipient).suppress_voice();
    Ok((channel_key.to_string(), msg))
}

impl ApprovalRouteAdapter for ChannelRouteAdapter {
    fn deliver(
        &self,
        kind: ApprovalNoticeKind,
        route: &str,
        notice: &GateNotice<'_>,
    ) -> anyhow::Result<()> {
        let (channel_key, msg) = build_delivery(kind, route, notice)?;
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
        // Native gate prompt first (buttons / keyboards, answered through the
        // channel's inbound path); channels without one fall back to the text
        // notice, whose `approve <run_id>` reply the orchestrator also resolves.
        let prompt = build_gate_prompt(kind, notice);
        let recipient = msg.recipient.clone();
        let run_id = notice.run_id.to_string();
        let route = route.to_string();
        self.handle.spawn(async move {
            let prompted = match channel.send_gate_prompt(&recipient, &prompt).await {
                Ok(prompted) => prompted,
                Err(e) => {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({
                                "route": route, "run_id": run_id, "error": e.to_string()
                            })),
                        "approval route gate prompt failed; falling back to text notice"
                    );
                    false
                }
            };
            if !prompted && let Err(e) = channel.send(&msg).await {
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

    #[test]
    fn template_resolves_dotted_paths_and_drops_missing_ones() {
        let ctx = serde_json::json!({
            "repo": "o/r", "number": 7,
            "author": {"login": "zeroclaw_user"},
            "title": "It broke"
        });
        assert_eq!(
            render_template(
                "Issue {{repo}}#{{number}} by {{author.login}}: {{title}} {{absent.field}}",
                &ctx
            ),
            "Issue o/r#7 by zeroclaw_user: It broke "
        );
        // Unclosed braces pass through untouched rather than panicking.
        assert_eq!(render_template("broken {{oops", &ctx), "broken {{oops");
    }

    #[test]
    fn notice_prefers_the_authored_prompt_and_always_appends_instructions() {
        let ctx = serde_json::json!({"repo": "o/r", "number": 9, "body": "hello"});
        let authored = GateNotice {
            run_id: "run-9",
            sop_name: "triage",
            step: 1,
            context: &ctx,
            gate_prompt: Some("Review {{repo}}#{{number}} please"),
            revision: 0,
            edit_field: None,
            can_revise: false,
        };
        let text = render_notice(ApprovalNoticeKind::Request, &authored);
        assert!(text.contains("Review o/r#9 please"));
        assert!(text.contains("approve run-9"));

        let auto = GateNotice {
            gate_prompt: None,
            ..authored
        };
        let text = render_notice(ApprovalNoticeKind::Request, &auto);
        assert!(
            text.contains("o/r#9"),
            "auto summary carries repo#number: {text}"
        );
        assert!(
            text.contains("hello"),
            "auto summary carries the body: {text}"
        );
    }
    #[test]
    fn gate_prompt_offers_edit_and_revise_with_prefill_and_versioned_reference() {
        let ctx = serde_json::json!({"body": "the model draft", "repo": "o/r"});
        let notice = GateNotice {
            run_id: "run-42",
            sop_name: "triage",
            step: 3,
            context: &ctx,
            gate_prompt: None,
            revision: 0,
            edit_field: Some("body"),
            can_revise: true,
        };
        let prompt = build_gate_prompt(ApprovalNoticeKind::Request, &notice);
        assert_eq!(
            prompt.reference, "run-42",
            "revision 0 keeps a bare reference"
        );
        let ids: Vec<&str> = prompt.choices.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, ["approve", "edit", "revise", "deny"]);
        let edit = prompt.choices.iter().find(|c| c.id == "edit").unwrap();
        assert_eq!(
            edit.input.as_ref().unwrap().prefill.as_deref(),
            Some("the model draft"),
            "edit pre-fills from the declared field"
        );
        let revise = prompt.choices.iter().find(|c| c.id == "revise").unwrap();
        assert!(revise.input.as_ref().unwrap().prefill.is_none());
        assert!(prompt.choices[0].input.is_none(), "approve stays plain");

        // Revision > 0: the reference (and the text-reply instructions) carry it,
        // so an answer on the superseded prompt can never resolve this one.
        let revised = GateNotice {
            revision: 2,
            ..notice
        };
        let prompt = build_gate_prompt(ApprovalNoticeKind::Request, &revised);
        assert_eq!(prompt.reference, "run-42#2");
        assert!(
            prompt.description.contains("approve run-42#2"),
            "text-reply instructions must name the versioned reference: {}",
            prompt.description
        );
        // The resolved body keeps WHAT was approved but drops the (no longer
        // actionable) reply instructions — the channel appends the outcome.
        let resolved = prompt.resolved_description.as_deref().unwrap();
        assert!(
            resolved.contains("the model draft"),
            "resolved body keeps the context: {resolved}"
        );
        assert!(
            !resolved.contains("Reply `approve"),
            "resolved body must not re-show the reply instructions: {resolved}"
        );

        // No edit declaration, no revisable predecessor → plain approve/deny.
        let plain = GateNotice {
            edit_field: None,
            can_revise: false,
            ..notice
        };
        let ids: Vec<String> = build_gate_prompt(ApprovalNoticeKind::Request, &plain)
            .choices
            .iter()
            .map(|c| c.id.clone())
            .collect();
        assert_eq!(ids, ["approve", "deny"]);
    }

    use async_trait::async_trait;
    use std::sync::Mutex;
    use zeroclaw_api::attribution::{Attributable, ChannelKind, Role};
    use zeroclaw_api::channel::ChannelMessage;

    // ── pure build_delivery / parse_approval_route ───────────────

    #[test]
    fn parse_approval_route_splits_on_first_colon_and_keeps_colons_in_recipient() {
        assert_eq!(
            parse_approval_route("discord.ops:12345"),
            Some(("discord.ops", "12345"))
        );
        // A Matrix-style recipient with its own ':' survives on the right.
        assert_eq!(
            parse_approval_route("matrix.main:@alice:server.example"),
            Some(("matrix.main", "@alice:server.example"))
        );
    }

    #[test]
    fn parse_approval_route_rejects_missing_or_empty_halves() {
        assert_eq!(parse_approval_route("discord.ops"), None, "no recipient");
        assert_eq!(
            parse_approval_route("discord.ops:"),
            None,
            "empty recipient"
        );
        assert_eq!(parse_approval_route(":12345"), None, "empty channel key");
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
            &GateNotice {
                run_id: "run-7",
                sop_name: "triage",
                step: 3,
                context: &serde_json::Value::Null,
                gate_prompt: None,
                revision: 0,
                edit_field: None,
                can_revise: false,
            },
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
        let notice = GateNotice {
            run_id: "run-7",
            sop_name: "triage",
            step: 3,
            context: &serde_json::Value::Null,
            gate_prompt: None,
            revision: 0,
            edit_field: None,
            can_revise: false,
        };
        let request = render_notice(ApprovalNoticeKind::Request, &notice);
        let escalation = render_notice(ApprovalNoticeKind::Escalation, &notice);
        assert!(request.contains("approval needed"));
        assert!(escalation.contains("approval escalation"));
        assert!(escalation.contains("timeout elapsed"));
        assert_ne!(request, escalation);
    }

    #[test]
    fn build_delivery_errors_on_a_route_without_a_recipient() {
        assert!(
            build_delivery(
                ApprovalNoticeKind::Request,
                "discord.ops",
                &GateNotice {
                    run_id: "r",
                    sop_name: "s",
                    step: 1,
                    context: &serde_json::Value::Null,
                    gate_prompt: None,
                    revision: 0,
                    edit_field: None,
                    can_revise: false,
                }
            )
            .is_err()
        );
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
                &GateNotice {
                    run_id: "run-1",
                    sop_name: "triage",
                    step: 1,
                    context: &serde_json::Value::Null,
                    gate_prompt: None,
                    revision: 0,
                    edit_field: None,
                    can_revise: false,
                },
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
                &GateNotice {
                    run_id: "run-7",
                    sop_name: "triage",
                    step: 3,
                    context: &serde_json::Value::Null,
                    gate_prompt: None,
                    revision: 0,
                    edit_field: None,
                    can_revise: false,
                },
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
                &GateNotice {
                    run_id: "run-7",
                    sop_name: "triage",
                    step: 3,
                    context: &serde_json::Value::Null,
                    gate_prompt: None,
                    revision: 0,
                    edit_field: None,
                    can_revise: false,
                },
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
                    &GateNotice {
                        run_id: "run-7",
                        sop_name: "triage",
                        step: 3,
                        context: &serde_json::Value::Null,
                        gate_prompt: None,
                        revision: 0,
                        edit_field: None,
                        can_revise: false,
                    },
                )
                .is_err(),
            "a route with no ':recipient' cannot be delivered"
        );
    }
}
