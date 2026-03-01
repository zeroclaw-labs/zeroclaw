pub mod backend;
pub mod chunker;
pub mod cli;
pub mod cortex;
pub mod decay;
pub mod embeddings;
pub mod hybrid;
pub mod hygiene;
pub mod lucid;
pub mod markdown;
pub mod none;
#[cfg(feature = "memory-postgres")]
pub mod postgres;
pub mod qdrant;
pub mod response_cache;
pub mod snapshot;
pub mod sqlite;
pub mod traits;
pub mod vector;

#[allow(unused_imports)]
pub use backend::{
    classify_memory_backend, default_memory_backend_key, memory_backend_profile,
    selectable_memory_backends, MemoryBackendKind, MemoryBackendProfile,
};
pub use cortex::CortexMemMemory;
pub use hybrid::SqliteQdrantHybridMemory;
pub use lucid::LucidMemory;
pub use markdown::MarkdownMemory;
pub use none::NoneMemory;
#[cfg(feature = "memory-postgres")]
pub use postgres::PostgresMemory;
pub use qdrant::QdrantMemory;
pub use response_cache::ResponseCache;
pub use sqlite::SqliteMemory;
pub use traits::Memory;
#[allow(unused_imports)]
pub use traits::{MemoryCategory, MemoryEntry};

use crate::config::{EmbeddingRouteConfig, MemoryConfig, StorageProviderConfig};
use anyhow::Context;
use std::path::Path;
use std::sync::Arc;

fn create_memory_with_builders<F, G>(
    backend_name: &str,
    workspace_dir: &Path,
    mut sqlite_builder: F,
    mut postgres_builder: G,
    unknown_context: &str,
) -> anyhow::Result<Box<dyn Memory>>
where
    F: FnMut() -> anyhow::Result<SqliteMemory>,
    G: FnMut() -> anyhow::Result<Box<dyn Memory>>,
{
    match classify_memory_backend(backend_name) {
        MemoryBackendKind::Sqlite | MemoryBackendKind::SqliteQdrantHybrid => {
            Ok(Box::new(sqlite_builder()?))
        }
        MemoryBackendKind::Lucid => {
            let local = sqlite_builder()?;
            Ok(Box::new(LucidMemory::new(workspace_dir, local)))
        }
        MemoryBackendKind::CortexMem => {
            let local = sqlite_builder()?;
            Ok(Box::new(CortexMemMemory::new(workspace_dir, local)))
        }
        MemoryBackendKind::Postgres => postgres_builder(),
        MemoryBackendKind::Qdrant | MemoryBackendKind::Markdown => {
            Ok(Box::new(MarkdownMemory::new(workspace_dir)))
        }
        MemoryBackendKind::None => Ok(Box::new(NoneMemory::new())),
        MemoryBackendKind::Unknown => {
            tracing::warn!(
                "Unknown memory backend '{backend_name}'{unknown_context}, falling back to markdown"
            );
            Ok(Box::new(MarkdownMemory::new(workspace_dir)))
        }
    }
}

pub fn effective_memory_backend_name(
    memory_backend: &str,
    storage_provider: Option<&StorageProviderConfig>,
) -> String {
    if let Some(override_provider) = storage_provider
        .map(|cfg| cfg.provider.trim())
        .filter(|provider| !provider.is_empty())
    {
        return override_provider.to_ascii_lowercase();
    }

    memory_backend.trim().to_ascii_lowercase()
}

/// Legacy auto-save key used for model-authored assistant summaries.
/// These entries are treated as untrusted context and should not be re-injected.
pub fn is_assistant_autosave_key(key: &str) -> bool {
    let normalized = key.trim().to_ascii_lowercase();
    normalized == "assistant_resp" || normalized.starts_with("assistant_resp_")
}

#[derive(Clone, PartialEq, Eq)]
struct ResolvedEmbeddingConfig {
    provider: String,
    model: String,
    dimensions: usize,
    api_key: Option<String>,
}

impl std::fmt::Debug for ResolvedEmbeddingConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResolvedEmbeddingConfig")
            .field("provider", &self.provider)
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
    let fallback_api_key = api_key
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let fallback = ResolvedEmbeddingConfig {
        provider: config.embedding_provider.trim().to_string(),
        model: config.embedding_model.trim().to_string(),
        dimensions: config.embedding_dimensions,
        api_key: fallback_api_key.clone(),
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
        tracing::warn!(
            hint,
            "Unknown embedding route hint; falling back to [memory] embedding settings"
        );
        return fallback;
    };

    let provider = route.provider.trim();
    let model = route.model.trim();
    let dimensions = route.dimensions.unwrap_or(config.embedding_dimensions);
    if provider.is_empty() || model.is_empty() || dimensions == 0 {
        tracing::warn!(
            hint,
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
        provider: provider.to_string(),
        model: model.to_string(),
        dimensions,
        api_key: routed_api_key.or(fallback_api_key),
    }
}

/// Factory: create the right memory backend from config
pub fn create_memory(
    config: &MemoryConfig,
    workspace_dir: &Path,
    api_key: Option<&str>,
) -> anyhow::Result<Box<dyn Memory>> {
    create_memory_with_storage_and_routes(config, &[], None, workspace_dir, api_key)
}

/// Factory: create memory with optional storage-provider override.
pub fn create_memory_with_storage(
    config: &MemoryConfig,
    storage_provider: Option<&StorageProviderConfig>,
    workspace_dir: &Path,
    api_key: Option<&str>,
) -> anyhow::Result<Box<dyn Memory>> {
    create_memory_with_storage_and_routes(config, &[], storage_provider, workspace_dir, api_key)
}

/// Factory: create memory with optional storage-provider override and embedding routes.
pub fn create_memory_with_storage_and_routes(
    config: &MemoryConfig,
    embedding_routes: &[EmbeddingRouteConfig],
    storage_provider: Option<&StorageProviderConfig>,
    workspace_dir: &Path,
    api_key: Option<&str>,
) -> anyhow::Result<Box<dyn Memory>> {
    let backend_name = effective_memory_backend_name(&config.backend, storage_provider);
    let backend_kind = classify_memory_backend(&backend_name);
    let resolved_embedding = resolve_embedding_config(config, embedding_routes, api_key);

    // Best-effort memory hygiene/retention pass (throttled by state file).
    if let Err(e) = hygiene::run_if_due(config, workspace_dir) {
        tracing::warn!("memory hygiene skipped: {e}");
    }

    // If snapshot_on_hygiene is enabled, export core memories during hygiene.
    if config.snapshot_enabled
        && config.snapshot_on_hygiene
        && matches!(
            backend_kind,
            MemoryBackendKind::Sqlite
                | MemoryBackendKind::SqliteQdrantHybrid
                | MemoryBackendKind::Lucid
                | MemoryBackendKind::CortexMem
        )
    {
        if let Err(e) = snapshot::export_snapshot(workspace_dir) {
            tracing::warn!("memory snapshot skipped: {e}");
        }
    }

    // Auto-hydration: if brain.db is missing but MEMORY_SNAPSHOT.md exists,
    // restore the "soul" from the snapshot before creating the backend.
    if config.auto_hydrate
        && matches!(
            backend_kind,
            MemoryBackendKind::Sqlite
                | MemoryBackendKind::SqliteQdrantHybrid
                | MemoryBackendKind::Lucid
                | MemoryBackendKind::CortexMem
        )
        && snapshot::should_hydrate(workspace_dir)
    {
        tracing::info!("🧬 Cold boot detected — hydrating from MEMORY_SNAPSHOT.md");
        match snapshot::hydrate_from_snapshot(workspace_dir) {
            Ok(count) => {
                if count > 0 {
                    tracing::info!("🧬 Hydrated {count} core memories from snapshot");
                }
            }
            Err(e) => {
                tracing::warn!("memory hydration failed: {e}");
            }
        }
    }

    fn build_sqlite_memory(
        config: &MemoryConfig,
        workspace_dir: &Path,
        resolved_embedding: &ResolvedEmbeddingConfig,
    ) -> anyhow::Result<SqliteMemory> {
        let embedder: Arc<dyn embeddings::EmbeddingProvider> =
            Arc::from(embeddings::create_embedding_provider(
                &resolved_embedding.provider,
                resolved_embedding.api_key.as_deref(),
                &resolved_embedding.model,
                resolved_embedding.dimensions,
            ));

        #[allow(clippy::cast_possible_truncation)]
        let mem = SqliteMemory::with_options(
            workspace_dir,
            embedder,
            config.vector_weight as f32,
            config.keyword_weight as f32,
            config.embedding_cache_size,
            config.sqlite_open_timeout_secs,
            &config.sqlite_journal_mode,
        )?;
        Ok(mem)
    }

    #[cfg(feature = "memory-postgres")]
    fn build_postgres_memory(
        storage_provider: Option<&StorageProviderConfig>,
    ) -> anyhow::Result<Box<dyn Memory>> {
        let storage_provider = storage_provider
            .context("memory backend 'postgres' requires [storage.provider.config] settings")?;
        let db_url = storage_provider
            .db_url
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .context(
                "memory backend 'postgres' requires [storage.provider.config].db_url (or dbURL)",
            )?;

        let memory = PostgresMemory::new(
            db_url,
            &storage_provider.schema,
            &storage_provider.table,
            storage_provider.connect_timeout_secs,
            storage_provider.tls,
        )?;
        Ok(Box::new(memory))
    }

    #[cfg(not(feature = "memory-postgres"))]
    fn build_postgres_memory(
        _storage_provider: Option<&StorageProviderConfig>,
    ) -> anyhow::Result<Box<dyn Memory>> {
        anyhow::bail!(
            "memory backend 'postgres' requested but this build was compiled without `memory-postgres`; rebuild with `--features memory-postgres`"
        );
    }

    fn build_qdrant_memory(
        config: &MemoryConfig,
        resolved_embedding: &ResolvedEmbeddingConfig,
    ) -> anyhow::Result<QdrantMemory> {
        let url = config
            .qdrant
            .url
            .clone()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| std::env::var("QDRANT_URL").ok())
            .filter(|s| !s.trim().is_empty())
            .context(
                "Qdrant memory backend requires url in [memory.qdrant] or QDRANT_URL env var",
            )?;
        let collection = std::env::var("QDRANT_COLLECTION")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| config.qdrant.collection.clone());
        let qdrant_api_key = config
            .qdrant
            .api_key
            .clone()
            .or_else(|| std::env::var("QDRANT_API_KEY").ok())
            .filter(|s| !s.trim().is_empty());
        let embedder: Arc<dyn embeddings::EmbeddingProvider> =
            Arc::from(embeddings::create_embedding_provider(
                &resolved_embedding.provider,
                resolved_embedding.api_key.as_deref(),
                &resolved_embedding.model,
                resolved_embedding.dimensions,
            ));
        tracing::info!(
            "📦 Qdrant memory backend configured (url: {}, collection: {})",
            url,
            collection
        );
        Ok(QdrantMemory::new_lazy(
            &url,
            &collection,
            qdrant_api_key,
            embedder,
        ))
    }

    if matches!(backend_kind, MemoryBackendKind::Qdrant) {
        return Ok(Box::new(build_qdrant_memory(config, &resolved_embedding)?));
    }

    if matches!(backend_kind, MemoryBackendKind::SqliteQdrantHybrid) {
        let sqlite: Arc<dyn Memory> = Arc::new(build_sqlite_memory(
            config,
            workspace_dir,
            &resolved_embedding,
        )?);
        let qdrant: Arc<dyn Memory> = Arc::new(build_qdrant_memory(config, &resolved_embedding)?);
        return Ok(Box::new(SqliteQdrantHybridMemory::new(sqlite, qdrant)));
    }

    create_memory_with_builders(
        &backend_name,
        workspace_dir,
        || build_sqlite_memory(config, workspace_dir, &resolved_embedding),
        || build_postgres_memory(storage_provider),
        "",
    )
}

pub fn create_memory_for_migration(
    backend: &str,
    workspace_dir: &Path,
) -> anyhow::Result<Box<dyn Memory>> {
    if matches!(classify_memory_backend(backend), MemoryBackendKind::None) {
        anyhow::bail!(
            "memory backend 'none' disables persistence; choose sqlite, lucid, cortex-mem, or markdown before migration"
        );
    }

    if matches!(
        classify_memory_backend(backend),
        MemoryBackendKind::Postgres
    ) {
        anyhow::bail!(
            "memory migration for backend 'postgres' is unsupported; migrate with sqlite or markdown first"
        );
    }

    create_memory_with_builders(
        backend,
        workspace_dir,
        || SqliteMemory::new(workspace_dir),
        || anyhow::bail!("postgres backend is not available in migration context"),
        " during migration",
    )
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
            tracing::info!(
                "💾 Response cache enabled (TTL: {}min, max: {} entries)",
                config.response_cache_ttl_minutes,
                config.response_cache_max_entries
            );
            Some(cache)
        }
        Err(e) => {
            tracing::warn!("Response cache disabled due to error: {e}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{EmbeddingRouteConfig, StorageProviderConfig};
    use tempfile::TempDir;

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
    fn factory_cortex_mem() {
        let tmp = TempDir::new().unwrap();
        let cfg = MemoryConfig {
            backend: "cortex-mem".into(),
            ..MemoryConfig::default()
        };
        let mem = create_memory(&cfg, tmp.path(), None).unwrap();
        assert_eq!(mem.name(), "cortex-mem");
    }

    #[test]
    fn factory_sqlite_qdrant_hybrid() {
        let tmp = TempDir::new().unwrap();
        let cfg = MemoryConfig {
            backend: "sqlite_qdrant_hybrid".into(),
            qdrant: crate::config::QdrantConfig {
                url: Some("http://localhost:6333".into()),
                ..crate::config::QdrantConfig::default()
            },
            ..MemoryConfig::default()
        };
        let mem = create_memory(&cfg, tmp.path(), None).unwrap();
        assert_eq!(mem.name(), "sqlite_qdrant_hybrid");
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
    fn migration_factory_cortex_mem() {
        let tmp = TempDir::new().unwrap();
        let mem = create_memory_for_migration("cortex-mem", tmp.path()).unwrap();
        assert_eq!(mem.name(), "cortex-mem");
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
    fn effective_backend_name_prefers_storage_override() {
        let storage = StorageProviderConfig {
            provider: "postgres".into(),
            ..StorageProviderConfig::default()
        };

        assert_eq!(
            effective_memory_backend_name("sqlite", Some(&storage)),
            "postgres"
        );
    }

    #[test]
    fn factory_postgres_without_db_url_is_rejected() {
        let tmp = TempDir::new().unwrap();
        let cfg = MemoryConfig {
            backend: "postgres".into(),
            ..MemoryConfig::default()
        };

        let storage = StorageProviderConfig {
            provider: "postgres".into(),
            db_url: None,
            ..StorageProviderConfig::default()
        };

        let error = create_memory_with_storage(&cfg, Some(&storage), tmp.path(), None)
            .err()
            .expect("postgres without db_url should be rejected");
        if cfg!(feature = "memory-postgres") {
            assert!(error.to_string().contains("db_url"));
        } else {
            assert!(error.to_string().contains("memory-postgres"));
        }
    }

    #[test]
    fn factory_hybrid_requires_qdrant_url() {
        let tmp = TempDir::new().unwrap();
        let cfg = MemoryConfig {
            backend: "sqlite_qdrant_hybrid".into(),
            qdrant: crate::config::QdrantConfig {
                url: None,
                ..crate::config::QdrantConfig::default()
            },
            ..MemoryConfig::default()
        };

        let error = create_memory(&cfg, tmp.path(), None)
            .err()
            .expect("hybrid backend should require qdrant url");
        assert!(error
            .to_string()
            .contains("Qdrant memory backend requires url"));
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
                provider: "openai".into(),
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
            provider: "custom:https://api.example.com/v1".into(),
            model: "custom-embed-v2".into(),
            dimensions: Some(1024),
            api_key: Some("route-key".into()),
        }];

        let resolved = resolve_embedding_config(&cfg, &routes, Some("base-key"));
        assert_eq!(
            resolved,
            ResolvedEmbeddingConfig {
                provider: "custom:https://api.example.com/v1".into(),
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
                provider: "openai".into(),
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
            provider: String::new(),
            model: "text-embedding-3-small".into(),
            dimensions: Some(0),
            api_key: None,
        }];

        let resolved = resolve_embedding_config(&cfg, &routes, Some("base-key"));
        assert_eq!(
            resolved,
            ResolvedEmbeddingConfig {
                provider: "openai".into(),
                model: "hint:semantic".into(),
                dimensions: 1536,
                api_key: Some("base-key".into()),
            }
        );
    }
}
