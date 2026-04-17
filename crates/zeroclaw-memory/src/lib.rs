//! Memory subsystem: backends, embeddings, consolidation, retrieval.

pub mod audit;
pub mod backend;
pub mod chunker;
pub mod conflict;
pub mod consolidation;
pub mod decay;
pub mod embeddings;
pub mod hygiene;
pub mod importance;
pub mod injection_guard;
pub mod knowledge_extraction;
pub mod knowledge_graph;
pub mod lucid;
pub mod markdown;
pub mod namespaced;
pub mod none;
pub mod policy;
pub mod qdrant;
pub mod response_cache;
pub mod retrieval;
pub mod snapshot;
pub mod sqlite;
pub mod traits;
pub mod vector;

#[allow(unused_imports)]
pub use audit::AuditedMemory;
#[allow(unused_imports)]
pub use backend::{
    MemoryBackendKind, MemoryBackendProfile, classify_memory_backend, default_memory_backend_key,
    memory_backend_profile, selectable_memory_backends,
};
pub use lucid::LucidMemory;
pub use markdown::MarkdownMemory;
pub use namespaced::NamespacedMemory;
pub use none::NoneMemory;
#[allow(unused_imports)]
pub use policy::PolicyEnforcer;
pub use qdrant::QdrantMemory;
pub use response_cache::ResponseCache;
#[allow(unused_imports)]
pub use retrieval::{RetrievalConfig, RetrievalPipeline};
pub use sqlite::SqliteMemory;
pub use traits::Memory;
#[allow(unused_imports)]
pub use traits::{ExportFilter, MemoryCategory, MemoryEntry, ProceduralMessage};

use anyhow::Context;
use std::path::Path;
use std::sync::Arc;
use zeroclaw_config::schema::{EmbeddingRouteConfig, MemoryConfig, StorageProviderConfig};

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
            Ok(Box::new(LucidMemory::new(workspace_dir, local)))
        }
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

fn create_audited_memory_with_builders<F>(
    backend_name: &str,
    workspace_dir: &Path,
    mut sqlite_builder: F,
    unknown_context: &str,
) -> anyhow::Result<Box<dyn Memory>>
where
    F: FnMut() -> anyhow::Result<SqliteMemory>,
{
    match classify_memory_backend(backend_name) {
        MemoryBackendKind::Sqlite => {
            let inner = sqlite_builder()?;
            Ok(Box::new(AuditedMemory::new(inner, workspace_dir)?))
        }
        MemoryBackendKind::Lucid => {
            let local = sqlite_builder()?;
            let inner = LucidMemory::new(workspace_dir, local);
            Ok(Box::new(AuditedMemory::new(inner, workspace_dir)?))
        }
        MemoryBackendKind::Qdrant | MemoryBackendKind::Markdown => {
            let inner = MarkdownMemory::new(workspace_dir);
            Ok(Box::new(AuditedMemory::new(inner, workspace_dir)?))
        }
        MemoryBackendKind::None => Ok(Box::new(NoneMemory::new())),
        MemoryBackendKind::Unknown => {
            tracing::warn!(
                "Unknown memory backend '{backend_name}'{unknown_context}, falling back to markdown"
            );
            let inner = MarkdownMemory::new(workspace_dir);
            Ok(Box::new(AuditedMemory::new(inner, workspace_dir)?))
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
        || lowered.starts_with("[memory context]")
        || lowered.contains("distilled_index_sig:")
}

#[derive(Clone, PartialEq, Eq)]
pub struct ResolvedEmbeddingConfig {
    pub provider: String,
    pub model: String,
    pub dimensions: usize,
    pub api_key: Option<String>,
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

/// Look up the provider-specific environment variable for common embedding providers,
/// so that `OPENAI_API_KEY` (etc.) takes precedence over the default-provider key
/// that the caller passes in. Returns `None` for unknown providers.
fn embedding_provider_env_key(provider: &str) -> Option<String> {
    let env_var = match provider.trim() {
        "openai" => "OPENAI_API_KEY",
        "openrouter" => "OPENROUTER_API_KEY",
        "cohere" => "COHERE_API_KEY",
        _ => return None,
    };
    std::env::var(env_var)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

pub fn resolve_embedding_config(
    config: &MemoryConfig,
    embedding_routes: &[EmbeddingRouteConfig],
    api_key: Option<&str>,
) -> ResolvedEmbeddingConfig {
    let caller_api_key = api_key
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    // Prefer a provider-specific env var over the caller-supplied key, which
    // may come from the default (chat) provider and differ from the embedding
    // provider (issue #3083: gemini key leaking to openai embeddings endpoint).
    let fallback_api_key =
        embedding_provider_env_key(config.embedding_provider.trim()).or(caller_api_key);
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
            MemoryBackendKind::Sqlite | MemoryBackendKind::Lucid
        )
        && let Err(e) = snapshot::export_snapshot(workspace_dir)
    {
        tracing::warn!("memory snapshot skipped: {e}");
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
        let mem = SqliteMemory::with_embedder(
            workspace_dir,
            embedder,
            config.vector_weight as f32,
            config.keyword_weight as f32,
            config.embedding_cache_size,
            config.sqlite_open_timeout_secs,
            config.search_mode.clone(),
        )?;
        Ok(mem)
    }

    if matches!(backend_kind, MemoryBackendKind::Qdrant) {
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
        return Ok(Box::new(QdrantMemory::new_lazy(
            &url,
            &collection,
            qdrant_api_key,
            embedder,
        )));
    }

    if config.audit_enabled {
        tracing::info!("🔒 Wrapping memory backend with AuditedMemory");
        return create_audited_memory_with_builders(
            &backend_name,
            workspace_dir,
            || build_sqlite_memory(config, workspace_dir, &resolved_embedding),
            "",
        );
    }

    create_memory_with_builders(
        &backend_name,
        workspace_dir,
        || build_sqlite_memory(config, workspace_dir, &resolved_embedding),
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
        || SqliteMemory::new(workspace_dir),
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
    use tempfile::TempDir;
    use zeroclaw_config::schema::{EmbeddingRouteConfig, StorageProviderConfig};

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
        assert!(should_skip_autosave_content(
            "[Memory context]\n- user_msg_abc: some recalled memory\n[/Memory context]\n\n[cron:uuid job] prompt"
        ));
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
    fn effective_backend_name_prefers_storage_override() {
        let storage = StorageProviderConfig {
            provider: "qdrant".into(),
            ..StorageProviderConfig::default()
        };

        assert_eq!(
            effective_memory_backend_name("sqlite", Some(&storage)),
            "qdrant"
        );
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

    // Regression guard for issue #3083: when default_provider is "gemini"
    // (api_key = gemini key) but embedding_provider is "cohere", the
    // embedding provider's own env var (COHERE_API_KEY) must take precedence
    // over the caller-supplied key (which belongs to the default provider).
    //
    // Uses COHERE_API_KEY to avoid accidental collision with OPENAI_API_KEY
    // that may be set in the developer environment.
    #[test]
    fn resolve_embedding_config_uses_embedding_provider_env_key_not_default_provider_key() {
        // COHERE_API_KEY is almost certainly unset in normal dev environments.
        let prev = std::env::var("COHERE_API_KEY").ok();
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var("COHERE_API_KEY", "cohere-from-env") };

        let cfg = MemoryConfig {
            embedding_provider: "cohere".into(),
            embedding_model: "embed-english-v3.0".into(),
            embedding_dimensions: 1024,
            ..MemoryConfig::default()
        };

        // Simulate: caller passes the Gemini (default_provider) api key.
        let resolved = resolve_embedding_config(&cfg, &[], Some("gemini-key-must-not-be-used"));

        // Restore env.
        match prev {
            // SAFETY: test-only, single-threaded test runner.
            Some(v) => unsafe { std::env::set_var("COHERE_API_KEY", v) },
            // SAFETY: test-only, single-threaded test runner.
            None => unsafe { std::env::remove_var("COHERE_API_KEY") },
        }

        assert_eq!(
            resolved.api_key.as_deref(),
            Some("cohere-from-env"),
            "embedding api_key must come from COHERE_API_KEY env var, not from the default provider key"
        );
        assert_ne!(
            resolved.api_key.as_deref(),
            Some("gemini-key-must-not-be-used"),
            "default_provider key must not leak to the embedding provider"
        );
    }

    // ── C3: AuditedMemory factory integration tests ──────────────

    #[test]
    fn factory_sqlite_with_audit_enabled() {
        let tmp = TempDir::new().unwrap();
        let cfg = MemoryConfig {
            backend: "sqlite".into(),
            audit_enabled: true,
            ..MemoryConfig::default()
        };
        let mem = create_memory(&cfg, tmp.path(), None).unwrap();
        assert_eq!(mem.name(), "sqlite", "AuditedMemory should delegate name() to inner backend");
    }

    #[test]
    fn factory_markdown_with_audit_enabled() {
        let tmp = TempDir::new().unwrap();
        let cfg = MemoryConfig {
            backend: "markdown".into(),
            audit_enabled: true,
            ..MemoryConfig::default()
        };
        let mem = create_memory(&cfg, tmp.path(), None).unwrap();
        assert_eq!(mem.name(), "markdown");
    }

    #[test]
    fn factory_lucid_with_audit_enabled() {
        let tmp = TempDir::new().unwrap();
        let cfg = MemoryConfig {
            backend: "lucid".into(),
            audit_enabled: true,
            ..MemoryConfig::default()
        };
        let mem = create_memory(&cfg, tmp.path(), None).unwrap();
        assert_eq!(mem.name(), "lucid");
    }

    #[test]
    fn factory_none_skips_audit_wrapper() {
        let tmp = TempDir::new().unwrap();
        let cfg = MemoryConfig {
            backend: "none".into(),
            audit_enabled: true,
            ..MemoryConfig::default()
        };
        let mem = create_memory(&cfg, tmp.path(), None).unwrap();
        assert_eq!(mem.name(), "none");
    }

    #[test]
    fn factory_unknown_with_audit_enabled_falls_back_to_markdown() {
        let tmp = TempDir::new().unwrap();
        let cfg = MemoryConfig {
            backend: "redis".into(),
            audit_enabled: true,
            ..MemoryConfig::default()
        };
        let mem = create_memory(&cfg, tmp.path(), None).unwrap();
        assert_eq!(mem.name(), "markdown");
    }

    #[test]
    fn factory_audit_disabled_returns_plain_backend() {
        let tmp = TempDir::new().unwrap();
        let cfg = MemoryConfig {
            backend: "sqlite".into(),
            audit_enabled: false,
            ..MemoryConfig::default()
        };
        let mem = create_memory(&cfg, tmp.path(), None).unwrap();
        assert_eq!(mem.name(), "sqlite");
    }
}
