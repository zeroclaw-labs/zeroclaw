//! Portable names for plugin-local host services.
//!
//! These names carry no instance namespace. Package, capability, and binding
//! always come from a host-issued plugin scope at the point of use.

use std::fmt;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Maximum portable plugin-local name length.
pub const MAX_PORTABLE_PLUGIN_KEY_BYTES: usize = 128;

/// The one canonical grammar for portable plugin-local names.
///
/// URI schemes, path separators, control bytes, and raw instance namespaces
/// are excluded. Semantic wrappers retain whether a name addresses a secret,
/// durable state, egress profile input, or another host service.
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct PortablePluginKey(String);

impl PortablePluginKey {
    /// Parse one portable plugin-local name.
    pub fn parse(key: impl Into<String>) -> Result<Self, PortablePluginKeyError> {
        let key = key.into();
        if !is_valid_portable_plugin_key(&key) {
            return Err(PortablePluginKeyError);
        }
        Ok(Self(key))
    }

    /// Borrow the validated name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for PortablePluginKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("PortablePluginKey")
            .field(&self.0)
            .finish()
    }
}

impl AsRef<str> for PortablePluginKey {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl TryFrom<String> for PortablePluginKey {
    type Error = PortablePluginKeyError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(value)
    }
}

impl From<PortablePluginKey> for String {
    fn from(value: PortablePluginKey) -> Self {
        value.0
    }
}

/// A validated reference to one top-level `x-secret: true` plugin property.
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct SecretPropertyRef(PortablePluginKey);

impl SecretPropertyRef {
    /// Parse a secret-property reference through the shared plugin-local key
    /// grammar. Manifest admission separately proves `x-secret: true`.
    pub fn parse(property: impl Into<String>) -> Result<Self, PortablePluginKeyError> {
        PortablePluginKey::parse(property).map(Self)
    }

    /// Borrow the validated top-level property name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl fmt::Debug for SecretPropertyRef {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("SecretPropertyRef")
            .field(&self.as_str())
            .finish()
    }
}

impl fmt::Display for SecretPropertyRef {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl AsRef<str> for SecretPropertyRef {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl TryFrom<String> for SecretPropertyRef {
    type Error = PortablePluginKeyError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(value)
    }
}

impl From<SecretPropertyRef> for String {
    fn from(value: SecretPropertyRef) -> Self {
        value.0.into()
    }
}

/// Detail-free invalid portable-key error shared across plugin host services.
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
#[error("plugin-local key must be a 1-128 byte portable ASCII name")]
pub struct PortablePluginKeyError;

/// Whether `key` follows the canonical portable plugin-local grammar.
#[must_use]
pub fn is_valid_portable_plugin_key(key: &str) -> bool {
    (1..=MAX_PORTABLE_PLUGIN_KEY_BYTES).contains(&key.len())
        && key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semantic_wrappers_share_one_portable_grammar() {
        for property in ["api_token", "client-cert", "client_key.pem", "v2"] {
            let key = PortablePluginKey::parse(property).expect("portable key");
            let reference = SecretPropertyRef::parse(property).expect("portable property");
            assert_eq!(key.as_str(), reference.as_str());
        }
    }

    #[test]
    fn rejects_uri_path_namespace_and_non_ascii_syntax() {
        for key in [
            "",
            "plugin://other/key",
            "../key",
            "key:value",
            "key/value",
            "other\\key",
            "instance@key",
            "café",
        ] {
            assert!(PortablePluginKey::parse(key).is_err(), "accepted {key:?}");
            assert!(SecretPropertyRef::parse(key).is_err(), "accepted {key:?}");
        }
        assert!(PortablePluginKey::parse("a".repeat(129)).is_err());
    }

    #[test]
    fn serde_uses_validated_strings() {
        let reference = SecretPropertyRef::parse("client_key.pem").unwrap();
        let encoded = serde_json::to_string(&reference).unwrap();
        assert_eq!(encoded, r#""client_key.pem""#);
        assert_eq!(
            serde_json::from_str::<SecretPropertyRef>(&encoded).unwrap(),
            reference
        );
        assert!(serde_json::from_str::<SecretPropertyRef>(r#""../key""#).is_err());
    }
}
