//! Pre-persist model liveness probe for Quickstart submissions.
//!
//! Quickstart historically persisted the chosen provider, credential, and
//! model without ever talking to the provider, so a mistyped API key or a
//! wrong model id produced a "successful" first run whose failure surfaced
//! only at the user's first real message (or in a later, user-initiated
//! `zeroclaw doctor`). This module closes that gap: prove the chosen model
//! can actually answer *before* anything is written.
//!
//! ## Where this runs — and deliberately does not
//!
//! The probe stages the submission onto a **clone** of the live config and
//! sends one minimal chat round-trip against the staged provider entry.
//! Staging runs in probe-only mode: it skips personality-file staging, which
//! would create the agent workspace, and the best-effort context-window
//! lookup, which is a second network request. Nothing is persisted, nothing
//! is written to disk, and the chat round-trip is the only network call, so
//! a refused probe leaves the instance byte-identical.
//!
//! Interactive surfaces call it between collecting the submission and
//! calling [`super::apply_with_surface`]. It is **not** wired inside the
//! apply path itself: apply is also driven by the control plane's
//! receipt-bound apply worker, and an approved apply must not acquire a
//! network dependency between approval and commit. Conversational
//! onboarding runs the same probe at the preview stage instead (via
//! [`probe_configured_model`]), where refusing early is cheap.
//!
//! ## Failure semantics
//!
//! - [`LivenessOutcome::AuthOrAccess`] is a *hard* signal: the provider
//!   itself refused the credential or access, so the config as submitted
//!   cannot work. Callers should not persist.
//! - [`LivenessOutcome::ModelRejected`] is also *hard*: the provider says the
//!   model does not exist, or the endpoint returned 404. Callers should not
//!   persist. Other client errors stay soft, because the probe's own request
//!   parameters can draw a 400 from a model that works.
//! - [`LivenessOutcome::Unreachable`] is a *soft* signal: the endpoint may
//!   be down, local-only, or the machine offline. Callers should warn and
//!   let the user decide.
//! - [`LivenessOutcome::NotProbed`] means the probe never ran; the apply
//!   path reports staging problems with full field context, so callers
//!   skip silently rather than double-reporting.

use std::time::Duration;

use zeroclaw_api::model_provider::{ChatMessage, ChatRequest};

use zeroclaw_config::presets::BuilderSubmission;
use zeroclaw_config::schema::Config;

use crate::doctor::{
    ModelProbeOutcome, classify_model_probe_error, create_doctor_model_provider, format_error_chain,
};

use super::{RunCtx, Surface, apply_into};

/// Default ceiling for one probe round-trip. Long enough for a cold
/// provider, short enough that a black-holed endpoint cannot stall an
/// interactive first run.
pub const DEFAULT_PROBE_TIMEOUT: Duration = Duration::from_secs(20);

/// The one-shot probe prompt. Trivial on purpose: the round-trip should be
/// as close to free as the provider allows, and the reply naturally tiny.
const PROBE_PROMPT: &str = "Reply with the single word: OK";

/// What the pre-persist probe learned about the submission's model provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LivenessOutcome {
    /// The staged provider answered a chat round-trip with the configured
    /// model: the credential and model id are live.
    Verified { provider_ref: String, model: String },
    /// The provider refused the credential or access (401/403, bad key,
    /// exhausted quota). Persisting this submission would persist a broken
    /// config.
    AuthOrAccess {
        provider_ref: String,
        model: String,
        detail: String,
    },
    /// The provider answered, but the model does not exist there: a
    /// model-not-found error or a 404 from the endpoint. Persisting this
    /// submission would persist a broken config.
    ModelRejected {
        provider_ref: String,
        model: String,
        detail: String,
    },
    /// The provider could not be reached, or failed in a way that does not
    /// prove the config wrong (DNS, refused connection, timeout, 5xx, or a
    /// client error that does not name the model).
    Unreachable {
        provider_ref: String,
        model: String,
        detail: String,
    },
    /// The probe never ran: the submission does not stage cleanly, or the
    /// staged entry is not probeable. The apply path surfaces those states
    /// itself.
    NotProbed { reason: String },
}

/// Stage `submission` onto a clone of `config` and probe the model provider
/// it resolves to. Never touches the live config or disk.
pub async fn probe_staged_model_liveness(
    submission: &BuilderSubmission,
    config: &Config,
    surface: Surface,
    timeout: Duration,
) -> LivenessOutcome {
    // Stage onto a clone in probe-only mode: no personality files are staged
    // and no context-window lookup runs, so nothing touches disk.
    let ctx = RunCtx::for_probe(surface);
    let mut staged = config.clone();
    let mut staged_files = Vec::new();
    let mut errors = Vec::new();
    let applied = apply_into(
        &mut staged,
        submission,
        &mut staged_files,
        &mut errors,
        Some(&ctx),
    );
    if !errors.is_empty() {
        return LivenessOutcome::NotProbed {
            reason: format!(
                "submission does not stage cleanly ({} validation error{})",
                errors.len(),
                if errors.len() == 1 { "" } else { "s" }
            ),
        };
    }
    let Some(applied) = applied else {
        return LivenessOutcome::NotProbed {
            reason: "staging produced no agent".to_string(),
        };
    };
    let provider_ref = applied.model_provider;
    let Some(model) = staged
        .providers
        .models
        .iter_entries()
        .find(|(ty, alias, _)| format!("{ty}.{alias}") == provider_ref)
        .and_then(|(_, _, entry)| entry.model.clone())
    else {
        return LivenessOutcome::NotProbed {
            reason: format!("`{provider_ref}` has no model configured"),
        };
    };
    probe_configured_model(&staged, &provider_ref, &model, timeout).await
}

/// Probe one configured `provider_ref` + `model` on `config` with a single
/// chat round-trip.
///
/// Public seam for the conversational-onboarding preview stage, which holds
/// a previewed config rather than a builder submission.
pub async fn probe_configured_model(
    config: &Config,
    provider_ref: &str,
    model: &str,
    timeout: Duration,
) -> LivenessOutcome {
    let provider = match create_doctor_model_provider(config, provider_ref) {
        Ok(provider) => provider,
        Err(err) => {
            return LivenessOutcome::NotProbed {
                reason: sanitized_error_chain(&err),
            };
        }
    };
    let messages = [ChatMessage {
        role: "user".to_string(),
        content: PROBE_PROMPT.to_string(),
    }];
    let request = ChatRequest {
        messages: &messages,
        tools: None,
        thinking: None,
    };
    let dispatch = zeroclaw_providers::ProviderDispatch::from_ref(&*provider);
    let chat = dispatch.chat(request, model, Some(0.0));
    match tokio::time::timeout(timeout, chat).await {
        Ok(Ok(_)) => LivenessOutcome::Verified {
            provider_ref: provider_ref.to_string(),
            model: model.to_string(),
        },
        Ok(Err(err)) => outcome_for_probe_error(provider_ref, model, &sanitized_error_chain(&err)),
        Err(_) => LivenessOutcome::Unreachable {
            provider_ref: provider_ref.to_string(),
            model: model.to_string(),
            detail: format!("no answer within {}s", timeout.as_secs()),
        },
    }
}

/// Format an error chain for display with credentials scrubbed.
///
/// `doctor::format_error_chain` concatenates raw causes, and a custom
/// endpoint may carry its credential in the URL query
/// (`https://gateway.example/v1/chat/completions?api_key=...`), which reqwest
/// repeats in connection and DNS errors. Every probe detail reaches stderr, so
/// the whole chain is scrubbed before it can be displayed, recorded, or pasted
/// into a diagnostic.
fn sanitized_error_chain(err: &anyhow::Error) -> String {
    zeroclaw_providers::sanitize_api_error(&format_error_chain(err))
}

/// True when a probe error says the model itself is not available: a 404
/// from the provider's API, or a model-not-found message. Deliberately
/// narrow. A generic 400 or 422 is not included, because the probe's own
/// request parameters can draw one from a model that works, and a false
/// hard stop would lock the user out of setup.
fn is_model_rejection(detail: &str) -> bool {
    let lower = detail.to_lowercase();
    lower.contains("api error (404")
        || lower.contains("model_not_found")
        || lower.contains("model not found")
        || lower.contains("no such model")
        || lower.contains("unknown model")
        || (lower.contains("model") && lower.contains("does not exist"))
}

/// Map a chat probe failure onto the outcome contract. Separated so the
/// hard-versus-soft split, which decides whether persist is blocked, is
/// unit-testable without a network.
fn outcome_for_probe_error(provider_ref: &str, model: &str, detail: &str) -> LivenessOutcome {
    let provider_ref = provider_ref.to_string();
    let model = model.to_string();
    let detail = detail.to_string();
    match classify_model_probe_error(&detail) {
        ModelProbeOutcome::AuthOrAccess => LivenessOutcome::AuthOrAccess {
            provider_ref,
            model,
            detail,
        },
        _ if is_model_rejection(&detail) => LivenessOutcome::ModelRejected {
            provider_ref,
            model,
            detail,
        },
        _ => LivenessOutcome::Unreachable {
            provider_ref,
            model,
            detail,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeroclaw_config::presets::{AgentIdentity, SelectorChoice};

    fn unresolvable_submission() -> BuilderSubmission {
        BuilderSubmission {
            model_provider: SelectorChoice::Existing("anthropic.no-such-alias".to_string()),
            risk_profile: SelectorChoice::Existing("no-such-profile".to_string()),
            runtime_profile: SelectorChoice::Existing("no-such-profile".to_string()),
            memory: SelectorChoice::Existing("no-such-backend".to_string()),
            channels: vec![],
            peer_groups: vec![],
            agent: AgentIdentity {
                name: "liveness-probe-test".to_string(),
                system_prompt: String::new(),
                personality_file: None,
                personality_files: vec![],
            },
        }
    }

    #[tokio::test]
    async fn a_submission_that_does_not_stage_cleanly_is_not_probed() {
        // The probe must never invent a network call (or a verdict) for a
        // submission the apply path is going to refuse anyway.
        let outcome = probe_staged_model_liveness(
            &unresolvable_submission(),
            &Config::default(),
            Surface::Test,
            DEFAULT_PROBE_TIMEOUT,
        )
        .await;
        assert!(
            matches!(outcome, LivenessOutcome::NotProbed { .. }),
            "expected NotProbed, got {outcome:?}"
        );
    }

    #[test]
    fn a_probe_failure_never_displays_an_endpoint_credential() {
        // The disclosure this guards: an existing custom alias can hold its
        // credential in the endpoint query string, and reqwest repeats the
        // full URL in connection and DNS errors. The probe detail is printed
        // to stderr, so the credential must not survive the chain.
        let transport = std::io::Error::other(
            "error sending request for url \
             (https://gateway.example/v1/chat/completions?api_key=super-secret-value)",
        );
        let err = anyhow::Error::new(transport).context("probe request failed");

        // Proves the fixture still reproduces the disclosure, so this test
        // cannot pass vacuously if the scrubbing is removed.
        let raw = format_error_chain(&err);
        assert!(
            raw.contains("super-secret-value"),
            "fixture no longer reaches the unscrubbed path: {raw}"
        );

        let detail = sanitized_error_chain(&err);
        assert!(
            !detail.contains("super-secret-value"),
            "probe detail leaked the endpoint credential: {detail}"
        );

        // The failure still has to be legible, and it still has to classify:
        // scrubbing must not turn a transport failure into an auth verdict.
        assert!(
            detail.contains("probe request failed"),
            "probe detail lost its context: {detail}"
        );
        let outcome = outcome_for_probe_error("custom.gateway", "m", &detail);
        assert!(
            matches!(outcome, LivenessOutcome::Unreachable { .. }),
            "got {outcome:?}"
        );
        if let LivenessOutcome::Unreachable { detail, .. } = outcome {
            assert!(
                !detail.contains("super-secret-value"),
                "outcome detail leaked the endpoint credential: {detail}"
            );
        }
    }

    #[test]
    fn only_auth_shaped_errors_block_a_persist() {
        // The load-bearing split: a credential/access refusal stops a
        // persist; transport and server failures stay a warning. Model
        // rejection, the other hard stop, is covered separately below.
        let auth = outcome_for_probe_error(
            "anthropic.default",
            "m",
            "HTTP 401 Unauthorized: invalid x-api-key",
        );
        assert!(
            matches!(auth, LivenessOutcome::AuthOrAccess { .. }),
            "got {auth:?}"
        );

        let refused = outcome_for_probe_error(
            "anthropic.default",
            "m",
            "error sending request: connection refused",
        );
        assert!(
            matches!(refused, LivenessOutcome::Unreachable { .. }),
            "got {refused:?}"
        );

        let server_error =
            outcome_for_probe_error("anthropic.default", "m", "internal server error (500)");
        assert!(
            matches!(server_error, LivenessOutcome::Unreachable { .. }),
            "got {server_error:?}"
        );
    }

    #[test]
    fn a_missing_model_blocks_a_persist() {
        // A mistyped model id is the other config that cannot work: the
        // provider answers, but says the model is not there.
        let not_found = outcome_for_probe_error(
            "openai.default",
            "gpt-typo",
            "openai API error (404 Not Found): The model `gpt-typo` does not exist",
        );
        assert!(
            matches!(not_found, LivenessOutcome::ModelRejected { .. }),
            "got {not_found:?}"
        );

        let coded = outcome_for_probe_error(
            "custom.gateway",
            "m",
            "custom API error (400 Bad Request): {\"error\":{\"code\":\"model_not_found\"}}",
        );
        assert!(
            matches!(coded, LivenessOutcome::ModelRejected { .. }),
            "got {coded:?}"
        );
    }

    #[test]
    fn a_client_error_that_does_not_name_the_model_stays_soft() {
        // The probe's own parameters can draw a 400 from a model that works
        // (some models reject an explicit temperature). A hard stop here
        // would lock the user out of setup for a config that is fine.
        let param = outcome_for_probe_error(
            "openai.default",
            "o-series",
            "openai API error (400 Bad Request): Unsupported parameter: 'temperature'",
        );
        assert!(
            matches!(param, LivenessOutcome::Unreachable { .. }),
            "got {param:?}"
        );
    }
}
