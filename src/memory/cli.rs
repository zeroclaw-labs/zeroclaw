use super::traits::{Memory, MemoryCategory};
use super::{
    classify_memory_backend, create_memory_for_migration, effective_memory_backend_name,
    MemoryBackendKind,
};
use crate::config::Config;
#[cfg(feature = "memory-postgres")]
use anyhow::Context;
use anyhow::{bail, Result};
use console::style;

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
    }
}

/// Create a lightweight memory backend for CLI management operations.
///
/// CLI commands (list/get/stats/clear) never use vector search, so we skip
/// embedding provider initialisation for local backends by using the
/// migration factory.  Postgres still needs its full connection config.
fn create_cli_memory(config: &Config) -> Result<Box<dyn Memory>> {
    let backend = effective_memory_backend_name(
        &config.memory.backend,
        Some(&config.storage.provider.config),
    );

    match classify_memory_backend(&backend) {
        MemoryBackendKind::None => {
            bail!("Memory backend is 'none' (disabled). No entries to manage.");
        }
        #[cfg(feature = "memory-postgres")]
        MemoryBackendKind::Postgres => {
            #[cfg(feature = "memory-postgres")]
            {
                let sp = &config.storage.provider.config;
                let db_url = sp
                    .db_url
                    .as_deref()
                    .map(str::trim)
                    .filter(|v| !v.is_empty())
                    .context(
                        "memory backend 'postgres' requires db_url in [storage.provider.config]",
                    )?;
                let mem = super::PostgresMemory::new(
                    db_url,
                    &sp.schema,
                    &sp.table,
                    sp.connect_timeout_secs,
                    sp.tls,
                )?;
                Ok(Box::new(mem))
            }
            #[cfg(not(feature = "memory-postgres"))]
            {
                bail!("Memory backend 'postgres' requires the 'memory-postgres' feature to be enabled at compile time.");
            }
        }
        #[cfg(not(feature = "memory-postgres"))]
        MemoryBackendKind::Postgres => {
            bail!("memory backend 'postgres' requires the 'memory-postgres' feature to be enabled");
        }
        _ => create_memory_for_migration(&backend, &config.workspace_dir),
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
        println!("No memory entries found.");
        return Ok(());
    }

    let total = entries.len();
    let page: Vec<_> = entries.into_iter().skip(offset).take(limit).collect();

    if page.is_empty() {
        println!("No entries at offset {offset} (total: {total}).");
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
        println!("\n  Use --offset {} to see the next page.", offset + limit);
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
        0 => println!("No memory entry found for key: {key}"),
        1 => print_entry(matches[0]),
        n => {
            println!("Prefix '{key}' matched {n} entries:\n");
            for entry in matches {
                println!(
                    "- {} [{}]",
                    style(&entry.key).white().bold(),
                    entry.category
                );
            }
            println!("\nSpecify a longer prefix to narrow the match.");
        }
    }

    Ok(())
}

fn print_entry(entry: &super::traits::MemoryEntry) {
    println!("Key:       {}", style(&entry.key).white().bold());
    println!("Category:  {}", entry.category);
    println!("Timestamp: {}", entry.timestamp);
    if let Some(sid) = &entry.session_id {
        println!("Session:   {sid}");
    }
    println!("\n{}", entry.content);
}

async fn handle_stats(config: &Config) -> Result<()> {
    let mem = create_cli_memory(config)?;
    let healthy = mem.health_check().await;
    let total = mem.count().await.unwrap_or(0);

    println!("Memory Statistics:\n");
    println!("  Backend:  {}", style(mem.name()).white().bold());
    println!(
        "  Health:   {}",
        if healthy {
            style("healthy").green().bold().to_string()
        } else {
            style("unhealthy").yellow().bold().to_string()
        }
    );
    println!("  Total:    {total}");

    let all = mem.list(None, None).await.unwrap_or_default();
    if !all.is_empty() {
        let mut counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        for entry in &all {
            *counts.entry(entry.category.to_string()).or_default() += 1;
        }

        println!("\n  By category:");
        let mut sorted: Vec<_> = counts.into_iter().collect();
        sorted.sort_by(|a, b| b.1.cmp(&a.1));
        for (cat, count) in sorted {
            println!("    {cat:<20} {count}");
        }
    }

    Ok(())
}

async fn handle_clear(
    config: &Config,
    key: Option<String>,
    category: Option<String>,
    yes: bool,
) -> Result<()> {
    let mem = create_cli_memory(config)?;

    // Single-key deletion (exact or prefix match).
    if let Some(key) = key {
        return handle_clear_key(&*mem, &key, yes).await;
    }

    // Batch deletion by category (or all).
    let cat = category.as_deref().map(parse_category);
    let entries = mem.list(cat.as_ref(), None).await?;

    if entries.is_empty() {
        println!("No entries to clear.");
        return Ok(());
    }

    let scope = category.as_deref().unwrap_or("all categories");
    println!("Found {} entries in '{scope}'.", entries.len());

    if !yes {
        let confirmed = dialoguer::Confirm::new()
            .with_prompt(format!("  Delete {} entries?", entries.len()))
            .default(false)
            .interact()?;
        if !confirmed {
            println!("Aborted.");
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
                println!("No memory entry found for key: {key}");
                return Ok(());
            }
            1 => matches[0].key.clone(),
            n => {
                println!("Prefix '{key}' matched {n} entries:\n");
                for entry in matches {
                    println!(
                        "- {} [{}]",
                        style(&entry.key).white().bold(),
                        entry.category
                    );
                }
                println!("\nSpecify a longer prefix to narrow the match.");
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
            println!("Aborted.");
            return Ok(());
        }
    }

    if mem.forget(&target).await? {
        println!("{} Deleted key: {target}", style("✓").green().bold());
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
}
