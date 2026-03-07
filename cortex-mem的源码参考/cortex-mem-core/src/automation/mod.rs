mod auto_extract;
mod indexer;
mod layer_generator;
mod manager;
mod sync;
mod watcher;

#[cfg(test)]
#[path = "layer_generator_tests.rs"]
mod layer_generator_tests;

pub use auto_extract::{AutoExtractConfig, AutoExtractStats, AutoExtractor};
pub use indexer::{AutoIndexer, IndexStats, IndexerConfig};
pub use layer_generator::{
    AbstractConfig, GenerationStats, LayerGenerationConfig, LayerGenerator, OverviewConfig,
    RegenerationStats,
};
pub use manager::{AutomationConfig, AutomationManager};
pub use sync::{SyncConfig, SyncManager, SyncStats};
pub use watcher::{FsEvent, FsWatcher, WatcherConfig};
