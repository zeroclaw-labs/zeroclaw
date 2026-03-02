//! Multi-model review pipeline.
//!
//! Orchestrates sequential code review by multiple AI models:
//!
//! 1. **Gemini** reviews first (architecture gatekeeper)
//! 2. **Claude** reviews second, seeing Gemini's findings
//!    (implementation quality + validates/refutes Gemini's points)
//! 3. Pipeline merges findings, deduplicates, and produces consensus
//!
//! This implements the "Claude codes, Gemini reviews, Claude validates"
//! workflow for autonomous coding quality assurance.

use std::collections::HashMap;

use super::reviewers::{ClaudeReviewer, GeminiReviewer};
use super::traits::{
    CodeReviewer, ConsensusReport, ReviewContext, ReviewFinding, ReviewReport, ReviewVerdict,
    Severity,
};

// ── Pipeline configuration ───────────────────────────────────────

/// Configuration for the multi-model review pipeline.
#[derive(Debug, Clone)]
pub struct PipelineConfig {
    /// Gemini API key.
    pub gemini_api_key: Option<String>,
    /// Gemini model to use.
    pub gemini_model: String,
    /// Anthropic API key.
    pub claude_api_key: Option<String>,
    /// Claude model to use.
    pub claude_model: String,
    /// Whether to run Claude as secondary reviewer.
    pub enable_secondary_review: bool,
}

impl Default for PipelineConfig {
    fn default() -> Self {
        Self {
            gemini_api_key: None,
            gemini_model: "gemini-2.5-flash".into(),
            claude_api_key: None,
            claude_model: "claude-sonnet-4-6".into(),
            enable_secondary_review: true,
        }
    }
}

// ── Review pipeline ──────────────────────────────────────────────

/// Multi-model code review pipeline.
///
/// Runs reviewers in sequence so each subsequent reviewer can see
/// prior reviews and validate/refute findings.
pub struct ReviewPipeline {
    reviewers: Vec<Box<dyn CodeReviewer>>,
}

impl ReviewPipeline {
    /// Create a pipeline from configuration.
    ///
    /// Builds the reviewer chain based on available API keys:
    /// - If Gemini key is available → adds Gemini as primary reviewer
    /// - If Claude key is available and secondary review enabled → adds Claude
    /// - If no keys → returns empty pipeline (no reviews will be performed)
    pub fn from_config(config: &PipelineConfig) -> Self {
        let mut reviewers: Vec<Box<dyn CodeReviewer>> = Vec::new();

        // Gemini as primary (architecture gatekeeper)
        if let Some(ref key) = config.gemini_api_key {
            reviewers.push(Box::new(GeminiReviewer::new(
                key.clone(),
                config.gemini_model.clone(),
            )));
        }

        // Claude as secondary (implementation quality + Gemini validation)
        if config.enable_secondary_review {
            if let Some(ref key) = config.claude_api_key {
                reviewers.push(Box::new(ClaudeReviewer::new(
                    key.clone(),
                    config.claude_model.clone(),
                )));
            }
        }

        Self { reviewers }
    }

    /// Create a pipeline with custom reviewers (for testing or extension).
    pub fn with_reviewers(reviewers: Vec<Box<dyn CodeReviewer>>) -> Self {
        Self { reviewers }
    }

    /// Whether the pipeline has any reviewers configured.
    pub fn is_empty(&self) -> bool {
        self.reviewers.is_empty()
    }

    /// Number of configured reviewers.
    pub fn reviewer_count(&self) -> usize {
        self.reviewers.len()
    }

    /// Run the full review pipeline and produce a consensus report.
    ///
    /// Reviewers run sequentially; each reviewer sees the accumulated
    /// reports from prior reviewers in the chain.
    pub async fn run(&self, ctx: &ReviewContext) -> anyhow::Result<ConsensusReport> {
        if self.reviewers.is_empty() {
            return Ok(ConsensusReport {
                reviews: vec![],
                merged_findings: vec![],
                verdict: ReviewVerdict::Comment,
                summary: "No reviewers configured — skipped automated review.".into(),
            });
        }

        let mut reviews: Vec<ReviewReport> = Vec::new();

        for reviewer in &self.reviewers {
            // Build context with accumulated prior reviews
            let review_ctx = ReviewContext {
                diff: ctx.diff.clone(),
                changed_files: ctx.changed_files.clone(),
                architecture_context: ctx.architecture_context.clone(),
                title: ctx.title.clone(),
                description: ctx.description.clone(),
                prior_reviews: reviews.clone(),
            };

            tracing::info!(
                reviewer = reviewer.id(),
                model = reviewer.model_name(),
                "Running code review"
            );

            match reviewer.review(&review_ctx).await {
                Ok(report) => {
                    tracing::info!(
                        reviewer = reviewer.id(),
                        verdict = report.verdict.label(),
                        findings = report.findings.len(),
                        duration_ms = report.duration_ms,
                        "Review completed"
                    );
                    reviews.push(report);
                }
                Err(e) => {
                    tracing::warn!(
                        reviewer = reviewer.id(),
                        error = %e,
                        "Review failed, continuing with next reviewer"
                    );
                    reviews.push(ReviewReport {
                        reviewer_id: reviewer.id().to_string(),
                        model: reviewer.model_name().to_string(),
                        summary: format!("Review failed: {}", e),
                        verdict: ReviewVerdict::Comment,
                        findings: vec![],
                        architecture_alignment: None,
                        duration_ms: 0,
                    });
                }
            }
        }

        // Merge findings and compute consensus
        let merged_findings = merge_findings(&reviews);
        let verdict = compute_consensus_verdict(&reviews);
        let summary = build_consensus_summary(&reviews, &merged_findings);

        Ok(ConsensusReport {
            reviews,
            merged_findings,
            verdict,
            summary,
        })
    }
}

// ── Finding merger ───────────────────────────────────────────────

/// Merge findings from multiple reviewers, deduplicating similar issues.
///
/// When multiple reviewers flag the same file+category, keeps the
/// highest severity and combines descriptions.
fn merge_findings(reviews: &[ReviewReport]) -> Vec<ReviewFinding> {
    // Key: (file_path, category) → highest-severity finding
    let mut merged: HashMap<(String, String), ReviewFinding> = HashMap::new();

    for review in reviews {
        for finding in &review.findings {
            let key = (
                finding.file_path.clone().unwrap_or_default(),
                finding.category.clone(),
            );

            match merged.get(&key) {
                Some(existing) if existing.severity >= finding.severity => {
                    // Keep existing (higher or equal severity)
                }
                _ => {
                    merged.insert(key, finding.clone());
                }
            }
        }
    }

    let mut result: Vec<ReviewFinding> = merged.into_values().collect();
    result.sort_by(|a, b| b.severity.cmp(&a.severity));
    result
}

/// Compute consensus verdict from multiple reviews.
///
/// Rules:
/// - If any reviewer says REQUEST_CHANGES → REQUEST_CHANGES
/// - If all reviewers say APPROVE → APPROVE
/// - Otherwise → COMMENT
fn compute_consensus_verdict(reviews: &[ReviewReport]) -> ReviewVerdict {
    if reviews.is_empty() {
        return ReviewVerdict::Comment;
    }

    let has_request_changes = reviews
        .iter()
        .any(|r| r.verdict == ReviewVerdict::RequestChanges);
    let all_approve = reviews.iter().all(|r| r.verdict == ReviewVerdict::Approve);

    if has_request_changes {
        ReviewVerdict::RequestChanges
    } else if all_approve {
        ReviewVerdict::Approve
    } else {
        ReviewVerdict::Comment
    }
}

/// Build a human-readable consensus summary.
fn build_consensus_summary(reviews: &[ReviewReport], merged: &[ReviewFinding]) -> String {
    let reviewer_count = reviews.len();
    let total_findings = merged.len();
    let critical = merged
        .iter()
        .filter(|f| f.severity == Severity::Critical)
        .count();
    let high = merged
        .iter()
        .filter(|f| f.severity == Severity::High)
        .count();

    if total_findings == 0 {
        format!(
            "{} reviewer(s) found no issues. Code looks good.",
            reviewer_count
        )
    } else if critical > 0 {
        format!(
            "{} reviewer(s) found {} issue(s) including {} critical. Changes requested.",
            reviewer_count, total_findings, critical,
        )
    } else if high > 0 {
        format!(
            "{} reviewer(s) found {} issue(s) including {} high-severity. Review recommended.",
            reviewer_count, total_findings, high,
        )
    } else {
        format!(
            "{} reviewer(s) found {} minor issue(s). Suggestions provided.",
            reviewer_count, total_findings,
        )
    }
}

// ── Tests ────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_finding(severity: Severity, category: &str, file: Option<&str>) -> ReviewFinding {
        ReviewFinding {
            severity,
            file_path: file.map(String::from),
            line_range: None,
            category: category.into(),
            description: format!("{} issue in {}", severity, category),
            suggestion: None,
        }
    }

    fn make_report(id: &str, verdict: ReviewVerdict, findings: Vec<ReviewFinding>) -> ReviewReport {
        ReviewReport {
            reviewer_id: id.into(),
            model: "test-model".into(),
            summary: "Test review".into(),
            verdict,
            findings,
            architecture_alignment: None,
            duration_ms: 100,
        }
    }

    #[test]
    fn merge_deduplicates_same_file_category() {
        let reviews = vec![
            make_report(
                "reviewer-a",
                ReviewVerdict::Comment,
                vec![make_finding(
                    Severity::Medium,
                    "security",
                    Some("src/lib.rs"),
                )],
            ),
            make_report(
                "reviewer-b",
                ReviewVerdict::Comment,
                vec![make_finding(
                    Severity::Critical,
                    "security",
                    Some("src/lib.rs"),
                )],
            ),
        ];

        let merged = merge_findings(&reviews);
        // Should keep only the critical one (higher severity wins)
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].severity, Severity::Critical);
    }

    #[test]
    fn merge_keeps_different_categories() {
        let reviews = vec![make_report(
            "reviewer-a",
            ReviewVerdict::Comment,
            vec![
                make_finding(Severity::High, "security", Some("src/lib.rs")),
                make_finding(Severity::Low, "style", Some("src/lib.rs")),
            ],
        )];

        let merged = merge_findings(&reviews);
        assert_eq!(merged.len(), 2);
    }

    #[test]
    fn consensus_request_changes_wins() {
        let reviews = vec![
            make_report("a", ReviewVerdict::Approve, vec![]),
            make_report("b", ReviewVerdict::RequestChanges, vec![]),
        ];
        assert_eq!(
            compute_consensus_verdict(&reviews),
            ReviewVerdict::RequestChanges
        );
    }

    #[test]
    fn consensus_all_approve() {
        let reviews = vec![
            make_report("a", ReviewVerdict::Approve, vec![]),
            make_report("b", ReviewVerdict::Approve, vec![]),
        ];
        assert_eq!(compute_consensus_verdict(&reviews), ReviewVerdict::Approve);
    }

    #[test]
    fn consensus_mixed_is_comment() {
        let reviews = vec![
            make_report("a", ReviewVerdict::Approve, vec![]),
            make_report("b", ReviewVerdict::Comment, vec![]),
        ];
        assert_eq!(compute_consensus_verdict(&reviews), ReviewVerdict::Comment);
    }

    #[test]
    fn empty_pipeline_returns_skip_report() {
        let pipeline = ReviewPipeline::with_reviewers(vec![]);
        assert!(pipeline.is_empty());
        assert_eq!(pipeline.reviewer_count(), 0);
    }

    #[test]
    fn pipeline_from_config_no_keys() {
        let config = PipelineConfig::default();
        let pipeline = ReviewPipeline::from_config(&config);
        assert!(pipeline.is_empty());
    }

    #[test]
    fn pipeline_from_config_gemini_only() {
        let config = PipelineConfig {
            gemini_api_key: Some("test-key".into()),
            enable_secondary_review: false,
            ..Default::default()
        };
        let pipeline = ReviewPipeline::from_config(&config);
        assert_eq!(pipeline.reviewer_count(), 1);
    }

    #[test]
    fn pipeline_from_config_both() {
        let config = PipelineConfig {
            gemini_api_key: Some("gemini-key".into()),
            claude_api_key: Some("claude-key".into()),
            enable_secondary_review: true,
            ..Default::default()
        };
        let pipeline = ReviewPipeline::from_config(&config);
        assert_eq!(pipeline.reviewer_count(), 2);
    }

    #[test]
    fn summary_no_findings() {
        let summary = build_consensus_summary(&[], &[]);
        assert!(summary.contains("no issues"));
    }

    #[test]
    fn summary_critical_findings() {
        let findings = vec![make_finding(Severity::Critical, "security", None)];
        let summary = build_consensus_summary(&[], &findings);
        assert!(summary.contains("critical"));
    }
}
