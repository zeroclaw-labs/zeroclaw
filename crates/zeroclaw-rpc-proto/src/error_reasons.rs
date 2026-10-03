//! `data.reason` values the daemon sets on an error when its code alone does
//! not say why.
//!
//! The JSON-RPC code of an error never changes for this. The reason is
//! optional, additive data that names the outcome when one code covers
//! several: an `INVALID_PARAMS` that means "no such SOP", an
//! `INTERNAL_ERROR` that means "the subsystem is off". A client that meets
//! an error without a reason, or with a value this build does not list,
//! keeps its code-based handling. Values are never removed or renamed.
//!
//! The values are [`RefusalReason`](crate::error_reasons::RefusalReason),
//! also spelled out as string constants for a client that compares
//! `data.reason` directly. The transport's own `id: null` frames (an
//! oversized or unfinished frame, a connection over the ceiling) carry their
//! own `data.reason` values, outside this set.

use serde::{Deserialize, Serialize};
use zeroclaw_api::jsonrpc::JsonRpcError;
use zeroclaw_config::api_error::{ConfigApiCode, ConfigApiError};

/// The named resource does not exist.
pub const NOT_FOUND: &str = "not_found";
/// The resource exists, but nothing the request names owns it.
pub const UNOWNED: &str = "unowned";
/// The request conflicts with the resource's current state.
pub const CONFLICT: &str = "conflict";
/// The subsystem that serves the method is not enabled.
pub const DISABLED: &str = "disabled";
/// Accepted but not acted on yet; a retry can succeed unchanged.
pub const DEFERRED: &str = "deferred";
/// A concurrency or size limit refused the request.
pub const CAPACITY: &str = "capacity";
/// The caller's principal may not perform the operation.
pub const FORBIDDEN: &str = "forbidden";
/// The request is malformed or names an invalid value.
pub const INVALID: &str = "invalid";
/// A safety screen refused the request's content.
pub const BLOCKED: &str = "blocked";
/// The resource exists, but what is stored for it does not parse.
pub const MALFORMED: &str = "malformed";
/// One entry alone is larger than the size bound the caller set.
pub const ENTRY_EXCEEDS_MAX_BYTES: &str = "entry_exceeds_max_bytes";

/// Any `sops/*` method: the SOP subsystem is disabled. The general
/// [`DISABLED`], named for the clients that check this case.
pub const SOP_DISABLED: &str = DISABLED;

/// Why the core refused a method call, carried in the JSON-RPC error's
/// `data.reason`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum RefusalReason {
    /// The named resource does not exist.
    NotFound,
    /// The resource exists, but nothing the request names owns it, so the
    /// core cannot act on the caller's behalf (a SOP with no agent owner,
    /// for example).
    Unowned,
    /// The request conflicts with the resource's current state (it already
    /// exists, or changed since the caller read it).
    Conflict,
    /// The subsystem that serves the method is not enabled.
    Disabled,
    /// The request was accepted but not acted on yet; retrying later can
    /// succeed without any change on the caller's side.
    Deferred,
    /// A concurrency or size limit refused the request.
    Capacity,
    /// The caller's principal may not perform this operation.
    Forbidden,
    /// The request itself is malformed or names an invalid value.
    Invalid,
    /// A safety screen refused the request's content (untrusted input it
    /// will not act on), however well-formed the request is.
    Blocked,
    /// The named resource exists, but what is stored for it does not parse
    /// (a skill document with no frontmatter, for example).
    Malformed,
    /// One entry alone is larger than the size bound the caller set
    /// (`session/messages` with `max_bytes`); `data` names its `index` and
    /// `bytes`, so the caller can tell the bound cannot be met.
    EntryExceedsMaxBytes,
}

impl RefusalReason {
    /// Every reason, in declaration order.
    pub const ALL: &[Self] = &[
        Self::NotFound,
        Self::Unowned,
        Self::Conflict,
        Self::Disabled,
        Self::Deferred,
        Self::Capacity,
        Self::Forbidden,
        Self::Invalid,
        Self::Blocked,
        Self::Malformed,
        Self::EntryExceedsMaxBytes,
    ];

    /// The reason's wire spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotFound => NOT_FOUND,
            Self::Unowned => UNOWNED,
            Self::Conflict => CONFLICT,
            Self::Disabled => DISABLED,
            Self::Deferred => DEFERRED,
            Self::Capacity => CAPACITY,
            Self::Forbidden => FORBIDDEN,
            Self::Invalid => INVALID,
            Self::Blocked => BLOCKED,
            Self::Malformed => MALFORMED,
            Self::EntryExceedsMaxBytes => ENTRY_EXCEEDS_MAX_BYTES,
        }
    }

    /// A JSON-RPC error with `code` and `message` that carries this reason.
    pub fn error(self, code: i32, message: impl Into<String>) -> JsonRpcError {
        RefusalData {
            reason: self,
            config_error: None,
            index: None,
            bytes: None,
        }
        .into_error(code, message)
    }

    /// The reason `error` carries, or `None` when its `data` names none or
    /// names one this build does not know.
    pub fn of(error: &JsonRpcError) -> Option<Self> {
        let reason = error.data.as_ref()?.get("reason")?.as_str()?;
        Self::ALL
            .iter()
            .copied()
            .find(|known| known.as_str() == reason)
    }

    /// The reason a config error with `code` refuses with: a path the schema
    /// lacks is `not_found`, a config changed underneath the caller is
    /// `conflict`, and every other caller-side code is `invalid`. `None` for
    /// the server's own failures (`reload_failed`, `internal_error`).
    pub fn for_config(code: ConfigApiCode) -> Option<Self> {
        match code {
            ConfigApiCode::PathNotFound => Some(Self::NotFound),
            ConfigApiCode::ConfigChangedExternally => Some(Self::Conflict),
            ConfigApiCode::ReloadFailed | ConfigApiCode::InternalError => None,
            ConfigApiCode::ValidationFailed
            | ConfigApiCode::OpNotSupported
            | ConfigApiCode::SecretTestForbidden
            | ConfigApiCode::ValueTypeMismatch
            | ConfigApiCode::RequiredFieldEmpty
            | ConfigApiCode::InvalidNumericRange
            | ConfigApiCode::InvalidFormat
            | ConfigApiCode::InvalidEnumVariant
            | ConfigApiCode::DanglingReference => Some(Self::Invalid),
        }
    }
}

/// The `data` member of a refused method call's error. Optional: a core that
/// predates it, and every error it does not classify, sends no `data`.
/// Members may be added; a client ignores those it does not know.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub struct RefusalData {
    pub reason: RefusalReason,
    /// The structured error a refused config operation carries, exactly as
    /// the config surfaces report it (`code`, `message`, `path`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_error: Option<ConfigApiError>,
    /// With `entry_exceeds_max_bytes`: the position of the entry too large
    /// for the bound.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index: Option<u64>,
    /// With `entry_exceeds_max_bytes`: that entry's serialized size.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
}

impl RefusalData {
    /// This data on a JSON-RPC error with `code` and `message`.
    pub fn into_error(self, code: i32, message: impl Into<String>) -> JsonRpcError {
        JsonRpcError {
            code,
            message: message.into(),
            data: serde_json::to_value(self).ok(),
        }
    }

    /// A failed config operation, answered with `code` and `message`: it
    /// carries its config error and the reason that error's code names. A
    /// failure that is the server's own carries no data.
    pub fn config_error(
        code: i32,
        message: impl Into<String>,
        error: ConfigApiError,
    ) -> JsonRpcError {
        match RefusalReason::for_config(error.code) {
            Some(reason) => Self {
                reason,
                config_error: Some(error),
                index: None,
                bytes: None,
            }
            .into_error(code, message),
            None => JsonRpcError {
                code,
                message: message.into(),
                data: None,
            },
        }
    }

    /// The config error a refusal carries, when it names a reason this build
    /// knows and its config error decodes.
    pub fn config_error_of(error: &JsonRpcError) -> Option<ConfigApiError> {
        RefusalReason::of(error)?;
        let config_error = error.data.as_ref()?.get("config_error")?;
        ConfigApiError::deserialize(config_error).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    #[test]
    fn refusal_reasons_spell_the_same_on_the_wire_and_in_as_str() {
        for reason in RefusalReason::ALL {
            assert_eq!(
                serde_json::to_value(reason).unwrap(),
                json!(reason.as_str())
            );
            let data = serde_json::to_value(RefusalData {
                reason: *reason,
                config_error: None,
                index: None,
                bytes: None,
            })
            .unwrap();
            assert_eq!(data, json!({ "reason": reason.as_str() }));
        }
        let distinct: std::collections::BTreeSet<_> =
            RefusalReason::ALL.iter().map(|r| r.as_str()).collect();
        assert_eq!(distinct.len(), RefusalReason::ALL.len());
    }

    #[test]
    fn a_refusal_error_carries_its_reason_and_keeps_its_code() {
        use zeroclaw_api::jsonrpc::JsonRpcError;
        use zeroclaw_api::jsonrpc::error_codes::INVALID_PARAMS;

        for reason in RefusalReason::ALL {
            let error = reason.error(INVALID_PARAMS, "refused");
            assert_eq!(error.code, INVALID_PARAMS);
            assert_eq!(error.message, "refused");
            let wire: JsonRpcError =
                serde_json::from_value(serde_json::to_value(&error).unwrap()).unwrap();
            assert_eq!(RefusalReason::of(&wire), Some(*reason));
        }
    }

    #[test]
    fn an_error_without_a_known_reason_has_none() {
        use zeroclaw_api::jsonrpc::JsonRpcError;

        let with = |data: Option<Value>| JsonRpcError {
            code: -32602,
            message: "refused".into(),
            data,
        };
        assert_eq!(RefusalReason::of(&with(None)), None);
        assert_eq!(RefusalReason::of(&with(Some(json!({})))), None);
        assert_eq!(RefusalReason::of(&with(Some(json!("not_found")))), None);
        assert_eq!(RefusalReason::of(&with(Some(json!({ "reason": 7 })))), None);
        // A value a newer core may add, and the transport's own frame reasons.
        assert_eq!(
            RefusalReason::of(&with(Some(json!({ "reason": "quota" })))),
            None
        );
        assert_eq!(
            RefusalReason::of(&with(Some(
                json!({ "reason": "frame_too_large", "limit_bytes": 1 })
            ))),
            None
        );
        // Members a client does not know never hide a reason it does.
        assert_eq!(
            RefusalReason::of(&with(Some(json!({ "reason": "conflict", "current": "x" })))),
            Some(RefusalReason::Conflict)
        );
    }

    #[test]
    fn a_config_refusal_names_the_reason_its_status_class_means() {
        use zeroclaw_api::jsonrpc::error_codes::{INTERNAL_ERROR, INVALID_PARAMS};

        let codes = [
            ConfigApiCode::PathNotFound,
            ConfigApiCode::ValidationFailed,
            ConfigApiCode::ConfigChangedExternally,
            ConfigApiCode::ReloadFailed,
            ConfigApiCode::OpNotSupported,
            ConfigApiCode::SecretTestForbidden,
            ConfigApiCode::ValueTypeMismatch,
            ConfigApiCode::RequiredFieldEmpty,
            ConfigApiCode::InvalidNumericRange,
            ConfigApiCode::InvalidFormat,
            ConfigApiCode::InvalidEnumVariant,
            ConfigApiCode::DanglingReference,
            ConfigApiCode::InternalError,
        ];
        for code in codes {
            let expected = match code.http_status() {
                404 => Some(RefusalReason::NotFound),
                409 => Some(RefusalReason::Conflict),
                400 => Some(RefusalReason::Invalid),
                _ => None,
            };
            assert_eq!(RefusalReason::for_config(code), expected, "{code:?}");
        }

        let missing = ConfigApiError::path_not_found("gateway.nope");
        let error = RefusalData::config_error(INVALID_PARAMS, "Unknown prop: x", missing.clone());
        assert_eq!(error.code, INVALID_PARAMS);
        assert_eq!(error.message, "Unknown prop: x");
        assert_eq!(RefusalReason::of(&error), Some(RefusalReason::NotFound));
        let carried = RefusalData::config_error_of(&error).expect("the config error rides along");
        assert_eq!(
            serde_json::to_value(carried).unwrap(),
            serde_json::to_value(missing).unwrap()
        );

        // The server's own failure carries nothing to classify.
        let failed = RefusalData::config_error(
            INTERNAL_ERROR,
            "Config save failed",
            ConfigApiError::new(ConfigApiCode::ReloadFailed, "save failed"),
        );
        assert!(failed.data.is_none());
        assert!(RefusalData::config_error_of(&failed).is_none());
    }

    #[test]
    fn an_entry_over_the_callers_bound_decodes_with_its_position() {
        use zeroclaw_api::jsonrpc::error_codes::INVALID_PARAMS;

        // The shape `session/messages` sends when one message alone is larger
        // than `max_bytes`.
        let error = JsonRpcError {
            code: INVALID_PARAMS,
            message: "message 4 is 9000000 bytes, more than max_bytes 65536".into(),
            data: Some(
                json!({ "reason": ENTRY_EXCEEDS_MAX_BYTES, "index": 4, "bytes": 9_000_000 }),
            ),
        };
        assert_eq!(
            RefusalReason::of(&error),
            Some(RefusalReason::EntryExceedsMaxBytes)
        );
        let data: RefusalData = serde_json::from_value(error.data.unwrap()).unwrap();
        assert_eq!((data.index, data.bytes), (Some(4), Some(9_000_000)));
        assert!(data.config_error.is_none());
    }

    #[test]
    fn the_constants_are_the_wire_spellings() {
        let constants = [
            NOT_FOUND,
            UNOWNED,
            CONFLICT,
            DISABLED,
            DEFERRED,
            CAPACITY,
            FORBIDDEN,
            INVALID,
            BLOCKED,
            MALFORMED,
            ENTRY_EXCEEDS_MAX_BYTES,
        ];
        let spelled: Vec<&str> = RefusalReason::ALL.iter().map(|r| r.as_str()).collect();
        assert_eq!(spelled, constants);
        assert_eq!(SOP_DISABLED, RefusalReason::Disabled.as_str());
    }
}
