// @Ref: SUMMARY §6D-8 — document converter abstraction wired into FolderWatcher.
//
// Converter pipeline:
//   Path(*.hwp|.docx|.pdf|.hwpx|.pptx|.xlsx|.doc|.xls|.ppt)
//     → Converter::convert(path) -> Result<Converted { markdown, html }>
//
// Three impls land here for a production system:
//   - `CliConverter`        — shells out to $PATH tools (pandoc, pdftotext,
//                             hwp5html). Graceful skip if a tool is missing.
//   - `NoopConverter`       — returns `Unsupported` for everything; used in
//                             tests and on devices without any converter.
//   - `MultiConverter`      — tries a chain in order, first non-None wins.
//
// A separate adapter `DocumentPipelineConverter` in the agent/tool layer
// wraps `tools::document_pipeline::DocumentPipelineTool` for runtimes
// that have API keys + SecurityPolicy; it's provided as Arc<dyn Converter>
// so the watcher stays agnostic.

use anyhow::Result;
use async_trait::async_trait;
use std::path::{Path, PathBuf};
use std::process::Stdio;

#[derive(Debug, Clone)]
pub struct Converted {
    pub markdown: String,
    pub html: Option<String>,
    pub source_ext: String,
}

/// Result of a conversion attempt.
#[derive(Debug)]
pub enum ConvertOutcome {
    /// Converted successfully.
    Ok(Converted),
    /// Extension not supported by this converter.
    Unsupported,
    /// Supported but failed (missing tool, malformed file, etc.) — skip
    /// this run; caller decides whether to retry later.
    Failed(anyhow::Error),
}

#[async_trait]
pub trait Converter: Send + Sync {
    fn name(&self) -> &'static str;
    async fn convert(&self, path: &Path) -> ConvertOutcome;
}

// ── NoopConverter ─────────────────────────────────────────────────────

pub struct NoopConverter;

#[async_trait]
impl Converter for NoopConverter {
    fn name(&self) -> &'static str {
        "noop"
    }
    async fn convert(&self, _path: &Path) -> ConvertOutcome {
        ConvertOutcome::Unsupported
    }
}

// ── MultiConverter ─────────────────────────────────────────────────────

pub struct MultiConverter {
    inner: Vec<std::sync::Arc<dyn Converter>>,
}

impl MultiConverter {
    pub fn new(inner: Vec<std::sync::Arc<dyn Converter>>) -> Self {
        Self { inner }
    }
}

#[async_trait]
impl Converter for MultiConverter {
    fn name(&self) -> &'static str {
        "multi"
    }
    async fn convert(&self, path: &Path) -> ConvertOutcome {
        let mut first_failed: Option<anyhow::Error> = None;
        for c in &self.inner {
            match c.convert(path).await {
                ConvertOutcome::Ok(v) => return ConvertOutcome::Ok(v),
                ConvertOutcome::Unsupported => {}
                ConvertOutcome::Failed(e) => {
                    tracing::warn!(
                        converter = c.name(),
                        path = %path.display(),
                        "converter failed, trying next: {e}"
                    );
                    if first_failed.is_none() {
                        first_failed = Some(e);
                    }
                }
            }
        }
        match first_failed {
            Some(e) => ConvertOutcome::Failed(e),
            None => ConvertOutcome::Unsupported,
        }
    }
}

// ── CliConverter ───────────────────────────────────────────────────────
//
// Shells out to locally-installed tools:
//   .docx/.doc/.pptx/.ppt/.xlsx/.xls  → pandoc (to both md + html)
//   .pdf                              → pdftotext (to md only; html absent)
//   .hwp/.hwpx                        → hwp5html (if present)
//
// Any missing tool → Unsupported (MultiConverter chains to next).

pub struct CliConverter {
    pub pandoc_bin: Option<PathBuf>,
    pub pdftotext_bin: Option<PathBuf>,
    pub hwp5html_bin: Option<PathBuf>,
}

impl CliConverter {
    /// Probe $PATH for supported binaries at construction. Absent tools
    /// simply won't be used; conversion just returns `Unsupported` for
    /// their formats.
    pub fn detect() -> Self {
        Self {
            pandoc_bin: which_bin("pandoc"),
            pdftotext_bin: which_bin("pdftotext"),
            hwp5html_bin: which_bin("hwp5html"),
        }
    }

    /// Allow tests / embedders to pin explicit paths.
    pub fn with_bins(
        pandoc: Option<PathBuf>,
        pdftotext: Option<PathBuf>,
        hwp5html: Option<PathBuf>,
    ) -> Self {
        Self {
            pandoc_bin: pandoc,
            pdftotext_bin: pdftotext,
            hwp5html_bin: hwp5html,
        }
    }
}

#[async_trait]
impl Converter for CliConverter {
    fn name(&self) -> &'static str {
        "cli"
    }

    async fn convert(&self, path: &Path) -> ConvertOutcome {
        let ext = match path
            .extension()
            .and_then(|e| e.to_str())
            .map(|s| s.to_ascii_lowercase())
        {
            Some(e) => e,
            None => return ConvertOutcome::Unsupported,
        };

        match ext.as_str() {
            "docx" | "doc" | "pptx" | "ppt" | "xlsx" | "xls" => {
                let Some(ref bin) = self.pandoc_bin else {
                    return ConvertOutcome::Unsupported;
                };
                run_pandoc(bin, path, &ext).await
            }
            "pdf" => {
                let Some(ref bin) = self.pdftotext_bin else {
                    return ConvertOutcome::Unsupported;
                };
                run_pdftotext(bin, path, &ext).await
            }
            "hwp" | "hwpx" => {
                let Some(ref bin) = self.hwp5html_bin else {
                    return ConvertOutcome::Unsupported;
                };
                run_hwp5html(bin, path, &ext).await
            }
            _ => ConvertOutcome::Unsupported,
        }
    }
}

async fn run_pandoc(bin: &Path, src: &Path, ext: &str) -> ConvertOutcome {
    // pandoc can emit markdown + html; run twice.
    let md = match run_with_output(bin, &["-f", &pandoc_from(ext), "-t", "gfm", src.to_str().unwrap_or_default()]).await {
        Ok(s) => s,
        Err(e) => return ConvertOutcome::Failed(e),
    };
    let html =
        match run_with_output(bin, &["-f", &pandoc_from(ext), "-t", "html", src.to_str().unwrap_or_default()]).await {
            Ok(s) => Some(s),
            Err(e) => {
                tracing::warn!("pandoc HTML conversion failed: {e}");
                None
            }
        };
    if md.trim().is_empty() {
        return ConvertOutcome::Failed(anyhow::anyhow!("pandoc produced empty markdown"));
    }
    ConvertOutcome::Ok(Converted {
        markdown: md,
        html,
        source_ext: ext.to_string(),
    })
}

fn pandoc_from(ext: &str) -> String {
    match ext {
        "docx" | "doc" => "docx".into(),
        "pptx" | "ppt" => "pptx".into(),
        "xlsx" | "xls" => "xlsx".into(),
        other => other.to_string(),
    }
}

async fn run_pdftotext(bin: &Path, src: &Path, ext: &str) -> ConvertOutcome {
    // pdftotext SRC - → write text to stdout with "-" target.
    let md = match run_with_output(
        bin,
        &["-layout", "-enc", "UTF-8", src.to_str().unwrap_or_default(), "-"],
    )
    .await
    {
        Ok(s) => s,
        Err(e) => return ConvertOutcome::Failed(e),
    };
    if md.trim().is_empty() {
        return ConvertOutcome::Failed(anyhow::anyhow!("pdftotext produced empty output"));
    }
    ConvertOutcome::Ok(Converted {
        markdown: md,
        html: None,
        source_ext: ext.to_string(),
    })
}

async fn run_hwp5html(bin: &Path, src: &Path, ext: &str) -> ConvertOutcome {
    // hwp5html writes an HTML to --output-dir. We use a tempdir.
    let td = match tempfile::tempdir() {
        Ok(d) => d,
        Err(e) => return ConvertOutcome::Failed(e.into()),
    };
    let args = &[
        "--output",
        td.path().to_str().unwrap_or_default(),
        src.to_str().unwrap_or_default(),
    ];
    if let Err(e) = run_with_output(bin, args).await {
        return ConvertOutcome::Failed(e);
    }

    // Read back the produced HTML (index.html in outdir).
    let html_path = td.path().join("index.xhtml");
    let html_path = if html_path.exists() {
        html_path
    } else {
        td.path().join("index.html")
    };
    let html = match std::fs::read_to_string(&html_path) {
        Ok(s) => s,
        Err(e) => {
            return ConvertOutcome::Failed(anyhow::anyhow!("hwp5html output missing: {e}"))
        }
    };
    // Lightweight HTML → Markdown: strip tags but keep paragraph breaks.
    let md = crate::tools::document_pipeline::html_to_markdown_public(&html);
    ConvertOutcome::Ok(Converted {
        markdown: md,
        html: Some(html),
        source_ext: ext.to_string(),
    })
}

async fn run_with_output(bin: &Path, args: &[&str]) -> Result<String> {
    use tokio::process::Command;
    let out = Command::new(bin)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await?;
    if !out.status.success() {
        anyhow::bail!(
            "{} failed (status={:?}): {}",
            bin.display(),
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn which_bin(name: &str) -> Option<PathBuf> {
    let Ok(path_env) = std::env::var("PATH") else {
        return None;
    };
    for dir in path_env.split(':') {
        let p = Path::new(dir).join(name);
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tempfile::TempDir;

    #[tokio::test]
    async fn noop_returns_unsupported() {
        let c = NoopConverter;
        let tmp = TempDir::new().unwrap();
        let p = tmp.path().join("x.docx");
        std::fs::write(&p, b"irrelevant").unwrap();
        assert!(matches!(c.convert(&p).await, ConvertOutcome::Unsupported));
    }

    #[tokio::test]
    async fn cli_converter_gracefully_skips_when_tools_missing() {
        // Construct CliConverter with no bins → every extension is Unsupported.
        let c = CliConverter::with_bins(None, None, None);
        let tmp = TempDir::new().unwrap();
        for ext in ["docx", "pdf", "hwp"] {
            let p = tmp.path().join(format!("x.{ext}"));
            std::fs::write(&p, b"x").unwrap();
            assert!(
                matches!(c.convert(&p).await, ConvertOutcome::Unsupported),
                "ext {ext} should be Unsupported when bins absent"
            );
        }
    }

    struct FakeConverter {
        target_ext: &'static str,
        md: &'static str,
    }
    #[async_trait]
    impl Converter for FakeConverter {
        fn name(&self) -> &'static str {
            "fake"
        }
        async fn convert(&self, path: &Path) -> ConvertOutcome {
            if path.extension().and_then(|e| e.to_str()) == Some(self.target_ext) {
                ConvertOutcome::Ok(Converted {
                    markdown: self.md.into(),
                    html: Some(format!("<p>{}</p>", self.md)),
                    source_ext: self.target_ext.into(),
                })
            } else {
                ConvertOutcome::Unsupported
            }
        }
    }

    #[tokio::test]
    async fn multi_converter_falls_through_to_first_supporting() {
        let a: Arc<dyn Converter> = Arc::new(FakeConverter {
            target_ext: "docx",
            md: "A",
        });
        let b: Arc<dyn Converter> = Arc::new(FakeConverter {
            target_ext: "pdf",
            md: "B",
        });
        let multi = MultiConverter::new(vec![a, b]);
        let tmp = TempDir::new().unwrap();
        let p = tmp.path().join("x.pdf");
        std::fs::write(&p, b"").unwrap();
        match multi.convert(&p).await {
            ConvertOutcome::Ok(c) => assert_eq!(c.markdown, "B"),
            other => panic!("expected Ok, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn multi_returns_unsupported_when_no_child_handles() {
        let a: Arc<dyn Converter> = Arc::new(NoopConverter);
        let multi = MultiConverter::new(vec![a]);
        let tmp = TempDir::new().unwrap();
        let p = tmp.path().join("x.xyz");
        std::fs::write(&p, b"").unwrap();
        assert!(matches!(multi.convert(&p).await, ConvertOutcome::Unsupported));
    }
}
