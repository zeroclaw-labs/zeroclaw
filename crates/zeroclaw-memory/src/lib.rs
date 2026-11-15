#![allow(clippy::to_string_in_format_args)]
//! Memory subsystem: backends, embeddings, consolidation, retrieval.

/// Opening delimiter for recalled memory injected into provider context.
pub const MEMORY_CONTEXT_OPEN: &str = "[Memory context]";
/// Closing delimiter for recalled memory injected into provider context.
pub const MEMORY_CONTEXT_CLOSE: &str = "[/Memory context]";
/// Whether this build includes the embedded Tachi `portable-kernel` backend.
pub const TACHI_COMPILED: bool = cfg!(feature = "tachi");

pub mod admissibility;
pub mod agent_scoped;
pub mod agent_scoped_markdown;
pub mod audit;
pub mod backend;
pub mod budget;
pub mod chunker;
pub mod classify;
pub mod companion;
pub mod conflict;
pub mod consolidation;
pub mod decay;
pub mod dedup;
pub mod embeddings;
pub mod hygiene;
pub mod importance;
#[cfg(feature = "tachi")]
pub mod kernel_store;
pub mod lucid;
pub mod markdown;
pub mod merge;
pub mod migration;
#[cfg(feature = "tachi")]
pub mod native_migration;
pub mod none;
pub mod normalize;
mod point_scope;
pub mod policy;
pub mod policy_gate;
#[cfg(feature = "tachi")]
pub mod portable_migration;
pub mod redact;
pub mod rerank;
pub mod response_cache;
pub mod retrieval;
pub mod scanned;
pub mod snapshot;
pub mod sqlite;
#[cfg(feature = "tachi")]
pub mod tachi;
#[cfg(feature = "tachi")]
mod tachi_enrichment;
#[cfg(feature = "tachi")]
pub mod tachi_governance;
#[cfg(all(test, feature = "tachi"))]
mod tachi_scenarios;
pub mod threat;
pub mod traits;
pub mod vector;

pub use agent_scoped::AgentScopedMemory;
pub use agent_scoped_markdown::{AgentScopedMarkdownMemory, MarkdownPeer};
pub use audit::AuditedMemory;
#[allow(unused_imports)]
pub use backend::{
    MemoryBackendKind, MemoryBackendProfile, RETIRED_MEMORY_BACKENDS, classify_memory_backend,
    default_memory_backend_key, memory_backend_profile, selectable_memory_backends,
};
pub use companion::{
    CompanionStore, OUTBOX_OBSERVE_INTERVAL_SECS, OUTBOX_PENDING_AGE_WARN_SECS,
    clone_for_subsystems, companion_outbox_health, create_companion_store,
    probe_companion_outbox_health, reload_companion_store,
};
#[allow(unused_imports)]
pub use embeddings::EmbeddingIdentity;
pub use lucid::LucidMemory;
pub use markdown::MarkdownMemory;
pub use none::NoneMemory;
#[allow(unused_imports)]
pub use policy::PolicyEnforcer;
pub use rerank::{RerankConfig, RerankStrategy};
pub use response_cache::ResponseCache;
#[allow(unused_imports)]
pub use retrieval::{RetrievalConfig, RetrievalPipeline};
pub use scanned::ScannedMemory;
pub use sqlite::SqliteMemory;
#[cfg(feature = "tachi")]
pub use tachi::TachiMemory;
#[cfg(feature = "tachi")]
pub use tachi_governance::{TachiGovernanceReport, run_tachi_governance};
pub use traits::Memory;

/// Run [`Memory::run_llm_enrichment`] when the 12h enrichment cadence is due.
///
/// Advances the enrichment state-file marker after the attempt. If consolidation
/// (the strategy call site) is never invoked, enrichment never runs — acceptable
/// without a speculative config key. Non-tachi backends return 0 via the trait
/// default.
pub async fn run_llm_enrichment_if_due(
    memory: &dyn Memory,
    workspace_dir: &Path,
    provider: &dyn zeroclaw_api::model_provider::ModelProvider,
    model: &str,
) -> anyhow::Result<usize> {
    if !hygiene::enrichment_is_due(workspace_dir)? {
        return Ok(0);
    }
    let n = memory.run_llm_enrichment(provider, model).await?;
    hygiene::enrichment_mark_ran(workspace_dir)?;
    Ok(n)
}
#[allow(unused_imports)]
pub use traits::{
    ExportFilter, MemoryCategory, MemoryEntry, ProceduralMessage, is_recent_recall_query,
    normalize_recent_recall_query,
};

use anyhow::Context;
use std::path::Path;
use std::sync::Arc;
use zeroclaw_config::providers::ModelProviders;
use zeroclaw_config::schema::{
    ActiveStorage, Config, EmbeddingRouteConfig, MemoryConfig, MemoryPolicyConfig,
};

/// Reserved storage namespace for Soul-shaped rows. Ambient memory
/// surfaces (plain recall, list, get, forget, and plain stores) exclude
/// and refuse this namespace at the storage layer (sqlite and tachi
/// backends, agent-scoped wrappers), so no memory tool, RPC, or wrapper
/// can host or surface Soul-looking content. The live Soul model is the
/// separate Soul profile store (`soul.db`); this reservation only keeps
/// the general memory store from becoming a second one.
pub(crate) const SOUL_NAMESPACE: &str = "soul";

/// Reserved key prefix paired with [`SOUL_NAMESPACE`]: keys under it may
/// exist only in the reserved namespace, and the reserved namespace
/// accepts only keys under it.
pub(crate) const SOUL_KEY_PREFIX: &str = "soul::";

/// Explicit construction error for a removed backend. Selecting one never
/// falls back to another store: the operator must pick a supported backend.
fn retired_backend_error(backend: &str, selector: &str) -> anyhow::Error {
    ::zeroclaw_log::record!(
        ERROR,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
            .with_attrs(::serde_json::json!({ "backend": backend, "selector": selector })),
        "memory backend was removed"
    );
    anyhow::Error::msg(format!(
        "{selector} selects the '{backend}' memory backend, which was removed; set it to \
         sqlite, lucid, markdown or none (existing data is not migrated automatically)"
    ))
}

/// Wrap the backend in the `AuditedMemory` decorator when
/// `[memory] audit_enabled = true`; pass it through untouched otherwise
/// (the default), so the flag-off path is byte-identical to an unwrapped
/// backend.
fn wrap_audit<M: Memory + 'static>(
    memory: M,
    workspace_dir: &Path,
    audit_enabled: bool,
) -> anyhow::Result<Box<dyn Memory>> {
    if audit_enabled {
        Ok(Box::new(AuditedMemory::new(memory, workspace_dir)?))
    } else {
        Ok(Box::new(memory))
    }
}

/// Compose the two install-wide decorators exactly once. Content scanning is
/// closest to storage; the optional audit wrapper observes the resulting
/// success or failure without bypassing the security boundary.
fn wrap_scanned_and_audit<M: Memory + 'static>(
    memory: M,
    policy: &MemoryPolicyConfig,
    workspace_dir: &Path,
    audit_enabled: bool,
) -> anyhow::Result<Box<dyn Memory>> {
    wrap_audit(
        ScannedMemory::new(memory, policy),
        workspace_dir,
        audit_enabled,
    )
}

fn create_memory_with_builders<F>(
    backend_name: &str,
    workspace_dir: &Path,
    mut sqlite_builder: F,
    unknown_context: &str,
    policy: &MemoryPolicyConfig,
    audit_enabled: bool,
) -> anyhow::Result<Box<dyn Memory>>
where
    F: FnMut() -> anyhow::Result<SqliteMemory>,
{
    match classify_memory_backend(backend_name) {
        MemoryBackendKind::Sqlite => {
            wrap_scanned_and_audit(sqlite_builder()?, policy, workspace_dir, audit_enabled)
        }
        MemoryBackendKind::Lucid => {
            let local = sqlite_builder()?;
            wrap_scanned_and_audit(
                LucidMemory::new("lucid", workspace_dir, local),
                policy,
                workspace_dir,
                audit_enabled,
            )
        }
        MemoryBackendKind::Retired(backend) => {
            Err(retired_backend_error(backend, "memory.backend"))
        }
        MemoryBackendKind::Markdown => wrap_scanned_and_audit(
            MarkdownMemory::new("markdown", workspace_dir),
            policy,
            workspace_dir,
            audit_enabled,
        ),
        #[cfg(feature = "tachi")]
        MemoryBackendKind::Tachi => {
            anyhow::bail!(
                "tachi backend requires embedding config; \
                 call create_memory_with_storage_and_routes instead of create_memory_with_builders"
            )
        }
        MemoryBackendKind::None => wrap_scanned_and_audit(
            NoneMemory::new("none"),
            policy,
            workspace_dir,
            audit_enabled,
        ),
        MemoryBackendKind::Unknown => {
            ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"backend_name": backend_name, "unknown_context": unknown_context})), "Unknown memory backend '', falling back to markdown");
            wrap_scanned_and_audit(
                MarkdownMemory::new("markdown", workspace_dir),
                policy,
                workspace_dir,
                audit_enabled,
            )
        }
    }
}

/// Extract the backend kind from a V3 dotted reference (`<kind>.<alias>`).
/// Bare names (`"sqlite"`) are returned as-is. Returned lowercase.
pub fn backend_kind_from_dotted(memory_backend: &str) -> String {
    memory_backend
        .trim()
        .split_once('.')
        .map_or(memory_backend.trim(), |(kind, _)| kind)
        .to_ascii_lowercase()
}

/// Legacy auto-save key used for model-authored assistant summaries.
/// These entries are treated as untrusted context and should not be re-injected.
pub fn is_assistant_autosave_key(key: &str) -> bool {
    let normalized = key.trim().to_ascii_lowercase();
    normalized == "assistant_resp" || normalized.starts_with("assistant_resp_")
}

pub fn is_user_autosave_key(key: &str) -> bool {
    let normalized = key.trim().to_ascii_lowercase();
    normalized == "user_msg" || normalized.starts_with("user_msg_")
}

/// Filter known synthetic autosave noise patterns that should not be
/// persisted as user conversation memories.
pub fn should_skip_autosave_content(content: &str) -> bool {
    let normalized = content.trim();
    if normalized.is_empty() {
        return true;
    }

    let lowered = normalized.to_ascii_lowercase();
    lowered.starts_with("[cron:")
        || lowered.starts_with("[heartbeat task")
        || lowered.starts_with("[distilled_")
        || starts_with_ignore_ascii_case(normalized, MEMORY_CONTEXT_OPEN)
        || lowered.contains("distilled_index_sig:")
}

fn starts_with_ignore_ascii_case(value: &str, prefix: &str) -> bool {
    value
        .get(..prefix.len())
        .is_some_and(|candidate| candidate.eq_ignore_ascii_case(prefix))
}

#[derive(Clone, PartialEq, Eq)]
struct ResolvedEmbeddingConfig {
    model_provider: String,
    model: String,
    dimensions: usize,
    api_key: Option<String>,
}

impl std::fmt::Debug for ResolvedEmbeddingConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResolvedEmbeddingConfig")
            .field("model_provider", &self.model_provider)
            .field("model", &self.model)
            .field("dimensions", &self.dimensions)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddingSettings {
    pub model_provider: String,
    pub model: String,
    pub dimensions: usize,
    pub api_key: Option<String>,
}

pub fn resolve_embedding_settings(
    config: &MemoryConfig,
    embedding_routes: &[EmbeddingRouteConfig],
    api_key: Option<&str>,
    providers: Option<&ModelProviders>,
) -> EmbeddingSettings {
    let resolved = resolve_embedding_config(config, embedding_routes, api_key, providers);
    EmbeddingSettings {
        model_provider: resolved.model_provider,
        model: resolved.model,
        dimensions: resolved.dimensions,
        api_key: resolved.api_key,
    }
}

fn resolve_embedding_config(
    config: &MemoryConfig,
    embedding_routes: &[EmbeddingRouteConfig],
    api_key: Option<&str>,
    providers: Option<&ModelProviders>,
) -> ResolvedEmbeddingConfig {
    let inherited_api_key = api_key
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let configured_api_key = config
        .embedding_api_key
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);

    let fallback = || {
        resolve_provider_ref(
            config.embedding_provider.trim().to_string(),
            config.embedding_model.trim().to_string(),
            config.embedding_dimensions,
            configured_api_key.clone(),
            inherited_api_key.clone(),
            providers,
        )
    };

    let Some(hint) = config
        .embedding_model
        .strip_prefix("hint:")
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return fallback();
    };

    let Some(route) = embedding_routes
        .iter()
        .find(|route| route.hint.trim() == hint)
    else {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                .with_attrs(::serde_json::json!({"hint": hint})),
            "Unknown embedding route hint; falling back to [memory] embedding settings"
        );
        return fallback();
    };

    let model_provider = route.model_provider.trim();
    let model = route.model.trim();
    let dimensions = route.dimensions.unwrap_or(config.embedding_dimensions);
    if model_provider.is_empty() || model.is_empty() || dimensions == 0 {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                .with_attrs(::serde_json::json!({"hint": hint})),
            "Invalid embedding route configuration; falling back to [memory] embedding settings"
        );
        return fallback();
    }

    let routed_api_key = route
        .api_key
        .as_deref()
        .map(str::trim)
        .filter(|value: &&str| !value.is_empty())
        .map(|value| value.to_string());

    resolve_provider_ref(
        model_provider.to_string(),
        model.to_string(),
        dimensions,
        routed_api_key.or(configured_api_key),
        inherited_api_key,
        providers,
    )
}

fn resolve_provider_ref(
    model_provider: String,
    model: String,
    dimensions: usize,
    explicit_api_key: Option<String>,
    inherited_api_key: Option<String>,
    providers: Option<&ModelProviders>,
) -> ResolvedEmbeddingConfig {
    let trimmed = model_provider.trim();
    let is_dotted_ref =
        !trimmed.is_empty() && !trimmed.starts_with("custom:") && trimmed.contains('.');
    if !is_dotted_ref {
        return ResolvedEmbeddingConfig {
            model_provider,
            model,
            dimensions,
            api_key: explicit_api_key.or(inherited_api_key),
        };
    }

    let reference = trimmed.to_string();
    let Some((kind, _alias, provider_cfg)) =
        providers.and_then(|catalog| catalog.find_by_name(&reference))
    else {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                .with_attrs(::serde_json::json!({
                    "error_key": "memory.embedding_route_unresolved",
                    "provider_ref": reference,
                })),
            "Embedding provider reference did not resolve against providers.models; \
             embeddings disabled (keyword-only) for this profile"
        );
        return ResolvedEmbeddingConfig {
            model_provider,
            model,
            dimensions,
            api_key: explicit_api_key.or(inherited_api_key),
        };
    };

    let provider_key = provider_cfg
        .api_key
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let concrete_provider = match provider_cfg
        .uri
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some(uri) => Some(format!("custom:{uri}")),
        None if matches!(kind, "openai" | "openrouter") => Some(kind.to_string()),
        None => None,
    };
    let Some(concrete_provider) = concrete_provider else {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                .with_attrs(::serde_json::json!({
                    "error_key": "memory.embedding_route_no_endpoint",
                    "provider_ref": reference,
                    "provider_kind": kind,
                })),
            "Embedding provider reference resolved but has no usable embeddings \
             endpoint (set its `uri`, or point the route at an openai/openrouter \
             compatible profile); embeddings disabled (keyword-only) for this profile"
        );
        return ResolvedEmbeddingConfig {
            model_provider,
            model,
            dimensions,
            api_key: explicit_api_key.or(inherited_api_key),
        };
    };

    ResolvedEmbeddingConfig {
        model_provider: concrete_provider,
        model,
        dimensions,
        api_key: explicit_api_key.or(provider_key).or(inherited_api_key),
    }
}

pub fn create_memory(
    config: &MemoryConfig,
    workspace_dir: &Path,
    api_key: Option<&str>,
) -> anyhow::Result<Box<dyn Memory>> {
    if config.backend.trim().contains('.') {
        anyhow::bail!(
            "memory backend {:?} references a storage alias; construct memory from the full Config so the selected alias is applied",
            config.backend
        );
    }

    create_memory_with_storage_and_routes(
        config,
        &[],
        ActiveStorage::None,
        workspace_dir,
        api_key,
        None,
    )
}

/// Construct memory from the canonical loaded configuration.
///
/// Config-aware production paths should use this entrypoint so the selected
/// storage alias, embedding route, and provider settings are applied together.
pub fn create_memory_from_config(
    config: &Config,
    api_key: Option<&str>,
) -> anyhow::Result<Box<dyn Memory>> {
    create_memory_with_storage_and_routes(
        &config.memory,
        &config.embedding_routes,
        config.resolve_active_storage(),
        &config.data_dir,
        api_key,
        Some(&config.providers.models),
    )
}

fn build_lucid_memory(
    workspace_dir: &Path,
    local: SqliteMemory,
    active_storage: ActiveStorage<'_>,
) -> LucidMemory {
    // Lucid predates typed storage aliases and still supports the bare
    // `memory.backend = "lucid"` form. A resolved alias overrides the
    // executable and deadlines; otherwise the constructor uses defaults.
    let (binary_path, recall_timeout_ms, store_timeout_ms) = match active_storage {
        ActiveStorage::Lucid(lucid) => (
            lucid.binary_path.clone(),
            lucid.recall_timeout_ms,
            lucid.store_timeout_ms,
        ),
        _ => (None, None, None),
    };

    LucidMemory::with_overrides(
        "lucid",
        workspace_dir,
        local,
        binary_path,
        recall_timeout_ms,
        store_timeout_ms,
    )
}

pub fn create_memory_with_storage_and_routes(
    config: &MemoryConfig,
    embedding_routes: &[EmbeddingRouteConfig],
    active_storage: ActiveStorage<'_>,
    workspace_dir: &Path,
    api_key: Option<&str>,
    providers: Option<&ModelProviders>,
) -> anyhow::Result<Box<dyn Memory>> {
    let backend_name = backend_kind_from_dotted(&config.backend);
    let backend_kind = classify_memory_backend(&backend_name);
    let resolved_embedding = resolve_embedding_config(config, embedding_routes, api_key, providers);

    // Same 12h cadence as hygiene — capture before `run_if_due` writes state so
    // tachi light-sleep can share the window without a second due-check race.
    #[cfg(feature = "tachi")]
    let light_sleep_due = config.hygiene_enabled && hygiene::is_due(workspace_dir).unwrap_or(true);

    // Best-effort memory hygiene/retention pass (throttled by state file).
    if let Err(e) = hygiene::run_if_due(config, workspace_dir) {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
            "memory hygiene skipped"
        );
    }

    // If snapshot_on_hygiene is enabled, export core memories during hygiene.
    if config.snapshot_enabled
        && config.snapshot_on_hygiene
        && matches!(
            backend_kind,
            MemoryBackendKind::Sqlite | MemoryBackendKind::Lucid
        )
        && let Err(e) = snapshot::export_snapshot(workspace_dir)
    {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
            "memory snapshot skipped"
        );
    }

    // Auto-hydration: if brain.db is missing but MEMORY_SNAPSHOT.md exists,
    // restore the "soul" from the snapshot before creating the backend.
    if config.auto_hydrate
        && matches!(
            backend_kind,
            MemoryBackendKind::Sqlite | MemoryBackendKind::Lucid
        )
        && snapshot::should_hydrate(workspace_dir)
    {
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
            "cold boot detected; hydrating from MEMORY_SNAPSHOT.md"
        );
        match snapshot::hydrate_from_snapshot(workspace_dir) {
            Ok(count) => {
                if count > 0 {
                    ::zeroclaw_log::record!(
                        INFO,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_attrs(::serde_json::json!({"count": count})),
                        "hydrated core memories from snapshot"
                    );
                }
            }
            Err(e) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                    "memory hydration failed"
                );
            }
        }
    }

    fn build_sqlite_memory(
        config: &MemoryConfig,
        sqlite_open_timeout_secs: Option<u64>,
        workspace_dir: &Path,
        resolved_embedding: &ResolvedEmbeddingConfig,
    ) -> anyhow::Result<SqliteMemory> {
        let embedder: Arc<dyn embeddings::EmbeddingProvider> =
            Arc::from(embeddings::create_embedding_provider(
                &resolved_embedding.model_provider,
                resolved_embedding.api_key.as_deref(),
                &resolved_embedding.model,
                resolved_embedding.dimensions,
            ));
        let has_embedder = embedder.dimensions() > 0;

        #[allow(clippy::cast_possible_truncation)]
        let mem = SqliteMemory::with_embedder(
            "sqlite",
            workspace_dir,
            embedder,
            config.vector_weight as f32,
            config.keyword_weight as f32,
            config.embedding_cache_size,
            sqlite_open_timeout_secs,
            config.search_mode.clone(),
        )?;

        if has_embedder {
            reconcile_embedding_identity(
                &mem,
                &embeddings::EmbeddingIdentity {
                    provider: resolved_embedding.model_provider.clone(),
                    model: resolved_embedding.model.clone(),
                    dimensions: resolved_embedding.dimensions,
                },
                config.auto_reindex_on_identity_change,
            );
        }
        Ok(mem)
    }

    // Per-backend SQLite open-timeout override comes from the active storage
    // alias (V3); when no typed entry resolves, sqlite waits indefinitely.
    let sqlite_open_timeout_secs = match active_storage {
        ActiveStorage::Sqlite(sq) => sq.open_timeout_secs,
        _ => None,
    };

    if let MemoryBackendKind::Retired(backend) = backend_kind {
        return Err(retired_backend_error(backend, "memory.backend"));
    }

    if matches!(backend_kind, MemoryBackendKind::Lucid) {
        let local = build_sqlite_memory(
            config,
            sqlite_open_timeout_secs,
            workspace_dir,
            &resolved_embedding,
        )?;
        return wrap_scanned_and_audit(
            build_lucid_memory(workspace_dir, local, active_storage),
            &config.policy,
            workspace_dir,
            config.audit_enabled,
        );
    }

    #[cfg(feature = "tachi")]
    if matches!(backend_kind, MemoryBackendKind::Tachi) {
        let embedder: Arc<dyn embeddings::EmbeddingProvider> =
            Arc::from(embeddings::create_embedding_provider(
                &resolved_embedding.model_provider,
                resolved_embedding.api_key.as_deref(),
                &resolved_embedding.model,
                resolved_embedding.dimensions,
            ));
        let mem = TachiMemory::with_embedder(
            "tachi",
            workspace_dir,
            embedder,
            config.vector_weight as f32,
            config.keyword_weight as f32,
        )?;
        // Live cadence: light-sleep on the factory-owned store handle (no
        // second open). Deleting this call makes near-dup cadence tests RED.
        if light_sleep_due && let Err(e) = mem.run_light_sleep_governance() {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                "tachi light-sleep governance skipped"
            );
        }
        return wrap_scanned_and_audit(mem, &config.policy, workspace_dir, config.audit_enabled);
    }

    #[cfg(not(feature = "tachi"))]
    if backend_name == "tachi" {
        anyhow::bail!(
            "memory backend 'tachi' requested but this build was compiled without \
             `memory-tachi`; rebuild with `--features memory-tachi`"
        );
    }

    create_memory_with_builders(
        &backend_name,
        workspace_dir,
        || {
            build_sqlite_memory(
                config,
                sqlite_open_timeout_secs,
                workspace_dir,
                &resolved_embedding,
            )
        },
        "",
        &config.policy,
        config.audit_enabled,
    )
}

/// Outcome of a startup embedding-identity reconciliation.
#[derive(Debug, PartialEq, Eq)]
enum EmbeddingIdentityOutcome {
    /// No identity was recorded (fresh store, or one predating identity
    /// tracking): the current identity was adopted without touching vectors.
    Adopted,
    /// Stored identity matches the current config — nothing to do.
    Match,
    /// Stored identity differed: vectors were invalidated (set to NULL),
    /// the embedding cache cleared, and the new identity stamped.
    Invalidated(usize),
    /// Reconciliation failed; the store is untouched and the error was
    /// logged. Startup proceeds — recall degrades no further than it
    /// already would, and the next boot retries.
    Failed,
}

fn reconcile_embedding_identity(
    mem: &SqliteMemory,
    current: &embeddings::EmbeddingIdentity,
    auto_reindex: bool,
) -> EmbeddingIdentityOutcome {
    let stored = match mem.stored_embedding_identity() {
        Ok(stored) => stored,
        Err(e) => {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"error": format!("{e}")})),
                "memory: failed to read stored embedding identity; skipping reconciliation"
            );
            return EmbeddingIdentityOutcome::Failed;
        }
    };

    match stored {
        None => match mem.record_embedding_identity(current) {
            Ok(()) => EmbeddingIdentityOutcome::Adopted,
            Err(e) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"error": format!("{e}")})),
                    "memory: failed to record embedding identity; will retry next startup"
                );
                EmbeddingIdentityOutcome::Failed
            }
        },
        Some(stored) if stored == *current => EmbeddingIdentityOutcome::Match,
        Some(stored) => match mem.invalidate_embeddings_for_identity_change(current) {
            Ok(invalidated) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_attrs(::serde_json::json!({
                            "stored_identity": stored.to_string(),
                            "current_identity": current.to_string(),
                            "vectors_invalidated": invalidated,
                        })),
                    "memory: embedding identity changed; stored vectors invalidated and \
                     embedding cache cleared (content retained). Semantic recall is \
                     keyword-only until re-embedded — run `zeroclaw memory reindex`"
                );
                if auto_reindex && invalidated > 0 {
                    spawn_auto_reindex(mem);
                }
                EmbeddingIdentityOutcome::Invalidated(invalidated)
            }
            Err(e) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "stored_identity": stored.to_string(),
                            "current_identity": current.to_string(),
                            "error": format!("{e}"),
                        })),
                    "memory: embedding identity changed but invalidation failed; \
                     store untouched, will retry next startup"
                );
                EmbeddingIdentityOutcome::Failed
            }
        },
    }
}

/// Kick off the gated re-embed in the background after an identity
/// migration, when `[memory] auto_reindex_on_identity_change` opts in.
/// Outside an async runtime (no tokio context) the spawn is skipped and the
/// operator is pointed at `zeroclaw memory reindex` instead.
fn spawn_auto_reindex(mem: &SqliteMemory) {
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
            "memory: auto_reindex_on_identity_change is set but no async runtime is \
             available here; run `zeroclaw memory reindex` to re-embed"
        );
        return;
    };
    let mem = mem.clone();
    handle.spawn(async move {
        match mem.reindex().await {
            Ok(count) => {
                ::zeroclaw_log::record!(
                    INFO,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_attrs(::serde_json::json!({"reembedded": count})),
                    "memory: background re-embed after embedding identity change complete"
                );
            }
            Err(e) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"error": format!("{e}")})),
                    "memory: background re-embed after embedding identity change failed; \
                     run `zeroclaw memory reindex` to retry"
                );
            }
        }
    });
}

pub fn create_memory_for_migration(config: &Config) -> anyhow::Result<Box<dyn Memory>> {
    let backend = backend_kind_from_dotted(&config.memory.backend);
    if matches!(classify_memory_backend(&backend), MemoryBackendKind::None) {
        anyhow::bail!(
            "memory backend 'none' disables persistence; choose sqlite, lucid, or markdown before migration"
        );
    }

    // Operator surface (bulk import + CLI management): writes are still
    // scanned and logged, but flagged rows are persisted rather than
    // rejected so an import never stops partway through, and read-time
    // withholding is disabled so `memory list` / `get` show every stored
    // row for inspection and removal. The runtime factory
    // (`create_memory_with_storage_and_routes`) applies the configured
    // `[memory.policy]`, so flagged rows remain withheld from recall
    // wherever `threat_scan_load_time` is enabled.
    let policy = MemoryPolicyConfig {
        threat_scan_on_hit: "block-on-read".into(),
        threat_scan_load_time: false,
        ..MemoryPolicyConfig::default()
    };

    // Migration writes bypass the audit trail: the imported rows are bulk
    // history, not live memory operations.
    if matches!(classify_memory_backend(&backend), MemoryBackendKind::Lucid) {
        let local = SqliteMemory::new("sqlite", &config.data_dir)?;
        return wrap_scanned_and_audit(
            build_lucid_memory(&config.data_dir, local, config.resolve_active_storage()),
            &policy,
            &config.data_dir,
            false,
        );
    }

    #[cfg(feature = "tachi")]
    if matches!(classify_memory_backend(&backend), MemoryBackendKind::Tachi) {
        let mem = TachiMemory::new("tachi", &config.data_dir)?;
        return wrap_scanned_and_audit(mem, &policy, &config.data_dir, false);
    }

    #[cfg(not(feature = "tachi"))]
    if backend == "tachi" {
        anyhow::bail!(
            "memory backend 'tachi' requested but this build was compiled without \
             `memory-tachi`; rebuild with `--features memory-tachi`"
        );
    }

    create_memory_with_builders(
        &backend,
        &config.data_dir,
        || SqliteMemory::new("sqlite", &config.data_dir),
        " during migration",
        &policy,
        false,
    )
}

/// Wrap an agent memory handle in the [`RetrievalPipeline`] decorator.
///
/// The decorator makes one hybrid backend-recall call per query. Its only
/// add-on is an optional in-process hot cache, enabled when `[memory]
/// retrieval_stages` names `"cache"`. The default carries no `"cache"`, so
/// activating the decorator does not change default per-agent recall. The
/// reserved `"fts"` / `"vector"` names and `fts_early_return_score` are inert
/// until `Memory` exposes distinct FTS and vector operations.
fn wrap_in_retrieval_pipeline(memory: Arc<dyn Memory>, config: &MemoryConfig) -> Arc<dyn Memory> {
    let cache_enabled = config.retrieval_stages.iter().any(|stage| stage == "cache");
    Arc::new(retrieval::RetrievalPipeline::new(
        memory,
        RetrievalConfig {
            cache_enabled,
            ..RetrievalConfig::default()
        },
    ))
}

/// Build the per-agent memory wrapper for `agent_alias`.
///
/// Wraps the appropriate inner backend with `AgentScopedMemory` (for
/// SQL-backed agents — single shared backend, agent_id
/// column distinguishes rows) or `AgentScopedMarkdownMemory` (for
/// Markdown-backed agents — per-agent dirs, peer set composed from
/// the resolved `read_memory_from` allowlist). `NoneMemory` agents
/// pass through unwrapped.
///
/// The scoped handle is then wrapped in the [`RetrievalPipeline`] decorator
/// (outermost), so per-turn injection recall and memory tools share one
/// `Memory` contract. `NoneMemory` agents skip the decorator.
///
/// Cross-backend allowlist entries are rejected at config load, so by
/// the time we get here every entry on
/// `agents.<alias>.workspace.read_memory_from` is guaranteed to point
/// at a sibling on the same backend kind.
pub async fn create_memory_for_agent(
    config: &zeroclaw_config::schema::Config,
    agent_alias: &str,
    api_key: Option<&str>,
) -> anyhow::Result<Arc<dyn Memory>> {
    use zeroclaw_config::multi_agent::MemoryBackendKind as ConfigBackend;
    let agent_cfg = config
        .agents
        .get(agent_alias)
        .with_context(|| format!("agents.{agent_alias} is not configured"))?;
    let backend_kind = agent_cfg.memory.backend;
    if matches!(
        backend_kind,
        ConfigBackend::Postgres | ConfigBackend::Qdrant
    ) {
        let backend = match backend_kind {
            ConfigBackend::Postgres => "postgres",
            _ => "qdrant",
        };
        return Err(retired_backend_error(
            backend,
            &format!("agents.{agent_alias}.memory.backend"),
        ));
    }

    // Typed-memory producers are SQLite-only. Config::validate already
    // rejects this combination on every save path, but boot is
    // deliberately validation-resilient (a hand-edited config still
    // starts the daemon so the operator can repair it via /config), so
    // enforce again here: failing agent-memory construction is an
    // operator-visible startup error and keeps background consolidation
    // from ever running typed writes into a backend that would reject
    // them deep inside spawned work.
    if config.memory.types.enabled || config.memory.consolidation_extract_facts {
        let flag = if config.memory.types.enabled {
            "memory.types.enabled"
        } else {
            "memory.consolidation_extract_facts"
        };
        let global_kind = backend_kind_from_dotted(&config.memory.backend);
        if !matches!(global_kind.as_str(), "sqlite" | "tachi") {
            anyhow::bail!(
                "{flag} = true requires memory.backend = \"sqlite\" or \"tachi\" (typed memory storage is SQLite-only/Tachi-only), but memory.backend = {:?}",
                config.memory.backend
            );
        }
        if !matches!(backend_kind, ConfigBackend::Sqlite | ConfigBackend::Tachi) {
            anyhow::bail!(
                "{flag} = true requires every agent on the sqlite or tachi memory backend (typed memory storage is SQLite-only/Tachi-only), but agents.{agent_alias}.memory.backend = {backend_kind:?}"
            );
        }
    }

    // Markdown branch: the wrapper composes per-agent dirs, not a
    // shared backend. Skip the inner-backend factory entirely, but still
    // apply the install-wide policy decorator to own and peer Markdown
    // stores before composition.
    if matches!(backend_kind, ConfigBackend::Markdown) {
        let own_workspace = config.agent_workspace_dir(agent_alias);
        let own: Arc<dyn Memory> = Arc::new(ScannedMemory::new(
            MarkdownMemory::new("markdown", &own_workspace),
            &config.memory.policy,
        ));
        let mut peers: Vec<agent_scoped_markdown::MarkdownPeer> = Vec::new();
        for peer in &agent_cfg.workspace.read_memory_from {
            let peer_alias = peer.as_str();
            let peer_workspace = config.agent_workspace_dir(peer_alias);
            peers.push(agent_scoped_markdown::MarkdownPeer {
                alias: peer_alias.to_string(),
                memory: Arc::new(ScannedMemory::new(
                    MarkdownMemory::new("markdown", &peer_workspace),
                    &config.memory.policy,
                )),
            });
        }
        let scoped = AgentScopedMarkdownMemory::new(agent_alias, own, peers);
        // Route the composed per-agent wrapper through the same audit
        // decision as the install-wide factory: with `[memory]
        // audit_enabled = true` the wrapper's store/recall operations
        // write `memory/audit.db` rows and emit the `memory.audit` event;
        // default-off passes it through untouched (byte-identical). The
        // audit db is rooted at the install `data_dir` (shared across
        // agents), mirroring how the SQL/Lucid arms compose it.
        let audited: Arc<dyn Memory> = Arc::from(wrap_audit(
            scoped,
            &config.data_dir,
            config.memory.audit_enabled,
        )?);
        return Ok(wrap_in_retrieval_pipeline(audited, &config.memory));
    }

    // None branch: nothing to scope, no agents-table lookup needed. Still
    // route through the audit decision so an audit-enabled install records
    // attempted store/recall operations on the no-op backend; the
    // install-wide factory wraps `NoneMemory` the same way, and opt-in
    // audit coverage must not become backend/path-dependent.
    if matches!(backend_kind, ConfigBackend::None) {
        return Ok(Arc::from(wrap_audit(
            NoneMemory::new("none"),
            &config.data_dir,
            config.memory.audit_enabled,
        )?));
    }

    let agent_backend_name = match backend_kind {
        ConfigBackend::Sqlite => "sqlite",
        ConfigBackend::Lucid => "lucid",
        ConfigBackend::Tachi => "tachi",
        ConfigBackend::Markdown => "markdown",
        ConfigBackend::None => "none",
        ConfigBackend::Postgres => "postgres",
        ConfigBackend::Qdrant => "qdrant",
    };

    let inner = if backend_kind == ConfigBackend::default()
        || backend_kind_from_dotted(&config.memory.backend) == agent_backend_name
    {
        create_memory_from_config(config, api_key)?
    } else {
        let mut agent_memory = config.memory.clone();
        agent_memory.backend = agent_backend_name.to_string();
        let active_storage = match backend_kind {
            ConfigBackend::Lucid => config
                .storage
                .lucid
                .get("default")
                .map_or(ActiveStorage::None, ActiveStorage::Lucid),
            ConfigBackend::Sqlite => config
                .storage
                .sqlite
                .get("default")
                .map_or(ActiveStorage::None, ActiveStorage::Sqlite),
            _ => ActiveStorage::None,
        };
        create_memory_with_storage_and_routes(
            &agent_memory,
            &config.embedding_routes,
            active_storage,
            &config.data_dir,
            api_key,
            Some(&config.providers.models),
        )?
    };
    let inner_arc: Arc<dyn Memory> = Arc::from(inner);

    let bound_id = inner_arc.ensure_agent_uuid(agent_alias).await?;
    let mut allowlist_ids = Vec::with_capacity(agent_cfg.workspace.read_memory_from.len());
    for peer in &agent_cfg.workspace.read_memory_from {
        let uuid = inner_arc.ensure_agent_uuid(peer.as_str()).await?;
        allowlist_ids.push(uuid);
    }

    let scoped = AgentScopedMemory::new(inner_arc, bound_id, allowlist_ids);
    Ok(wrap_in_retrieval_pipeline(Arc::new(scoped), &config.memory))
}

/// Factory: create an optional response cache from config.
pub fn create_response_cache(config: &MemoryConfig, workspace_dir: &Path) -> Option<ResponseCache> {
    if !config.response_cache_enabled {
        return None;
    }

    match ResponseCache::new(
        workspace_dir,
        config.response_cache_ttl_minutes,
        config.response_cache_max_entries,
    ) {
        Ok(cache) => {
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                &format!(
                    "💾 Response cache enabled (TTL: {}min, max: {} entries)",
                    config.response_cache_ttl_minutes, config.response_cache_max_entries
                )
            );
            Some(cache)
        }
        Err(e) => {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                "Response cache disabled due to error"
            );
            None
        }
    }
}

#[cfg(test)]
mod tests;
