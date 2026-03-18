//! Vault file watcher — auto-ingests markdown files from the vault directory
//! into the embedded brain (RVF + knowledge graph) on create/modify.

use anyhow::Result;
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};

use super::learning::LearningBrain;
use ruvector_onnx_embeddings::Embedder;

/// Spawn a background vault watcher that ingests `.md` files into the brain.
///
/// Returns the watcher handle (must be kept alive — dropping it stops watching).
pub fn spawn_vault_watcher(
    vault_path: PathBuf,
    brain: Arc<Mutex<LearningBrain>>,
    embedder: Arc<Mutex<Embedder>>,
) -> Result<RecommendedWatcher> {
    let (tx, rx) = mpsc::channel::<Event>(256);

    let mut watcher = notify::recommended_watcher(move |res: notify::Result<Event>| {
        if let Ok(event) = res {
            let _ = tx.blocking_send(event);
        }
    })?;

    watcher.watch(&vault_path, RecursiveMode::Recursive)?;
    tracing::info!(path = %vault_path.display(), "vault watcher started");

    tokio::spawn(handle_vault_events(rx, brain, embedder));

    Ok(watcher)
}

/// Process incoming file events, debouncing and ingesting markdown content.
async fn handle_vault_events(
    mut rx: mpsc::Receiver<Event>,
    brain: Arc<Mutex<LearningBrain>>,
    embedder: Arc<Mutex<Embedder>>,
) {
    // Simple debounce: track last-processed path + time
    let mut last: Option<(PathBuf, tokio::time::Instant)> = None;

    while let Some(event) = rx.recv().await {
        let dominated = matches!(
            event.kind,
            EventKind::Create(_) | EventKind::Modify(_)
        );
        if !dominated {
            continue;
        }

        for path in &event.paths {
            if !is_markdown(path) {
                continue;
            }

            // Debounce: skip if same file within 2 seconds
            let now = tokio::time::Instant::now();
            if let Some((ref prev, ts)) = last {
                if prev == path && now.duration_since(ts).as_secs() < 2 {
                    continue;
                }
            }
            last = Some((path.clone(), now));

            if let Err(e) = ingest_file(path, &brain, &embedder).await {
                tracing::warn!(path = %path.display(), error = %e, "vault ingest failed");
            }
        }
    }
}

/// Read a markdown file and ingest it into the brain.
async fn ingest_file(
    path: &Path,
    brain: &Arc<Mutex<LearningBrain>>,
    embedder: &Arc<Mutex<Embedder>>,
) -> Result<()> {
    let content = tokio::fs::read_to_string(path).await?;
    if content.trim().is_empty() {
        return Ok(());
    }

    // Derive tags from the path: vault subdirectory + filename stem
    let subdir = path
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|s| s.to_str())
        .unwrap_or("vault");
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("note");

    let embedding = {
        let mut emb = embedder.lock().await;
        emb.embed_one(&content)
            .map_err(|e| anyhow::anyhow!("embed failed: {e}"))?
    };

    let tags = ["vault", subdir, stem];
    let mut brain = brain.lock().await;
    brain
        .process_and_learn(&content, embedding, "vault", subdir, &tags, false)
        .await?;

    tracing::info!(path = %path.display(), "vault file ingested");
    Ok(())
}

fn is_markdown(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("md"))
}
