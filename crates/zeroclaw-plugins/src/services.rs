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
    wake: AgentWakeService,
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
    /// A state service with no storage behind it: every call is unavailable.
    #[must_use]
    pub(crate) fn unavailable() -> Self {
        Self::new(UnavailableState)
    }

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

/// A backend that answers every call with [`PluginStateError::Unavailable`],
/// for host paths that instantiate a plugin without serving it (install-time
/// load verification) and for tests.
struct UnavailableState;

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

/// Detail-free failures from the host-mediated agent wake service.
///
/// This is the native mirror of the WIT `dispatch-error` enum, so the guest
/// boundary never has to infer a category from a message.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum AgentWakeError {
    /// The admitted instance lacks `agent_dispatch`, or the target is not in
    /// the instance row's operator-granted `dispatch_agents` list.
    #[error("plugin agent dispatch access denied")]
    AccessDenied,
    /// The target alias names no enabled agent.
    #[error("plugin agent dispatch target unknown")]
    UnknownAgent,
    /// The instance exceeded its wake budget.
    #[error("plugin agent dispatch rate limited")]
    RateLimited,
    /// The target or prompt failed host validation.
    #[error("plugin agent dispatch request invalid")]
    InvalidRequest,
    /// Outside an authorized tool frame, or no wake backend is wired.
    #[error("plugin agent dispatch service unavailable")]
    Unavailable,
}

/// Async wake backend owned by the host runtime.
///
/// The backend receives the immutable host-issued scope, so per-instance
/// operator grants (`dispatch_agents`, `dispatch_rate_per_min`) and the
/// dispatch-hop ceiling are resolved from canonical host state rather than from
/// anything the guest supplied. `caller_alias` is the host-owned alias of the
/// agent whose turn is executing the tool, and is never guest-controlled.
#[async_trait]
pub trait AgentWakeBackend: Send + Sync {
    /// Admit one asynchronous wake turn. Returning `Ok` means the turn was
    /// admitted, not that it has run.
    async fn wake(
        &self,
        scope: &PluginInstanceScope,
        caller_alias: Option<&str>,
        target_agent: &str,
        prompt: &str,
    ) -> Result<(), AgentWakeError>;
}

/// Required wake service injected into plugin stores.
#[derive(Clone)]
pub struct AgentWakeService {
    backend: Arc<dyn AgentWakeBackend>,
}

impl AgentWakeService {
    /// A wake service with no backend: every call is unavailable. This is the
    /// default for host paths that build stores without serving dispatch.
    #[must_use]
    pub(crate) fn unavailable() -> Self {
        Self::new(UnavailableWake)
    }

    /// Wrap the runtime-owned wake backend.
    #[must_use]
    pub fn new(backend: impl AgentWakeBackend + 'static) -> Self {
        Self {
            backend: Arc::new(backend),
        }
    }

    pub(crate) async fn wake(
        &self,
        scope: &PluginInstanceScope,
        caller_alias: Option<&str>,
        target_agent: &str,
        prompt: &str,
    ) -> Result<(), AgentWakeError> {
        if !scope.grants().allows(PluginPermission::AgentDispatch) {
            return Err(AgentWakeError::AccessDenied);
        }
        self.backend
            .wake(scope, caller_alias, target_agent, prompt)
            .await
    }
}

/// A backend that answers every call with [`AgentWakeError::Unavailable`], for
/// host paths that instantiate a plugin without serving it (install-time load
/// verification) and for tests.
struct UnavailableWake;

#[async_trait]
impl AgentWakeBackend for UnavailableWake {
    async fn wake(
        &self,
        _scope: &PluginInstanceScope,
        _caller_alias: Option<&str>,
        _target_agent: &str,
        _prompt: &str,
    ) -> Result<(), AgentWakeError> {
        Err(AgentWakeError::Unavailable)
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
    pub fn new(config: PluginConfigResolver, state: PluginStateService) -> Self {
        Self {
            config,
            state,
            wake: AgentWakeService::unavailable(),
        }
    }

    /// Replace the wake service for a host path that serves dispatch.
    ///
    /// A builder rather than a constructor argument: every existing caller
    /// builds stores without serving dispatch, and must keep that behaviour
    /// fail-closed rather than being forced to name an authority it has none of.
    #[must_use]
    pub fn with_wake(mut self, wake: AgentWakeService) -> Self {
        self.wake = wake;
        self
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

    /// Runtime-owned agent wake service for this store.
    #[must_use]
    pub(crate) fn wake(&self) -> &AgentWakeService {
        &self.wake
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
            provides: None,
            permissions: vec![PluginPermission::ConfigRead],
            config_schema: Some(serde_json::json!({
                "$schema": "https://json-schema.org/draft/2020-12/schema",
                "type": "object",
                "properties": {},
                "additionalProperties": false
            })),
            signature: None,
            publisher_key: None,
            egress: Default::default(),
        };
        resolve_plugin_config(&manifest, scope, None)
    });
    test_services(config)
}

/// Complete test bundle around a test-owned config resolver.
#[cfg(test)]
pub(crate) fn test_services(config: PluginConfigResolver) -> PluginHostServices {
    PluginHostServices::new(config, PluginStateService::unavailable())
}

/// A test bundle that serves agent dispatch through `wake`.
#[cfg(test)]
pub(crate) fn test_host_services_with_wake(wake: AgentWakeService) -> PluginHostServices {
    test_host_services().with_wake(wake)
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
            provides: None,
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
            egress: Default::default(),
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
            provides: None,
            permissions: vec![PluginPermission::StateRead, PluginPermission::StateWrite],
            config_schema: None,
            signature: None,
            publisher_key: None,
            egress: Default::default(),
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

/// A recording [`AgentWakeBackend`] for host-service tests.
///
/// It records the exact caller alias, target, and prompt it received, so a test
/// can assert the store forwarded host-owned identity unchanged rather than
/// re-deriving it from guest input.
#[cfg(test)]
pub(crate) struct RecordingWake {
    calls: Arc<std::sync::Mutex<Vec<RecordedWake>>>,
}

/// One recorded [`RecordingWake`] call.
#[cfg(test)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RecordedWake {
    pub(crate) caller_alias: Option<String>,
    pub(crate) target_agent: String,
    pub(crate) prompt: String,
}

#[cfg(test)]
impl RecordingWake {
    pub(crate) fn service() -> (AgentWakeService, Self) {
        let recorder = Self {
            calls: Arc::new(std::sync::Mutex::new(Vec::new())),
        };
        (AgentWakeService::new(recorder.clone()), recorder)
    }

    pub(crate) fn recorded(&self) -> Vec<RecordedWake> {
        self.calls.lock().expect("recorded wakes").clone()
    }
}

#[cfg(test)]
impl Default for RecordingWake {
    fn default() -> Self {
        Self {
            calls: Arc::new(std::sync::Mutex::new(Vec::new())),
        }
    }
}

#[cfg(test)]
impl Clone for RecordingWake {
    fn clone(&self) -> Self {
        Self {
            calls: Arc::clone(&self.calls),
        }
    }
}

#[cfg(test)]
#[async_trait]
impl AgentWakeBackend for RecordingWake {
    async fn wake(
        &self,
        _scope: &PluginInstanceScope,
        caller_alias: Option<&str>,
        target_agent: &str,
        prompt: &str,
    ) -> Result<(), AgentWakeError> {
        self.calls
            .lock()
            .expect("recorded wakes")
            .push(RecordedWake {
                caller_alias: caller_alias.map(ToOwned::to_owned),
                target_agent: target_agent.to_string(),
                prompt: prompt.to_string(),
            });
        Ok(())
    }
}

#[cfg(test)]
mod wake_tests {
    use super::*;
    use crate::{PluginCapability, PluginManifest};

    fn wake_scope(granted: bool) -> PluginInstanceScope {
        let manifest = PluginManifest {
            name: "waker".to_string(),
            version: "0.0.0-test".to_string(),
            description: None,
            author: None,
            wasm_path: Some("waker.wasm".to_string()),
            wasm_sha256: None,
            capabilities: vec![PluginCapability::Tool],
            provides: None,
            permissions: vec![PluginPermission::AgentDispatch],
            config_schema: None,
            signature: None,
            publisher_key: None,
            egress: Default::default(),
        };
        let grants: Vec<PluginPermission> = if granted {
            vec![PluginPermission::AgentDispatch]
        } else {
            Vec::new()
        };
        PluginInstanceScope::from_manifest(&manifest, PluginCapability::Tool, "main", grants)
            .expect("valid wake test scope")
    }

    #[tokio::test]
    async fn wake_service_rechecks_permissions_before_backend_dispatch() {
        let (service, recorder) = RecordingWake::service();

        assert_eq!(
            service.wake(&wake_scope(false), None, "alpha", "go").await,
            Err(AgentWakeError::AccessDenied),
            "an ungranted scope must be refused before the backend runs"
        );
        assert!(
            recorder.recorded().is_empty(),
            "a denied wake must not reach the backend"
        );
    }

    #[tokio::test]
    async fn wake_service_forwards_caller_target_and_prompt_unchanged() {
        let (service, recorder) = RecordingWake::service();

        assert_eq!(
            service
                .wake(
                    &wake_scope(true),
                    Some("redaction-primary"),
                    "alpha-1_b",
                    "line one\nline two\t"
                )
                .await,
            Ok(())
        );
        assert_eq!(
            recorder.recorded(),
            vec![RecordedWake {
                caller_alias: Some("redaction-primary".to_string()),
                target_agent: "alpha-1_b".to_string(),
                prompt: "line one\nline two\t".to_string(),
            }]
        );
    }

    #[tokio::test]
    async fn default_wake_service_fails_closed_even_when_granted() {
        let services = test_host_services();
        assert_eq!(
            services
                .wake()
                .wake(&wake_scope(true), None, "alpha", "go")
                .await,
            Err(AgentWakeError::Unavailable),
            "a host path that serves no dispatch must not report success"
        );
    }
}
