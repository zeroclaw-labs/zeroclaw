use anyhow::Result;
use console::style;
use dialoguer::{Confirm, Select};

use crate::config::MemoryConfig;
use crate::onboard::common::print_bullet;

pub(crate) fn setup_memory() -> Result<MemoryConfig> {
    print_bullet("Choose how ZeroClaw stores and searches memories.");
    print_bullet("You can always change this later in config.toml.");
    println!();

    let options = vec![
        "SQLite with Vector Search (recommended) — fast, hybrid search, embeddings",
        "Markdown Files — simple, human-readable, no dependencies",
        "None — disable persistent memory",
    ];

    let choice = Select::new()
        .with_prompt("  Select memory backend")
        .items(&options)
        .default(0)
        .interact()?;

    let backend = match choice {
        1 => "markdown",
        2 => "none",
        _ => "sqlite",
    };

    let auto_save = if backend == "none" {
        false
    } else {
        Confirm::new()
            .with_prompt("  Auto-save conversations to memory?")
            .default(true)
            .interact()?
    };

    println!(
        "  {} Memory: {} (auto-save: {})",
        style("✓").green().bold(),
        style(backend).green(),
        if auto_save { "on" } else { "off" }
    );

    Ok(MemoryConfig {
        backend: backend.to_string(),
        auto_save,
        hygiene_enabled: backend == "sqlite",
        archive_after_days: if backend == "sqlite" { 7 } else { 0 },
        purge_after_days: if backend == "sqlite" { 30 } else { 0 },
        conversation_retention_days: 30,
        embedding_provider: "none".to_string(),
        embedding_model: "text-embedding-3-small".to_string(),
        embedding_dimensions: 1536,
        vector_weight: 0.7,
        keyword_weight: 0.3,
        embedding_cache_size: if backend == "sqlite" { 10000 } else { 0 },
        chunk_max_tokens: 512,
    })
}
