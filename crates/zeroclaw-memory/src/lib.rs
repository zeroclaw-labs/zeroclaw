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
) -> ResolvedEmbeddingConfig {
    // Key resolution precedence (highest first):
    //   1. per-route `api_key` override (handled in the routed branch below)
    //   2. `[memory].embedding_api_key` — operator-set, decoupled from chat
    //   3. the seed model provider's key, inherited via `api_key`
    // (2) lets embeddings keep their own credential when the chat model runs on
    // a provider that carries no usable embedding key; (3) preserves the prior
    // behavior verbatim when neither override is set.
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
    let base_api_key = configured_api_key.or(inherited_api_key);
    let fallback = ResolvedEmbeddingConfig {
        model_provider: config.embedding_provider.trim().to_string(),
        model: config.embedding_model.trim().to_string(),
        dimensions: config.embedding_dimensions,
        api_key: base_api_key.clone(),
    };

    let Some(hint) = config
        .embedding_model
        .strip_prefix("hint:")
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return fallback;
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
        return fallback;
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
        return fallback;
    }

    let routed_api_key = route
        .api_key
        .as_deref()
        .map(str::trim)
        .filter(|value: &&str| !value.is_empty())
        .map(|value| value.to_string());

    ResolvedEmbeddingConfig {
        model_provider: model_provider.to_string(),
        model: model.to_string(),
        dimensions,
        api_key: routed_api_key.or(base_api_key),
    }
}

/// Factory: create the right memory backend from config
pub fn create_memory(
    config: &MemoryConfig,
    workspace_dir: &Path,
    api_key: Option<&str>,
) -> anyhow::Result<Box<dyn Memory>> {
    create_memory_with_storage_and_routes(config, &[], ActiveStorage::None, workspace_dir, api_key)
}

/// Factory: create memory with a resolved active storage backend and embedding routes.
///
/// Pass [`ActiveStorage::None`] when no typed storage config is needed (sqlite,
/// markdown, lucid, none — all infer settings from the workspace). Postgres and
/// Qdrant require their typed variants and will error if the wrong variant is
/// supplied.
pub fn create_memory_with_storage_and_routes(
    config: &MemoryConfig,
    embedding_routes: &[EmbeddingRouteConfig],
    active_storage: ActiveStorage<'_>,
    workspace_dir: &Path,
    api_key: Option<&str>,
) -> anyhow::Result<Box<dyn Memory>> {
    let backend_name = backend_kind_from_dotted(&config.backend);
    let backend_kind = classify_memory_backend(&backend_name);
    let resolved_embedding = resolve_embedding_config(config, embedding_routes, api_key);

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

        let resolved = resolve_embedding_config(&cfg, &[], Some("base-key"));
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

        let resolved = resolve_embedding_config(&cfg, &routes, Some("base-key"));
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

        let resolved = resolve_embedding_config(&cfg, &[], Some("base-key"));
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

        let resolved = resolve_embedding_config(&cfg, &routes, Some("base-key"));
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

        let resolved = resolve_embedding_config(&cfg, &[], Some("caller-supplied-key"));

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
        let resolved = resolve_embedding_config(&cfg, &[], Some("chat-provider-key"));

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
        let resolved = resolve_embedding_config(&cfg, &[], None);

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
        let resolved = resolve_embedding_config(&cfg, &[], Some("chat-provider-key"));

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
        let resolved = resolve_embedding_config(&cfg, &routes, Some("chat-provider-key"));

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
        let resolved = resolve_embedding_config(&cfg, &routes, Some("chat-provider-key"));

        assert_eq!(resolved.api_key.as_deref(), Some("memory-embed-key"));
    }
}
