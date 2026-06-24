//! ACP elicitation primitives.
//!
//! Implements the subset of the ACP `elicitation/create` RFD that
//! ZeroClaw uses for multiple-choice prompts. See
//! `docs/superpowers/specs/2026-06-24-acp-elicitation-multiple-choice-design.md`
//! for the design rationale.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Capability block parsed from `initialize.clientCapabilities.elicitation`.
///
/// Per the RFD's backward-compat rule, an empty object (`{}`) is
/// treated as form-only. A missing parent key is treated as no
/// support (both `false`).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ElicitationCapabilities {
    pub form: bool,
    pub url: bool,
}

impl ElicitationCapabilities {
    /// Parse the `clientCapabilities.elicitation` JSON value.
    /// Pass `None` when the parent key is absent.
    ///
    /// Sub-key presence is checked structurally — `{"form": {}}` and
    /// `{"form": null}` both count as advertised. ACP itself encodes
    /// sub-capabilities as objects (`"form": {}`) and has no "disabled"
    /// shape, so we don't try to inspect the sub-value's type.
    pub fn from_value(v: Option<&Value>) -> Self {
        let Some(v) = v else {
            return Self::default();
        };
        let Some(obj) = v.as_object() else {
            return Self::default();
        };
        if obj.is_empty() {
            // RFD backward-compat: empty object == form only.
            return Self {
                form: true,
                url: false,
            };
        }
        Self {
            form: obj.contains_key("form"),
            url: obj.contains_key("url"),
        }
    }
}

/// Elicitation transport mode.
///
/// Phase 1 callers only ever emit `Form`. `Url` is defined so the wire
/// types are complete and so a stray future caller compiles, but the
/// send-site in `AcpChannel` asserts the mode is `Form`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ElicitationMode {
    Form,
    Url,
}

/// Params for an outbound `elicitation/create` JSON-RPC request.
///
/// Only the session-scoped variant is modeled — Phase 1 has no
/// caller for request-scoped elicitation (auth/config phase).
#[derive(Debug, Clone, Serialize)]
pub struct ElicitationRequest {
    #[serde(rename = "sessionId")]
    pub session_id: String,
    pub mode: ElicitationMode,
    pub message: String,
    #[serde(rename = "requestedSchema")]
    pub requested_schema: Value,
}

/// Response to an `elicitation/create` request.
///
/// Three-action model per the RFD. `Decline` and `Cancel` both
/// collapse to `Ok(None)` at the `Channel::request_choice` layer
/// in Phase 1 — see the design spec's "Open Questions" for the
/// rationale on deferring the distinction.
#[derive(Debug, Deserialize)]
#[serde(tag = "action", rename_all = "lowercase")]
pub enum ElicitationResponse {
    Accept { content: Value },
    Decline,
    Cancel,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn missing_key_is_no_support() {
        let caps = ElicitationCapabilities::from_value(None);
        assert!(!caps.form);
        assert!(!caps.url);
    }

    #[test]
    fn empty_object_is_form_only_per_rfd_compat() {
        let v = json!({});
        let caps = ElicitationCapabilities::from_value(Some(&v));
        assert!(caps.form);
        assert!(!caps.url);
    }

    #[test]
    fn form_only() {
        let v = json!({ "form": {} });
        let caps = ElicitationCapabilities::from_value(Some(&v));
        assert!(caps.form);
        assert!(!caps.url);
    }

    #[test]
    fn url_only() {
        let v = json!({ "url": {} });
        let caps = ElicitationCapabilities::from_value(Some(&v));
        assert!(!caps.form);
        assert!(caps.url);
    }

    #[test]
    fn both() {
        let v = json!({ "form": {}, "url": {} });
        let caps = ElicitationCapabilities::from_value(Some(&v));
        assert!(caps.form);
        assert!(caps.url);
    }

    #[test]
    fn non_object_is_no_support() {
        let v = json!("nonsense");
        let caps = ElicitationCapabilities::from_value(Some(&v));
        assert!(!caps.form);
        assert!(!caps.url);
    }

    #[test]
    fn form_with_null_value_is_still_support() {
        // Pin the structural-presence interpretation: ACP encodes
        // sub-capabilities as objects, but a forgiving parser should
        // accept `null` too rather than silently dropping support.
        let v = json!({ "form": null });
        let caps = ElicitationCapabilities::from_value(Some(&v));
        assert!(caps.form);
        assert!(!caps.url);
    }

    #[test]
    fn request_serializes_with_camelcase_keys() {
        let req = ElicitationRequest {
            session_id: "sess_1".to_string(),
            mode: ElicitationMode::Form,
            message: "Pick one".to_string(),
            requested_schema: json!({ "type": "object" }),
        };
        let v = serde_json::to_value(&req).unwrap();
        assert_eq!(v["sessionId"], "sess_1");
        assert_eq!(v["mode"], "form");
        assert_eq!(v["message"], "Pick one");
        assert!(v["requestedSchema"].is_object());
    }

    #[test]
    fn response_accept_parses() {
        let raw = json!({ "action": "accept", "content": { "choice": "choice-1" } });
        let parsed: ElicitationResponse = serde_json::from_value(raw).unwrap();
        match parsed {
            ElicitationResponse::Accept { content } => {
                assert_eq!(content["choice"], "choice-1");
            }
            other => panic!("expected Accept, got {other:?}"),
        }
    }

    #[test]
    fn response_decline_parses() {
        let raw = json!({ "action": "decline" });
        let parsed: ElicitationResponse = serde_json::from_value(raw).unwrap();
        assert!(matches!(parsed, ElicitationResponse::Decline));
    }

    #[test]
    fn response_cancel_parses() {
        let raw = json!({ "action": "cancel" });
        let parsed: ElicitationResponse = serde_json::from_value(raw).unwrap();
        assert!(matches!(parsed, ElicitationResponse::Cancel));
    }

    #[test]
    fn response_unknown_action_is_error() {
        let raw = json!({ "action": "frobnicate" });
        let res: Result<ElicitationResponse, _> = serde_json::from_value(raw);
        assert!(res.is_err());
    }
}
