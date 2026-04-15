// MoA Vault — Second Brain (v6)
// @Ref: .planning/vault-v6/SUMMARY.md
//
// Second-brain layer storing reference knowledge (documents) alongside
// the first brain (episodic memories + ontology). Pipeline:
// chat paste / local file → convert → 7-step wikilink extraction →
// vault_documents + links + tags + aliases + FTS5 index →
// delta-journal sync to peers → unified parallel search with first brain.

pub mod ingest;
pub mod schema;
pub mod store;
pub mod unified_search;
pub mod wikilink;

pub use ingest::{IngestInput, IngestOutput, SourceType};
pub use store::VaultStore;
pub use unified_search::{unified_search, SearchScope, UnifiedHit};
pub use wikilink::{
    AIEngine, CompoundToken, CompoundTokenKind, GatekeepVerdict, HeuristicAIEngine, KeyConcept,
    LinkRecord, WikilinkPipeline,
};

/// Minimum chars for chat-paste ingestion into the vault (§4 of spec).
/// Shorter inputs are treated as transient chat, not second-brain material.
pub const DOCUMENT_MIN_CHARS: usize = 2000;
