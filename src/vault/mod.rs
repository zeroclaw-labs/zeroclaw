// MoA Vault — Second Brain (v6)
// @Ref: .planning/vault-v6/SUMMARY.md
//
// Second-brain layer storing reference knowledge (documents) alongside
// the first brain (episodic memories + ontology). Pipeline:
// chat paste / local file → convert → 7-step wikilink extraction →
// vault_documents + links + tags + aliases + FTS5 index →
// delta-journal sync to peers → unified parallel search with first brain.

pub mod briefing;
pub mod converter;
pub mod health;
pub mod hub;
pub mod ingest;
pub mod llm_engine;
pub mod schema;
pub mod scheduler;
pub mod slm_engine;
pub mod store;
pub mod unified_search;
pub mod watcher;
pub mod wikilink;

pub use ingest::{IngestInput, IngestOutput, SourceType};
pub use store::VaultStore;
pub use unified_search::{unified_search, SearchScope, UnifiedHit};
pub use wikilink::{
    AIEngine, CompoundToken, CompoundTokenKind, GatekeepVerdict, HeuristicAIEngine, KeyConcept,
    LinkRecord, WikilinkPipeline,
};

/// Quantitative threshold: chat-paste ≥ this auto-ingests.
pub const DOCUMENT_MIN_CHARS: usize = 2000;

/// Qualitative lower bound: texts below this are never ingested, regardless
/// of AI classification — too short to carry standalone knowledge.
pub const DOCUMENT_QUALITATIVE_MIN_CHARS: usize = 200;
