use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum FileStatus {
    Added,
    Deleted,
    Modified,
}

/// A (tree-hash, files) pair produced by [`super::shadow::ShadowSnapshot::patch`].
/// Files are absolute paths in the worktree with forward-slash separators.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Patch {
    pub hash: String,
    pub files: Vec<PathBuf>,
}

/// Per-file statistics + optional inline unified diff returned by
/// [`super::shadow::ShadowSnapshot::diff_full`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileDiff {
    pub file: String,
    pub additions: u32,
    pub deletions: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<FileStatus>,
    /// Unified-diff text; empty for binary files.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub patch: String,
}
