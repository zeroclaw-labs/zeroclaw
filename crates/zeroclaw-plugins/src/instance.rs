//! Host-owned identity and authority for one admitted plugin instance.
//!
//! Package manifests describe what a component requests. The host creates one
//! [`PluginInstanceScope`] only after selecting a concrete capability binding
//! and deciding which requested permissions are granted. Every adapter and
//! Wasmtime store shares that immutable scope, so future host services can use
//! the state-owned identity instead of accepting a guest-supplied namespace.

use std::collections::HashSet;
use std::sync::Arc;

use crate::error::PluginError;
use crate::{PluginCapability, PluginManifest, PluginPermission};

/// The canonical logical namespace of one plugin capability binding.
///
/// `package` comes from an admitted manifest, `capability` selects one of its
/// declared worlds, and `binding` is assigned by the host. Versions, payload
/// digests, configuration revisions, and guest-exported names are intentionally
/// excluded so an upgrade does not change the logical namespace. This ID is not
/// artifact or publisher provenance; security-sensitive services must also use
/// the host's admission decision when authorizing access.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct PluginInstanceId {
    package: String,
    capability: PluginCapability,
    binding: String,
}

impl PluginInstanceId {
    /// Create a host-owned instance identity.
    ///
    /// # Errors
    ///
    /// Returns [`PluginError::InvalidInstanceId`] when `package` is not a
    /// canonical plugin slug or `binding` is empty or contains control bytes.
    fn new(
        package: impl Into<String>,
        capability: PluginCapability,
        binding: impl Into<String>,
    ) -> Result<Self, PluginError> {
        let package = package.into();
        validate_package_name(&package).map_err(PluginError::InvalidInstanceId)?;

        let binding = binding.into();
        if binding.is_empty() {
            return Err(PluginError::InvalidInstanceId(
                "plugin binding must not be empty".to_string(),
            ));
        }
        if binding.chars().any(char::is_control) {
            return Err(PluginError::InvalidInstanceId(format!(
                "plugin binding must not contain control characters (got {binding:?})"
            )));
        }

        Ok(Self {
            package,
            capability,
            binding,
        })
    }

    /// Validated manifest package slug.
    #[must_use]
    pub fn package(&self) -> &str {
        &self.package
    }

    /// Capability world represented by this runtime instance.
    #[must_use]
    pub fn capability(&self) -> PluginCapability {
        self.capability
    }

    /// Opaque host-owned binding within the package capability.
    #[must_use]
    pub fn binding(&self) -> &str {
        &self.binding
    }
}

/// Immutable effective permissions for one admitted instance.
///
/// Manifest permissions are requests. This set is the host's grant decision;
/// today callers may grant every request, while a later authority layer can
/// supply the request/operator-policy intersection without changing service
/// consumers.
#[derive(Clone, Debug)]
pub struct PluginGrantSet {
    grants: HashSet<PluginPermission>,
}

impl PluginGrantSet {
    /// Build a deduplicated effective grant set.
    #[must_use]
    fn new(grants: impl IntoIterator<Item = PluginPermission>) -> Self {
        Self {
            grants: grants.into_iter().collect(),
        }
    }

    /// Whether this instance may use `permission`.
    #[must_use]
    pub fn allows(&self, permission: PluginPermission) -> bool {
        self.grants.contains(&permission)
    }
}

/// Shared identity and authority injected into every store for one instance.
///
/// Cloning this value shares the same immutable scope. It does not snapshot
/// configuration, secrets, allowlists, limits, routes, or network policy.
#[derive(Clone, Debug)]
pub struct PluginInstanceScope {
    inner: Arc<PluginInstanceScopeInner>,
}

#[derive(Debug)]
struct PluginInstanceScopeInner {
    id: PluginInstanceId,
    grants: PluginGrantSet,
}

impl PluginInstanceScope {
    /// Admit a host-selected binding from a validated manifest and grant set.
    ///
    /// The caller remains responsible for signature and publisher policy. This
    /// constructor enforces the structural authority invariants: the capability
    /// must be declared and every effective grant must have been requested.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid package slug, undeclared capability, or
    /// grant absent from the manifest's requested permissions.
    pub fn from_manifest(
        manifest: &PluginManifest,
        capability: PluginCapability,
        binding: impl Into<String>,
        grants: impl IntoIterator<Item = PluginPermission>,
    ) -> Result<Self, PluginError> {
        if !manifest.capabilities.contains(&capability) {
            return Err(PluginError::UnsupportedCapability(format!(
                "plugin '{}' does not declare {capability:?}",
                manifest.name
            )));
        }

        let grants = PluginGrantSet::new(grants);
        if let Some(permission) = grants
            .grants
            .iter()
            .find(|permission| !manifest.permissions.contains(permission))
        {
            return Err(PluginError::PermissionDenied {
                plugin: manifest.name.clone(),
                permission: format!("{permission:?}"),
            });
        }

        let id = PluginInstanceId::new(manifest.name.clone(), capability, binding)?;
        Ok(Self {
            inner: Arc::new(PluginInstanceScopeInner { id, grants }),
        })
    }

    /// Canonical instance identity used to namespace host services.
    #[must_use]
    pub fn id(&self) -> &PluginInstanceId {
        &self.inner.id
    }

    /// Effective host permission grants for this instance.
    #[must_use]
    pub fn grants(&self) -> &PluginGrantSet {
        &self.inner.grants
    }

    /// Reject wiring this scope into an adapter for another capability world.
    ///
    /// # Errors
    ///
    /// Returns [`PluginError::InvalidInstanceId`] when the instance capability
    /// does not equal `expected`.
    #[cfg(any(feature = "plugins-wasmtime", test))]
    pub(crate) fn require_capability(&self, expected: PluginCapability) -> Result<(), PluginError> {
        if self.id().capability() != expected {
            return Err(PluginError::InvalidInstanceId(format!(
                "plugin package {:?} binding {:?} has capability {:?}, not {expected:?}",
                self.id().package(),
                self.id().binding(),
                self.id().capability(),
            )));
        }
        Ok(())
    }
}

/// Validate the package-name grammar once for both manifest admission and
/// runtime instance construction.
pub(crate) fn validate_package_name(name: &str) -> Result<(), String> {
    let bytes = name.as_bytes();
    let valid = (1..=128).contains(&bytes.len())
        && bytes
            .first()
            .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit());
    let valid = valid
        && bytes.last().is_some_and(u8::is_ascii_alphanumeric)
        && bytes.iter().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_' | b'.')
        });
    if !valid {
        return Err(format!(
            "plugin package must be a 1-128 character lowercase ASCII slug, start with a letter or digit, end with a letter or digit, and use only '-', '_', or '.' as separators (got {name:?})"
        ));
    }
    Ok(())
}

#[cfg(test)]
pub(crate) fn test_scope(
    capability: PluginCapability,
    binding: &str,
    grants: impl IntoIterator<Item = PluginPermission>,
) -> PluginInstanceScope {
    let permissions: Vec<_> = grants.into_iter().collect();
    PluginInstanceScope::from_manifest(
        &test_manifest(capability, permissions.clone()),
        capability,
        binding,
        permissions,
    )
    .expect("valid fixture scope")
}

#[cfg(test)]
fn test_manifest(
    capability: PluginCapability,
    permissions: Vec<PluginPermission>,
) -> PluginManifest {
    PluginManifest {
        name: "fixture".to_string(),
        version: "0.1.0".to_string(),
        description: None,
        author: None,
        wasm_path: Some("plugin.wasm".to_string()),
        capabilities: vec![capability],
        permissions,
        signature: None,
        publisher_key: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(package: &str, capability: PluginCapability, binding: &str) -> PluginInstanceId {
        PluginInstanceId::new(package, capability, binding).expect("valid instance id")
    }

    #[test]
    fn identity_is_structural_across_every_dimension() {
        let base = id("messaging", PluginCapability::Channel, "telegram.main");
        let identities = HashSet::from([
            base.clone(),
            id("messaging-alt", PluginCapability::Channel, "telegram.main"),
            id("messaging", PluginCapability::Tool, "telegram.main"),
            id("messaging", PluginCapability::Channel, "telegram.backup"),
        ]);

        assert_eq!(identities.len(), 4);
        assert_eq!(
            base,
            id("messaging", PluginCapability::Channel, "telegram.main"),
            "artifact version and digest are deliberately not identity fields"
        );
    }

    #[test]
    fn invalid_identity_parts_fail_closed() {
        assert!(PluginInstanceId::new("../escape", PluginCapability::Tool, "tool").is_err());
        assert!(PluginInstanceId::new("Mail", PluginCapability::Tool, "tool").is_err());
        assert!(PluginInstanceId::new("mail.", PluginCapability::Tool, "tool").is_err());
        assert!(PluginInstanceId::new("tool", PluginCapability::Tool, "").is_err());
        assert!(PluginInstanceId::new("tool", PluginCapability::Tool, "bad\nbinding").is_err());
    }

    #[test]
    fn grant_set_checks_effective_permissions() {
        let grants = PluginGrantSet::new([
            PluginPermission::ConfigRead,
            PluginPermission::ConfigRead,
            PluginPermission::HttpClient,
        ]);
        assert!(grants.allows(PluginPermission::ConfigRead));
        assert!(grants.allows(PluginPermission::HttpClient));
        assert!(!grants.allows(PluginPermission::MemoryWrite));
    }

    #[test]
    fn cloned_scope_shares_one_identity_and_checks_capability() {
        let scope = test_scope(PluginCapability::Channel, "telegram.main", []);
        let clone = scope.clone();

        assert!(std::ptr::eq(scope.id(), clone.id()));
        assert!(scope.require_capability(PluginCapability::Channel).is_ok());
        assert!(scope.require_capability(PluginCapability::Tool).is_err());
    }

    #[test]
    fn manifest_admission_rejects_undeclared_capabilities_and_grants() {
        let manifest = test_manifest(PluginCapability::Tool, vec![PluginPermission::ConfigRead]);

        assert!(
            PluginInstanceScope::from_manifest(&manifest, PluginCapability::Channel, "main", [])
                .is_err()
        );
        assert!(
            PluginInstanceScope::from_manifest(
                &manifest,
                PluginCapability::Tool,
                "main",
                [PluginPermission::HttpClient]
            )
            .is_err()
        );
        assert!(
            PluginInstanceScope::from_manifest(
                &manifest,
                PluginCapability::Tool,
                "main",
                [PluginPermission::ConfigRead]
            )
            .is_ok()
        );
    }
}
