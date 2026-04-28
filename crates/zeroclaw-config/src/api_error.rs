//! Structured error type for the gateway HTTP CRUD surface and its CLI peer.
//!
//! Every fallible operation against the new per-property endpoints (`/api/config/prop`,
//! `/api/config/list`, `OPTIONS /api/config*`, `PATCH /api/config`) and the matching
//! `zeroclaw config` CLI subcommands returns this error type. The `code` field is
//! a stable string the dashboard / scripts can match programmatically; `message`
//! is human-readable for terminal output and tooltip text. `path` carries the
//! offending field (when applicable) so the dashboard can render the error
//! contextually next to the input.
//!
//! This replaces the prior pattern of returning `anyhow::Error` strings that
//! consumers had to substring-match. Existing `anyhow::bail!(...)` sites in
//! `Config::validate()` are wrapped via `ConfigApiError::from_validation` —
//! the friendly text becomes `message`, the code stays generic
//! (`validation_failed`) until callers refine to a more specific code.

use serde::{Deserialize, Serialize};

/// Stable error code consumed by HTTP / CLI / dashboard. Add codes here as new
/// failure cases land — never invent codes ad-hoc at call sites.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigApiCode {
    /// The supplied property path is not defined in the schema.
    PathNotFound,
    /// Generic schema validation failure (catch-all wrapping `Config::validate()` bails).
    /// Specific failure modes (`provider_fallback_dangling`, etc.) get their own
    /// codes as the validator is incrementally restructured.
    ValidationFailed,
    /// `providers.fallback` references a key not present in `providers.models`.
    ProviderFallbackDangling,
    /// On-disk config differs from in-memory state (an out-of-band file edit
    /// happened despite the daemon-running rule). Caller should reload.
    ConfigChangedExternally,
    /// The daemon-reload step after a successful save failed; on-disk config
    /// has been reverted to the pre-write snapshot to keep state consistent.
    ReloadFailed,
    /// JSON Patch operation type is not supported in this PR (`move` / `copy`).
    OpNotSupported,
    /// JSON Patch `test` operation targeted a secret or derived-from-secret
    /// path; rejected to prevent differential value inference.
    SecretTestForbidden,
    /// The supplied JSON value does not match the field's declared type
    /// (e.g. an array passed where a scalar was expected, or a non-string
    /// element in a `Vec<String>`).
    ValueTypeMismatch,
    /// Catch-all server failure not classified above. Avoid in code; log the
    /// original error and convert to a more specific code where possible.
    InternalError,
}

impl ConfigApiCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PathNotFound => "path_not_found",
            Self::ValidationFailed => "validation_failed",
            Self::ProviderFallbackDangling => "provider_fallback_dangling",
            Self::ConfigChangedExternally => "config_changed_externally",
            Self::ReloadFailed => "reload_failed",
            Self::OpNotSupported => "op_not_supported",
            Self::SecretTestForbidden => "secret_test_forbidden",
            Self::ValueTypeMismatch => "value_type_mismatch",
            Self::InternalError => "internal_error",
        }
    }

    /// HTTP status that the gateway returns when this code is the response.
    pub fn http_status(self) -> u16 {
        match self {
            Self::PathNotFound => 404,
            Self::ValidationFailed
            | Self::ProviderFallbackDangling
            | Self::OpNotSupported
            | Self::SecretTestForbidden
            | Self::ValueTypeMismatch => 400,
            Self::ConfigChangedExternally => 409,
            Self::ReloadFailed | Self::InternalError => 500,
        }
    }
}

/// Structured error returned by the new HTTP CRUD endpoints and the `zeroclaw config`
/// subcommands they share infrastructure with.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigApiError {
    /// Stable error code for programmatic matching.
    pub code: ConfigApiCode,
    /// Human-readable message. Safe to render directly in dashboards / terminals.
    pub message: String,
    /// Property path the error pertains to, when applicable. Empty when the
    /// error is whole-config (e.g. `ReloadFailed`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
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
            op_index: None,
        }
    }

    pub fn with_path(mut self, path: impl Into<String>) -> Self {
        self.path = Some(path.into());
        self
    }

    pub fn with_op_index(mut self, index: usize) -> Self {
        self.op_index = Some(index);
        self
    }

    /// Wrap an `anyhow::Error` from `Config::validate()` (or similar bail
    /// sites) into a structured error. The error string becomes `message`;
    /// the code defaults to `ValidationFailed`. Callers should override with
    /// a more specific code where the failure mode is known.
    pub fn from_validation(err: anyhow::Error) -> Self {
        Self::new(ConfigApiCode::ValidationFailed, err.to_string())
    }

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_str_round_trip() {
        for code in [
            ConfigApiCode::PathNotFound,
            ConfigApiCode::ValidationFailed,
            ConfigApiCode::ProviderFallbackDangling,
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
    fn path_not_found_carries_path() {
        let err = ConfigApiError::path_not_found("providers.fallback");
        assert_eq!(err.code, ConfigApiCode::PathNotFound);
        assert_eq!(err.path.as_deref(), Some("providers.fallback"));
        assert!(err.message.contains("providers.fallback"));
    }

    #[test]
    fn secret_test_forbidden_carries_path() {
        let err = ConfigApiError::secret_test_forbidden("providers.models.openrouter.api-key");
        assert_eq!(err.code, ConfigApiCode::SecretTestForbidden);
        assert!(
            err.message
                .contains("providers.models.openrouter.api-key")
        );
    }

    #[test]
    fn from_validation_uses_message() {
        let anyhow_err = anyhow::anyhow!("gateway.host must not be empty");
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
