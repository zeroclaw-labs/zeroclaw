// @Ref: SUMMARY §4 — ingestion input/output types.

use serde::{Deserialize, Serialize};

/// What brought this document into the vault.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SourceType {
    /// Watched local folder file (hwp/docx/pdf/…).
    LocalFile,
    /// File the user uploaded in chat.
    ChatUpload,
    /// Plain text the user pasted in chat (≥2000 chars).
    ChatPaste,
}

impl SourceType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::LocalFile => "local_file",
            Self::ChatUpload => "chat_upload",
            Self::ChatPaste => "chat_paste",
        }
    }
}

#[derive(Debug, Clone)]
pub struct IngestInput<'a> {
    pub source_type: SourceType,
    pub source_device_id: &'a str,
    pub original_path: Option<&'a str>,
    pub title: Option<&'a str>,
    /// Markdown body (no wikilinks yet; pipeline will add them).
    pub markdown: &'a str,
    /// Optional pre-rendered HTML (dual-format — §1).
    pub html_content: Option<&'a str>,
    pub doc_type: Option<&'a str>,
    /// Domain bucket for boilerplate/vocabulary lookups (e.g. "legal").
    pub domain: &'a str,
}

#[derive(Debug, Clone)]
pub struct IngestOutput {
    pub vault_doc_id: i64,
    pub uuid: String,
    /// True if this checksum was already in the vault (no-op ingest).
    pub already_present: bool,
    /// Number of wikilinks written.
    pub link_count: usize,
    /// Final keyword set after the 7-step pipeline.
    pub keywords: Vec<String>,
}
