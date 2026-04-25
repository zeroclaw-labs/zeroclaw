/// Configuration for file rotation behaviour.
#[derive(Debug, Clone)]
pub struct RotationConfig {
    /// Maximum size of a single file in bytes before rotation is triggered.
    pub max_file_size_bytes: u64,

    /// Maximum age in days for rotated files. Files older than this are deleted.
    pub max_age_days: u64,

    /// Maximum number of rotated files to keep. Oldest files are deleted first.
    pub max_rotated_files: usize,

    /// Unix file permission applied to newly created files (e.g. `0o600`).
    /// `None` means use the OS default.
    #[cfg(unix)]
    pub file_permissions: Option<u32>,

    /// Whether to call `sync_data()` after each write for durability.
    pub sync_on_write: bool,
}

impl Default for RotationConfig {
    fn default() -> Self {
        Self {
            max_file_size_bytes: 100 * 1024 * 1024, // 100 MB
            max_age_days: 30,
            max_rotated_files: 100,
            #[cfg(unix)]
            file_permissions: None,
            sync_on_write: true,
        }
    }
}
