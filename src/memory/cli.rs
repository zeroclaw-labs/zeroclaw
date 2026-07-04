use super::traits::{Memory, MemoryCategory};
use super::{
    MemoryBackendKind, backend_kind_from_dotted, classify_memory_backend,
    create_memory_for_migration, create_memory_with_storage_and_routes,
};
use crate::config::Config;
use anyhow::{Result, bail};
use console::style;
#[cfg(feature = "agent-runtime")]
use zeroclaw_runtime::i18n;

/// Resolve a `cli-*` Fluent key for memory CLI output. Under `agent-runtime`
/// (default, and what CI/release build) this routes through Fluent; without it
/// the runtime i18n crate is absent, so the English `fallback` is used.
#[allow(unused_variables)]
fn mt(key: &str, fallback: &str) -> String {
    #[cfg(feature = "agent-runtime")]
    {
        i18n::get_required_cli_string(key)
    }
    #[cfg(not(feature = "agent-runtime"))]
    {
        fallback.to_string() // i18n-exempt: English fallback when Fluent (agent-runtime) is disabled
    }
}

/// `mt` with `{$name}` arguments.
#[allow(unused_variables)]
fn mt_args(key: &str, args: &[(&str, &str)], fallback: &str) -> String {
    #[cfg(feature = "agent-runtime")]
    {
        i18n::get_required_cli_string_with_args(key, args)
    }
    #[cfg(not(feature = "agent-runtime"))]
    {
        fallback.to_string() // i18n-exempt: English fallback when Fluent (agent-runtime) is disabled
    }
}

/// Handle `zeroclaw memory <subcommand>` CLI commands.
pub async fn handle_command(command: crate::MemoryCommands, config: &Config) -> Result<()> {
    match command {
        crate::MemoryCommands::List {
            category,
            session,
            limit,
            offset,
        } => handle_list(config, category, session, limit, offset).await,
        crate::MemoryCommands::Get { key } => handle_get(config, &key).await,
        crate::MemoryCommands::Stats => handle_stats(config).await,
        crate::MemoryCommands::Clear { key, category, yes } => {
            handle_clear(config, key, category, yes).await
        }
        crate::MemoryCommands::Reindex => handle_reindex(config).await,
    }
}

/// Create a memory backend with the configured embedder wired in.
///
/// Unlike `create_cli_memory`, which skips embedding setup for pure
/// read/delete operations, this factory is used by commands that must
/// actually compute embeddings (e.g. `reindex`). Mirrors the gateway's
/// memory construction so the same model provider / route resolution
/// applies. Removed `model_providers.fallback`; the embedder API key falls
/// back to the first configured model provider, matching how the gateway
/// resolves it (`crates/zeroclaw-gateway/src/lib.rs` `fallback`).
fn create_memory_with_embedder(config: &Config) -> Result<Box<dyn Memory>> {
    let backend = backend_kind_from_dotted(&config.memory.backend);
    if matches!(classify_memory_backend(&backend), MemoryBackendKind::None) {
        bail!("Memory backend is 'none' (disabled). No entries to manage.");
    }
    create_memory_with_storage_and_routes(
        &config.memory,
        &config.embedding_routes,
        config.resolve_active_storage(),
        &config.data_dir,
        None,
        Some(&config.providers.models),
    )
}

async fn handle_reindex(config: &Config) -> Result<()> {
    let mem = create_memory_with_embedder(config)?;
    println!(
        "{} {}",
        style("→").cyan(),
        mt("cli-memory-reindexing", "Reindexing memory backend...")
    );
    let count = mem.reindex().await?;
    if count == 0 {
        println!(
            "{} FTS rebuilt. No embeddings to fill in (either everything is already embedded or the backend has no embedder configured).",
            style("✓").green()
        );
    } else {
        println!(
            "{} FTS rebuilt. Re-embedded {count} {}.",
            style("✓").green(),
            if count == 1 { "entry" } else { "entries" }
        );
    }
    Ok(())
}

/// Create a lightweight memory backend for CLI management operations.
///
/// CLI commands (list/get/stats/clear) never use vector search, so we skip
/// embedding model_provider initialisation for local backends by using the
/// migration factory.
fn create_cli_memory(config: &Config) -> Result<Box<dyn Memory>> {
    let backend = backend_kind_from_dotted(&config.memory.backend);

    match classify_memory_backend(&backend) {
        MemoryBackendKind::None => {
            bail!("Memory backend is 'none' (disabled). No entries to manage.");
        }
        _ => create_memory_for_migration(&backend, &config.data_dir),
    }
}

async fn handle_list(
    config: &Config,
    category: Option<String>,
    session: Option<String>,
    limit: usize,
    offset: usize,
) -> Result<()> {
    let mem = create_cli_memory(config)?;
    let cat = category.as_deref().map(parse_category);
    let entries = mem.list(cat.as_ref(), session.as_deref()).await?;

    if entries.is_empty() {
        println!("{}", mt("cli-memory-none", "No memory entries found."));
        return Ok(());
    }

    let total = entries.len();
    let page: Vec<_> = entries.into_iter().skip(offset).take(limit).collect();

    if page.is_empty() {
        println!(
            "{}",
            mt_args(
                "cli-memory-none-at-offset",
                &[
                    ("offset", &offset.to_string()),
                    ("total", &total.to_string())
                ],
                "No entries at offset"
            )
        );
        return Ok(());
    }

    println!(
        "Memory entries ({total} total, showing {}-{}):\n",
        offset + 1,
        offset + page.len(),
    );

    for entry in &page {
        println!(
            "- {} [{}]",
            style(&entry.key).white().bold(),
            entry.category,
        );
        println!("    {}", truncate_content(&entry.content, 80));
    }

    if offset + page.len() < total {
        println!(
            "\n{}",
            mt_args(
                "cli-memory-next-page",
                &[("offset", &(offset + limit).to_string())],
                "Use --offset to see the next page"
            )
        );
    }

    Ok(())
}

async fn handle_get(config: &Config, key: &str) -> Result<()> {
    let mem = create_cli_memory(config)?;

    // Try exact match first.
    if let Some(entry) = mem.get(key).await? {
        print_entry(&entry);
        return Ok(());
    }

    // Fall back to prefix match so users can copy partial keys from `list`.
    let all = mem.list(None, None).await?;
    let matches: Vec<_> = all.iter().filter(|e| e.key.starts_with(key)).collect();

    match matches.len() {
        0 => println!(
            "{}",
            mt_args(
                "cli-memory-key-not-found",
                &[("key", key)],
                "No memory entry found for key"
            )
        ),
        1 => print_entry(matches[0]),
        n => {
            println!(
                "{}\n",
                mt_args(
                    "cli-memory-prefix-matched",
                    &[("key", key), ("n", &n.to_string())],
                    "Prefix matched entries"
                )
            );
            for entry in matches {
                println!(
                    "- {} [{}]",
                    style(&entry.key).white().bold(),
                    entry.category
                );
            }
            println!(
                "\n{}",
                mt(
                    "cli-memory-narrow-prefix",
                    "Specify a longer prefix to narrow the match."
                )
            );
        }
    }

    Ok(())
}

fn print_entry(entry: &super::traits::MemoryEntry) {
    println!(
        "{}",
        mt_args(
            "cli-memory-key",
            &[("value", &style(&entry.key).white().bold().to_string())],
            "Key"
        )
    );
    println!(
        "{}",
        mt_args(
            "cli-memory-category",
            &[("value", &entry.category.to_string())],
            "Category"
        )
    );
    println!(
        "{}",
        mt_args(
            "cli-memory-timestamp",
            &[("value", &entry.timestamp.to_string())],
            "Timestamp"
        )
    );
    if let Some(sid) = &entry.session_id {
        println!(
            "{}",
            mt_args("cli-memory-session", &[("value", sid)], "Session")
        );
    }
    println!("\n{}", entry.content);
}

async fn handle_stats(config: &Config) -> Result<()> {
    let mem = create_cli_memory(config)?;
    let healthy = mem.health_check().await;
    let total = mem.count().await.unwrap_or(0);

    println!("{}\n", mt("cli-memory-stats-header", "Memory Statistics:"));
    println!(
        "{}",
        mt_args(
            "cli-memory-backend",
            &[("value", &style(mem.name()).white().bold().to_string())],
            "Backend"
        )
    );
    println!(
        "  Health:   {}",
        if healthy {
            style("healthy").green().bold().to_string()
        } else {
            style("unhealthy").yellow().bold().to_string()
        }
    );
    println!(
        "{}",
        mt_args(
            "cli-memory-total",
            &[("value", &total.to_string())],
            "Total"
        )
    );

    let all = mem.list(None, None).await.unwrap_or_default();
    if !all.is_empty() {
        let mut counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        for entry in &all {
            *counts.entry(entry.category.to_string()).or_default() += 1;
        }

        println!("\n{}", mt("cli-memory-by-category", "  By category:"));
        let mut sorted: Vec<_> = counts.into_iter().collect();
        sorted.sort_by_key(|entry| std::cmp::Reverse(entry.1));
        for (cat, count) in sorted {
            println!("    {cat:<20} {count}");
        }
    }

    Ok(())
}

fn unsupported_clear_backend_message(backend: &str) -> String {
    #[cfg(feature = "agent-runtime")]
    {
        i18n::get_required_cli_string_with_args(
            "cli-memory-clear-unsupported-backend",
            &[("backend", backend)],
        )
    }

    #[cfg(not(feature = "agent-runtime"))]
    {
        format!(
            "memory clear is unsupported for append-only backend '{backend}'; switch to a deletable backend (sqlite, lucid, or postgres)"
        )
    }
}

async fn handle_clear(
    config: &Config,
    key: Option<String>,
    category: Option<String>,
    yes: bool,
) -> Result<()> {
    let backend = backend_kind_from_dotted(&config.memory.backend);
    if matches!(
        classify_memory_backend(&backend),
        MemoryBackendKind::Markdown | MemoryBackendKind::Qdrant
    ) {
        bail!(unsupported_clear_backend_message(&backend));
    }
    let mem = create_cli_memory(config)?;

    // Single-key deletion (exact or prefix match).
    if let Some(key) = key {
        return handle_clear_key(&*mem, &key, yes).await;
    }

    // Batch deletion by category (or all).
    let cat = category.as_deref().map(parse_category);
    let entries = mem.list(cat.as_ref(), None).await?;

    if entries.is_empty() {
        println!("{}", mt("cli-memory-none-to-clear", "No entries to clear."));
        return Ok(());
    }

    let scope = category.as_deref().unwrap_or("all categories");
    println!(
        "{}",
        mt_args(
            "cli-memory-found-in-scope",
            &[("count", &entries.len().to_string()), ("scope", scope)],
            "Found entries"
        )
    );

    if !yes {
        let confirmed = dialoguer::Confirm::new()
            .with_prompt(format!("  Delete {} entries?", entries.len()))
            .default(false)
            .interact()?;
        if !confirmed {
            println!("{}", mt("cli-memory-aborted", "Aborted."));
            return Ok(());
        }
    }

    let mut deleted = 0usize;
    for entry in &entries {
        if mem.forget(&entry.key).await? {
            deleted += 1;
        }
    }

    println!(
        "{} Cleared {deleted}/{} entries.",
        style("✓").green().bold(),
        entries.len(),
    );

    Ok(())
}

/// Delete a single entry by exact key or prefix match.
async fn handle_clear_key(mem: &dyn Memory, key: &str, yes: bool) -> Result<()> {
    // Resolve the target key (exact match or unique prefix).
    let target = if mem.get(key).await?.is_some() {
        key.to_string()
    } else {
        let all = mem.list(None, None).await?;
        let matches: Vec<_> = all.iter().filter(|e| e.key.starts_with(key)).collect();
        match matches.len() {
            0 => {
                println!(
                    "{}",
                    mt_args(
                        "cli-memory-key-not-found",
                        &[("key", key)],
                        "No memory entry found for key"
                    )
                );
                return Ok(());
            }
            1 => matches[0].key.clone(),
            n => {
                println!(
                    "{}\n",
                    mt_args(
                        "cli-memory-prefix-matched",
                        &[("key", key), ("n", &n.to_string())],
                        "Prefix matched entries"
                    )
                );
                for entry in matches {
                    println!(
                        "- {} [{}]",
                        style(&entry.key).white().bold(),
                        entry.category
                    );
                }
                println!(
                    "\n{}",
                    mt(
                        "cli-memory-narrow-prefix",
                        "Specify a longer prefix to narrow the match."
                    )
                );
                return Ok(());
            }
        }
    };

    if !yes {
        let confirmed = dialoguer::Confirm::new()
            .with_prompt(format!("  Delete '{target}'?"))
            .default(false)
            .interact()?;
        if !confirmed {
            println!("{}", mt("cli-memory-aborted", "Aborted."));
            return Ok(());
        }
    }

    if mem.forget(&target).await? {
        println!(
            "{} {}",
            style("✓").green().bold(),
            mt_args("cli-memory-deleted-key", &[("key", &target)], "Deleted key")
        );
    }

    Ok(())
}

fn parse_category(s: &str) -> MemoryCategory {
    match s.trim().to_ascii_lowercase().as_str() {
        "core" => MemoryCategory::Core,
        "daily" => MemoryCategory::Daily,
        "conversation" => MemoryCategory::Conversation,
        other => MemoryCategory::Custom(other.to_string()),
    }
}

fn truncate_content(s: &str, max_len: usize) -> String {
    let line = s.lines().next().unwrap_or(s);
    if line.len() <= max_len {
        return line.to_string();
    }
    let truncated: String = line.chars().take(max_len.saturating_sub(3)).collect();
    format!("{truncated}...")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn parse_category_known_variants() {
        assert_eq!(parse_category("core"), MemoryCategory::Core);
        assert_eq!(parse_category("daily"), MemoryCategory::Daily);
        assert_eq!(parse_category("conversation"), MemoryCategory::Conversation);
        assert_eq!(parse_category("CORE"), MemoryCategory::Core);
        assert_eq!(parse_category("  Daily  "), MemoryCategory::Daily);
    }

    #[test]
    fn parse_category_custom_fallback() {
        assert_eq!(
            parse_category("project_notes"),
            MemoryCategory::Custom("project_notes".into())
        );
    }

    #[test]
    fn truncate_content_short_text_unchanged() {
        assert_eq!(truncate_content("hello", 10), "hello");
    }

    #[test]
    fn truncate_content_long_text_truncated() {
        let result = truncate_content("this is a very long string", 10);
        assert!(result.ends_with("..."));
        assert!(result.chars().count() <= 10);
    }

    #[test]
    fn truncate_content_multiline_uses_first_line() {
        assert_eq!(truncate_content("first\nsecond", 20), "first");
    }

    #[test]
    fn truncate_content_empty_string() {
        assert_eq!(truncate_content("", 10), "");
    }

    #[tokio::test]
    async fn clear_rejects_append_only_markdown_backend() {
        let tmp = TempDir::new().unwrap();
        let mut config = Config::default();
        config.data_dir = tmp.path().to_path_buf();
        config.memory.backend = "markdown".into();

        let err = handle_command(
            crate::MemoryCommands::Clear {
                key: None,
                category: None,
                yes: true,
            },
            &config,
        )
        .await
        .unwrap_err();

        let msg = err.to_string();
        // The backend name is interpolated verbatim into the (localized) error,
        // so assert on the locale-stable name rather than the translated prose.
        assert!(msg.contains("'markdown'"), "got: {msg}");
    }

    #[tokio::test]
    async fn clear_rejects_qdrant_backend_constructed_as_markdown() {
        let tmp = TempDir::new().unwrap();
        let mut config = Config::default();
        config.data_dir = tmp.path().to_path_buf();
        config.memory.backend = "qdrant".into();

        let err = handle_command(
            crate::MemoryCommands::Clear {
                key: None,
                category: None,
                yes: true,
            },
            &config,
        )
        .await
        .unwrap_err();

        let msg = err.to_string();
        assert!(msg.contains("'qdrant'"), "got: {msg}");
    }

    #[tokio::test]
    async fn clear_rejects_dotted_qdrant_backend() {
        let tmp = TempDir::new().unwrap();
        let mut config = Config::default();
        config.data_dir = tmp.path().to_path_buf();
        config.memory.backend = "qdrant.default".into();

        let err = handle_command(
            crate::MemoryCommands::Clear {
                key: None,
                category: None,
                yes: true,
            },
            &config,
        )
        .await
        .unwrap_err();

        let msg = err.to_string();
        assert!(msg.contains("'qdrant'"), "got: {msg}");
    }
}
