use crate::strategy::FileMeta;
use anyhow::{Context, Result};
use glob::Pattern;
use std::path::Path;
use std::time::SystemTime;

pub struct DirMonitor;

impl DirMonitor {
    /// Enumerate all matching files under a directory.
    pub fn enumerate_files(dir: &Path, pattern: Option<&Pattern>) -> Result<Vec<FileMeta>> {
        if !dir.exists() {
            return Ok(vec![]);
        }

        let mut files = Vec::new();
        for entry in std::fs::read_dir(dir)
            .with_context(|| format!("Failed to read directory: {}", dir.display()))?
        {
            let entry = entry?;
            let path = entry.path();

            // Only handle files; skip subdirectories.
            if !path.is_file() {
                continue;
            }

            // Check whether the file name matches the glob pattern.
            if let Some(pat) = pattern {
                let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if !pat.matches(file_name) {
                    continue;
                }
            }

            // Load file metadata.
            let metadata = path.metadata()?;
            let mtime = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            let size_bytes = metadata.len();

            files.push(FileMeta {
                path,
                mtime,
                size_bytes,
            });
        }

        Ok(files)
    }

    /// Compute the total size of matching files in a directory.
    pub fn calculate_size(dir: &Path, pattern: Option<&Pattern>) -> Result<u64> {
        let files = Self::enumerate_files(dir, pattern)?;
        Ok(files.iter().map(|f| f.size_bytes).sum())
    }
}
