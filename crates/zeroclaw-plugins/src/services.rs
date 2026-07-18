//! Typed host services injected into every plugin store.
//!
//! The bundle contains service handles, not materialized values. Each lookup
//! resolves from canonical host state under the store's host-issued instance
//! scope, keeping identity and authorization out of guest-controlled inputs.

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use zeroclaw_api::plugin_key::{PortablePluginKey, SecretPropertyRef};
use zeroize::Zeroizing;

use crate::PluginPermission;
use crate::config::{PluginConfigResolver, ResolvedPluginConfig};
use crate::egress::EgressHostService;
use crate::error::PluginError;
use crate::instance::PluginInstanceScope;

/// Required host-service bundle for one plugin store.
///
/// This is deliberately a typed struct rather than a string-keyed or `Any`
/// registry. Adding a service is an explicit API change, and a store cannot be
/// constructed while a required service is missing.
#[derive(Clone)]
pub struct PluginHostServices {
    config: PluginConfigResolver,
    state: PluginStateService,
    egress: EgressHostService,
}

/// One coherent, transient bundle of schema-designated instance secrets.
///
/// Values are resolved from one config service frame and zeroized on drop.
/// Debug output deliberately reports only bundle size.
pub struct ResolvedSecretBundle {
    values: HashMap<SecretPropertyRef, Zeroizing<String>>,
}

impl ResolvedSecretBundle {
    /// Borrow one resolved value without copying it into another owner.
    #[must_use]
    pub fn get(&self, reference: &SecretPropertyRef) -> Option<&str> {
        self.values.get(reference).map(|value| value.as_str())
    }

    /// Number of resolved references in this coherent bundle.
    #[must_use]
    pub fn len(&self) -> usize {
        self.values.len()
    }

    /// Whether the bundle contains no references.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }
}

impl fmt::Debug for ResolvedSecretBundle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResolvedSecretBundle")
            .field("value_count", &self.values.len())
            .finish_non_exhaustive()
    }
}

/// Detail-free failure from coherent secret-bundle resolution.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SecretBundleError {
    /// The admitted instance lacks `config_read`.
    #[error("plugin secret access denied")]
    AccessDenied,
    /// At least one reference is absent or is not schema-designated as secret.
    #[error("plugin secret reference not found")]
    NotFound,
    /// Canonical config could not be resolved coherently.
    #[error("plugin secret service unavailable")]
    Unavailable,
}

/// Validated logical key within one exact plugin instance's durable state.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct PluginStateKey(PortablePluginKey);

impl PluginStateKey {
    /// Parse a portable state key.
    ///
    /// Keys are deliberately not paths or namespaces. Structural package,
    /// capability, and binding identity always comes from the host scope.
    pub fn parse(key: impl Into<String>) -> Result<Self, PluginStateError> {
        PortablePluginKey::parse(key)
            .map(Self)
            .map_err(|_| PluginStateError::InvalidKey)
    }

    /// Borrow the validated logical key.
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl fmt::Debug for PluginStateKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("PluginStateKey([REDACTED])")
    }
}

/// Decrypted state returned only to the exact requesting instance.
pub struct PluginStateValue {
    revision: u64,
    value: Zeroizing<Vec<u8>>,
}

impl PluginStateValue {
    /// Construct one backend result after its envelope has been authenticated.
    #[must_use]
    pub fn new(revision: u64, value: Vec<u8>) -> Self {
        Self {
            revision,
            value: Zeroizing::new(value),
        }
    }

    /// Monotonic compare-and-swap revision.
    #[must_use]
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Borrow the transient plaintext value.
    #[must_use]
    pub fn value(&self) -> &[u8] {
        self.value.as_slice()
    }
}

impl fmt::Debug for PluginStateValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PluginStateValue")
            .field("revision", &self.revision)
            .field("value_bytes", &self.value.len())
            .finish_non_exhaustive()
    }
}

/// Closed durable-state failures shared by native and WIT adapters.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PluginStateError {
    /// The guest supplied a non-portable logical key.
    #[error("invalid plugin state key")]
    InvalidKey,
    /// The admitted instance lacks the required state permission.
    #[error("plugin state access denied")]
    AccessDenied,
    /// No authenticated value exists for the requested key.
    #[error("plugin state value not found")]
    NotFound,
    /// The expected compare-and-swap revision did not match.
    #[error("plugin state revision conflict")]
    Conflict,
    /// The operation would exceed a fixed host quota.
    #[error("plugin state quota exceeded")]
    QuotaExceeded,
    /// Storage, key, decryption, or integrity validation failed closed.
    #[error("plugin state service unavailable")]
    Unavailable,
}

/// Async durable-state backend owned by the host runtime.
///
/// Guest inputs never include an instance namespace. Every operation receives
/// the immutable host-issued scope that owns the component store.
#[async_trait]
pub trait PluginStateBackend: Send + Sync {
    /// Read one authenticated value.
    async fn get(
        &self,
        scope: &PluginInstanceScope,
        key: &PluginStateKey,
    ) -> Result<Option<PluginStateValue>, PluginStateError>;

    /// Create or replace a value with compare-and-swap semantics.
    ///
    /// `None` requires the key to be absent; `Some(revision)` requires an exact
    /// match. The returned revision is the newly committed revision.
    async fn put(
        &self,
        scope: &PluginInstanceScope,
        key: &PluginStateKey,
        value: &[u8],
        expected_revision: Option<u64>,
    ) -> Result<u64, PluginStateError>;

    /// Delete a value only when `expected_revision` matches exactly.
    async fn delete(
        &self,
        scope: &PluginInstanceScope,
        key: &PluginStateKey,
        expected_revision: u64,
    ) -> Result<(), PluginStateError>;
}

/// Required generic state service injected into plugin stores.
#[derive(Clone)]
pub struct PluginStateService {
    backend: Arc<dyn PluginStateBackend>,
}

impl PluginStateService {
    /// Wrap the runtime-owned durable backend.
    #[must_use]
    pub fn new(backend: impl PluginStateBackend + 'static) -> Self {
        Self {
            backend: Arc::new(backend),
        }
    }

    pub(crate) async fn get(
        &self,
        scope: &PluginInstanceScope,
        key: &PluginStateKey,
    ) -> Result<Option<PluginStateValue>, PluginStateError> {
        if !scope.grants().allows(PluginPermission::StateRead) {
            return Err(PluginStateError::AccessDenied);
        }
        self.backend.get(scope, key).await
    }

    pub(crate) async fn put(
        &self,
        scope: &PluginInstanceScope,
        key: &PluginStateKey,
        value: &[u8],
        expected_revision: Option<u64>,
    ) -> Result<u64, PluginStateError> {
        if !scope.grants().allows(PluginPermission::StateWrite) {
            return Err(PluginStateError::AccessDenied);
        }
        self.backend.put(scope, key, value, expected_revision).await
    }

    pub(crate) async fn delete(
        &self,
        scope: &PluginInstanceScope,
        key: &PluginStateKey,
        expected_revision: u64,
    ) -> Result<(), PluginStateError> {
        if !scope.grants().allows(PluginPermission::StateWrite) {
            return Err(PluginStateError::AccessDenied);
        }
        self.backend.delete(scope, key, expected_revision).await
    }
}

#[cfg(test)]
struct UnavailableState;

#[cfg(test)]
#[async_trait]
impl PluginStateBackend for UnavailableState {
    async fn get(
        &self,
        _scope: &PluginInstanceScope,
        _key: &PluginStateKey,
    ) -> Result<Option<PluginStateValue>, PluginStateError> {
        Err(PluginStateError::Unavailable)
    }

    async fn put(
        &self,
        _scope: &PluginInstanceScope,
        _key: &PluginStateKey,
        _value: &[u8],
        _expected_revision: Option<u64>,
    ) -> Result<u64, PluginStateError> {
        Err(PluginStateError::Unavailable)
    }

    async fn delete(
        &self,
        _scope: &PluginInstanceScope,
        _key: &PluginStateKey,
        _expected_revision: u64,
    ) -> Result<(), PluginStateError> {
        Err(PluginStateError::Unavailable)
    }
}

/// Detail-free internal result converted to each WIT world's generated enum.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SecretLookupError {
    AccessDenied,
    NotFound,
    Unavailable,
}

/// Detail-free result for the guest-facing public-config service.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ConfigLookupError {
    AccessDenied,
    Unavailable,
}

impl PluginHostServices {
    /// Build the complete required host-service bundle.
    #[must_use]
    pub fn new(
        config: PluginConfigResolver,
        state: PluginStateService,
        egress: EgressHostService,
    ) -> Self {
        Self {
            config,
            state,
            egress,
        }
    }

    /// Resolve a typed, validated config view from canonical host state.
    ///
    /// # Errors
    ///
    /// Returns an error when canonical config cannot be read, does not satisfy
    /// the manifest schema, or was issued for a different instance scope.
    pub fn resolve_config(
        &self,
        scope: &PluginInstanceScope,
    ) -> Result<ResolvedPluginConfig, PluginError> {
        self.config.resolve(scope)
    }

    /// Resolve multiple secret properties from exactly one canonical config
    /// frame for this admitted instance.
    ///
    /// The operation is all-or-nothing: missing properties and resolver,
    /// rotation, or decryption failures return a closed error without exposing
    /// a partial bundle.
    pub fn resolve_secret_bundle(
        &self,
        scope: &PluginInstanceScope,
        references: impl IntoIterator<Item = SecretPropertyRef>,
    ) -> Result<ResolvedSecretBundle, SecretBundleError> {
        if !scope.grants().allows(PluginPermission::ConfigRead) {
            return Err(SecretBundleError::AccessDenied);
        }
        let config = self
            .config
            .resolve(scope)
            .map_err(|_| SecretBundleError::Unavailable)?;
        let mut values = HashMap::new();
        for reference in references {
            let value = config
                .secret_ref(&reference)
                .ok_or(SecretBundleError::NotFound)?;
            values.insert(reference, Zeroizing::new(value.to_string()));
        }
        Ok(ResolvedSecretBundle { values })
    }

    /// Runtime-owned durable state service for this store.
    #[must_use]
    pub(crate) fn state(&self) -> &PluginStateService {
        &self.state
    }

    /// Shared live-policy egress boundary for every plugin transport.
    #[must_use]
    pub fn egress(&self) -> &EgressHostService {
        &self.egress
    }
}

/// Empty config service for store-plumbing tests. Production callers must
/// inject their canonical resolver explicitly.
#[cfg(test)]
pub(crate) fn test_host_services() -> PluginHostServices {
    use crate::{PluginManifest, PluginPermission, config::resolve_plugin_config};

    let config = PluginConfigResolver::new(|scope| {
        let manifest = PluginManifest {
            name: scope.id().package().to_string(),
            version: "0.0.0-test".to_string(),
            description: None,
            author: None,
            wasm_path: None,
            wasm_sha256: None,
            capabilities: vec![scope.id().capability()],
            permissions: vec![PluginPermission::ConfigRead],
            config_schema: Some(serde_json::json!({
                "$schema": "https://json-schema.org/draft/2020-12/schema",
                "type": "object",
                "properties": {},
                "additionalProperties": false
            })),
            signature: None,
            publisher_key: None,
        };
        resolve_plugin_config(&manifest, scope, None)
    });
    test_services(config)
}

/// Complete test bundle around a test-owned config resolver.
#[cfg(test)]
pub(crate) fn test_services(config: PluginConfigResolver) -> PluginHostServices {
    PluginHostServices::new(
        config,
        PluginStateService::new(UnavailableState),
        test_egress_service(),
    )
}

#[cfg(test)]
pub(crate) fn test_egress_service() -> EgressHostService {
    use crate::egress::{EgressPolicy, EgressPolicyResolver};

    EgressHostService::new(EgressPolicyResolver::new(|_| {
        EgressPolicy::new([], [], [], 16)
    }))
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::config::resolve_plugin_config;
    use crate::{PluginCapability, PluginManifest};

    fn secret_manifest() -> PluginManifest {
        PluginManifest {
            name: "service-fixture".to_string(),
            version: "0.0.0-test".to_string(),
            description: None,
            author: None,
            wasm_path: Some("fixture.wasm".to_string()),
            wasm_sha256: None,
            capabilities: vec![PluginCapability::Channel],
            permissions: vec![PluginPermission::ConfigRead],
            config_schema: Some(serde_json::json!({
                "$schema": "https://json-schema.org/draft/2020-12/schema",
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "ca": {"type": "string", "x-secret": true},
                    "cert": {"type": "string", "x-secret": true},
                    "key": {"type": "string", "x-secret": true}
                }
            })),
            signature: None,
            publisher_key: None,
        }
    }

    fn scope(manifest: &PluginManifest) -> PluginInstanceScope {
        PluginInstanceScope::from_manifest(
            manifest,
            PluginCapability::Channel,
            "main",
            [PluginPermission::ConfigRead],
        )
        .unwrap()
    }

    #[test]
    fn secret_bundle_uses_one_coherent_epoch_and_redacts_debug() {
        let manifest = Arc::new(secret_manifest());
        let scope = scope(&manifest);
        let calls = Arc::new(AtomicUsize::new(0));
        let resolver_manifest = Arc::clone(&manifest);
        let resolver_calls = Arc::clone(&calls);
        let resolver = PluginConfigResolver::new(move |scope| {
            let epoch = resolver_calls.fetch_add(1, Ordering::SeqCst) + 1;
            let values = HashMap::from([
                ("ca".to_string(), format!("ca-{epoch}")),
                ("cert".to_string(), format!("cert-{epoch}")),
                ("key".to_string(), format!("key-{epoch}")),
            ]);
            resolve_plugin_config(&resolver_manifest, scope, Some(&values))
        });
        let services = test_services(resolver);
        let ca = SecretPropertyRef::parse("ca").unwrap();
        let cert = SecretPropertyRef::parse("cert").unwrap();
        let key = SecretPropertyRef::parse("key").unwrap();

        let bundle = services
            .resolve_secret_bundle(&scope, [ca.clone(), cert.clone(), key.clone()])
            .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(bundle.get(&ca), Some("ca-1"));
        assert_eq!(bundle.get(&cert), Some("cert-1"));
        assert_eq!(bundle.get(&key), Some("key-1"));
        let debug = format!("{bundle:?}");
        for secret in ["ca-1", "cert-1", "key-1"] {
            assert!(!debug.contains(secret));
        }
    }

    #[test]
    fn secret_bundle_missing_and_resolver_failures_return_no_partial_values() {
        let manifest = Arc::new(secret_manifest());
        let scope = scope(&manifest);
        let missing_manifest = Arc::clone(&manifest);
        let missing = test_services(PluginConfigResolver::new(move |scope| {
            let values = HashMap::from([
                ("ca".to_string(), "ca-only".to_string()),
                ("cert".to_string(), "cert-only".to_string()),
            ]);
            resolve_plugin_config(&missing_manifest, scope, Some(&values))
        }));
        let references = [
            SecretPropertyRef::parse("ca").unwrap(),
            SecretPropertyRef::parse("key").unwrap(),
        ];
        assert!(matches!(
            missing.resolve_secret_bundle(&scope, references.clone()),
            Err(SecretBundleError::NotFound)
        ));

        let failed = test_services(PluginConfigResolver::new(|_| {
            Err(PluginError::InvalidConfig("decryption failed".to_string()))
        }));
        assert!(matches!(
            failed.resolve_secret_bundle(&scope, references),
            Err(SecretBundleError::Unavailable)
        ));
    }

    #[derive(Clone, Default)]
    struct CountingState {
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl PluginStateBackend for CountingState {
        async fn get(
            &self,
            _scope: &PluginInstanceScope,
            _key: &PluginStateKey,
        ) -> Result<Option<PluginStateValue>, PluginStateError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(None)
        }

        async fn put(
            &self,
            _scope: &PluginInstanceScope,
            _key: &PluginStateKey,
            _value: &[u8],
            _expected_revision: Option<u64>,
        ) -> Result<u64, PluginStateError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(1)
        }

        async fn delete(
            &self,
            _scope: &PluginInstanceScope,
            _key: &PluginStateKey,
            _expected_revision: u64,
        ) -> Result<(), PluginStateError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    #[tokio::test]
    async fn state_service_rechecks_permissions_before_backend_dispatch() {
        let manifest = PluginManifest {
            name: "state-service-fixture".to_string(),
            version: "0.0.0-test".to_string(),
            description: None,
            author: None,
            wasm_path: Some("fixture.wasm".to_string()),
            wasm_sha256: None,
            capabilities: vec![PluginCapability::Tool],
            permissions: vec![PluginPermission::StateRead, PluginPermission::StateWrite],
            config_schema: None,
            signature: None,
            publisher_key: None,
        };
        let scope =
            PluginInstanceScope::from_manifest(&manifest, PluginCapability::Tool, "denied", [])
                .unwrap();
        let backend = CountingState::default();
        let calls = Arc::clone(&backend.calls);
        let service = PluginStateService::new(backend);
        let key = PluginStateKey::parse("value").unwrap();

        assert!(matches!(
            service.get(&scope, &key).await,
            Err(PluginStateError::AccessDenied)
        ));
        assert_eq!(
            service.put(&scope, &key, b"secret", None).await,
            Err(PluginStateError::AccessDenied)
        );
        assert_eq!(
            service.delete(&scope, &key, 1).await,
            Err(PluginStateError::AccessDenied)
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
}
