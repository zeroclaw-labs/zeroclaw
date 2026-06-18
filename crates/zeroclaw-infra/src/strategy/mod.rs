use std::path::PathBuf;
use std::time::SystemTime;

/// File metadata abstraction used by cleanup strategies and tests.
#[derive(Debug, Clone)]
pub struct FileMeta {
    pub path: PathBuf,
    pub mtime: SystemTime,
    pub size_bytes: u64,
}

/// Strategy configuration shared by cleanup policies.
#[derive(Debug, Clone)]
pub struct StrategyConfig {
    pub retention_hours: u64,
    pub max_size_bytes: u64,
    pub current_dir_size_bytes: u64,
}

/// Cleanup strategy interface.
pub trait CleanupStrategy: Send + Sync {
    /// Returns the files that should be deleted.
    fn find_files_to_delete(&self, files: &[FileMeta], config: &StrategyConfig) -> Vec<PathBuf>;
}

/// Size-limit cleanup strategy.
pub mod space_based;
/// Time-based expiration cleanup strategy.
pub mod time_based;

pub use space_based::SpaceBasedStrategy;
pub use time_based::TimeBasedStrategy;
