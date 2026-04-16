// @Ref: SUMMARY §6D-8 — file watcher + automatic ingest.
//
// Polling-based folder watcher (no external `notify` dep — simple enough
// for a human-authored document vault). Scans the connected folder on
// an interval, detects new/modified files, reads the content, routes
// by extension, and calls `VaultStore::ingest_markdown`.
//
// Extension handling (MVP):
//   .md / .markdown / .txt  →  ingest raw text
//   .hwp / .hwpx / .docx / .pdf  →  delegate to `document_pipeline` tool
//     (left as Phase 5 follow-up; current impl stubs with a warning so
//      the watch loop doesn't crash on an .hwp drop).
//
// State is kept in-memory (PathBuf → SystemTime). The vault itself is
// authoritative via checksum uniqueness — restart is safe even without
// the seen-map: repeated files are detected as `already_present`.

use super::converter::{ConvertOutcome, Converter, NoopConverter};
use super::ingest::{IngestInput, SourceType};
use super::store::VaultStore;
use anyhow::Result;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

/// Default poll interval.
pub const DEFAULT_POLL: Duration = Duration::from_secs(2);

pub struct FolderWatcher {
    root: PathBuf,
    vault: Arc<VaultStore>,
    seen: Mutex<HashMap<PathBuf, SystemTime>>,
    device_id: String,
    domain: String,
    /// Converter chain for non-plaintext files (hwp/docx/pdf/...).
    /// Defaults to `NoopConverter`; callers inject a production chain
    /// via `with_converter`.
    converter: Arc<dyn Converter>,
    /// Output directory for dual-format artifacts (.moa-vault/converted/).
    /// If None, converted output isn't written to disk (still ingested
    /// into SQLite). Set via `with_converted_dir`.
    converted_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ScanStats {
    pub inspected: usize,
    pub newly_ingested: usize,
    pub already_present: usize,
    pub skipped: usize,
    pub errors: usize,
}

impl FolderWatcher {
    pub fn new(
        root: impl Into<PathBuf>,
        vault: Arc<VaultStore>,
        device_id: impl Into<String>,
        domain: impl Into<String>,
    ) -> Self {
        Self {
            root: root.into(),
            vault,
            seen: Mutex::new(HashMap::new()),
            device_id: device_id.into(),
            domain: domain.into(),
            converter: Arc::new(NoopConverter),
            converted_dir: None,
        }
    }

    /// Inject a production converter chain for HWP/DOCX/PDF/... files.
    pub fn with_converter(mut self, converter: Arc<dyn Converter>) -> Self {
        self.converter = converter;
        self
    }

    /// Write converted .md + .html artifacts into this directory
    /// (created if missing). Typically `<root>/.moa-vault/converted/`.
    pub fn with_converted_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.converted_dir = Some(dir.into());
        self
    }

    /// Walk the folder once and ingest any new/modified supported files.
    /// Returns aggregate stats. Safe to call repeatedly.
    pub async fn scan_once(&self) -> Result<ScanStats> {
        let mut stats = ScanStats::default();

        // Collect eligible paths (flat + 1-deep recursion).
        let paths = collect_paths(&self.root, 4)?;
        for path in paths {
            stats.inspected += 1;

            let mtime = match std::fs::metadata(&path).and_then(|m| m.modified()) {
                Ok(m) => m,
                Err(_) => {
                    stats.errors += 1;
                    continue;
                }
            };

            // Already seen and unchanged?
            let changed = {
                let map = self.seen.lock();
                map.get(&path).map(|t| *t != mtime).unwrap_or(true)
            };
            if !changed {
                continue;
            }

            // Route by extension.
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .map(|s| s.to_ascii_lowercase())
                .unwrap_or_default();

            let (markdown, html): (Option<String>, Option<String>) = match ext.as_str() {
                "md" | "markdown" | "txt" => (std::fs::read_to_string(&path).ok(), None),
                _ => match self.converter.convert(&path).await {
                    ConvertOutcome::Ok(c) => {
                        // Persist dual-format artifacts to .moa-vault/converted/
                        // if a directory was configured.
                        if let Some(ref out_dir) = self.converted_dir {
                            if let Err(e) = write_artifacts(out_dir, &path, &c) {
                                tracing::warn!(
                                    path = %path.display(),
                                    "failed to write converted artifacts: {e}"
                                );
                            }
                        }
                        (Some(c.markdown), c.html)
                    }
                    ConvertOutcome::Unsupported => {
                        stats.skipped += 1;
                        (None, None)
                    }
                    ConvertOutcome::Failed(e) => {
                        tracing::warn!(path = %path.display(), "converter failed: {e}");
                        stats.errors += 1;
                        (None, None)
                    }
                },
            };

            if let Some(md) = markdown {
                // No min-char guard for local files — only chat_paste is
                // threshold-gated (enforced inside VaultStore::ingest_markdown).
                let title_guess = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .map(String::from);
                let original_path_str = path.display().to_string();
                match self
                    .vault
                    .ingest_markdown(IngestInput {
                        source_type: SourceType::LocalFile,
                        source_device_id: &self.device_id,
                        original_path: Some(&original_path_str),
                        title: title_guess.as_deref(),
                        markdown: &md,
                        html_content: html.as_deref(),
                        doc_type: Some(ext.as_str()),
                        domain: &self.domain,
                    })
                    .await
                {
                    Ok(out) if out.already_present => stats.already_present += 1,
                    Ok(_) => stats.newly_ingested += 1,
                    Err(e) => {
                        stats.errors += 1;
                        tracing::warn!(path = %path.display(), "vault ingest error: {e}");
                    }
                }
            }

            self.seen.lock().insert(path, mtime);
        }

        Ok(stats)
    }

    /// Run the watcher loop until `shutdown` resolves.
    /// Call this from a background tokio task.
    pub async fn run(
        &self,
        poll_interval: Duration,
        mut shutdown: tokio::sync::oneshot::Receiver<()>,
    ) -> Result<()> {
        let mut ticker = tokio::time::interval(poll_interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = &mut shutdown => {
                    tracing::info!(
                        root = %self.root.display(),
                        "FolderWatcher shutting down"
                    );
                    return Ok(());
                }
                _ = ticker.tick() => {
                    if let Err(e) = self.scan_once().await {
                        tracing::warn!("FolderWatcher scan error: {e}");
                    }
                }
            }
        }
    }
}

fn write_artifacts(out_dir: &Path, src: &Path, c: &crate::vault::converter::Converted) -> Result<()> {
    std::fs::create_dir_all(out_dir)?;
    let stem = src
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("converted");
    let md_path = out_dir.join(format!("{stem}.md"));
    std::fs::write(&md_path, &c.markdown)?;
    if let Some(ref html) = c.html {
        let html_path = out_dir.join(format!("{stem}.html"));
        std::fs::write(&html_path, html)?;
    }
    Ok(())
}

fn collect_paths(root: &Path, max_depth: u32) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    fn walk(dir: &Path, depth: u32, max: u32, out: &mut Vec<PathBuf>) {
        if depth > max {
            return;
        }
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let path = e.path();
            // Skip hidden .moa-vault/ and dotfiles to avoid recursion on our own output.
            if let Some(name) = path.file_name().and_then(|s| s.to_str()) {
                if name.starts_with('.') {
                    continue;
                }
            }
            if path.is_dir() {
                walk(&path, depth + 1, max, out);
            } else {
                out.push(path);
            }
        }
    }
    walk(root, 0, max_depth, &mut out);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use parking_lot::Mutex as PLMutex;
    use rusqlite::Connection;
    use std::sync::Arc;
    use tempfile::TempDir;

    fn setup() -> (TempDir, Arc<VaultStore>) {
        let tmp = TempDir::new().unwrap();
        let conn = Arc::new(PLMutex::new(Connection::open_in_memory().unwrap()));
        let vault = Arc::new(VaultStore::with_shared_connection(conn).unwrap());
        (tmp, vault)
    }

    fn sample_md(tag: &str) -> String {
        format!(
            "# {tag} 테스트 문서\n\n본 문서는 {tag}에 관한 해설이다. 민법 제750조가 쟁점. {}",
            "추가 본문 ".repeat(500) // well over DOCUMENT_MIN_CHARS (2000)
        )
    }

    #[tokio::test]
    async fn scan_once_ingests_markdown_file() {
        let (tmp, vault) = setup();
        std::fs::write(tmp.path().join("case1.md"), sample_md("case1")).unwrap();

        let watcher =
            FolderWatcher::new(tmp.path(), vault.clone(), "dev_a", "legal");
        let stats = watcher.scan_once().await.unwrap();
        assert_eq!(stats.newly_ingested, 1);
        assert_eq!(stats.already_present, 0);
    }

    #[tokio::test]
    async fn scan_once_is_idempotent_on_unchanged_files() {
        let (tmp, vault) = setup();
        std::fs::write(tmp.path().join("case1.md"), sample_md("case1")).unwrap();

        let watcher =
            FolderWatcher::new(tmp.path(), vault.clone(), "dev_a", "legal");
        let s1 = watcher.scan_once().await.unwrap();
        let s2 = watcher.scan_once().await.unwrap();
        assert_eq!(s1.newly_ingested, 1);
        assert_eq!(s2.newly_ingested, 0); // no change → no re-ingest
    }

    #[tokio::test]
    async fn scan_skips_dotfiles_and_hidden_dirs() {
        let (tmp, vault) = setup();
        std::fs::create_dir_all(tmp.path().join(".moa-vault")).unwrap();
        std::fs::write(
            tmp.path().join(".moa-vault").join("noise.md"),
            sample_md("noise"),
        )
        .unwrap();
        std::fs::write(tmp.path().join(".hidden_case.md"), sample_md("hidden")).unwrap();

        let watcher =
            FolderWatcher::new(tmp.path(), vault.clone(), "dev_a", "legal");
        let stats = watcher.scan_once().await.unwrap();
        assert_eq!(stats.newly_ingested, 0);
    }

    #[tokio::test]
    async fn scan_skips_unsupported_extensions_gracefully() {
        let (tmp, vault) = setup();
        std::fs::write(tmp.path().join("file.hwp"), b"binary content").unwrap();
        std::fs::write(tmp.path().join("note.md"), sample_md("note")).unwrap();

        let watcher =
            FolderWatcher::new(tmp.path(), vault.clone(), "dev_a", "legal");
        let stats = watcher.scan_once().await.unwrap();
        assert_eq!(stats.newly_ingested, 1);
        assert!(stats.skipped >= 1);
    }

    /// Stub converter that always returns fixed markdown/html for .docx.
    struct DocxStubConverter;
    #[async_trait::async_trait]
    impl super::super::converter::Converter for DocxStubConverter {
        fn name(&self) -> &'static str {
            "docx_stub"
        }
        async fn convert(
            &self,
            path: &std::path::Path,
        ) -> super::super::converter::ConvertOutcome {
            if path.extension().and_then(|e| e.to_str()) == Some("docx") {
                super::super::converter::ConvertOutcome::Ok(
                    super::super::converter::Converted {
                        markdown: format!(
                            "# 변환 결과\n\n본문 민법 제750조 설명. {body}",
                            body = "본문 ".repeat(500)
                        ),
                        html: Some("<h1>변환</h1>".into()),
                        source_ext: "docx".into(),
                    },
                )
            } else {
                super::super::converter::ConvertOutcome::Unsupported
            }
        }
    }

    #[tokio::test]
    async fn converter_routes_docx_to_ingest() {
        let (tmp, vault) = setup();
        std::fs::write(tmp.path().join("contract.docx"), b"binary").unwrap();
        let out_dir = tmp.path().join(".moa-vault/converted");
        let watcher = FolderWatcher::new(tmp.path(), vault.clone(), "dev", "legal")
            .with_converter(Arc::new(DocxStubConverter))
            .with_converted_dir(&out_dir);
        let stats = watcher.scan_once().await.unwrap();
        assert_eq!(stats.newly_ingested, 1);
        // Dual-format artifacts produced.
        assert!(out_dir.join("contract.md").exists());
        assert!(out_dir.join("contract.html").exists());
    }

    #[tokio::test]
    async fn converter_failure_increments_errors() {
        struct FailingConverter;
        #[async_trait::async_trait]
        impl super::super::converter::Converter for FailingConverter {
            fn name(&self) -> &'static str {
                "fail"
            }
            async fn convert(
                &self,
                _path: &std::path::Path,
            ) -> super::super::converter::ConvertOutcome {
                super::super::converter::ConvertOutcome::Failed(anyhow::anyhow!("boom"))
            }
        }
        let (tmp, vault) = setup();
        std::fs::write(tmp.path().join("x.docx"), b"").unwrap();
        let watcher = FolderWatcher::new(tmp.path(), vault.clone(), "dev", "legal")
            .with_converter(Arc::new(FailingConverter));
        let stats = watcher.scan_once().await.unwrap();
        assert!(stats.errors >= 1);
        assert_eq!(stats.newly_ingested, 0);
    }

    #[tokio::test]
    async fn run_exits_on_shutdown_signal() {
        let (tmp, vault) = setup();
        let watcher =
            FolderWatcher::new(tmp.path(), vault.clone(), "dev_a", "legal");
        let (tx, rx) = tokio::sync::oneshot::channel();
        let handle = tokio::spawn(async move {
            watcher
                .run(Duration::from_millis(50), rx)
                .await
                .unwrap();
        });
        tokio::time::sleep(Duration::from_millis(120)).await;
        tx.send(()).unwrap();
        handle.await.unwrap();
    }
}
