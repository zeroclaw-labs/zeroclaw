//! ACP elicitation primitives.
//!
//! Implements the subset of the ACP `elicitation/create` RFD that
//! ZeroClaw uses for multiple-choice prompts. See
//! `docs/superpowers/specs/2026-06-24-acp-elicitation-multiple-choice-design.md`
//! for the design rationale.

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
}
