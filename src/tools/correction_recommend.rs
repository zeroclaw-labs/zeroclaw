//! correction_recommend tool — scan a document and return learned correction suggestions.
//!
//! Exposes the self-learning correction system to the agent. The agent can use
//! this to proactively propose corrections based on patterns mined from the
//! user's own prior edits.

use super::traits::{Tool, ToolResult};
use crate::skills::correction::{scan_and_recommend, CorrectionStore};
use async_trait::async_trait;
use serde_json::json;
use std::fmt::Write;
use std::sync::Arc;

pub struct CorrectionRecommendTool {
    store: Arc<CorrectionStore>,
}

impl CorrectionRecommendTool {
    pub fn new(store: Arc<CorrectionStore>) -> Self {
        Self { store }
    }
}

#[async_trait]
impl Tool for CorrectionRecommendTool {
    fn name(&self) -> &str {
        "correction_recommend"
    }

    fn description(&self) -> &str {
        "Scan a document using learned correction patterns and return suggestions \
         based on the user's own prior editing history. Only patterns with confidence ≥ 0.7 \
         produce recommendations. Returns pattern type, location, original, suggested."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "document": {
                    "type": "string",
                    "description": "Document text to scan for correction opportunities"
                },
                "doc_type": {
                    "type": "string",
                    "description": "Document type scope (legal_brief, email, code, or 'all')"
                },
                "min_confidence": {
                    "type": "number",
                    "description": "Minimum pattern confidence (default: 0.7)"
                }
            },
            "required": ["document", "doc_type"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let document = args
            .get("document")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'document' parameter"))?;
        let doc_type = args
            .get("doc_type")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'doc_type' parameter"))?;
        let min_confidence = args.get("min_confidence").and_then(serde_json::Value::as_f64);

        let recs = scan_and_recommend(&self.store, document, doc_type, min_confidence)?;

        if recs.is_empty() {
            return Ok(ToolResult {
                success: true,
                output: "No learned patterns matched this document.".into(),
                error: None,
            });
        }

        let mut out = format!("Found {} learned correction suggestions:\n\n", recs.len());
        for (i, r) in recs.iter().enumerate() {
            let _ = writeln!(
                out,
                "{}. [{:?}] \"{}\" → \"{}\" (confidence: {:.0}%, pos: {}, based on {} observations)",
                i + 1,
                r.pattern_type,
                r.original,
                r.suggested,
                r.confidence * 100.0,
                r.location_start,
                r.observation_count
            );
        }

        Ok(ToolResult {
            success: true,
            output: out,
            error: None,
        })
    }
}
