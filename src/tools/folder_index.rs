//! `folder_index` tool — recursively convert and cache every supported
//! document inside a linked folder so the LLM can read & search them.
//!
//! # Why
//!
//! Most user files are PDF / HWP / DOC / XLS / PPT — formats no LLM can
//! read directly. This tool walks a directory the user has just granted
//! access to (via `workspace_folder_link` / `grant_folder_access`) and
//! pushes every supported document through the existing
//! [`DocumentPipelineTool`], persisting the Markdown + HTML output via
//! [`DocumentCache`]. After it runs, every document inside the folder
//! is searchable by `content_search` and accessible by `file_read`
//! via its cache path.
//!
//! # Idempotency
//!
//! `DocumentCache::convert_and_cache` already short-circuits when the
//! source mtime + size match a previous run, so re-running this tool on
//! the same folder is cheap — it only converts files added or modified
//! since the last pass.
//!
//! # Cost awareness
//!
//! Image PDFs route through the paid Upstage OCR pipeline (2.2× credit
//! billing). To avoid surprise charges this tool defaults to
//! `skip_image_pdfs = true`. When the agent wants to process them too,
//! it must explicitly pass `skip_image_pdfs: false` after asking the user.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::security::SecurityPolicy;
use crate::services::document_cache::DocumentCache;
use crate::tools::document_pipeline::DocumentPipelineTool;
use crate::tools::traits::{Tool, ToolResult};

/// File extensions the document pipeline can convert.
const SUPPORTED_EXTENSIONS: &[&str] = &[
    "pdf", "hwp", "hwpx", "doc", "docx", "xls", "xlsx", "ppt", "pptx",
];

/// Maximum number of files this tool will convert in a single call.
/// Prevents accidentally indexing the user's entire `Downloads/` folder
/// in one shot. The agent can call the tool again to continue.
const DEFAULT_MAX_FILES: usize = 50;

/// Maximum recursion depth into subdirectories.
const DEFAULT_MAX_DEPTH: usize = 4;

#[derive(Debug, Deserialize)]
struct Args {
    folder: String,
    #[serde(default = "default_max_files")]
    max_files: usize,
    #[serde(default = "default_max_depth")]
    max_depth: usize,
    #[serde(default = "default_skip_image_pdfs")]
    skip_image_pdfs: bool,
}

fn default_max_files() -> usize {
    DEFAULT_MAX_FILES
}
fn default_max_depth() -> usize {
    DEFAULT_MAX_DEPTH
}
fn default_skip_image_pdfs() -> bool {
    true
}

#[derive(Debug, Serialize)]
struct ConvertedReport {
    folder: String,
    workspace_dir: String,
    converted: usize,
    cached: usize,
    skipped_unsupported: usize,
    skipped_image_pdf: usize,
    failed: Vec<FailureReport>,
    truncated: bool,
    cache_root: String,
}

#[derive(Debug, Serialize)]
struct FailureReport {
    path: String,
    error: String,
}

/// Recursive directory walker that yields supported document paths in
/// stable order. Uses a manual stack instead of `walkdir` to avoid pulling
/// in another dependency just for this tool.
fn walk_supported(
    root: &Path,
    max_depth: usize,
) -> std::io::Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    let mut stack: Vec<(PathBuf, usize)> = vec![(root.to_path_buf(), 0)];
    while let Some((dir, depth)) = stack.pop() {
        let read = match std::fs::read_dir(&dir) {
            Ok(r) => r,
            Err(_) => continue,
        };
        let mut subdirs = Vec::new();
        let mut files = Vec::new();
        for entry in read.flatten() {
            let path = entry.path();
            // Skip hidden entries (`.git`, `.DS_Store`, ...) and common
            // dependency caches that should never be indexed.
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                if name.starts_with('.') {
                    continue;
                }
                if matches!(name, "node_modules" | "target" | "venv" | ".venv") {
                    continue;
                }
            }
            let ft = match entry.file_type() {
                Ok(t) => t,
                Err(_) => continue,
            };
            if ft.is_dir() {
                if depth + 1 < max_depth {
                    subdirs.push(path);
                }
            } else if ft.is_file() {
                if let Some(ext) = path
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(|e| e.to_ascii_lowercase())
                {
                    if SUPPORTED_EXTENSIONS.contains(&ext.as_str()) {
                        files.push(path);
                    }
                }
            }
        }
        files.sort();
        subdirs.sort();
        out.extend(files);
        // Push subdirs in reverse so the pop order matches sorted order.
        for d in subdirs.into_iter().rev() {
            stack.push((d, depth + 1));
        }
    }
    Ok(out)
}

/// LLM-callable tool that batch-converts a folder.
pub struct FolderIndexTool {
    workspace_dir: PathBuf,
    security: Arc<SecurityPolicy>,
}

impl FolderIndexTool {
    pub fn new(workspace_dir: PathBuf, security: Arc<SecurityPolicy>) -> Self {
        Self {
            workspace_dir,
            security,
        }
    }
}

#[async_trait]
impl Tool for FolderIndexTool {
    fn name(&self) -> &str {
        "folder_index"
    }

    fn description(&self) -> &str {
        "Recursively convert every supported document (PDF, HWP/HWPX, DOC/DOCX, \
         XLS/XLSX, PPT/PPTX) inside a folder into Markdown + HTML and persist \
         them to the document cache so the LLM can read and search them later. \
         Idempotent: re-runs only convert files added or modified since the \
         last pass. Image-PDF OCR is opt-in via `skip_image_pdfs: false` \
         because it costs credits (Upstage OCR, 2.2× billing). Use this \
         immediately after `workspace_folder_link` to make a folder \
         searchable. Default limits: 50 files per call, 4 levels deep."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "folder": {
                    "type": "string",
                    "description": "Absolute path to the folder to index. Must be inside an allowed workspace root."
                },
                "max_files": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 500,
                    "description": "Maximum number of files to convert in this call (default 50)."
                },
                "max_depth": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 10,
                    "description": "Maximum directory recursion depth (default 4)."
                },
                "skip_image_pdfs": {
                    "type": "boolean",
                    "description": "If true (default), image PDFs are skipped to avoid Upstage OCR credit charges."
                }
            },
            "required": ["folder"]
        })
    }

    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        let parsed: Args = serde_json::from_value(args)
            .map_err(|e| anyhow::anyhow!("invalid folder_index arguments: {e}"))?;

        let folder = PathBuf::from(parsed.folder.trim());
        if folder.as_os_str().is_empty() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("folder must not be empty".into()),
            });
        }
        if !folder.is_absolute() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "folder path must be absolute (got '{}')",
                    folder.display()
                )),
            });
        }
        if !folder.exists() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("folder does not exist: {}", folder.display())),
            });
        }
        // Enforce the same allowlist as file_read / content_search:
        // the folder must already be inside an `allowed_root` (which is
        // how `workspace_folder_link` granted access in the first place).
        let canonical = folder.canonicalize().unwrap_or_else(|_| folder.clone());
        if !self.security.is_resolved_path_allowed(&canonical) {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "folder is not in any allowed workspace root: {}. \
                     Call workspace_folder_link first.",
                    folder.display()
                )),
            });
        }

        let max_files = parsed.max_files.clamp(1, 500);
        let max_depth = parsed.max_depth.clamp(1, 10);

        let candidates = walk_supported(&folder, max_depth)
            .map_err(|e| anyhow::anyhow!("walk failed: {e}"))?;

        let truncated = candidates.len() > max_files;
        let to_process: Vec<PathBuf> = candidates.into_iter().take(max_files).collect();

        let cache = DocumentCache::new(&self.workspace_dir)
            .map_err(|e| anyhow::anyhow!("init document cache: {e}"))?;
        let pipeline = DocumentPipelineTool::new((*self.security).clone());

        let mut converted = 0usize;
        let mut cached_hits = 0usize;
        let mut skipped_image_pdf = 0usize;
        let skipped_unsupported = 0usize;
        let mut failures: Vec<FailureReport> = Vec::new();

        for path in &to_process {
            // Cheap stale-check: if a fresh entry already exists, count
            // it as a cache hit and move on.
            match cache.lookup(path).await {
                Ok(Some(_)) => {
                    cached_hits += 1;
                    continue;
                }
                Ok(None) => {}
                Err(e) => {
                    failures.push(FailureReport {
                        path: path.to_string_lossy().into_owned(),
                        error: format!("cache lookup failed: {e}"),
                    });
                    continue;
                }
            }

            // Optionally skip image PDFs to avoid surprise OCR charges.
            // We delegate the classification to the existing pipeline tool
            // so the rules stay in one place.
            if parsed.skip_image_pdfs
                && path
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(|e| e.eq_ignore_ascii_case("pdf"))
                    .unwrap_or(false)
            {
                let classify = pipeline
                    .execute(json!({
                        "file_path": path.to_string_lossy(),
                        "classify_only": true,
                    }))
                    .await;
                if let Ok(result) = classify {
                    if result.success {
                        if let Ok(report) = serde_json::from_str::<Value>(&result.output) {
                            if report.get("doc_type").and_then(|v| v.as_str())
                                == Some("image_pdf")
                            {
                                skipped_image_pdf += 1;
                                continue;
                            }
                        }
                    }
                }
            }

            match cache.convert_and_cache(path, &pipeline).await {
                Ok(_) => converted += 1,
                Err(e) => failures.push(FailureReport {
                    path: path.to_string_lossy().into_owned(),
                    error: e.to_string(),
                }),
            }
        }

        let report = ConvertedReport {
            folder: folder.to_string_lossy().into_owned(),
            workspace_dir: self.workspace_dir.to_string_lossy().into_owned(),
            converted,
            cached: cached_hits,
            skipped_unsupported,
            skipped_image_pdf,
            failed: failures,
            truncated,
            cache_root: cache.root().to_string_lossy().into_owned(),
        };

        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&report).unwrap_or_default(),
            error: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn walk_collects_supported_files_only() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("a.pdf"), b"x").unwrap();
        std::fs::write(tmp.path().join("b.txt"), b"x").unwrap();
        std::fs::write(tmp.path().join("c.docx"), b"x").unwrap();
        std::fs::create_dir_all(tmp.path().join("sub")).unwrap();
        std::fs::write(tmp.path().join("sub/d.hwpx"), b"x").unwrap();
        std::fs::write(tmp.path().join("sub/e.unknown"), b"x").unwrap();

        let files = walk_supported(tmp.path(), 4).unwrap();
        let names: Vec<_> = files
            .iter()
            .filter_map(|p| p.file_name())
            .filter_map(|n| n.to_str())
            .map(|s| s.to_string())
            .collect();
        assert_eq!(names.len(), 3);
        assert!(names.contains(&"a.pdf".to_string()));
        assert!(names.contains(&"c.docx".to_string()));
        assert!(names.contains(&"d.hwpx".to_string()));
    }

    #[test]
    fn walk_skips_hidden_and_common_caches() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("real.pdf"), b"x").unwrap();
        std::fs::create_dir_all(tmp.path().join(".git")).unwrap();
        std::fs::write(tmp.path().join(".git/foo.pdf"), b"x").unwrap();
        std::fs::create_dir_all(tmp.path().join("node_modules")).unwrap();
        std::fs::write(tmp.path().join("node_modules/bar.pdf"), b"x").unwrap();
        std::fs::create_dir_all(tmp.path().join("target")).unwrap();
        std::fs::write(tmp.path().join("target/baz.pdf"), b"x").unwrap();

        let files = walk_supported(tmp.path(), 4).unwrap();
        assert_eq!(files.len(), 1);
        assert!(files[0].ends_with("real.pdf"));
    }

    #[test]
    fn walk_respects_max_depth() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("top.pdf"), b"x").unwrap();
        std::fs::create_dir_all(tmp.path().join("a/b/c")).unwrap();
        std::fs::write(tmp.path().join("a/inner.pdf"), b"x").unwrap();
        std::fs::write(tmp.path().join("a/b/deep.pdf"), b"x").unwrap();
        std::fs::write(tmp.path().join("a/b/c/deepest.pdf"), b"x").unwrap();

        // depth=1 → only top-level
        let d1 = walk_supported(tmp.path(), 1).unwrap();
        assert_eq!(d1.len(), 1);
        // depth=2 → top + a/inner
        let d2 = walk_supported(tmp.path(), 2).unwrap();
        assert_eq!(d2.len(), 2);
        // depth=3 → top + a/inner + a/b/deep
        let d3 = walk_supported(tmp.path(), 3).unwrap();
        assert_eq!(d3.len(), 3);
        // depth=4 → all
        let d4 = walk_supported(tmp.path(), 4).unwrap();
        assert_eq!(d4.len(), 4);
    }

    #[tokio::test]
    async fn execute_rejects_relative_path() {
        let tool = FolderIndexTool::new(
            std::env::temp_dir(),
            Arc::new(SecurityPolicy::default()),
        );
        let result = tool
            .execute(json!({ "folder": "relative/path" }))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("absolute"));
    }

    #[tokio::test]
    async fn execute_rejects_missing_folder() {
        let tool = FolderIndexTool::new(
            std::env::temp_dir(),
            Arc::new(SecurityPolicy::default()),
        );
        let result = tool
            .execute(json!({ "folder": "/definitely/not/here/12345" }))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("does not exist"));
    }

    #[tokio::test]
    async fn execute_rejects_folder_outside_allowed_roots() {
        // Construct a workspace dir that does NOT contain the target folder.
        let workspace = TempDir::new().unwrap();
        let other = TempDir::new().unwrap();
        let policy = SecurityPolicy::default();
        // Constrain the policy to its (default) workspace_dir so the
        // unrelated `other` directory is correctly rejected.
        let tool = FolderIndexTool::new(workspace.path().to_path_buf(), Arc::new(policy));
        let result = tool
            .execute(json!({
                "folder": other.path().canonicalize().unwrap().to_string_lossy(),
            }))
            .await
            .unwrap();
        assert!(!result.success);
        let err = result.error.unwrap();
        assert!(
            err.contains("workspace_folder_link") || err.contains("not in any allowed"),
            "unexpected error: {err}"
        );
    }
}
