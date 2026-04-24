//! Legal-domain extraction + ingestion for the Second Brain (vault).
//!
//! Pipeline:
//!   markdown file → [`extract_statute`] / [`extract_case`] → [`LegalIngestor`] →
//!   `vault_documents` + `vault_links` + `vault_aliases` + `vault_frontmatter` + `vault_tags`
//!
//! Design notes
//! ─────────────
//! * Extraction is **deterministic (regex-based)**. No LLM in the
//!   write path — hallucinated citations in legal data are catastrophic.
//! * Slugs are the canonical `vault_documents.title` so the existing
//!   target-resolution logic (`SELECT id FROM vault_documents WHERE title = ?`)
//!   wires edges automatically once the target node is ingested.
//! * We do NOT extend `vault_links.link_type` (which has a CHECK constraint);
//!   the relation type is carried in `display_text` (`"cites"` /
//!   `"ref-case"` / `"internal-ref"` / `"cross-law"`) and evidence in
//!   `context`.
//! * Bypasses `WikilinkPipeline` — legal citations are not fuzzy wikilinks,
//!   and we want auditable SQL, not AI keyword gatekeeping.

pub mod case_extractor;
pub mod citation_patterns;
pub mod cli;
pub mod date_parse;
pub mod encoding;
pub mod graph_query;
pub mod ingest;
pub mod law_aliases;
pub mod slug;
pub mod statute_extractor;
pub mod vendor;

pub use case_extractor::{extract_case, looks_like_case, CaseDoc};
pub use citation_patterns::{
    extract_case_numbers, extract_statute_citations, CaseRef, StatuteRef,
};
pub use graph_query::{
    find_nodes, get_node, induced_subgraph, neighbors, pick_applicable_version, read_article,
    shortest_path, ApplicableVersion, ArticleContent, Edge, FindHit, Node, NodeKind, Subgraph,
    MAX_NODES,
};
pub use ingest::{ingest_case, ingest_statute, IngestCounts, IngestReport};
pub use slug::{case_slug, statute_slug};
pub use statute_extractor::{
    extract_statute, looks_like_statute, StatuteArticle, StatuteDoc, Supplement,
};
