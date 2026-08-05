//! Host-owned logical endpoints for plugin traffic.
//!
//! A channel plugin may describe the platform payload it received, but only the
//! host selects the ZeroClaw route that owns that payload. The endpoint keeps
//! the channel type alongside the admitted instance scope while deriving the
//! alias from that scope, so routing identity cannot drift across two fields.

use std::sync::Arc;

use crate::PluginCapability;
use crate::error::PluginError;
use crate::instance::{PluginInstanceId, PluginInstanceScope};

/// The host-issued route for one admitted channel-plugin binding.
///
/// `channel_type` is the canonical channel family used by the orchestrator.
/// The route alias is always [`PluginInstanceId::binding`]; it is deliberately
/// not stored again here. Clones share the immutable route and instance scope.
#[derive(Clone, Debug)]
pub struct PluginChannelEndpoint {
    scope: PluginInstanceScope,
    channel_type: Arc<str>,
}

impl PluginChannelEndpoint {
    /// Bind an admitted channel scope to a canonical host routing type.
    ///
    /// # Errors
    ///
    /// Returns an error when `scope` is not a channel capability or when
    /// `channel_type` is not a canonical snake_case channel key.
    pub fn new(
        scope: PluginInstanceScope,
        channel_type: impl Into<String>,
    ) -> Result<Self, PluginError> {
        scope.require_capability(PluginCapability::Channel)?;

        let channel_type = channel_type.into();
        validate_channel_type(&channel_type)?;

        Ok(Self {
            scope,
            channel_type: Arc::from(channel_type),
        })
    }

    /// Canonical channel family selected by the host.
    #[must_use]
    pub fn channel_type(&self) -> &str {
        &self.channel_type
    }

    /// Canonical configured alias, sourced from the admitted instance binding.
    #[must_use]
    pub fn alias(&self) -> &str {
        self.scope.id().binding()
    }

    /// Host-owned instance identity behind this endpoint.
    #[must_use]
    pub fn instance_id(&self) -> &PluginInstanceId {
        self.scope.id()
    }

    #[cfg(feature = "plugins-wasmtime")]
    pub(crate) fn scope(&self) -> &PluginInstanceScope {
        &self.scope
    }
}

pub(crate) fn validate_channel_type(channel_type: &str) -> Result<(), PluginError> {
    let bytes = channel_type.as_bytes();
    let valid = (1..=128).contains(&bytes.len())
        && bytes.first().is_some_and(u8::is_ascii_lowercase)
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'_');

    if !valid {
        return Err(PluginError::InvalidEndpoint(format!(
            "channel type must be a 1-128 character lowercase snake_case key that starts with a letter (got {channel_type:?})"
        )));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_uses_the_scope_binding_as_its_only_alias() {
        let scope = crate::instance::test_scope(PluginCapability::Channel, "operations", []);
        let endpoint = PluginChannelEndpoint::new(scope.clone(), "nextcloud_talk")
            .expect("valid channel endpoint");
        let clone = endpoint.clone();

        assert_eq!(endpoint.channel_type(), "nextcloud_talk");
        assert_eq!(endpoint.alias(), "operations");
        assert!(std::ptr::eq(endpoint.instance_id(), scope.id()));
        assert!(std::ptr::eq(endpoint.channel_type(), clone.channel_type()));
    }

    #[test]
    fn endpoint_rejects_ambiguous_channel_types() {
        for channel_type in [
            "",
            "Telegram",
            "plugin.echo",
            "bad/type",
            "bad:type",
            "bad type",
            "bad\nkey",
            "-bad",
            "nextcloud-talk",
            "1channel",
        ] {
            let scope = crate::instance::test_scope(PluginCapability::Channel, "main", []);
            assert!(PluginChannelEndpoint::new(scope, channel_type).is_err());
        }

        for channel_type in ["telegram", "gmail_push", "nextcloud_talk"] {
            let scope = crate::instance::test_scope(PluginCapability::Channel, "main", []);
            assert!(PluginChannelEndpoint::new(scope, channel_type).is_ok());
        }
    }

    #[test]
    fn endpoint_rejects_a_non_channel_scope() {
        let scope = crate::instance::test_scope(PluginCapability::Tool, "main", []);
        assert!(PluginChannelEndpoint::new(scope, "telegram").is_err());
    }
}
