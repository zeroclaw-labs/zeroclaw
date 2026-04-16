// @Ref: SUMMARY §3 — 7-step wikilink extraction pipeline.

pub mod ai_stub;
pub mod boilerplate;
pub mod cross_validate;
pub mod frequency;
pub mod insert;
pub mod tokens;
pub mod vocabulary;

pub use ai_stub::{
    heuristic_knowledge_classify, AIEngine, ContentClaim, Contradiction, GatekeepVerdict,
    HeuristicAIEngine, KeyConcept, KnowledgeVerdict,
};
pub use tokens::{CompoundToken, CompoundTokenKind};

use anyhow::Result;
use rusqlite::Connection;
use std::collections::HashMap;

/// A single link produced by the pipeline, ready for DB insertion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkRecord {
    /// The canonical target (e.g. "민법 제750조").
    pub target_raw: String,
    /// What the reader sees inline (may differ from target for aliases).
    pub display_text: String,
    /// alias | wikilink | embed | block. P1: wikilink/alias only.
    pub link_type: String,
    /// 80-char context slice centred on the link position (for backlinks UI).
    pub context: String,
    /// 1-based line number in the rewritten markdown.
    pub line_number: u32,
}

/// Output of a pipeline run.
#[derive(Debug, Clone)]
pub struct WikilinkOutput {
    /// Markdown with `[[]]` and `[[rep|alias]]` inserted.
    pub annotated_content: String,
    /// Every link discovered, in document order.
    pub links: Vec<LinkRecord>,
    /// Final key concepts (canonical form) after gatekeeping.
    pub keywords: Vec<String>,
    /// synonym → representative mapping applied.
    pub synonyms: HashMap<String, String>,
}

/// Orchestrates the 7 pipeline steps.
///
/// Thread-safe: holds no mutable state. Call `run()` per document.
pub struct WikilinkPipeline<'a> {
    pub ai: &'a dyn AIEngine,
    pub conn: &'a parking_lot::Mutex<Connection>,
    pub domain: &'a str,
}

impl<'a> WikilinkPipeline<'a> {
    pub fn new(
        ai: &'a dyn AIEngine,
        conn: &'a parking_lot::Mutex<Connection>,
        domain: &'a str,
    ) -> Self {
        Self { ai, conn, domain }
    }

    /// Execute all 7 steps, producing annotated markdown + link records.
    ///
    /// AI calls (Steps 2a, 4) delegate to the injected engine; the default
    /// `HeuristicAIEngine` is provider-free so this runs without network.
    pub async fn run(&self, markdown: &str) -> Result<WikilinkOutput> {
        // Step 0 — compound token recognition (Korean legal regex).
        let compounds = tokens::detect_compound_tokens(markdown);

        // Step 1 — quantitative scoring (TF + heading boost + synonym collapse).
        let synonyms = {
            let conn = self.conn.lock();
            frequency::load_synonyms(&conn)?
        };
        let tf_scores = frequency::quantitative_scores(markdown, &compounds, &synonyms);

        // Step 2a — qualitative AI key concept extraction.
        let ai_concepts = self
            .ai
            .extract_key_concepts(markdown, &compounds)
            .await
            .unwrap_or_default();

        // Step 2b — boilerplate filter.
        let boilerplate_set = {
            let conn = self.conn.lock();
            boilerplate::load_set(&conn, self.domain)?
        };
        let tf_filtered =
            boilerplate::filter_tf(&tf_scores, &boilerplate_set);
        let ai_filtered = boilerplate::filter_ai(&ai_concepts, &boilerplate_set);

        // Step 3 — cross-validation + size-based cap.
        let candidates =
            cross_validate::merge(&tf_filtered, &ai_filtered, markdown.chars().count());

        // Step 4 — AI gatekeeper (synonym pairs returned here).
        let preview = preview_slice(markdown, 1200);
        let verdict = self
            .ai
            .gatekeep(&candidates, &preview)
            .await
            .unwrap_or_else(|_| GatekeepVerdict {
                kept: candidates.clone(),
                synonym_pairs: vec![],
            });

        // Merge persisted synonyms with gatekeeper-proposed ones (non-destructive).
        let mut active_synonyms = synonyms.clone();
        for (rep, alias) in &verdict.synonym_pairs {
            active_synonyms.insert(alias.clone(), rep.clone());
        }

        // Step 5 — insert into markdown.
        let inserted =
            insert::insert_wikilinks(markdown, &verdict.kept, &active_synonyms);

        // Step 6 — vocabulary learning (co_pairs + synonym relations).
        {
            let mut conn = self.conn.lock();
            vocabulary::learn(
                &mut conn,
                &verdict.kept,
                &verdict.synonym_pairs,
                self.domain,
            )?;
        }

        Ok(WikilinkOutput {
            annotated_content: inserted.content,
            links: inserted.links,
            keywords: verdict.kept,
            synonyms: active_synonyms,
        })
    }
}

fn preview_slice(s: &str, max_chars: usize) -> String {
    s.chars().take(max_chars).collect()
}
