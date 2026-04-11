//! On-disk cache of LLM-readable conversions of user documents.
//!
//! # Why this exists
//!
//! Most files on a real user's computer are PDF / HWP / HWPX / DOC(X) /
//! XLS(X) / PPT(X) — none of which an LLM can read directly. MoA already
//! has [`crate::tools::document_pipeline::DocumentPipelineTool`] which
//! converts any of those formats into Markdown + HTML using:
//!
//! - bundled `pdf-extract` (digital PDF, free, local)
//! - the operator's Hancom DocsConverter server (HWP/Office, free)
//! - Upstage Document Parser API (image PDF, paid via 2.2× credit billing)
//!
//! This module wraps that converter in an idempotent on-disk cache so the
//! conversion only runs **once per file revision**, the result is
//! discoverable by the agent's existing `content_search` / `glob_search`
//! tools (because it lives inside the workspace), and re-uploads of the
//! same file don't waste credits or wall-clock time.
//!
//! # Layout
//!
//! ```text
//! {workspace_dir}/documents_cache/
//! ├── <16-hex source hash>/
//! │   ├── <original_filename>.md     ← Markdown for the LLM
//! │   ├── <original_filename>.html   ← Optional HTML (kept when non-empty)
//! │   └── meta.json                  ← Source path, mtime, size, engine, ts
//! └── ...
//! ```
//!
//! `meta.json` records the source file's last-modified timestamp and byte
//! size. On the next request the cache compares the current mtime + size
//! against the recorded values and skips conversion if both still match.
//! Same filename, different format — exactly what the user asked for.
//!
//! # Thread-safety / concurrency
//!
//! `DocumentCache` is `Clone` and holds nothing but an `Arc<PathBuf>` to
//! the root, so multiple background tasks can share it cheaply. Per-file
//! work is serialized via the filesystem (the cache writes are atomic via
//! `tokio::fs::rename` from a temp file in the same directory).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::tools::document_pipeline::DocumentPipelineTool;
use crate::tools::traits::Tool;

/// Persistent cache root inside the workspace.
const CACHE_DIR_NAME: &str = "documents_cache";

/// Length of the hex prefix used for the per-source directory name.
/// 16 hex chars = 64 bits of entropy = 1.8e19 possible values, far more
/// than enough to avoid accidental collisions even on machines with
/// hundreds of thousands of cached documents.
const ID_HEX_LEN: usize = 16;

/// Metadata file name inside each cache subdirectory.
const META_FILENAME: &str = "meta.json";

/// Persisted metadata for one cached document.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentMeta {
    /// Absolute path to the original source file at the time of conversion.
    pub source_path: String,
    /// Original filename (with extension), used for the .md / .html name.
    pub original_filename: String,
    /// Source file size at conversion time (bytes).
    pub source_size: u64,
    /// Source file mtime at conversion time (seconds since epoch).
    pub source_mtime_secs: u64,
    /// Conversion timestamp (seconds since epoch).
    pub converted_at_secs: u64,
    /// Engine that produced the cache: "pdf-extract", "upstage", "hancom", "web-pdf", ...
    pub engine: String,
    /// Whether the .html sibling was produced (false → only .md exists).
    pub has_html: bool,
}

/// Result of a cache hit or freshly performed conversion.
#[derive(Debug, Clone)]
pub struct CachedDocument {
    /// Per-source directory name (hex hash).
    pub id: String,
    /// Path to the cached `.md` file.
    pub markdown_path: PathBuf,
    /// Path to the cached `.html` file (only if `meta.has_html`).
    pub html_path: Option<PathBuf>,
    /// Metadata read from `meta.json`.
    pub meta: DocumentMeta,
    /// True if conversion was actually performed; false if returned from cache.
    pub from_cache: bool,
}

/// Idempotent document conversion cache.
#[derive(Clone)]
pub struct DocumentCache {
    root: Arc<PathBuf>,
}

impl DocumentCache {
    /// Construct a cache rooted at `{workspace_dir}/documents_cache/`.
    /// Creates the directory if it does not yet exist.
    pub fn new(workspace_dir: impl AsRef<Path>) -> Result<Self> {
        let root = workspace_dir.as_ref().join(CACHE_DIR_NAME);
        std::fs::create_dir_all(&root)
            .with_context(|| format!("create cache root {}", root.display()))?;
        Ok(Self {
            root: Arc::new(root),
        })
    }

    /// Cache root for inspection (used by tests + CLI).
    pub fn root(&self) -> &Path {
        self.root.as_path()
    }

    /// Compute the per-source 16-hex id from the absolute source path.
    pub fn id_for(source: impl AsRef<Path>) -> String {
        let abs = source
            .as_ref()
            .to_string_lossy()
            .into_owned();
        let mut hasher = Sha256::new();
        hasher.update(abs.as_bytes());
        let digest = hasher.finalize();
        let mut hex = String::with_capacity(ID_HEX_LEN);
        for byte in digest.iter().take(ID_HEX_LEN / 2) {
            hex.push_str(&format!("{byte:02x}"));
        }
        hex
    }

    /// Per-source cache directory (may not yet exist).
    pub fn cache_dir_for(&self, source: impl AsRef<Path>) -> PathBuf {
        self.root.join(Self::id_for(source))
    }

    /// Metadata path for the given source file.
    fn meta_path_for(&self, source: impl AsRef<Path>) -> PathBuf {
        self.cache_dir_for(source).join(META_FILENAME)
    }

    /// Look up a cached entry by source path. Returns `None` if no cache
    /// exists OR if the cache is stale (source mtime / size differs from
    /// the recorded values).
    pub async fn lookup(&self, source: impl AsRef<Path>) -> Result<Option<CachedDocument>> {
        let source = source.as_ref();
        let meta_path = self.meta_path_for(source);
        if !meta_path.exists() {
            return Ok(None);
        }

        let raw = tokio::fs::read(&meta_path)
            .await
            .with_context(|| format!("read meta {}", meta_path.display()))?;
        let meta: DocumentMeta = serde_json::from_slice(&raw)
            .with_context(|| format!("parse meta {}", meta_path.display()))?;

        // Stale-cache detection: if the source file changed since the
        // cache was written, force a re-conversion. This is the cheap
        // path — we only stat() the source, not re-hash its contents.
        if let Ok(source_meta) = tokio::fs::metadata(source).await {
            let cur_size = source_meta.len();
            let cur_mtime = source_meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
            if cur_size != meta.source_size || cur_mtime != meta.source_mtime_secs {
                return Ok(None);
            }
        }

        let cache_dir = self.cache_dir_for(source);
        let md_path = cache_dir.join(format!("{}.md", file_stem(&meta.original_filename)));
        if !md_path.exists() {
            return Ok(None);
        }
        let html_path = if meta.has_html {
            let p = cache_dir.join(format!("{}.html", file_stem(&meta.original_filename)));
            p.exists().then_some(p)
        } else {
            None
        };

        Ok(Some(CachedDocument {
            id: Self::id_for(source),
            markdown_path: md_path,
            html_path,
            meta,
            from_cache: true,
        }))
    }

    /// Convert a single source document and persist the result. If a
    /// fresh cache entry already exists, returns it without re-running
    /// the converter.
    pub async fn convert_and_cache(
        &self,
        source: impl AsRef<Path>,
        tool: &DocumentPipelineTool,
    ) -> Result<CachedDocument> {
        let source = source.as_ref();
        if let Some(hit) = self.lookup(source).await? {
            return Ok(hit);
        }

        let original_filename = source
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| anyhow!("source path has no filename: {}", source.display()))?
            .to_string();

        let cache_dir = self.cache_dir_for(source);
        tokio::fs::create_dir_all(&cache_dir)
            .await
            .with_context(|| format!("create cache dir {}", cache_dir.display()))?;

        // DocumentPipelineTool already supports an `output_dir` arg that
        // writes `<stem>.html` and `<stem>.md` to a directory of our
        // choosing — exactly what we want. Reuse it instead of duplicating
        // the save logic here.
        let args = serde_json::json!({
            "file_path": source.to_string_lossy(),
            "output_dir": cache_dir.to_string_lossy(),
        });
        let result = tool
            .execute(args)
            .await
            .with_context(|| format!("convert {}", source.display()))?;
        if !result.success {
            return Err(anyhow!(
                "document_process failed for {}: {}",
                source.display(),
                result.error.as_deref().unwrap_or(&result.output)
            ));
        }

        // Read what the converter produced. The tool writes <stem>.md
        // and <stem>.html using the source file's basename.
        let stem = file_stem(&original_filename);
        let md_path = cache_dir.join(format!("{stem}.md"));
        if !md_path.exists() {
            return Err(anyhow!(
                "converter reported success but {} is missing",
                md_path.display()
            ));
        }
        let html_path_candidate = cache_dir.join(format!("{stem}.html"));
        let has_html = html_path_candidate.exists()
            && tokio::fs::metadata(&html_path_candidate)
                .await
                .map(|m| m.len() > 0)
                .unwrap_or(false);

        // Resolve source mtime/size for the meta record.
        let source_meta = tokio::fs::metadata(source)
            .await
            .with_context(|| format!("stat source {}", source.display()))?;
        let source_size = source_meta.len();
        let source_mtime_secs = source_meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let converted_at_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        // Best-effort engine label parsed from the tool's output text.
        // The tool currently does not return a structured engine field,
        // so we keep this conservative — agents primarily care about
        // mtime/size for staleness, not the engine name.
        let engine = guess_engine_label(source);

        let meta = DocumentMeta {
            source_path: source.to_string_lossy().to_string(),
            original_filename: original_filename.clone(),
            source_size,
            source_mtime_secs,
            converted_at_secs,
            engine,
            has_html,
        };

        let meta_bytes =
            serde_json::to_vec_pretty(&meta).context("serialize meta.json")?;
        let meta_path = cache_dir.join(META_FILENAME);
        write_atomic(&meta_path, &meta_bytes).await?;

        Ok(CachedDocument {
            id: Self::id_for(source),
            markdown_path: md_path,
            html_path: has_html.then_some(html_path_candidate),
            meta,
            from_cache: false,
        })
    }

    /// Convert and cache a document that has already been written to disk
    /// at `source`, but where the markdown / html content was produced
    /// out-of-band (e.g. for the `web-pdf` flow that downloads a remote
    /// PDF and converts it before saving locally).
    pub async fn store_precomputed(
        &self,
        source: impl AsRef<Path>,
        markdown: &str,
        html: Option<&str>,
        engine: &str,
    ) -> Result<CachedDocument> {
        let source = source.as_ref();
        let original_filename = source
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| anyhow!("source path has no filename: {}", source.display()))?
            .to_string();

        let cache_dir = self.cache_dir_for(source);
        tokio::fs::create_dir_all(&cache_dir)
            .await
            .with_context(|| format!("create cache dir {}", cache_dir.display()))?;

        let stem = file_stem(&original_filename);
        let md_path = cache_dir.join(format!("{stem}.md"));
        write_atomic(&md_path, markdown.as_bytes()).await?;

        let mut has_html = false;
        if let Some(h) = html {
            if !h.is_empty() {
                let html_path = cache_dir.join(format!("{stem}.html"));
                write_atomic(&html_path, h.as_bytes()).await?;
                has_html = true;
            }
        }

        let source_meta = tokio::fs::metadata(source)
            .await
            .with_context(|| format!("stat source {}", source.display()))?;
        let meta = DocumentMeta {
            source_path: source.to_string_lossy().to_string(),
            original_filename: original_filename.clone(),
            source_size: source_meta.len(),
            source_mtime_secs: source_meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0),
            converted_at_secs: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
            engine: engine.to_string(),
            has_html,
        };
        let meta_bytes = serde_json::to_vec_pretty(&meta)?;
        write_atomic(&cache_dir.join(META_FILENAME), &meta_bytes).await?;

        Ok(CachedDocument {
            id: Self::id_for(source),
            markdown_path: md_path.clone(),
            html_path: if has_html {
                Some(cache_dir.join(format!("{stem}.html")))
            } else {
                None
            },
            meta,
            from_cache: false,
        })
    }

    /// Enumerate every cached document by walking the cache root.
    /// Used by the gateway `GET /api/documents/list` endpoint.
    pub async fn list_all(&self) -> Result<Vec<DocumentMeta>> {
        let mut out = Vec::new();
        let mut entries = match tokio::fs::read_dir(self.root.as_path()).await {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
            Err(e) => return Err(e.into()),
        };
        while let Some(entry) = entries.next_entry().await? {
            if !entry.file_type().await?.is_dir() {
                continue;
            }
            let meta_path = entry.path().join(META_FILENAME);
            if !meta_path.exists() {
                continue;
            }
            let raw = match tokio::fs::read(&meta_path).await {
                Ok(b) => b,
                Err(_) => continue,
            };
            if let Ok(meta) = serde_json::from_slice::<DocumentMeta>(&raw) {
                out.push(meta);
            }
        }
        Ok(out)
    }
}

/// Conservatively pick an engine label based on file extension.
fn guess_engine_label(source: &Path) -> String {
    let ext = source
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "pdf" => "document_pipeline (pdf)".to_string(),
        "hwp" | "hwpx" | "doc" | "docx" | "xls" | "xlsx" | "ppt" | "pptx" => {
            "document_pipeline (hancom)".to_string()
        }
        "txt" | "md" | "html" | "htm" => "document_pipeline (passthrough)".to_string(),
        _ => "document_pipeline".to_string(),
    }
}

/// Filename stem ("report" from "report.pdf").
fn file_stem(filename: &str) -> String {
    Path::new(filename)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(filename)
        .to_string()
}

/// Atomic write: write to `<path>.tmp`, then rename. Both files are in
/// the same directory so rename stays on a single filesystem.
async fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension("tmp");
    tokio::fs::write(&tmp, bytes)
        .await
        .with_context(|| format!("write {}", tmp.display()))?;
    tokio::fs::rename(&tmp, path)
        .await
        .with_context(|| format!("rename {} -> {}", tmp.display(), path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_cache() -> (DocumentCache, TempDir) {
        let tmp = TempDir::new().unwrap();
        let cache = DocumentCache::new(tmp.path()).unwrap();
        (cache, tmp)
    }

    #[test]
    fn id_is_stable_for_same_path() {
        let id1 = DocumentCache::id_for("/Users/foo/Documents/report.pdf");
        let id2 = DocumentCache::id_for("/Users/foo/Documents/report.pdf");
        assert_eq!(id1, id2);
        assert_eq!(id1.len(), ID_HEX_LEN);
    }

    #[test]
    fn id_differs_for_different_paths() {
        let a = DocumentCache::id_for("/Users/foo/a.pdf");
        let b = DocumentCache::id_for("/Users/foo/b.pdf");
        assert_ne!(a, b);
    }

    #[test]
    fn cache_root_is_created_under_workspace() {
        let (cache, tmp) = make_cache();
        let expected = tmp.path().join(CACHE_DIR_NAME);
        assert_eq!(cache.root(), expected.as_path());
        assert!(expected.exists());
    }

    #[test]
    fn file_stem_strips_extension() {
        assert_eq!(file_stem("report.pdf"), "report");
        assert_eq!(file_stem("report.tar.gz"), "report.tar");
        assert_eq!(file_stem("noext"), "noext");
    }

    #[tokio::test]
    async fn lookup_returns_none_for_unseen_source() {
        let (cache, _tmp) = make_cache();
        let result = cache.lookup("/nonexistent/path.pdf").await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn store_precomputed_persists_and_lookup_returns_hit() {
        let (cache, _tmp) = make_cache();
        let src_dir = TempDir::new().unwrap();
        let source = src_dir.path().join("report.pdf");
        tokio::fs::write(&source, b"fake pdf bytes").await.unwrap();

        let stored = cache
            .store_precomputed(
                &source,
                "# Report\n\nHello world",
                Some("<h1>Report</h1>"),
                "test-engine",
            )
            .await
            .unwrap();
        assert!(!stored.from_cache);
        assert!(stored.markdown_path.exists());
        assert!(stored.html_path.unwrap().exists());

        let hit = cache.lookup(&source).await.unwrap().unwrap();
        assert!(hit.from_cache);
        assert_eq!(hit.meta.source_path, source.to_string_lossy());
        assert_eq!(hit.meta.engine, "test-engine");
        assert_eq!(hit.meta.original_filename, "report.pdf");
    }

    #[tokio::test]
    async fn lookup_invalidates_on_source_size_change() {
        let (cache, _tmp) = make_cache();
        let src_dir = TempDir::new().unwrap();
        let source = src_dir.path().join("doc.pdf");
        tokio::fs::write(&source, b"v1").await.unwrap();

        cache
            .store_precomputed(&source, "# v1", None, "test")
            .await
            .unwrap();
        assert!(cache.lookup(&source).await.unwrap().is_some());

        tokio::fs::write(&source, b"version two longer bytes")
            .await
            .unwrap();
        // Filesystem mtime granularity is fine on macOS/Linux; even if
        // mtime stayed equal, the size differs which is enough.
        assert!(cache.lookup(&source).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn store_precomputed_without_html_skips_html_file() {
        let (cache, _tmp) = make_cache();
        let src_dir = TempDir::new().unwrap();
        let source = src_dir.path().join("nohtml.pdf");
        tokio::fs::write(&source, b"x").await.unwrap();

        let stored = cache
            .store_precomputed(&source, "# md only", None, "test")
            .await
            .unwrap();
        assert!(stored.html_path.is_none());
        assert!(!stored.meta.has_html);
        let hit = cache.lookup(&source).await.unwrap().unwrap();
        assert!(hit.html_path.is_none());
    }

    #[tokio::test]
    async fn list_all_returns_every_cached_meta() {
        let (cache, _tmp) = make_cache();
        let src_dir = TempDir::new().unwrap();
        for name in ["a.pdf", "b.docx", "c.hwpx"] {
            let p = src_dir.path().join(name);
            tokio::fs::write(&p, b"x").await.unwrap();
            cache
                .store_precomputed(&p, "# x", None, "test")
                .await
                .unwrap();
        }

        let listed = cache.list_all().await.unwrap();
        assert_eq!(listed.len(), 3);
        let mut names: Vec<_> = listed.iter().map(|m| m.original_filename.clone()).collect();
        names.sort();
        assert_eq!(names, vec!["a.pdf", "b.docx", "c.hwpx"]);
    }

    #[tokio::test]
    async fn list_all_returns_empty_when_cache_root_empty() {
        let (cache, _tmp) = make_cache();
        let listed = cache.list_all().await.unwrap();
        assert!(listed.is_empty());
    }

    #[tokio::test]
    async fn engine_label_picks_pdf_branch() {
        assert_eq!(
            guess_engine_label(Path::new("/x/y.pdf")),
            "document_pipeline (pdf)"
        );
        assert_eq!(
            guess_engine_label(Path::new("/x/y.HWPX")),
            "document_pipeline (hancom)"
        );
        assert_eq!(
            guess_engine_label(Path::new("/x/y.unknown")),
            "document_pipeline"
        );
    }
}
