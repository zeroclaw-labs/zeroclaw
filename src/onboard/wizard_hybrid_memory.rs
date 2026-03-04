use crate::config::MemoryConfig;
use crate::memory::MemoryBackendKind;
use anyhow::{bail, Result};
use console::style;
use dialoguer::Input;

pub(super) fn configure_hybrid_qdrant_memory(
    config: &mut MemoryConfig,
    backend_kind: MemoryBackendKind,
) -> Result<()> {
    match backend_kind {
        MemoryBackendKind::SqliteQdrantHybrid => {
            super::print_bullet(
                "Hybrid memory keeps local SQLite metadata and uses Qdrant for semantic ranking.",
            );
            super::print_bullet("SQLite storage path stays at the default workspace database.");
        }
        MemoryBackendKind::PostgresQdrantHybrid => {
            super::print_bullet(
                "Hybrid memory keeps Postgres as source-of-truth and uses Qdrant for semantic ranking.",
            );
            super::print_bullet(
                "postgres_qdrant_hybrid requires [storage.provider.config].db_url in config.toml.",
            );
        }
        _ => {}
    }

    let qdrant_url_default = config
        .qdrant
        .url
        .clone()
        .unwrap_or_else(|| "http://localhost:6333".to_string());
    let qdrant_url: String = Input::with_theme(super::wizard_theme())
        .with_prompt("  Qdrant URL")
        .default(qdrant_url_default)
        .interact_text()?;
    let qdrant_url = qdrant_url.trim();
    if qdrant_url.is_empty() {
        bail!("Qdrant URL is required for hybrid memory backends");
    }
    config.qdrant.url = Some(qdrant_url.to_string());

    let qdrant_collection: String = Input::with_theme(super::wizard_theme())
        .with_prompt("  Qdrant collection")
        .default(config.qdrant.collection.clone())
        .interact_text()?;
    let qdrant_collection = qdrant_collection.trim();
    if !qdrant_collection.is_empty() {
        config.qdrant.collection = qdrant_collection.to_string();
    }

    let qdrant_api_key: String = Input::with_theme(super::wizard_theme())
        .with_prompt("  Qdrant API key (optional, Enter to skip)")
        .allow_empty(true)
        .interact_text()?;
    let qdrant_api_key = qdrant_api_key.trim();
    config.qdrant.api_key = if qdrant_api_key.is_empty() {
        None
    } else {
        Some(qdrant_api_key.to_string())
    };

    println!(
        "  {} Qdrant: {} (collection: {}, api key: {})",
        style("✓").green().bold(),
        style(config.qdrant.url.as_deref().unwrap_or_default()).green(),
        style(&config.qdrant.collection).green(),
        if config.qdrant.api_key.is_some() {
            style("set").green().to_string()
        } else {
            style("not set").dim().to_string()
        }
    );

    Ok(())
}
