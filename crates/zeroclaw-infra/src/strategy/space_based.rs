use super::{CleanupStrategy, FileMeta, StrategyConfig};
use std::path::PathBuf;

pub struct SpaceBasedStrategy;

impl CleanupStrategy for SpaceBasedStrategy {
    fn find_files_to_delete(&self, files: &[FileMeta], config: &StrategyConfig) -> Vec<PathBuf> {
        if config.max_size_bytes == 0 || config.current_dir_size_bytes <= config.max_size_bytes {
            return vec![];
        }

        // Sort by modification time from oldest to newest.
        let mut sorted: Vec<&FileMeta> = files.iter().collect();
        sorted.sort_by_key(|f| f.mtime);

        let mut current_size = sorted.iter().map(|f| f.size_bytes).sum::<u64>();
        let mut to_delete = Vec::new();

        for file in sorted {
            if current_size <= config.max_size_bytes {
                break;
            }
            current_size -= file.size_bytes;
            to_delete.push(file.path.clone());
        }

        to_delete
    }
}
