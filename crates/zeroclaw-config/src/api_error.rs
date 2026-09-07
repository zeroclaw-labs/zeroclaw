//! Structured error type for the gateway HTTP CRUD surface and its CLI peer.

use serde::{Deserialize, Serialize};

/// Stable error code consumed by HTTP / CLI / dashboard. Add codes here as new
/// failure cases land — never invent codes ad-hoc at call sites.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ConfigApiCode {
    /// The supplied property path is not defined in the schema.
    PathNotFound,
    /// Generic schema validation failure (catch-all wrapping `Config::validate()` bails).
    ValidationFailed,
    /// On-disk config differs from in-memory state (an out-of-band file edit
    /// happened despite the daemon-running rule). Caller should reload.
    ConfigChangedExternally,
    /// The daemon-reload step after a successful save failed; on-disk config
    /// has been reverted to the pre-write snapshot to keep state consistent.
    ReloadFailed,
    /// JSON Patch operation type is not supported (`move` / `copy`).
    OpNotSupported,
    /// JSON Patch `test` operation targeted a secret or derived-from-secret
    /// path; rejected to prevent differential value inference.
    SecretTestForbidden,
    /// The supplied JSON value does not match the field's declared type
    /// (e.g. an array passed where a scalar was expected, or a non-string
    /// element in a `Vec<String>`).
    ValueTypeMismatch,
    /// A required scalar field was empty / missing / blank.
    /// Path identifies which one (e.g. `gateway.host`,
    /// `tunnel.openvpn.config_file`).
    RequiredFieldEmpty,
    /// A numeric field was out of its allowed range (zero, negative, or
    /// above an upper bound). Path identifies which one.
    InvalidNumericRange,
    /// A string did not match its allowed format — invalid URL, bad
    /// scheme, invalid path prefix, characters outside the allowed set.
    InvalidFormat,
    /// An enum / discriminator field carried a value not in the allowed
    /// set (e.g. `tunnel.tunnel_provider` with an unknown name).
    InvalidEnumVariant,
    /// A reference to another config entry pointed at something that
    /// doesn't exist (e.g. `agents.<x>.delegate_to` naming a missing agent).
    DanglingReference,
    /// Catch-all server failure not classified above. Avoid in code; log the
    /// original error and convert to a more specific code where possible.
    InternalError,
}

impl ConfigApiCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PathNotFound => "path_not_found",
            Self::ValidationFailed => "validation_failed",
            Self::ConfigChangedExternally => "config_changed_externally",
            Self::ReloadFailed => "reload_failed",
            Self::OpNotSupported => "op_not_supported",
            Self::SecretTestForbidden => "secret_test_forbidden",
            Self::ValueTypeMismatch => "value_type_mismatch",
            Self::RequiredFieldEmpty => "required_field_empty",
            Self::InvalidNumericRange => "invalid_numeric_range",
            Self::InvalidFormat => "invalid_format",
            Self::InvalidEnumVariant => "invalid_enum_variant",
            Self::DanglingReference => "dangling_reference",
            Self::InternalError => "internal_error",
        }
    }

    /// HTTP status that the gateway returns when this code is the response.
    pub fn http_status(self) -> u16 {
        match self {
            Self::PathNotFound => 404,
            Self::ValidationFailed
            | Self::OpNotSupported
            | Self::SecretTestForbidden
            | Self::ValueTypeMismatch
            | Self::RequiredFieldEmpty
            | Self::InvalidNumericRange
            | Self::InvalidFormat
            | Self::InvalidEnumVariant
            | Self::DanglingReference => 400,
            Self::ConfigChangedExternally => 409,
            Self::ReloadFailed | Self::InternalError => 500,
        }
    }
}

/// Structured error returned by the new HTTP CRUD endpoints and the `zeroclaw config`
/// subcommands they share infrastructure with.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct ConfigApiError {
    /// Stable error code for programmatic matching.
    pub code: ConfigApiCode,
    /// Human-readable message. Safe to render directly in dashboards / terminals.
    pub message: String,
    /// Property path the error pertains to, when applicable. Empty when the
    /// error is whole-config (e.g. `ReloadFailed`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Other configuration paths whose values participate in this error.
    ///
    /// `path` remains the primary display target for existing clients. This
    /// additive field lets mutation/repair callers distinguish an error that
    /// was already present from one introduced through a sibling or referenced
    /// value.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub related_paths: Vec<String>,
    /// Index into the JSON Patch operation array, when the error originated
    /// from a specific op in a `PATCH /api/config` batch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub op_index: Option<usize>,
}

impl ConfigApiError {
    pub fn new(code: ConfigApiCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            path: None,
            related_paths: Vec::new(),
            op_index: None,
        }
    }

    pub fn with_path(mut self, path: impl Into<String>) -> Self {
        let path = path.into();
        self.related_paths.retain(|related| related != &path);
        self.path = Some(path);
        self
    }

    /// Attach the non-primary paths that causally participate in this error.
    /// Empty paths and duplicates of the primary path are omitted so wire
    /// clients can treat this as an additive list of *other* fields.
    pub fn with_related_paths(
        mut self,
        paths: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        for path in paths {
            let path = path.into();
            if !path.is_empty()
                && self.path.as_deref() != Some(path.as_str())
                && !self.related_paths.contains(&path)
            {
                self.related_paths.push(path);
            }
        }
        self
    }

    /// Iterate over the primary display path followed by every causal sibling.
    pub fn affected_paths(&self) -> impl Iterator<Item = &str> {
        self.path
            .as_deref()
            .into_iter()
            .chain(self.related_paths.iter().map(String::as_str))
    }

    pub fn with_op_index(mut self, index: usize) -> Self {
        self.op_index = Some(index);
        self
    }

    pub fn from_validation(err: anyhow::Error) -> Self {
        if let Some(structured) = err.downcast_ref::<ConfigApiError>() {
            return structured.clone();
        }
        let msg = err.to_string();
        let code = classify_validation_message(&msg);
        Self::new(code, msg)
    }
}

pub fn classify_validation_message(msg: &str) -> ConfigApiCode {
    let lower = msg.to_lowercase();
    if lower.contains("type mismatch") || lower.contains("invalid value") {
        return ConfigApiCode::ValueTypeMismatch;
    }
    if lower.starts_with("unknown property") {
        return ConfigApiCode::PathNotFound;
    }
    ConfigApiCode::ValidationFailed
}

impl ConfigApiError {
    /// Convenience: a `path_not_found` error for the given path.
    pub fn path_not_found(path: impl Into<String>) -> Self {
        let path = path.into();
        Self::new(
            ConfigApiCode::PathNotFound,
            format!("property path not found in schema: {path}"),
        )
        .with_path(path)
    }

    /// Convenience: a `secret_test_forbidden` error for the given path.
    pub fn secret_test_forbidden(path: impl Into<String>) -> Self {
        let path = path.into();
        Self::new(
            ConfigApiCode::SecretTestForbidden,
            format!(
                "JSON Patch `test` operations against secret or derived-from-secret paths \
                 are forbidden: {path}"
            ),
        )
        .with_path(path)
    }

    /// Convenience: an `op_not_supported` error.
    pub fn op_not_supported(op: impl Into<String>) -> Self {
        let op = op.into();
        Self::new(
            ConfigApiCode::OpNotSupported,
            format!("JSON Patch operation `{op}` is not supported in this version"),
        )
    }
}

impl std::fmt::Display for ConfigApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.path {
            Some(path) => write!(f, "[{}] {} ({})", self.code.as_str(), self.message, path),
            None => write!(f, "[{}] {}", self.code.as_str(), self.message),
        }
    }
}

impl std::error::Error for ConfigApiError {}

#[macro_export]
macro_rules! validation_bail {
    ($code:ident, $path:expr, related [$($related:expr),* $(,)?], $($msg:tt)*) => {{
        let err = $crate::api_error::ConfigApiError::new(
            $crate::api_error::ConfigApiCode::$code,
            format!($($msg)*),
        )
        .with_path($path)
        .with_related_paths([$($related),*]);
        return Err(::anyhow::Error::from(err));
    }};
    ($code:ident, $path:expr, $($msg:tt)*) => {{
        let err = $crate::api_error::ConfigApiError::new(
            $crate::api_error::ConfigApiCode::$code,
            format!($($msg)*),
        )
        .with_path($path);
        return Err(::anyhow::Error::from(err));
    }};
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_str_round_trip() {
        for code in [
            ConfigApiCode::PathNotFound,
            ConfigApiCode::ValidationFailed,
            ConfigApiCode::ConfigChangedExternally,
            ConfigApiCode::ReloadFailed,
            ConfigApiCode::OpNotSupported,
            ConfigApiCode::SecretTestForbidden,
            ConfigApiCode::ValueTypeMismatch,
            ConfigApiCode::InternalError,
        ] {
            let serialized = serde_json::to_value(code).unwrap();
            let s = serialized.as_str().unwrap();
            assert_eq!(s, code.as_str());
        }
    }

    #[test]
    fn http_status_matches_intent() {
        assert_eq!(ConfigApiCode::PathNotFound.http_status(), 404);
        assert_eq!(ConfigApiCode::ValidationFailed.http_status(), 400);
        assert_eq!(ConfigApiCode::ConfigChangedExternally.http_status(), 409);
        assert_eq!(ConfigApiCode::ReloadFailed.http_status(), 500);
    }

    #[test]
    fn classify_unknown_property() {
        assert_eq!(
            classify_validation_message("Unknown property 'foo.bar'"),
            ConfigApiCode::PathNotFound
        );
    }

    #[test]
    fn classify_falls_back_to_validation_failed() {
        assert_eq!(
            classify_validation_message("some unrelated random validator output"),
            ConfigApiCode::ValidationFailed
        );
    }

    #[test]
    fn path_not_found_carries_path() {
        let err = ConfigApiError::path_not_found("providers.models");
        assert_eq!(err.code, ConfigApiCode::PathNotFound);
        assert_eq!(err.path.as_deref(), Some("providers.models"));
        assert!(err.message.contains("providers.models"));
    }

    #[test]
    fn related_paths_are_additive_and_drive_affected_path_iteration() {
        let err = ConfigApiError::new(ConfigApiCode::InvalidFormat, "conflicting values")
            .with_path("nodes.mdns.peer_ttl_secs")
            .with_related_paths([
                "nodes.mdns.announce_interval_secs",
                "nodes.mdns.peer_ttl_secs",
                "",
            ]);

        assert_eq!(err.related_paths, vec!["nodes.mdns.announce_interval_secs"]);
        assert_eq!(
            err.affected_paths().collect::<Vec<_>>(),
            vec![
                "nodes.mdns.peer_ttl_secs",
                "nodes.mdns.announce_interval_secs"
            ]
        );
        let serialized = serde_json::to_value(&err).expect("serialize error");
        assert_eq!(
            serialized["related_paths"],
            serde_json::json!(["nodes.mdns.announce_interval_secs"])
        );
    }

    #[test]
    fn setting_primary_path_after_related_paths_keeps_them_disjoint() {
        let err = ConfigApiError::new(ConfigApiCode::InvalidFormat, "conflicting values")
            .with_related_paths(["nodes.mdns.announce_interval_secs"])
            .with_path("nodes.mdns.announce_interval_secs");

        assert!(err.related_paths.is_empty());
        assert_eq!(
            err.affected_paths().collect::<Vec<_>>(),
            vec!["nodes.mdns.announce_interval_secs"]
        );
        let serialized = serde_json::to_value(&err).expect("serialize error");
        assert!(serialized.get("related_paths").is_none());
    }

    #[test]
    fn secret_test_forbidden_carries_path() {
        let err = ConfigApiError::secret_test_forbidden("providers.models.openrouter.api-key");
        assert_eq!(err.code, ConfigApiCode::SecretTestForbidden);
        assert!(err.message.contains("providers.models.openrouter.api-key"));
    }

    #[test]
    fn from_validation_uses_message() {
        let anyhow_err = anyhow::Error::msg("gateway.host must not be empty");
        let api_err = ConfigApiError::from_validation(anyhow_err);
        assert_eq!(api_err.code, ConfigApiCode::ValidationFailed);
        assert!(api_err.message.contains("gateway.host"));
    }

    #[test]
    fn display_includes_code_and_path() {
        let err = ConfigApiError::path_not_found("foo.bar");
        let s = format!("{err}");
        assert!(s.contains("path_not_found"));
        assert!(s.contains("foo.bar"));
    }
}
