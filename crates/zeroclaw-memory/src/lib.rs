#![allow(clippy::to_string_in_format_args)]
//! Memory subsystem: backends, embeddings, consolidation, retrieval.
//!
//! ## Reserved Key Prefixes
//!
//! The following key prefixes are reserved for the auto-save system. Any memory
//! stored under these keys will be **excluded from context assembly** by all
//! three context-building paths (`build_context`, `DefaultMemoryLoader`, and
//! `should_skip_memory_context_entry`). Do not use these prefixes for semantic
//! memories that should surface in agent context.
//!
//! | Prefix | Purpose | Detection function |
//! |---|---|---|
//! | `assistant_resp` / `assistant_resp_*` | Model-authored assistant summaries (untrusted context) | [`is_assistant_autosave_key`] |
//! | `user_msg` / `user_msg_*` | Raw per-turn user messages (consolidation queue) | [`is_user_autosave_key`] |
//!
//! Channel-scoped variants (e.g. `telegram_user_msg_*`, `discord_*`) are
//! **not** filtered — they use different prefixes and are handled separately.

/// Opening delimiter for recalled memory injected into provider context.
pub const MEMORY_CONTEXT_OPEN: &str = "[Memory context]";
/// Closing delimiter for recalled memory injected into provider context.
pub const MEMORY_CONTEXT_CLOSE: &str = "[/Memory context]";

pub mod agent_scoped;
pub mod agent_scoped_markdown;
pub mod audit;
pub mod backend;
pub mod chunker;
pub mod conflict;
pub mod consolidation;
pub mod decay;
pub mod embeddings;
pub mod hygiene;
pub mod importance;
pub mod knowledge_graph;
#[cfg(feature = "memory-postgres")]
pub mod knowledge_graph_pg;
pub mod lucid;
pub mod markdown;
pub mod none;
pub mod policy;
#[cfg(feature = "memory-postgres")]
pub mod postgres;
pub mod qdrant;
pub mod response_cache;
pub mod retrieval;
pub mod snapshot;
pub mod sqlite;
pub mod traits;
pub mod vector;

pub use agent_scoped::AgentScopedMemory;
pub use agent_scoped_markdown::{AgentScopedMarkdownMemory, MarkdownPeer};
#[allow(unused_imports)]
pub use audit::AuditedMemory;
#[allow(unused_imports)]
pub use backend::{
    MemoryBackendKind, MemoryBackendProfile, classify_memory_backend, default_memory_backend_key,
    memory_backend_profile, selectable_memory_backends,
};
pub use lucid::LucidMemory;
pub use markdown::MarkdownMemory;
pub use none::NoneMemory;
#[allow(unused_imports)]
pub use policy::PolicyEnforcer;
#[cfg(feature = "memory-postgres")]
#[allow(unused_imports)]
pub use postgres::PostgresMemory;
pub use qdrant::QdrantMemory;
pub use response_cache::ResponseCache;
#[allow(unused_imports)]
pub use retrieval::{RetrievalConfig, RetrievalPipeline};
pub use sqlite::SqliteMemory;
pub use traits::Memory;
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
    ActiveStorage, EmbeddingRouteConfig, MemoryConfig, PostgresStorageConfig,
};

#[cfg(feature = "memory-postgres")]
fn build_postgres_memory(storage: &PostgresStorageConfig) -> anyhow::Result<Box<dyn Memory>> {
    use postgres::PostgresMemory;
    let db_url = storage
        .db_url
        .as_deref()
        .context("memory backend 'postgres' requires [storage.postgres.<alias>].db_url")?;
    let memory = PostgresMemory::new(
        "postgres",
        db_url,
        &storage.schema,
        &storage.table,
        storage.connect_timeout_secs,
        Some(storage.vector_enabled),
        Some(storage.vector_dimensions),
    )?;
    Ok(Box::new(memory))
}

#[cfg(not(feature = "memory-postgres"))]
fn build_postgres_memory(_storage: &PostgresStorageConfig) -> anyhow::Result<Box<dyn Memory>> {
    anyhow::bail!(
        "memory backend 'postgres' requested but this build was compiled without \
         `memory-postgres`; rebuild with `--features memory-postgres`"
    )
}

fn create_memory_with_builders<F>(
    backend_name: &str,
    workspace_dir: &Path,
    mut sqlite_builder: F,
    unknown_context: &str,
) -> anyhow::Result<Box<dyn Memory>>
where
    F: FnMut() -> anyhow::Result<SqliteMemory>,
{
    match classify_memory_backend(backend_name) {
        MemoryBackendKind::Sqlite => Ok(Box::new(sqlite_builder()?)),
        MemoryBackendKind::Lucid => {
            let local = sqlite_builder()?;
            Ok(Box::new(LucidMemory::new("lucid", workspace_dir, local)))
        }
        MemoryBackendKind::Postgres => {
            // Postgres requires a typed `[storage.postgres.<alias>]` config, which this
            // builder-only entry point does not receive. All supported call paths go
            // through `create_memory_with_storage_and_routes`, which handles postgres via
            // an early return. Fail loudly if a caller ever reaches this arm, rather than
            // pretending to work with default configs that can never connect.
            anyhow::bail!(
                "postgres backend requires storage config; \
                 call create_memory_with_storage_and_routes instead of create_memory_with_builders"
            )
        }
        MemoryBackendKind::Qdrant | MemoryBackendKind::Markdown => {
            Ok(Box::new(MarkdownMemory::new("markdown", workspace_dir)))
        }
        MemoryBackendKind::None => Ok(Box::new(NoneMemory::new("none"))),
        MemoryBackendKind::Unknown => {
            ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"backend_name": backend_name, "unknown_context": unknown_context})), "Unknown memory backend '', falling back to markdown");
            Ok(Box::new(MarkdownMemory::new("markdown", workspace_dir)))
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

/// Auto-save key used for raw user messages captured per-turn.
/// Re-injecting these into build_context causes exponential bloat: each recalled
/// entry contains prior generations' context verbatim, growing unboundedly.
/// Consolidated knowledge is already promoted to Core/Daily entries.
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

fn resolve_embedding_config(
    config: &MemoryConfig,
    embedding_routes: &[EmbeddingRouteConfig],
    api_key: Option<&str>,
    providers: Option<&ModelProviders>,
) -> ResolvedEmbeddingConfig {
    // Key resolution precedence (highest first):
    //   1. per-route `api_key` override (routed branch) / `[memory].embedding_api_key` (base branch)
    //   2. the referenced provider profile's own key, when `model_provider`
    //      is a dotted `<type>.<alias>` catalog ref (resolved in `resolve_provider_ref`)
    //   3. the seed model provider's key, inherited via `api_key`
    // (1)/(2) let embeddings keep their own credential when the chat model runs
    // on a provider that carries no usable embedding key; (3) preserves the
    // prior behavior verbatim when neither override nor a catalog ref applies.
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

/// Finalize an embedding profile, resolving a dotted `<type>.<alias>`
/// `model_provider` reference against the canonical `providers.models`
/// catalog.
///
/// The embeddings factory ([`embeddings::create_embedding_provider`]) only
/// understands the literal providers `openai`, `openrouter`, and
/// `custom:<base_url>`. An `[[embedding_routes]]` entry, however, points at a
/// configured provider profile by dotted reference (e.g. `openai.default`),
/// which passes config validation but matches none of those arms — so without
/// this step it would silently fall through to [`embeddings::NoopEmbedding`]
/// and degrade retrieval to keyword-only (issue #7949).
///
/// Resolution maps the dotted ref to the referenced profile's concrete
/// endpoint and key:
///   - an explicit `uri` override becomes `custom:<uri>`;
///   - with no `uri`, an `openai` / `openrouter` family passes through so the
///     factory applies its built-in family default endpoint.
///
/// Non-dotted providers (`openai`, `openrouter`, `custom:<url>`, `none`, or any
/// empty/literal value) are returned unchanged. A dotted ref is logged loudly
/// and left unresolved — never silently degraded — when it cannot be resolved
/// (no catalog, or missing from `providers.models`) OR when it resolves to a
/// family with no usable embeddings endpoint (a non-`openai`/`openrouter`
/// family configured without a `uri`).
///
/// Key precedence: `explicit_api_key` (per-route / `[memory]` override) wins,
/// then the referenced profile's own key, then `inherited_api_key` (the chat
/// seed key). No provider state is cached: the key and endpoint are read from
/// the live catalog on each call.
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
    // Map the resolved profile to a form the embeddings factory understands.
    // An explicit `uri` becomes `custom:<uri>` (works for any OpenAI-compatible
    // endpoint). With no `uri`, only `openai` / `openrouter` have a built-in
    // default endpoint in the factory; any other resolved family has no
    // embeddings endpoint to hit, so we must NOT pass its bare name through —
    // that would silently fall back to `NoopEmbedding`. Report it loudly
    // instead, leaving the reference unresolved (keyword-only), so a configured
    // route never degrades in silence (issue #7949).
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

/// Factory: create the right memory backend from config
///
/// Passes no provider catalog, so a dotted `<type>.<alias>` embedding provider
/// reference cannot be resolved through this entrypoint — callers that wire
/// `[[embedding_routes]]` should use
/// [`create_memory_with_storage_and_routes`] with the live
/// `providers.models` catalog instead.
pub fn create_memory(
    config: &MemoryConfig,
    workspace_dir: &Path,
    api_key: Option<&str>,
) -> anyhow::Result<Box<dyn Memory>> {
    create_memory_with_storage_and_routes(
        config,
        &[],
        ActiveStorage::None,
        workspace_dir,
        api_key,
        None,
    )
}

/// Factory: create memory with a resolved active storage backend and embedding routes.
///
/// Pass [`ActiveStorage::None`] when no typed storage config is needed (sqlite,
/// markdown, lucid, none — all infer settings from the workspace). Postgres and
/// Qdrant require their typed variants and will error if the wrong variant is
/// supplied.
///
/// `providers` is the canonical `providers.models` catalog, used to resolve a
/// dotted `<type>.<alias>` embedding `model_provider` reference (from
/// `[[embedding_routes]]` or `[memory].embedding_provider`) to a concrete
/// endpoint + key. Pass `None` only when no catalog is available (e.g. the
/// bare [`create_memory`] entrypoint); dotted refs then stay unresolved and
/// are logged rather than silently disabled.
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
        Ok(mem)
    }

    // Per-backend SQLite open-timeout override comes from the active storage
    // alias (V3); when no typed entry resolves, sqlite waits indefinitely.
    let sqlite_open_timeout_secs = match active_storage {
        ActiveStorage::Sqlite(sq) => sq.open_timeout_secs,
        _ => None,
    };

    if matches!(backend_kind, MemoryBackendKind::Qdrant) {
        let qdrant_cfg = match active_storage {
            ActiveStorage::Qdrant(q) => q,
            _ => anyhow::bail!(
                "memory backend 'qdrant' requires a `[storage.qdrant.<alias>]` entry \
                 referenced by `memory.backend = \"qdrant.<alias>\"`"
            ),
        };
        let url = qdrant_cfg
            .url
            .clone()
            .filter(|s| !s.trim().is_empty())
            .context("Qdrant memory backend requires `url` in [storage.qdrant.<alias>]")?;
        let collection = qdrant_cfg.collection.clone();
        let qdrant_api_key = qdrant_cfg.api_key.clone().filter(|s| !s.trim().is_empty());
        let embedder: Arc<dyn embeddings::EmbeddingProvider> =
            Arc::from(embeddings::create_embedding_provider(
                &resolved_embedding.model_provider,
                resolved_embedding.api_key.as_deref(),
                &resolved_embedding.model,
                resolved_embedding.dimensions,
            ));
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
            &format!(
                "📦 Qdrant memory backend configured (url: {}, collection: {})",
                url, collection
            )
        );
        return Ok(Box::new(QdrantMemory::new_lazy(
            "qdrant",
            &url,
            &collection,
            qdrant_api_key,
            embedder,
        )));
    }

    if matches!(backend_kind, MemoryBackendKind::Postgres) {
        let pg_cfg = match active_storage {
            ActiveStorage::Postgres(p) => p,
            _ => anyhow::bail!(
                "memory backend 'postgres' requires a `[storage.postgres.<alias>]` entry \
                 referenced by `memory.backend = \"postgres.<alias>\"`"
            ),
        };
        return build_postgres_memory(pg_cfg);
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
    )
}

pub fn create_memory_for_migration(
    backend: &str,
    workspace_dir: &Path,
) -> anyhow::Result<Box<dyn Memory>> {
    if matches!(classify_memory_backend(backend), MemoryBackendKind::None) {
        anyhow::bail!(
            "memory backend 'none' disables persistence; choose sqlite, lucid, or markdown before migration"
        );
    }

    create_memory_with_builders(
        backend,
        workspace_dir,
        || SqliteMemory::new("sqlite", workspace_dir),
        " during migration",
    )
}

/// Build the per-agent memory wrapper for `agent_alias`.
///
/// Wraps the appropriate inner backend with `AgentScopedMemory` (for
/// SQL- and Qdrant-backed agents — single shared backend, agent_id
/// column distinguishes rows) or `AgentScopedMarkdownMemory` (for
/// Markdown-backed agents — per-agent dirs, peer set composed from
/// the resolved `read_memory_from` allowlist). `NoneMemory` agents
/// pass through unwrapped.
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

    // Markdown branch: the wrapper composes per-agent dirs, not a
    // shared backend. Skip the inner-backend factory entirely.
    if matches!(backend_kind, ConfigBackend::Markdown) {
        let own_workspace = config.agent_workspace_dir(agent_alias);
        let own = MarkdownMemory::new("markdown", &own_workspace);
        let mut peers: Vec<agent_scoped_markdown::MarkdownPeer> = Vec::new();
        for peer in &agent_cfg.workspace.read_memory_from {
            let peer_alias = peer.as_str();
            let peer_workspace = config.agent_workspace_dir(peer_alias);
            peers.push(agent_scoped_markdown::MarkdownPeer {
                alias: peer_alias.to_string(),
                memory: MarkdownMemory::new("markdown", &peer_workspace),
            });
        }
        let scoped = AgentScopedMarkdownMemory::new(agent_alias, own, peers);
        return Ok(Arc::new(scoped));
    }

    // None branch: nothing to scope, no agents-table lookup needed.
    if matches!(backend_kind, ConfigBackend::None) {
        return Ok(Arc::new(NoneMemory::new("none")));
    }

    // SQL / Qdrant / Lucid: single install-wide backend; the
    // agent_id column (or payload field) carries the per-agent
    // attribution. We synthesize the inner backend from the existing
    // install-wide factory using the install workspace_dir, then wrap
    // with AgentScopedMemory holding the agent's UUID + resolved
    // allowlist UUIDs.
    let inner = create_memory_with_storage_and_routes(
        &config.memory,
        &config.embedding_routes,
        config.resolve_active_storage(),
        &config.data_dir,
        api_key,
        Some(&config.providers.models),
    )?;
    let inner_arc: Arc<dyn Memory> = Arc::from(inner);

    // Resolve the bound agent's identifier + the allowlist
    // identifiers via the trait method `ensure_agent_uuid`. SQL
    // backends override to look up agents-table UUIDs; Markdown,
    // Qdrant, None use the trait default that returns the alias
    // verbatim (alias-keyed; no UUID indirection at the storage
    // layer). The factory is therefore backend-agnostic past the
    // Markdown branch above.
    let bound_id = inner_arc.ensure_agent_uuid(agent_alias).await?;
    let mut allowlist_ids = Vec::with_capacity(agent_cfg.workspace.read_memory_from.len());
    for peer in &agent_cfg.workspace.read_memory_from {
        let uuid = inner_arc.ensure_agent_uuid(peer.as_str()).await?;
        allowlist_ids.push(uuid);
    }

    let scoped = AgentScopedMemory::new(inner_arc, bound_id, allowlist_ids);
    Ok(Arc::new(scoped))
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
mod tests {
    use super::*;
    use tempfile::TempDir;
    use zeroclaw_config::schema::EmbeddingRouteConfig;

    #[test]
    fn factory_sqlite() {
        let tmp = TempDir::new().unwrap();
        let cfg = MemoryConfig {
            backend: "sqlite".into(),
            ..MemoryConfig::default()
        };
        let mem = create_memory(&cfg, tmp.path(), None).unwrap();
        assert_eq!(mem.name(), "sqlite");
    }

    #[test]
    fn assistant_autosave_key_detection_matches_legacy_patterns() {
        assert!(is_assistant_autosave_key("assistant_resp"));
        assert!(is_assistant_autosave_key("assistant_resp_1234"));
        assert!(is_assistant_autosave_key("ASSISTANT_RESP_abcd"));
        assert!(!is_assistant_autosave_key("assistant_response"));
        assert!(!is_assistant_autosave_key("user_msg_1234"));
    }

    #[test]
    fn user_autosave_key_detection_matches_per_turn_patterns() {
        assert!(is_user_autosave_key("user_msg"));
        assert!(is_user_autosave_key("user_msg_1234"));
        assert!(is_user_autosave_key("USER_MSG_abcd"));
        assert!(!is_user_autosave_key("user_message"));
        assert!(!is_user_autosave_key("assistant_resp_1234"));
    }

    #[test]
    fn autosave_content_filter_drops_cron_and_distilled_noise() {
        assert!(should_skip_autosave_content("[cron:auto] patrol check"));
        assert!(should_skip_autosave_content(
            "[DISTILLED_MEMORY_CHUNK 1/2] DISTILLED_INDEX_SIG:abc123"
        ));
        assert!(should_skip_autosave_content(
            "[Heartbeat Task | decision] Should I run tasks?"
        ));
        assert!(should_skip_autosave_content(
            "[Heartbeat Task | high] Execute scheduled patrol"
        ));
        assert!(should_skip_autosave_content(&format!(
            "{MEMORY_CONTEXT_OPEN}\n- user_msg_abc: some recalled memory\n{MEMORY_CONTEXT_CLOSE}\n\n[cron:uuid job] prompt"
        )));
        assert!(!should_skip_autosave_content(
            "User prefers concise answers."
        ));
    }

    #[test]
    fn factory_markdown() {
        let tmp = TempDir::new().unwrap();
        let cfg = MemoryConfig {
            backend: "markdown".into(),
            ..MemoryConfig::default()
        };
        let mem = create_memory(&cfg, tmp.path(), None).unwrap();
        assert_eq!(mem.name(), "markdown");
    }

    #[test]
    fn factory_lucid() {
        let tmp = TempDir::new().unwrap();
        let cfg = MemoryConfig {
            backend: "lucid".into(),
            ..MemoryConfig::default()
        };
        let mem = create_memory(&cfg, tmp.path(), None).unwrap();
        assert_eq!(mem.name(), "lucid");
    }

    #[test]
    fn factory_none_uses_noop_memory() {
        let tmp = TempDir::new().unwrap();
        let cfg = MemoryConfig {
            backend: "none".into(),
            ..MemoryConfig::default()
        };
        let mem = create_memory(&cfg, tmp.path(), None).unwrap();
        assert_eq!(mem.name(), "none");
    }

    #[cfg(not(feature = "memory-postgres"))]
    #[test]
    fn factory_postgres_without_feature_gives_clear_error() {
        use zeroclaw_config::schema::PostgresStorageConfig;
        let tmp = TempDir::new().unwrap();
        let cfg = MemoryConfig {
            backend: "postgres.default".into(),
            ..MemoryConfig::default()
        };
        let storage = PostgresStorageConfig {
            db_url: Some("postgres://placeholder".into()),
            ..PostgresStorageConfig::default()
        };
        let error = create_memory_with_storage_and_routes(
            &cfg,
            &[],
            ActiveStorage::Postgres(&storage),
            tmp.path(),
            None,
            None,
        )
        .err()
        .expect("backend=postgres without memory-postgres feature should fail");
        assert!(
            error.to_string().contains("memory-postgres"),
            "error should mention the feature flag: {error}"
        );
    }

    #[test]
    fn factory_postgres_without_storage_alias_errors() {
        let tmp = TempDir::new().unwrap();
        let cfg = MemoryConfig {
            backend: "postgres.default".into(),
            ..MemoryConfig::default()
        };
        let error = create_memory(&cfg, tmp.path(), None)
            .err()
            .expect("backend=postgres requires a [storage.postgres.<alias>] entry");
        assert!(
            error.to_string().contains("storage.postgres"),
            "error should reference storage.postgres alias: {error}"
        );
    }

    #[test]
    fn factory_qdrant_without_storage_alias_errors() {
        let tmp = TempDir::new().unwrap();
        let cfg = MemoryConfig {
            backend: "qdrant.default".into(),
            ..MemoryConfig::default()
        };
        let error = create_memory(&cfg, tmp.path(), None)
            .err()
            .expect("backend=qdrant requires a [storage.qdrant.<alias>] entry");
        assert!(
            error.to_string().contains("storage.qdrant"),
            "error should reference storage.qdrant alias: {error}"
        );
    }

    #[test]
    fn backend_kind_extraction_strips_alias_suffix() {
        assert_eq!(backend_kind_from_dotted("sqlite"), "sqlite");
        assert_eq!(backend_kind_from_dotted("sqlite.default"), "sqlite");
        assert_eq!(backend_kind_from_dotted("postgres.work"), "postgres");
        assert_eq!(backend_kind_from_dotted("  Qdrant.Prod  "), "qdrant");
    }

    #[test]
    fn factory_unknown_falls_back_to_markdown() {
        let tmp = TempDir::new().unwrap();
        let cfg = MemoryConfig {
            backend: "redis".into(),
            ..MemoryConfig::default()
        };
        let mem = create_memory(&cfg, tmp.path(), None).unwrap();
        assert_eq!(mem.name(), "markdown");
    }

    #[test]
    fn migration_factory_lucid() {
        let tmp = TempDir::new().unwrap();
        let mem = create_memory_for_migration("lucid", tmp.path()).unwrap();
        assert_eq!(mem.name(), "lucid");
    }

    #[test]
    fn migration_factory_none_is_rejected() {
        let tmp = TempDir::new().unwrap();
        let error = create_memory_for_migration("none", tmp.path())
            .err()
            .expect("backend=none should be rejected for migration");
        assert!(error.to_string().contains("disables persistence"));
    }

    #[test]
    fn resolve_embedding_config_uses_base_config_when_model_is_not_hint() {
        let cfg = MemoryConfig {
            embedding_provider: "openai".into(),
            embedding_model: "text-embedding-3-small".into(),
            embedding_dimensions: 1536,
            ..MemoryConfig::default()
        };

        let resolved = resolve_embedding_config(&cfg, &[], Some("base-key"), None);
        assert_eq!(
            resolved,
            ResolvedEmbeddingConfig {
                model_provider: "openai".into(),
                model: "text-embedding-3-small".into(),
                dimensions: 1536,
                api_key: Some("base-key".into()),
            }
        );
    }

    #[test]
    fn resolve_embedding_config_uses_matching_route_with_api_key_override() {
        let cfg = MemoryConfig {
            embedding_provider: "none".into(),
            embedding_model: "hint:semantic".into(),
            embedding_dimensions: 1536,
            ..MemoryConfig::default()
        };
        let routes = vec![EmbeddingRouteConfig {
            hint: "semantic".into(),
            model_provider: "custom:https://api.example.com/v1".into(),
            model: "custom-embed-v2".into(),
            dimensions: Some(1024),
            api_key: Some("route-key".into()),
        }];

        let resolved = resolve_embedding_config(&cfg, &routes, Some("base-key"), None);
        assert_eq!(
            resolved,
            ResolvedEmbeddingConfig {
                model_provider: "custom:https://api.example.com/v1".into(),
                model: "custom-embed-v2".into(),
                dimensions: 1024,
                api_key: Some("route-key".into()),
            }
        );
    }

    #[test]
    fn resolve_embedding_config_falls_back_when_hint_is_missing() {
        let cfg = MemoryConfig {
            embedding_provider: "openai".into(),
            embedding_model: "hint:semantic".into(),
            embedding_dimensions: 1536,
            ..MemoryConfig::default()
        };

        let resolved = resolve_embedding_config(&cfg, &[], Some("base-key"), None);
        assert_eq!(
            resolved,
            ResolvedEmbeddingConfig {
                model_provider: "openai".into(),
                model: "hint:semantic".into(),
                dimensions: 1536,
                api_key: Some("base-key".into()),
            }
        );
    }

    #[test]
    fn resolve_embedding_config_falls_back_when_route_is_invalid() {
        let cfg = MemoryConfig {
            embedding_provider: "openai".into(),
            embedding_model: "hint:semantic".into(),
            embedding_dimensions: 1536,
            ..MemoryConfig::default()
        };
        let routes = vec![EmbeddingRouteConfig {
            hint: "semantic".into(),
            model_provider: String::new(),
            model: "text-embedding-3-small".into(),
            dimensions: Some(0),
            api_key: None,
        }];

        let resolved = resolve_embedding_config(&cfg, &routes, Some("base-key"), None);
        assert_eq!(
            resolved,
            ResolvedEmbeddingConfig {
                model_provider: "openai".into(),
                model: "hint:semantic".into(),
                dimensions: 1536,
                api_key: Some("base-key".into()),
            }
        );
    }

    #[test]
    fn resolve_embedding_config_uses_caller_api_key_when_no_route_override() {
        let cfg = MemoryConfig {
            embedding_provider: "cohere".into(),
            embedding_model: "embed-english-v3.0".into(),
            embedding_dimensions: 1024,
            ..MemoryConfig::default()
        };

        let resolved = resolve_embedding_config(&cfg, &[], Some("caller-supplied-key"), None);

        assert_eq!(resolved.api_key.as_deref(), Some("caller-supplied-key"));
    }

    #[test]
    fn resolve_embedding_config_memory_key_overrides_inherited() {
        let cfg = MemoryConfig {
            embedding_provider: "custom:https://generativelanguage.googleapis.com/v1beta/openai"
                .into(),
            embedding_model: "gemini-embedding-001".into(),
            embedding_dimensions: 3072,
            embedding_api_key: Some("memory-embed-key".into()),
            ..MemoryConfig::default()
        };

        // The seed/chat provider supplies a different (here: unusable) key; the
        // explicit `[memory].embedding_api_key` must win so embeddings stay
        // decoupled from the chat model provider.
        let resolved = resolve_embedding_config(&cfg, &[], Some("chat-provider-key"), None);

        assert_eq!(resolved.api_key.as_deref(), Some("memory-embed-key"));
    }

    #[test]
    fn resolve_embedding_config_memory_key_used_when_no_inherited_key() {
        let cfg = MemoryConfig {
            embedding_provider: "custom:https://api.example.com/v1".into(),
            embedding_model: "custom-embed".into(),
            embedding_dimensions: 1024,
            embedding_api_key: Some("memory-embed-key".into()),
            ..MemoryConfig::default()
        };

        // OAuth-only chat provider → no inherited key. The memory key fills the gap.
        let resolved = resolve_embedding_config(&cfg, &[], None, None);

        assert_eq!(resolved.api_key.as_deref(), Some("memory-embed-key"));
    }

    #[test]
    fn resolve_embedding_config_blank_memory_key_is_ignored() {
        let cfg = MemoryConfig {
            embedding_provider: "openai".into(),
            embedding_model: "text-embedding-3-small".into(),
            embedding_dimensions: 1536,
            embedding_api_key: Some("   ".into()),
            ..MemoryConfig::default()
        };

        // Whitespace-only override is treated as unset → inheritance preserved.
        let resolved = resolve_embedding_config(&cfg, &[], Some("chat-provider-key"), None);

        assert_eq!(resolved.api_key.as_deref(), Some("chat-provider-key"));
    }

    #[test]
    fn resolve_embedding_config_route_key_beats_memory_key() {
        let cfg = MemoryConfig {
            embedding_provider: "none".into(),
            embedding_model: "hint:semantic".into(),
            embedding_dimensions: 1536,
            embedding_api_key: Some("memory-embed-key".into()),
            ..MemoryConfig::default()
        };
        let routes = vec![EmbeddingRouteConfig {
            hint: "semantic".into(),
            model_provider: "custom:https://api.example.com/v1".into(),
            model: "custom-embed-v2".into(),
            dimensions: Some(1024),
            api_key: Some("route-key".into()),
        }];

        // Precedence: per-route override > [memory].embedding_api_key > inherited.
        let resolved = resolve_embedding_config(&cfg, &routes, Some("chat-provider-key"), None);

        assert_eq!(resolved.api_key.as_deref(), Some("route-key"));
    }

    #[test]
    fn resolve_embedding_config_memory_key_used_for_route_without_override() {
        let cfg = MemoryConfig {
            embedding_provider: "none".into(),
            embedding_model: "hint:semantic".into(),
            embedding_dimensions: 1536,
            embedding_api_key: Some("memory-embed-key".into()),
            ..MemoryConfig::default()
        };
        let routes = vec![EmbeddingRouteConfig {
            hint: "semantic".into(),
            model_provider: "custom:https://api.example.com/v1".into(),
            model: "custom-embed-v2".into(),
            dimensions: Some(1024),
            api_key: None,
        }];

        // Route carries no key of its own → falls through to the memory key
        // before the inherited chat-provider key.
        let resolved = resolve_embedding_config(&cfg, &routes, Some("chat-provider-key"), None);

        assert_eq!(resolved.api_key.as_deref(), Some("memory-embed-key"));
    }

    /// Build a one-entry provider catalog (`providers.models.<family>.<alias>`)
    /// with the given endpoint + key, mirroring a `[providers.models.…]` block.
    fn catalog_with(
        family: &str,
        alias: &str,
        uri: Option<&str>,
        api_key: Option<&str>,
    ) -> ModelProviders {
        let mut providers = ModelProviders::default();
        let entry = providers
            .ensure(family, alias)
            .expect("known provider family");
        entry.uri = uri.map(str::to_string);
        entry.api_key = api_key.map(str::to_string);
        providers
    }

    #[test]
    fn resolve_embedding_config_resolves_dotted_route_ref_to_provider_uri() {
        let cfg = MemoryConfig {
            embedding_provider: "none".into(),
            embedding_model: "hint:semantic".into(),
            embedding_dimensions: 1536,
            ..MemoryConfig::default()
        };
        let routes = vec![EmbeddingRouteConfig {
            hint: "semantic".into(),
            model_provider: "openai.default".into(),
            model: "text-embedding-3-small".into(),
            dimensions: Some(1024),
            api_key: None,
        }];
        let providers = catalog_with(
            "openai",
            "default",
            Some("https://api.example.com/v1"),
            Some("sk-provider"),
        );

        let resolved =
            resolve_embedding_config(&cfg, &routes, Some("chat-provider-key"), Some(&providers));

        // The dotted `<type>.<alias>` ref resolves to the referenced profile's
        // concrete endpoint + key — not a silent NoopEmbedding (issue #7949).
        // The provider's own key beats the inherited chat-provider key.
        assert_eq!(
            resolved,
            ResolvedEmbeddingConfig {
                model_provider: "custom:https://api.example.com/v1".into(),
                model: "text-embedding-3-small".into(),
                dimensions: 1024,
                api_key: Some("sk-provider".into()),
            }
        );

        // End-to-end: the resolved profile builds a real OpenAI-compatible
        // embedder, not the keyword-only Noop fallback.
        let embedder = embeddings::create_embedding_provider(
            &resolved.model_provider,
            resolved.api_key.as_deref(),
            &resolved.model,
            resolved.dimensions,
        );
        assert_eq!(embedder.name(), "openai");
    }

    #[test]
    fn resolve_embedding_config_dotted_ref_without_uri_uses_provider_kind() {
        let cfg = MemoryConfig {
            embedding_provider: "none".into(),
            embedding_model: "hint:semantic".into(),
            embedding_dimensions: 1536,
            ..MemoryConfig::default()
        };
        let routes = vec![EmbeddingRouteConfig {
            hint: "semantic".into(),
            model_provider: "openai.default".into(),
            model: "text-embedding-3-small".into(),
            dimensions: None,
            api_key: None,
        }];
        // No `uri` override → fall through to the factory's built-in family
        // default by passing the bare provider kind.
        let providers = catalog_with("openai", "default", None, Some("sk-provider"));

        let resolved = resolve_embedding_config(&cfg, &routes, None, Some(&providers));

        assert_eq!(resolved.model_provider, "openai");
        assert_eq!(resolved.api_key.as_deref(), Some("sk-provider"));
        assert_eq!(resolved.dimensions, 1536);

        let embedder = embeddings::create_embedding_provider(
            &resolved.model_provider,
            resolved.api_key.as_deref(),
            &resolved.model,
            resolved.dimensions,
        );
        assert_eq!(embedder.name(), "openai");
    }

    #[test]
    fn resolve_embedding_config_route_key_overrides_provider_key() {
        let cfg = MemoryConfig {
            embedding_provider: "none".into(),
            embedding_model: "hint:semantic".into(),
            embedding_dimensions: 1536,
            ..MemoryConfig::default()
        };
        let routes = vec![EmbeddingRouteConfig {
            hint: "semantic".into(),
            model_provider: "openai.default".into(),
            model: "text-embedding-3-small".into(),
            dimensions: Some(1024),
            api_key: Some("route-key".into()),
        }];
        let providers = catalog_with(
            "openai",
            "default",
            Some("https://api.example.com/v1"),
            Some("sk-provider"),
        );

        let resolved =
            resolve_embedding_config(&cfg, &routes, Some("chat-provider-key"), Some(&providers));

        // Precedence: explicit per-route override > referenced provider key > inherited.
        assert_eq!(resolved.api_key.as_deref(), Some("route-key"));
        assert_eq!(resolved.model_provider, "custom:https://api.example.com/v1");
    }

    #[test]
    fn resolve_embedding_config_unknown_dotted_ref_is_left_unresolved_not_silent() {
        let cfg = MemoryConfig {
            embedding_provider: "none".into(),
            embedding_model: "hint:semantic".into(),
            embedding_dimensions: 1536,
            ..MemoryConfig::default()
        };
        let routes = vec![EmbeddingRouteConfig {
            hint: "semantic".into(),
            model_provider: "openai.missing".into(),
            model: "text-embedding-3-small".into(),
            dimensions: Some(1024),
            api_key: None,
        }];
        // Catalog only has `openai.default`; the route names a missing alias.
        let providers = catalog_with(
            "openai",
            "default",
            Some("https://api.example.com/v1"),
            Some("sk-provider"),
        );

        let resolved =
            resolve_embedding_config(&cfg, &routes, Some("chat-provider-key"), Some(&providers));

        // An unresolvable ref is preserved verbatim (and logged loudly), never
        // silently rewritten to a working provider; the key precedence falls
        // back to the inherited chat key.
        assert_eq!(resolved.model_provider, "openai.missing");
        assert_eq!(resolved.api_key.as_deref(), Some("chat-provider-key"));
    }

    #[test]
    fn resolve_embedding_config_resolves_dotted_base_provider_ref() {
        let cfg = MemoryConfig {
            embedding_provider: "openai.default".into(),
            embedding_model: "text-embedding-3-small".into(),
            embedding_dimensions: 1536,
            ..MemoryConfig::default()
        };
        let providers = catalog_with(
            "openai",
            "default",
            Some("https://api.example.com/v1"),
            Some("sk-provider"),
        );

        // Even outside `[[embedding_routes]]`, a dotted `[memory].embedding_provider`
        // ref resolves against the catalog rather than degrading to Noop.
        let resolved = resolve_embedding_config(&cfg, &[], None, Some(&providers));

        assert_eq!(resolved.model_provider, "custom:https://api.example.com/v1");
        assert_eq!(resolved.api_key.as_deref(), Some("sk-provider"));
    }

    #[test]
    fn resolve_embedding_config_resolved_family_without_endpoint_is_not_silent() {
        let cfg = MemoryConfig {
            embedding_provider: "none".into(),
            embedding_model: "hint:semantic".into(),
            embedding_dimensions: 1536,
            ..MemoryConfig::default()
        };
        let routes = vec![EmbeddingRouteConfig {
            hint: "semantic".into(),
            model_provider: "custom.myembed".into(),
            model: "text-embedding-3-small".into(),
            dimensions: Some(1024),
            api_key: None,
        }];
        // The ref RESOLVES (the `custom.myembed` profile exists) but carries no
        // `uri`, and `custom` has no built-in embeddings endpoint — so there is
        // no concrete form for the factory.
        let providers = catalog_with("custom", "myembed", None, Some("sk-provider"));

        let resolved =
            resolve_embedding_config(&cfg, &routes, Some("chat-provider-key"), Some(&providers));

        // It must NOT be rewritten to a bare `custom` (which would silently
        // Noop); it is left unresolved and logged loudly. The end-to-end
        // embedder is the keyword-only Noop, surfaced rather than hidden.
        assert_eq!(resolved.model_provider, "custom.myembed");
        let embedder = embeddings::create_embedding_provider(
            &resolved.model_provider,
            resolved.api_key.as_deref(),
            &resolved.model,
            resolved.dimensions,
        );
        assert_eq!(embedder.name(), "none");
    }

    #[test]
    fn resolve_embedding_config_custom_family_with_uri_resolves() {
        let cfg = MemoryConfig {
            embedding_provider: "none".into(),
            embedding_model: "hint:semantic".into(),
            embedding_dimensions: 1536,
            ..MemoryConfig::default()
        };
        let routes = vec![EmbeddingRouteConfig {
            hint: "semantic".into(),
            model_provider: "custom.myembed".into(),
            model: "text-embedding-3-small".into(),
            dimensions: Some(1024),
            api_key: None,
        }];
        // A `custom` profile WITH an explicit `uri` is a fully usable
        // OpenAI-compatible endpoint.
        let providers = catalog_with(
            "custom",
            "myembed",
            Some("https://embed.local/v1"),
            Some("sk-local"),
        );

        let resolved = resolve_embedding_config(&cfg, &routes, None, Some(&providers));

        assert_eq!(resolved.model_provider, "custom:https://embed.local/v1");
        assert_eq!(resolved.api_key.as_deref(), Some("sk-local"));
        let embedder = embeddings::create_embedding_provider(
            &resolved.model_provider,
            resolved.api_key.as_deref(),
            &resolved.model,
            resolved.dimensions,
        );
        assert_eq!(embedder.name(), "openai");
    }

    /// The "not silent" contract is the WARN itself: a resolved-but-unusable
    /// route must emit an operator-visible, structured diagnostic. Asserting
    /// only the keyword-only fallback (as the sibling test does) would stay
    /// green if the WARN were deleted — so capture the broadcast event and
    /// assert its severity + stable `error_key`.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn resolve_embedding_config_no_endpoint_emits_loud_warning() {
        let _writer_guard = zeroclaw_log::__private_test_writer_lock();
        let _hook_guard = zeroclaw_log::__private_test_hook_lock();
        zeroclaw_log::try_install_capture_subscriber();
        let mut rx = zeroclaw_log::subscribe_or_install();
        while rx.try_recv().is_ok() {}

        let cfg = MemoryConfig {
            embedding_provider: "none".into(),
            embedding_model: "hint:semantic".into(),
            embedding_dimensions: 1536,
            ..MemoryConfig::default()
        };
        let routes = vec![EmbeddingRouteConfig {
            hint: "semantic".into(),
            model_provider: "custom.myembed".into(),
            model: "text-embedding-3-small".into(),
            dimensions: Some(1024),
            api_key: None,
        }];
        let providers = catalog_with("custom", "myembed", None, Some("sk-provider"));

        let _ =
            resolve_embedding_config(&cfg, &routes, Some("chat-provider-key"), Some(&providers));

        // Find our diagnostic among any concurrently-broadcast events.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let mut found = None;
        while std::time::Instant::now() < deadline {
            match tokio::time::timeout(std::time::Duration::from_millis(50), rx.recv()).await {
                Ok(Ok(value)) => {
                    if value["attributes"]["error_key"] == "memory.embedding_route_no_endpoint" {
                        found = Some(value);
                        break;
                    }
                }
                Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => {}
                Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) => break,
                Err(_elapsed) => {}
            }
        }

        let value = found.expect("expected a loud memory.embedding_route_no_endpoint WARN event");
        assert_eq!(value["severity_text"], "WARN");
        assert_eq!(value["attributes"]["provider_ref"], "custom.myembed");
        assert_eq!(value["attributes"]["provider_kind"], "custom");
    }
}
