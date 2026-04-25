//! CLI handlers for `augusta brain` subcommands.

use super::{db, index, query, BrainIndex};
use anyhow::Result;
use console::style;
use std::sync::Arc;

/// Handle `augusta brain <subcommand>` CLI commands.
pub async fn handle_command(command: crate::BrainCommands) -> Result<()> {
    let brain_dir = super::brain_dir();
    let brain = BrainIndex::new(&brain_dir);

    match command {
        crate::BrainCommands::Index { full } => handle_index(&brain, full).await,
        crate::BrainCommands::Query {
            text,
            session,
            budget,
            top_k,
            format,
            categories,
        } => handle_query(&brain, &text, session, budget, top_k, &format, categories).await,
        crate::BrainCommands::Stats => handle_stats(&brain),
        crate::BrainCommands::Validate => handle_validate(&brain),
        crate::BrainCommands::Compile {
            agent_id,
            force,
            dry_run,
            paperclip_host,
        } => {
            let opts = super::compile::CompileOptions {
                brain_dir: brain_dir.clone(),
                agent_id,
                force,
                dry_run,
                paperclip_host: paperclip_host
                    .or_else(|| std::env::var("LW_PAPERCLIP_HOST").ok())
                    .unwrap_or_else(|| "http://127.0.0.1:3100".to_string()),
            };
            let report = super::compile::run(opts).await?;
            println!(
                "{} compile {}: total={} written={} skipped_unchanged={} failed={}",
                style(">>>").cyan().bold(),
                if report.dry_run { "(dry-run)" } else { "" },
                report.total_agents,
                report.written,
                report.skipped_unchanged,
                report.failed,
            );
            for err in &report.errors {
                eprintln!("  {}: {err}", style("warn").yellow());
            }
            Ok(())
        }
    }
}

async fn handle_index(brain: &BrainIndex, full: bool) -> Result<()> {
    let mode = if full { "full" } else { "incremental" };
    println!("{} Indexing brain ({mode})...", style(">>>").cyan().bold());

    let embedder = create_embedder();
    let result = index::index_brain(brain, embedder.as_ref(), full).await?;

    println!(
        "{} Indexed {}/{} files, {} chunks created",
        style("done").green().bold(),
        result.files_indexed,
        result.files_scanned,
        result.chunks_created,
    );

    if result.files_skipped > 0 {
        println!("  {} files unchanged (skipped)", result.files_skipped);
    }

    for err in &result.errors {
        eprintln!("  {}: {err}", style("warn").yellow());
    }

    Ok(())
}

async fn handle_query(
    brain: &BrainIndex,
    text: &str,
    session: Option<String>,
    budget: usize,
    top_k: usize,
    format: &str,
    categories: Option<String>,
) -> Result<()> {
    let categories_vec: Vec<String> = categories
        .map(|c| c.split(',').map(|s| s.trim().to_string()).collect())
        .unwrap_or_default();

    // Try to compute query embedding for hybrid search
    let query_embedding = {
        let embedder = create_embedder();
        if embedder.dimensions() > 0 {
            embedder.embed_one(text).await.ok()
        } else {
            None
        }
    };

    let opts = query::QueryOptions {
        session,
        categories: categories_vec,
        top_k,
        budget,
        query_embedding,
    };

    let results = query::query(brain, text, &opts)?;

    if results.is_empty() {
        println!("No results found.");
        return Ok(());
    }

    if format == "markdown" {
        print!("{}", query::format_results_markdown(&results));
    } else {
        let total_tokens: i32 = results.iter().map(|r| r.token_count).sum();
        println!("{} results (~{} tokens):\n", results.len(), total_tokens);
        for (i, r) in results.iter().enumerate() {
            println!(
                "{}. {} {} [{}, score: {:.3}]",
                i + 1,
                style(&r.chunk_key).white().bold(),
                style(format!("({})", r.file_path)).dim(),
                r.category,
                r.score,
            );
            // Show first 120 chars of content
            let preview: String = r.content.chars().take(120).collect();
            let preview = preview.replace('\n', " ");
            println!("   {preview}...\n");
        }
    }

    Ok(())
}

fn handle_stats(brain: &BrainIndex) -> Result<()> {
    if !brain.db_path.exists() {
        println!("Brain index not found. Run `augusta brain index` first.");
        return Ok(());
    }

    let conn = db::open_db(&brain.db_path)?;
    let (chunks, files, stale) = db::stats(&conn)?;

    println!("Brain Index Statistics:\n");
    println!("  DB:     {}", brain.db_path.display());
    println!("  Files:  {files}");
    println!("  Chunks: {chunks}");
    println!("  Stale:  {stale}");

    // DB file size
    if let Ok(metadata) = std::fs::metadata(&brain.db_path) {
        let size_kb = metadata.len() / 1024;
        println!("  Size:   {} KB", size_kb);
    }

    Ok(())
}

fn handle_validate(brain: &BrainIndex) -> Result<()> {
    if !brain.db_path.exists() {
        println!("Brain index not found. Run `augusta brain index` first.");
        return Ok(());
    }

    let mismatches = index::validate_hashes(brain)?;

    if mismatches.is_empty() {
        println!("{} All content hashes valid.", style("ok").green().bold());
    } else {
        println!(
            "{} Found {} mismatches:\n",
            style("warn").yellow().bold(),
            mismatches.len()
        );
        for m in &mismatches {
            println!("  - {m}");
        }
        println!("\nRun `augusta brain index` to fix.");
    }

    Ok(())
}

/// Create the best available embedding provider.
fn create_embedder() -> Arc<dyn crate::memory::embeddings::EmbeddingProvider> {
    #[cfg(feature = "brain")]
    {
        match super::onnx_embedding::OnnxEmbedding::new() {
            Ok(provider) => return Arc::new(provider),
            Err(e) => {
                tracing::warn!("ONNX embedding init failed, using keyword-only: {e}");
            }
        }
    }

    Arc::new(crate::memory::embeddings::NoopEmbedding)
}
