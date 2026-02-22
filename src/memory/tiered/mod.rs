//! Three-tier memory system: STM (short-term), MTM (medium-term), LTM (long-term).

pub mod types;

#[allow(unused_imports)]
pub use types::{
    CompressionJob, CompressionJobKind, CompressionJobStatus, IndexEntry, MemoryTier, TierCommand,
    TierConfig,
};
