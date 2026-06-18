use super::{CleanupStrategy, FileMeta, StrategyConfig};
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

pub struct TimeBasedStrategy;

impl CleanupStrategy for TimeBasedStrategy {
    fn find_files_to_delete(&self, files: &[FileMeta], config: &StrategyConfig) -> Vec<PathBuf> {
        if config.retention_hours == 0 {
            return vec![];
        }

        let cutoff = SystemTime::now() - Duration::from_secs(config.retention_hours * 3600);
        files
            .iter()
            .filter(|f| f.mtime < cutoff)
            .map(|f| f.path.clone())
            .collect()
    }
}
