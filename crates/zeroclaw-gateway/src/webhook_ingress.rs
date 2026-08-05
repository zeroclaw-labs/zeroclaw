//! Authenticated webhook ingress: the typed boundary between transport-level
//! request verification and shared gateway-webhook dispatch.
//!
//! Channel webhook handlers used to treat "this request was verified" as a
//! per-handler convention: each handler carried its own signature check and
//! its own copy of the parse, autosave, chat, reply lifecycle, and nothing
//! stopped a handler from skipping verification or treating a missing secret
//! as optional. This module makes the requirement structural:
//!
//! - [`VerifiedWebhookIngress`] is an unforgeable proof value. Its fields are
//!   private, it implements neither `Clone` nor `Default`, and the only way to
//!   obtain one is a successful [`authenticate`] call.
//! - [`authenticate`] owns the fail-closed credential policy: a missing,
//!   blank, or unresolvable required credential refuses the request before
//!   any payload byte is parsed. The provider-specific signature algorithm
//!   stays with the transport adapter as a closure, but the closure only runs
//!   after the credential resolves, and only over the exact bytes the proof
//!   will carry.
//! - [`dispatch_verified_webhook`] is the shared gateway-webhook helper for
//!   the current inbound log, session key, autosave, agent chat, and
//!   reply/error-delivery path. It consumes the proof, so a webhook message
//!   cannot enter agent dispatch without a successful verification result for
//!   the same request body. It remains an intermediate boundary until gateway
//!   webhooks enter the shared channel turn lifecycle.
//! - [`MESSAGE_DISPATCHING_WEBHOOKS`] is the canonical registry of every
//!   message-dispatching webhook adapter and its authentication mode.
//!   [`authenticate`] refuses specs that are not registered, and the drift
//!   guard tests derive the allowed route surface from this registry.
//!
//! Transport adapters keep ownership of route and alias resolution, signature
//! algorithms and header formats, challenge/verification endpoints, body
//! decoding, payload parsing, and HTTP response policy for non-authentication
//! failures. This module owns only the trust decision and the current
//! gateway-specific post-trust dispatch path.

use std::sync::Arc;

use axum::body::Bytes;
use axum::http::{HeaderMap, StatusCode};
use axum::response::Json;

use zeroclaw_api::channel::{Channel, ChannelMessage, SendMessage};
use zeroclaw_memory::MemoryCategory;

use crate::{
    AppState, GatewayChatOutcome, is_needs_quickstart_err, needs_quickstart_channel_reply,
    run_gateway_chat_with_tools, sender_session_id,
};

/// How a message-dispatching webhook adapter authenticates inbound requests.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum CredentialPolicy {
    /// The adapter requires a per-alias secret and verifies every request
    /// against it. `display` names the config field in refusal responses.
    Required { display: &'static str },
    /// The adapter has no inbound credential mechanism at all, so no request
    /// can ever be verified and ingress always refuses. This exists so a
    /// channel awaiting removal stays declared and fail-closed instead of
    /// silently unauthenticated; it is not a mode new adapters may choose.
    Unconfigurable { reason: &'static str },
}

/// One message-dispatching webhook adapter's ingress contract.
///
/// This registry entry is the canonical place where an adapter declares its
/// authentication mode. Handlers reference their spec when authenticating,
/// [`authenticate`] refuses unregistered specs, and the drift guard tests
/// cross-check the gateway route table against `dispatch_routes`.
#[derive(Debug)]
pub(crate) struct WebhookAdapterSpec {
    /// Stable channel id used in log attributes and session keys.
    pub(crate) channel: &'static str,
    /// Human-readable name used in operator-facing log text.
    pub(crate) display_name: &'static str,
    /// Fail-closed credential policy for inbound requests.
    pub(crate) credential: CredentialPolicy,
    /// The header carrying the request signature, when the scheme has one.
    /// Only used to report "missing" vs "invalid" in refusal logs.
    pub(crate) signature_header: Option<&'static str>,
    /// Router paths (POST) whose requests dispatch inbound messages.
    #[cfg(test)]
    pub(crate) dispatch_routes: &'static [&'static str],
}

pub(crate) static WHATSAPP_WEBHOOK: WebhookAdapterSpec = WebhookAdapterSpec {
    channel: "whatsapp",
    display_name: "WhatsApp",
    credential: CredentialPolicy::Required {
        display: "app_secret",
    },
    signature_header: Some("X-Hub-Signature-256"),
    #[cfg(test)]
    dispatch_routes: &["/whatsapp", "/whatsapp/{alias}"],
};

pub(crate) static LINQ_WEBHOOK: WebhookAdapterSpec = WebhookAdapterSpec {
    channel: "linq",
    display_name: "Linq",
    credential: CredentialPolicy::Required {
        display: "signing_secret",
    },
    signature_header: Some("X-Webhook-Signature"),
    #[cfg(test)]
    dispatch_routes: &["/linq", "/linq/{alias}"],
};

pub(crate) static WATI_WEBHOOK: WebhookAdapterSpec = WebhookAdapterSpec {
    channel: "wati",
    display_name: "WATI",
    credential: CredentialPolicy::Unconfigurable {
        reason: "inbound webhooks cannot be authenticated; refusing to dispatch",
    },
    signature_header: None,
    #[cfg(test)]
    dispatch_routes: &["/wati", "/wati/{alias}"],
};

pub(crate) static NEXTCLOUD_TALK_WEBHOOK: WebhookAdapterSpec = WebhookAdapterSpec {
    channel: "nextcloud_talk",
    display_name: "Nextcloud Talk",
    credential: CredentialPolicy::Required {
        display: "bot secret",
    },
    signature_header: Some("X-Nextcloud-Talk-Signature"),
    #[cfg(test)]
    dispatch_routes: &["/nextcloud-talk", "/nextcloud-talk/{alias}"],
};

/// Canonical registry of every message-dispatching webhook adapter.
///
/// Adding a webhook route that dispatches inbound messages requires adding
/// its spec here: [`authenticate`] asserts membership at runtime and the
/// drift guard fails when the route table and this registry disagree.
pub(crate) static MESSAGE_DISPATCHING_WEBHOOKS: [&WebhookAdapterSpec; 4] = [
    &WHATSAPP_WEBHOOK,
    &LINQ_WEBHOOK,
    &WATI_WEBHOOK,
    &NEXTCLOUD_TALK_WEBHOOK,
];

/// Why an inbound webhook request was refused before dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IngressRefusal {
    /// The adapter requires a credential and none is configured (missing,
    /// blank, or unresolved) for the target alias.
    MissingCredential,
    /// The adapter has no credential mechanism, so verification is
    /// impossible by construction.
    Unverifiable,
    /// A credential is configured but the request failed verification.
    InvalidSignature,
}

impl IngressRefusal {
    /// The transport response for a refused request. Deliberately terse:
    /// refusal bodies never echo request contents or credential material.
    pub(crate) fn into_response(
        self,
        spec: &WebhookAdapterSpec,
    ) -> (StatusCode, Json<serde_json::Value>) {
        let error = match (self, &spec.credential) {
            (IngressRefusal::InvalidSignature, _) => "Invalid signature".to_string(),
            (IngressRefusal::Unverifiable, CredentialPolicy::Unconfigurable { reason }) => {
                format!("{}: {}", spec.channel, reason)
            }
            // An `Unverifiable` refusal from a `Required` adapter (or a
            // missing-credential refusal in general) reports the credential
            // the operator must configure.
            (_, credential) => {
                let display = match credential {
                    CredentialPolicy::Required { display } => display,
                    CredentialPolicy::Unconfigurable { .. } => "credential",
                };
                format!(
                    "{}: no {} configured; refusing to accept an unverified webhook",
                    spec.channel, display
                )
            }
        };
        (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({ "error": error })),
        )
    }
}

/// Emit the refusal log. One WARN per refused request, shaped so operators
/// can tell authentication refusals apart from normal inbound dispatch and
/// from delivery failures. Never logs body bytes or credential material.
fn log_refusal(
    spec: &WebhookAdapterSpec,
    alias: &str,
    refusal: IngressRefusal,
    signature_state: Option<&'static str>,
) {
    let reason = match refusal {
        IngressRefusal::MissingCredential => "missing_credential",
        IngressRefusal::Unverifiable => "unverifiable",
        IngressRefusal::InvalidSignature => "invalid_signature",
    };
    let mut attrs = serde_json::json!({
        "channel": spec.channel,
        "alias": alias,
        "reason": reason,
    });
    if let (Some(map), Some(signature_state)) = (attrs.as_object_mut(), signature_state) {
        map.insert(
            "signature".to_string(),
            serde_json::Value::from(signature_state),
        );
    }
    ::zeroclaw_log::record!(
        WARN,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
            .with_attrs(attrs),
        "webhook ingress refused: request not authenticated"
    );
}

/// Proof that one inbound webhook request passed its adapter's required
/// verification. Carries the exact bytes that were verified so the handler
/// parses what was authenticated, not a second copy. Cannot be constructed
/// outside this module, cannot be cloned, and is consumed by
/// [`dispatch_verified_webhook`], so one proof feeds at most one dispatch.
pub(crate) struct VerifiedWebhookIngress {
    spec: &'static WebhookAdapterSpec,
    alias: String,
    body: Bytes,
}

impl VerifiedWebhookIngress {
    /// The verified request body. Parse payloads from this, never from a
    /// separately retained copy of the request.
    pub(crate) fn body(&self) -> &[u8] {
        &self.body
    }
}

/// Authenticate one inbound webhook request against its adapter's registered
/// credential policy.
///
/// Fail-closed order, enforced structurally:
/// 1. the spec must be registered in [`MESSAGE_DISPATCHING_WEBHOOKS`];
/// 2. an [`CredentialPolicy::Unconfigurable`] adapter refuses immediately;
/// 3. a required credential that is missing, blank, or unresolved refuses
///    without running the verifier;
/// 4. only then does the adapter's `verify` closure run, with the resolved
///    credential and the exact body bytes the proof will carry.
///
/// The closure owns the provider-specific algorithm (headers, HMAC scheme,
/// timestamp rules). Returning `true` mints the proof for `body`.
pub(crate) fn authenticate(
    spec: &'static WebhookAdapterSpec,
    alias: &str,
    secret: Option<&str>,
    headers: &HeaderMap,
    body: Bytes,
    verify: impl FnOnce(&str, &HeaderMap, &[u8]) -> bool,
) -> Result<VerifiedWebhookIngress, IngressRefusal> {
    // Invariant: only registered adapters can mint ingress proofs. A spec
    // constructed outside the registry is a wiring bug, not a request-time
    // condition, so this is an assert rather than a refusal.
    assert!(
        MESSAGE_DISPATCHING_WEBHOOKS
            .iter()
            .any(|registered| std::ptr::eq(*registered, spec)),
        "webhook adapter spec for '{}' is not in MESSAGE_DISPATCHING_WEBHOOKS; \
         register it before authenticating requests with it",
        spec.channel
    );

    let display_secret = match &spec.credential {
        CredentialPolicy::Unconfigurable { .. } => {
            log_refusal(spec, alias, IngressRefusal::Unverifiable, None);
            return Err(IngressRefusal::Unverifiable);
        }
        CredentialPolicy::Required { .. } => secret.map(str::trim).filter(|s| !s.is_empty()),
    };

    let Some(secret) = display_secret else {
        log_refusal(spec, alias, IngressRefusal::MissingCredential, None);
        return Err(IngressRefusal::MissingCredential);
    };

    if !verify(secret, headers, &body) {
        let signature_state = spec.signature_header.map(|header| {
            if headers.get(header).is_some() {
                "invalid"
            } else {
                "missing"
            }
        });
        log_refusal(
            spec,
            alias,
            IngressRefusal::InvalidSignature,
            signature_state,
        );
        return Err(IngressRefusal::InvalidSignature);
    }

    Ok(VerifiedWebhookIngress {
        spec,
        alias: alias.to_string(),
        body,
    })
}

/// Refuse a request for an adapter whose credential policy is
/// [`CredentialPolicy::Unconfigurable`]. Such an adapter can never mint a
/// proof, so its handler refuses directly instead of carrying an
/// unreachable dispatch path.
pub(crate) fn refuse_unverifiable(
    spec: &'static WebhookAdapterSpec,
    alias: &str,
) -> (StatusCode, Json<serde_json::Value>) {
    assert!(
        matches!(spec.credential, CredentialPolicy::Unconfigurable { .. }),
        "refuse_unverifiable is only for adapters without a credential \
         mechanism; '{}' declares a required credential and must call \
         authenticate instead",
        spec.channel
    );
    log_refusal(spec, alias, IngressRefusal::Unverifiable, None);
    IngressRefusal::Unverifiable.into_response(spec)
}

/// Whether the shared gateway-webhook helper blocks the response on dispatch.
pub(crate) enum WebhookDispatchMode {
    /// Process every message before acknowledging the webhook.
    Synchronous,
    /// Acknowledge immediately and process each message in a background
    /// task, for providers that cancel slow webhook deliveries.
    FastAck,
}

/// Handler-supplied wiring for the shared gateway-webhook dispatch helper.
pub(crate) struct WebhookDispatchContext {
    /// Reply-delivery channel for agent responses and error fallbacks.
    pub(crate) channel: Arc<dyn Channel>,
    /// Autosave key derivation for inbound messages.
    pub(crate) memory_key: fn(&ChannelMessage) -> String,
    /// Configured agent alias override; `None` uses the gateway default
    /// resolution.
    pub(crate) agent_override: Option<String>,
    /// Response-blocking behavior.
    pub(crate) mode: WebhookDispatchMode,
}

/// The shared gateway-webhook dispatch helper: inbound log, session key,
/// autosave, agent chat, reply delivery, and quickstart/error fallback.
///
/// Consuming [`VerifiedWebhookIngress`] is the contract: this is the only
/// path from a channel webhook into gateway agent chat, and it is
/// unreachable without a successful [`authenticate`] result for the request.
/// This helper still uses the gateway chat path; it does not replace the
/// shared channel turn lifecycle.
pub(crate) async fn dispatch_verified_webhook(
    state: &AppState,
    ingress: VerifiedWebhookIngress,
    messages: Vec<ChannelMessage>,
    ctx: WebhookDispatchContext,
) -> (StatusCode, Json<serde_json::Value>) {
    let VerifiedWebhookIngress { spec, alias, .. } = ingress;

    match ctx.mode {
        WebhookDispatchMode::Synchronous => {
            for msg in &messages {
                process_verified_message(
                    state,
                    spec,
                    &alias,
                    ctx.channel.as_ref(),
                    ctx.memory_key,
                    ctx.agent_override.as_deref(),
                    msg,
                )
                .await;
            }
        }
        WebhookDispatchMode::FastAck => {
            // The provider cancels webhook requests that do not complete
            // quickly; slow local models routinely exceed that. Each message
            // gets its own task so the model call and reply delivery are
            // independent of the acknowledgement.
            for msg in messages {
                let state = state.clone();
                let channel = Arc::clone(&ctx.channel);
                let alias = alias.clone();
                let agent_override = ctx.agent_override.clone();
                let memory_key = ctx.memory_key;
                zeroclaw_spawn::spawn!(async move {
                    process_verified_message(
                        &state,
                        spec,
                        &alias,
                        channel.as_ref(),
                        memory_key,
                        agent_override.as_deref(),
                        &msg,
                    )
                    .await;
                });
            }
        }
    }

    (StatusCode::OK, Json(serde_json::json!({"status": "ok"})))
}

/// One verified message through the shared gateway-webhook dispatch helper.
async fn process_verified_message(
    state: &AppState,
    spec: &'static WebhookAdapterSpec,
    alias: &str,
    channel: &dyn Channel,
    memory_key: fn(&ChannelMessage) -> String,
    agent_override: Option<&str>,
    msg: &ChannelMessage,
) {
    ::zeroclaw_log::record!(
        INFO,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(
            ::serde_json::json!({
                "channel": spec.channel,
                "alias": alias,
                "sender": msg.sender,
                "content": msg.content,
            })
        ),
        "inbound webhook message"
    );

    let session_id = sender_session_id(spec.channel, msg);

    if state.auto_save && !zeroclaw_memory::should_skip_autosave_content(&msg.content) {
        let key = memory_key(msg);
        let _ = state
            .mem
            .store(
                &key,
                &msg.content,
                MemoryCategory::Conversation,
                Some(&session_id),
            )
            .await;
    }

    match Box::pin(run_gateway_chat_with_tools(
        state,
        &msg.content,
        Some(&session_id),
        agent_override,
    ))
    .await
    {
        Ok(GatewayChatOutcome { response, .. }) => {
            if let Err(e) = channel
                .send(&SendMessage::new(response, &msg.reply_target))
                .await
            {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "channel": spec.channel,
                            "error": format!("{}", e),
                        })),
                    &format!("Failed to send {} reply", spec.display_name)
                );
            }
        }
        Err(e) => {
            let reply = if is_needs_quickstart_err(&e) {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                    &format!(
                        "{} chat refused: gateway has no model configured; visit /quickstart",
                        spec.display_name
                    )
                );
                needs_quickstart_channel_reply()
            } else {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "channel": spec.channel,
                            "error": format!("{}", e),
                        })),
                    "LLM error"
                );
                "Sorry, I couldn't process your message right now.".to_string()
            };
            let _ = channel
                .send(&SendMessage::new(reply, &msg.reply_target))
                .await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn header_map(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for (name, value) in pairs {
            headers.insert(
                axum::http::HeaderName::from_static(name),
                axum::http::HeaderValue::from_str(value).expect("test header value"),
            );
        }
        headers
    }

    #[test]
    fn missing_credential_refuses_before_running_the_verifier() {
        let verifier_ran = Cell::new(false);
        let result = authenticate(
            &LINQ_WEBHOOK,
            "default",
            None,
            &HeaderMap::new(),
            Bytes::from_static(b"{}"),
            |_, _, _| {
                verifier_ran.set(true);
                true
            },
        );
        assert!(matches!(result, Err(IngressRefusal::MissingCredential)));
        assert!(
            !verifier_ran.get(),
            "verifier must not run without a resolved credential"
        );
    }

    #[test]
    fn blank_credential_refuses_before_running_the_verifier() {
        let verifier_ran = Cell::new(false);
        let result = authenticate(
            &LINQ_WEBHOOK,
            "default",
            Some("   "),
            &HeaderMap::new(),
            Bytes::from_static(b"{}"),
            |_, _, _| {
                verifier_ran.set(true);
                true
            },
        );
        assert!(matches!(result, Err(IngressRefusal::MissingCredential)));
        assert!(
            !verifier_ran.get(),
            "a blank credential is not a credential"
        );
    }

    #[test]
    fn unconfigurable_adapter_refuses_even_a_lying_verifier() {
        let result = authenticate(
            &WATI_WEBHOOK,
            "default",
            Some("a-secret-nobody-can-configure"),
            &HeaderMap::new(),
            Bytes::from_static(b"{}"),
            |_, _, _| true,
        );
        assert!(matches!(result, Err(IngressRefusal::Unverifiable)));
    }

    #[test]
    fn failed_verification_refuses() {
        let result = authenticate(
            &LINQ_WEBHOOK,
            "default",
            Some("secret"),
            &HeaderMap::new(),
            Bytes::from_static(b"{}"),
            |_, _, _| false,
        );
        assert!(matches!(result, Err(IngressRefusal::InvalidSignature)));
    }

    #[test]
    fn successful_verification_mints_a_proof_carrying_the_verified_bytes() {
        let body = Bytes::from_static(b"{\"payload\":1}");
        let observed = Cell::new(0usize);
        let verified = authenticate(
            &LINQ_WEBHOOK,
            "work",
            Some(" secret "),
            &header_map(&[("x-webhook-signature", "sha256=abc")]),
            body.clone(),
            |secret, headers, bytes| {
                observed.set(bytes.len());
                assert_eq!(secret, "secret", "credential reaches the verifier trimmed");
                assert!(headers.get("x-webhook-signature").is_some());
                true
            },
        )
        .expect("verification succeeded");
        assert_eq!(observed.get(), body.len());
        assert_eq!(
            verified.body(),
            body.as_ref(),
            "the proof must carry exactly the bytes the verifier saw"
        );
    }

    #[test]
    #[should_panic(expected = "not in MESSAGE_DISPATCHING_WEBHOOKS")]
    fn unregistered_specs_cannot_mint_proofs() {
        static ROGUE: WebhookAdapterSpec = WebhookAdapterSpec {
            channel: "rogue",
            display_name: "Rogue",
            credential: CredentialPolicy::Required { display: "secret" },
            signature_header: None,
            dispatch_routes: &["/rogue"],
        };
        let _ = authenticate(
            &ROGUE,
            "default",
            Some("secret"),
            &HeaderMap::new(),
            Bytes::new(),
            |_, _, _| true,
        );
    }

    #[test]
    fn refusal_responses_are_401_and_never_echo_request_material() {
        let (status, Json(body)) =
            IngressRefusal::MissingCredential.into_response(&NEXTCLOUD_TALK_WEBHOOK);
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(
            body["error"],
            "nextcloud_talk: no bot secret configured; refusing to accept an unverified webhook"
        );

        let (status, Json(body)) = IngressRefusal::InvalidSignature.into_response(&LINQ_WEBHOOK);
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body["error"], "Invalid signature");

        let (status, Json(body)) = IngressRefusal::Unverifiable.into_response(&WATI_WEBHOOK);
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(
            body["error"],
            "wati: inbound webhooks cannot be authenticated; refusing to dispatch"
        );
    }

    // ── Drift guards ──────────────────────────────────────────────────────
    //
    // The registry above is the canonical owner of the webhook ingress
    // surface. These tests derive the allowed gateway route table and the
    // allowed agent-dispatch callsites from it, so a new webhook route or a
    // handler that bypasses the funnel fails loudly here instead of shipping
    // as an unauthenticated dispatch path.

    /// Channel webhook routes that intentionally do not dispatch inbound
    /// messages and therefore sit outside the authenticated-ingress
    /// contract. Every entry needs a reason a reviewer can check.
    const NON_DISPATCHING_WEBHOOK_ROUTES: &[(&str, &str, &str)] = &[
        (
            "/whatsapp",
            "get",
            "Meta webhook verification challenge echo; dispatches nothing",
        ),
        (
            "/whatsapp/{alias}",
            "get",
            "Meta webhook verification challenge echo; dispatches nothing",
        ),
        (
            "/wati",
            "get",
            "Meta-style verification challenge echo; dispatches nothing",
        ),
        (
            "/wati/{alias}",
            "get",
            "Meta-style verification challenge echo; dispatches nothing",
        ),
        (
            "/webhook/gmail",
            "post",
            "poll trigger: the envelope carries a numeric history cursor, \
             never dispatchable content; mail is fetched with operator \
             credentials and sender-allowlisted",
        ),
    ];

    /// Extract `(path, method)` pairs from every `.route("...", method(...))`
    /// registration in a router-construction source region.
    fn parse_routes(region: &str) -> Vec<(String, String)> {
        let mut routes = Vec::new();
        let mut cursor = 0;
        while let Some(found) = region[cursor..].find(".route(") {
            let after = cursor + found + ".route(".len();
            let rest = &region[after..];
            let Some(open_quote) = rest.find('"') else {
                break;
            };
            let path_start = open_quote + 1;
            let Some(path_len) = rest[path_start..].find('"') else {
                break;
            };
            let path = &rest[path_start..path_start + path_len];
            let tail = &rest[path_start + path_len..];
            let method = ["get(", "post(", "put(", "delete(", "patch("]
                .iter()
                .filter_map(|needle| tail.find(needle).map(|at| (at, *needle)))
                .min_by_key(|(at, _)| *at)
                .map(|(_, needle)| needle.trim_end_matches('(').to_string());
            let Some(method) = method else {
                break;
            };
            routes.push((path.to_string(), method));
            cursor = after;
        }
        routes
    }

    #[test]
    fn every_channel_webhook_route_is_classified_by_the_registry() {
        let source = include_str!("lib.rs");
        let start = source.find("fn optional_channel_routes(").expect(
            "gateway router must keep optional_channel_routes as the channel webhook route owner",
        );
        let end = source[start..]
            .find("/// Webhook request body")
            .map(|offset| start + offset)
            .expect("marker after optional_channel_routes moved; update this drift guard");
        let region = &source[start..end];

        let found = parse_routes(region);
        // Tripwire on the scanner itself: if parsing breaks, this count
        // collapses and the guard must fail rather than pass vacuously.
        assert!(
            found.len() >= 13,
            "route scanner found only {} routes in optional_channel_routes; \
             the parser or the source region marker is broken",
            found.len()
        );

        let mut allowed: Vec<(String, String)> = Vec::new();
        for spec in MESSAGE_DISPATCHING_WEBHOOKS {
            for path in spec.dispatch_routes {
                allowed.push(((*path).to_string(), "post".to_string()));
            }
        }
        for (path, method, _reason) in NON_DISPATCHING_WEBHOOK_ROUTES {
            allowed.push(((*path).to_string(), (*method).to_string()));
        }

        for route in &found {
            assert!(
                allowed.contains(route),
                "unclassified channel webhook route {route:?}: declare it in \
                 MESSAGE_DISPATCHING_WEBHOOKS (and authenticate it) or, if it \
                 provably dispatches no inbound message, add it to \
                 NON_DISPATCHING_WEBHOOK_ROUTES with a reason"
            );
        }
        for route in &allowed {
            assert!(
                found.contains(route),
                "registry entry {route:?} has no matching route in \
                 optional_channel_routes; remove the stale entry or restore \
                 the route"
            );
        }
    }

    /// Every gateway source file that mentions the shared agent chat entry
    /// point, with the number of expected mentions. The funnel in this
    /// module is the only permitted webhook-side caller; the others are
    /// operator-authenticated surfaces with their own auth (bearer pairing
    /// for the generic webhook API, agent-card auth for agent-to-agent).
    #[test]
    fn agent_chat_dispatch_callsites_are_pinned_to_authenticated_surfaces() {
        let needle: String = ["run_gateway_chat_with_tools", "("].concat();
        let mut counts: Vec<(String, usize)> = Vec::new();
        let src_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut stack = vec![src_root.clone()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).expect("gateway src dir is readable") {
                let path = entry.expect("readable dir entry").path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.extension().is_some_and(|ext| ext == "rs") {
                    let source = std::fs::read_to_string(&path).expect("readable source file");
                    let hits = source.match_indices(&needle).count();
                    if hits > 0 {
                        let rel = path
                            .strip_prefix(&src_root)
                            .expect("source path under src root")
                            .to_string_lossy()
                            .into_owned();
                        counts.push((rel, hits));
                    }
                }
            }
        }
        counts.sort();

        let expected: Vec<(String, usize)> = vec![
            ("a2a.rs".to_string(), 1),
            ("lib.rs".to_string(), 2),
            ("webhook_ingress.rs".to_string(), 1),
        ];
        assert_eq!(
            counts, expected,
            "agent chat dispatch callsites changed. Channel webhook handlers \
             must reach agent chat only through dispatch_verified_webhook so \
             the authenticated-ingress proof stays mandatory. If you added a \
             legitimate operator-authenticated (non channel-webhook) caller, \
             update this pinned inventory deliberately"
        );
    }

    #[test]
    fn registry_channels_are_unique_and_routes_do_not_overlap() {
        let mut channels: Vec<&str> = MESSAGE_DISPATCHING_WEBHOOKS
            .iter()
            .map(|spec| spec.channel)
            .collect();
        channels.sort_unstable();
        let mut deduped = channels.clone();
        deduped.dedup();
        assert_eq!(channels, deduped, "duplicate channel ids in the registry");

        let mut routes: Vec<&str> = MESSAGE_DISPATCHING_WEBHOOKS
            .iter()
            .flat_map(|spec| spec.dispatch_routes.iter().copied())
            .collect();
        routes.sort_unstable();
        let mut deduped = routes.clone();
        deduped.dedup();
        assert_eq!(routes, deduped, "two adapters claim the same route");
    }
}
