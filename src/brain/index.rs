//! Brain file scanner, classifier, and indexer.
//!
//! Scans ~/.brain/, chunks files (YAML-aware or markdown), classifies by
//! path, computes embeddings, and stores in brain.db.

use super::db;
use super::yaml_chunker;
use super::{classify_file, BrainIndex, FileType};
use crate::memory::chunker;
use crate::memory::embeddings::EmbeddingProvider;
use crate::memory::vector;
use anyhow::Result;
use sha2::{Digest, Sha256};
use std::path::Path;

/// Maximum chars per chunk (500 tokens * 4 chars/token).
const MAX_CHUNK_CHARS: usize = 2000;
/// Maximum tokens per chunk for markdown chunker.
const MAX_CHUNK_TOKENS: usize = 500;

/// Index result statistics.
#[derive(Debug, Default)]
pub struct IndexResult {
    pub files_scanned: usize,
    pub files_indexed: usize,
    pub files_skipped: usize,
    pub chunks_created: usize,
    pub errors: Vec<String>,
}

/// Scan the brain directory and index all files.
///
/// If `full` is true, re-indexes everything. Otherwise, only indexes
/// files whose SHA-256 hash differs from the stored hash.
pub async fn index_brain(
    brain: &BrainIndex,
    embedder: &dyn EmbeddingProvider,
    full: bool,
) -> Result<IndexResult> {
    let conn = db::open_db(&brain.db_path)?;
    let mut result = IndexResult::default();

    // Collect all indexable files
    let files = collect_brain_files(&brain.brain_dir)?;
    result.files_scanned = files.len();

    // Track which files we've seen (for cleanup)
    let mut seen_paths = std::collections::HashSet::new();

    for rel_path in &files {
        seen_paths.insert(rel_path.clone());
        let abs_path = brain.brain_dir.join(rel_path);

        let content = match std::fs::read_to_string(&abs_path) {
            Ok(c) => c,
            Err(e) => {
                result.errors.push(format!("{rel_path}: read error: {e}"));
                continue;
            }
        };

        let file_hash = sha256_hex(&content);

        // Skip unchanged files in incremental mode
        if !full {
            if let Ok(Some(stored_hash)) = db::get_file_hash(&conn, rel_path) {
                if stored_hash == file_hash {
                    result.files_skipped += 1;
                    continue;
                }
            }
        }

        // Classify and chunk
        let (category, file_type, subject) = classify_file(rel_path);

        let text_chunks = match file_type {
            FileType::Yaml => {
                let yaml_chunks = yaml_chunker::chunk_yaml(&content, MAX_CHUNK_CHARS);
                yaml_chunks
                    .into_iter()
                    .map(|c| (c.key, c.content))
                    .collect::<Vec<_>>()
            }
            FileType::Markdown => {
                let md_chunks = chunker::chunk_markdown(&content, MAX_CHUNK_TOKENS);
                md_chunks
                    .into_iter()
                    .map(|c| {
                        let key = c
                            .heading
                            .map(|h| h.to_string())
                            .unwrap_or_else(|| format!("chunk_{}", c.index));
                        (key, c.content)
                    })
                    .collect::<Vec<_>>()
            }
            FileType::Mermaid => {
                // Whole file = 1 chunk, filename stem as key
                let stem = rel_path
                    .rsplit_once('/')
                    .map(|(_, f)| f)
                    .unwrap_or(rel_path);
                let key = stem.rsplit_once('.').map(|(s, _)| s).unwrap_or(stem);
                vec![(key.to_string(), content.clone())]
            }
        };

        // Remove existing chunks for this file (full re-index of this file)
        db::remove_file_chunks(&conn, rel_path)?;

        // Embed and store chunks
        for (chunk_idx, (chunk_key, chunk_content)) in text_chunks.iter().enumerate() {
            if chunk_content.trim().is_empty() {
                continue;
            }

            let content_hash = sha256_hex(chunk_content);
            #[allow(clippy::cast_possible_truncation)]
            let token_count = (chunk_content.len() / 4) as i32;

            // Compute embedding
            let embedding_bytes = if embedder.dimensions() > 0 {
                match embedder.embed_one(chunk_content).await {
                    Ok(vec) => Some(vector::vec_to_bytes(&vec)),
                    Err(e) => {
                        tracing::warn!(
                            file = %rel_path,
                            chunk = chunk_idx,
                            error = %e,
                            "Embedding failed, storing without vector"
                        );
                        None
                    }
                }
            } else {
                None
            };

            #[allow(clippy::cast_possible_truncation)]
            db::upsert_chunk(
                &conn,
                rel_path,
                &file_hash,
                chunk_idx as i32,
                chunk_key,
                chunk_content,
                &content_hash,
                token_count,
                category.as_str(),
                file_type.as_str(),
                subject.as_deref(),
                embedding_bytes.as_deref(),
            )?;

            result.chunks_created += 1;
        }

        result.files_indexed += 1;
    }

    // Clean up chunks for deleted files
    let indexed = db::indexed_files(&conn)?;
    for indexed_path in &indexed {
        if !seen_paths.contains(indexed_path) {
            db::remove_file_chunks(&conn, indexed_path)?;
        }
    }

    Ok(result)
}

/// Collect all indexable files in the brain directory.
fn collect_brain_files(brain_dir: &Path) -> Result<Vec<String>> {
    let mut files = Vec::new();
    collect_files_recursive(brain_dir, brain_dir, &mut files)?;
    files.sort();
    Ok(files)
}

fn collect_files_recursive(dir: &Path, root: &Path, files: &mut Vec<String>) -> Result<()> {
    if !dir.is_dir() {
        return Ok(());
    }

    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();

        // Skip hidden dirs, .git, .claude, _archive, tools, evaluation, research, users
        if name.starts_with('.') || name.starts_with('_') {
            continue;
        }
        // Skip non-content directories
        if path == root.join("tools")
            || path == root.join("evaluation")
            || path == root.join("research")
            || path == root.join("users")
        {
            continue;
        }

        if path.is_dir() {
            collect_files_recursive(&path, root, files)?;
        } else {
            let ext = name.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
            match ext {
                "yaml" | "yml" | "md" | "mmd" | "mermaid" => {
                    let rel = path
                        .strip_prefix(root)
                        .unwrap_or(&path)
                        .to_string_lossy()
                        .to_string();
                    files.push(rel);
                }
                _ => {}
            }
        }
    }

    Ok(())
}

fn sha256_hex(data: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data.as_bytes());
    hex::encode(hasher.finalize())
}

/// Validate stored content hashes against disk.
pub fn validate_hashes(brain: &BrainIndex) -> Result<Vec<String>> {
    let conn = db::open_db(&brain.db_path)?;
    let indexed = db::indexed_files(&conn)?;
    let mut mismatches = Vec::new();

    for rel_path in &indexed {
        let abs_path = brain.brain_dir.join(rel_path);

        if !abs_path.exists() {
            mismatches.push(format!("{rel_path}: file deleted from disk"));
            continue;
        }

        let content = std::fs::read_to_string(&abs_path)?;
        let disk_hash = sha256_hex(&content);

        if let Ok(Some(stored_hash)) = db::get_file_hash(&conn, rel_path) {
            if stored_hash != disk_hash {
                mismatches.push(format!("{rel_path}: file hash mismatch (needs re-index)"));
            }
        }
    }

    Ok(mismatches)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_hex_deterministic() {
        let hash = sha256_hex("hello world");
        assert_eq!(hash.len(), 64);
        assert_eq!(sha256_hex("hello world"), hash);
        assert_ne!(sha256_hex("hello world!"), hash);
    }

    #[test]
    fn collect_brain_files_skips_hidden() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        std::fs::write(dir.path().join(".git/config"), "x").unwrap();
        std::fs::create_dir(dir.path().join("soul")).unwrap();
        std::fs::write(dir.path().join("soul/identity.yaml"), "x: 1").unwrap();

        let files = collect_brain_files(dir.path()).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0], "soul/identity.yaml");
    }
}
