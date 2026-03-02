//! Trait definitions for the multi-model code review pipeline.
//!
//! Follows ZeroClaw's trait-driven architecture: concrete reviewers
//! (Gemini, Claude, local linters) implement [`CodeReviewer`], and
//! the [`ReviewPipeline`] orchestrates them in sequence.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

// ── Review severity ──────────────────────────────────────────────

/// Severity level for a review finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// Informational suggestion, not a blocker.
    Low,
    /// Should be addressed but not urgent.
    Medium,
    /// Important issue that should be fixed before merge.
    High,
    /// Must-fix: correctness, security, or architecture violation.
    Critical,
}

impl Severity {
    pub fn label(self) -> &'static str {
        match self {
            Self::Low => "LOW",
            Self::Medium => "MEDIUM",
            Self::High => "HIGH",
            Self::Critical => "CRITICAL",
        }
    }
}

impl std::fmt::Display for Severity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

// ── Review finding ───────────────────────────────────────────────

/// A single issue or suggestion found during code review.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewFinding {
    /// Severity of this finding.
    pub severity: Severity,
    /// Which file this finding relates to (if applicable).
    pub file_path: Option<String>,
    /// Line number range (if applicable).
    pub line_range: Option<(usize, usize)>,
    /// Category of the finding (e.g. "architecture", "efficiency", "security").
    pub category: String,
    /// Human-readable description of the issue.
    pub description: String,
    /// Suggested fix or improvement (if any).
    pub suggestion: Option<String>,
}

// ── Review report ────────────────────────────────────────────────

/// Complete review report from a single reviewer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewReport {
    /// Which reviewer produced this report.
    pub reviewer_id: String,
    /// Model or tool that performed the review.
    pub model: String,
    /// One-line summary of the review.
    pub summary: String,
    /// Overall verdict.
    pub verdict: ReviewVerdict,
    /// Detailed findings.
    pub findings: Vec<ReviewFinding>,
    /// Architecture alignment assessment.
    pub architecture_alignment: Option<String>,
    /// Duration of the review in milliseconds.
    pub duration_ms: u64,
}

impl ReviewReport {
    /// Count findings by severity.
    pub fn count_by_severity(&self, severity: Severity) -> usize {
        self.findings
            .iter()
            .filter(|f| f.severity == severity)
            .count()
    }

    /// Whether this report has any critical or high severity findings.
    pub fn has_blockers(&self) -> bool {
        self.findings
            .iter()
            .any(|f| matches!(f.severity, Severity::Critical | Severity::High))
    }

    /// Format the report as a markdown string for PR comments.
    pub fn to_markdown(&self) -> String {
        let mut md = String::new();

        md.push_str(&format!(
            "### Review by `{}` ({})\n\n",
            self.reviewer_id, self.model
        ));
        md.push_str(&format!("**Verdict**: {}\n\n", self.verdict.label()));
        md.push_str(&format!("**Summary**: {}\n\n", self.summary));

        if let Some(ref arch) = self.architecture_alignment {
            md.push_str(&format!("**Architecture Alignment**: {}\n\n", arch));
        }

        if self.findings.is_empty() {
            md.push_str("No issues found.\n");
        } else {
            md.push_str("| Severity | Category | Description |\n");
            md.push_str("|----------|----------|-------------|\n");
            for f in &self.findings {
                let location = match (&f.file_path, f.line_range) {
                    (Some(path), Some((start, end))) => format!(" (`{}:{}-{}`)", path, start, end),
                    (Some(path), None) => format!(" (`{}`)", path),
                    _ => String::new(),
                };
                md.push_str(&format!(
                    "| {} | {} | {}{} |\n",
                    f.severity.label(),
                    f.category,
                    f.description,
                    location,
                ));
            }
        }

        md.push_str(&format!("\n*Review completed in {}ms*\n", self.duration_ms));
        md
    }
}

// ── Review verdict ───────────────────────────────────────────────

/// Overall verdict from a reviewer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewVerdict {
    /// No issues found, approve.
    Approve,
    /// Issues found, changes requested.
    RequestChanges,
    /// Informational comments only.
    Comment,
}

impl ReviewVerdict {
    pub fn label(self) -> &'static str {
        match self {
            Self::Approve => "APPROVE",
            Self::RequestChanges => "REQUEST_CHANGES",
            Self::Comment => "COMMENT",
        }
    }
}

// ── Review context ───────────────────────────────────────────────

/// Context provided to a reviewer for a code review session.
#[derive(Debug, Clone)]
pub struct ReviewContext {
    /// The code diff to review (unified diff format).
    pub diff: String,
    /// List of changed file paths.
    pub changed_files: Vec<String>,
    /// Architecture/guidelines documents content.
    pub architecture_context: String,
    /// PR title.
    pub title: String,
    /// PR description/body.
    pub description: String,
    /// Previous review reports (for multi-round review).
    pub prior_reviews: Vec<ReviewReport>,
}

// ── Code reviewer trait ──────────────────────────────────────────

/// Trait for a code review agent.
///
/// Implementations wrap a specific model or tool (Gemini, Claude, local
/// linter) and produce a [`ReviewReport`] for a given [`ReviewContext`].
#[async_trait]
pub trait CodeReviewer: Send + Sync {
    /// Unique identifier for this reviewer (e.g. "gemini-architect", "claude-implementer").
    fn id(&self) -> &str;

    /// The model or tool name used by this reviewer.
    fn model_name(&self) -> &str;

    /// Perform a code review and return a report.
    async fn review(&self, ctx: &ReviewContext) -> anyhow::Result<ReviewReport>;
}

// ── Consensus result ─────────────────────────────────────────────

/// Result of multi-model consensus review.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsensusReport {
    /// Individual reports from each reviewer.
    pub reviews: Vec<ReviewReport>,
    /// Merged findings (deduplicated, highest severity wins).
    pub merged_findings: Vec<ReviewFinding>,
    /// Final consensus verdict.
    pub verdict: ReviewVerdict,
    /// Summary of the consensus.
    pub summary: String,
}

impl ConsensusReport {
    /// Format the full consensus report as markdown.
    pub fn to_markdown(&self) -> String {
        let mut md = String::new();

        md.push_str("## Multi-Model Code Review Consensus\n\n");
        md.push_str(&format!("**Final Verdict**: {}\n\n", self.verdict.label()));
        md.push_str(&format!("{}\n\n", self.summary));

        // Individual reviews
        for review in &self.reviews {
            md.push_str("---\n\n");
            md.push_str(&review.to_markdown());
            md.push('\n');
        }

        // Merged findings summary
        if !self.merged_findings.is_empty() {
            md.push_str("---\n\n### Merged Findings\n\n");
            let critical = self
                .merged_findings
                .iter()
                .filter(|f| f.severity == Severity::Critical)
                .count();
            let high = self
                .merged_findings
                .iter()
                .filter(|f| f.severity == Severity::High)
                .count();
            let medium = self
                .merged_findings
                .iter()
                .filter(|f| f.severity == Severity::Medium)
                .count();
            let low = self
                .merged_findings
                .iter()
                .filter(|f| f.severity == Severity::Low)
                .count();

            md.push_str(&format!(
                "Critical: {} | High: {} | Medium: {} | Low: {}\n",
                critical, high, medium, low,
            ));
        }

        md
    }
}

// ── Tests ────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn severity_ordering() {
        assert!(Severity::Critical > Severity::High);
        assert!(Severity::High > Severity::Medium);
        assert!(Severity::Medium > Severity::Low);
    }

    #[test]
    fn review_report_markdown() {
        let report = ReviewReport {
            reviewer_id: "test-reviewer".into(),
            model: "test-model".into(),
            summary: "Looks good overall".into(),
            verdict: ReviewVerdict::Approve,
            findings: vec![ReviewFinding {
                severity: Severity::Low,
                file_path: Some("src/main.rs".into()),
                line_range: Some((10, 15)),
                category: "style".into(),
                description: "Minor formatting issue".into(),
                suggestion: None,
            }],
            architecture_alignment: Some("Follows trait-driven pattern".into()),
            duration_ms: 1500,
        };

        let md = report.to_markdown();
        assert!(md.contains("test-reviewer"));
        assert!(md.contains("APPROVE"));
        assert!(md.contains("src/main.rs:10-15"));
    }

    #[test]
    fn review_report_blocker_detection() {
        let report = ReviewReport {
            reviewer_id: "test".into(),
            model: "test".into(),
            summary: "Issues found".into(),
            verdict: ReviewVerdict::RequestChanges,
            findings: vec![ReviewFinding {
                severity: Severity::Critical,
                file_path: None,
                line_range: None,
                category: "security".into(),
                description: "SQL injection vulnerability".into(),
                suggestion: Some("Use parameterized queries".into()),
            }],
            architecture_alignment: None,
            duration_ms: 500,
        };
        assert!(report.has_blockers());
        assert_eq!(report.count_by_severity(Severity::Critical), 1);
    }

    #[test]
    fn consensus_report_markdown() {
        let report = ConsensusReport {
            reviews: vec![],
            merged_findings: vec![],
            verdict: ReviewVerdict::Approve,
            summary: "All reviewers agree".into(),
        };
        let md = report.to_markdown();
        assert!(md.contains("APPROVE"));
        assert!(md.contains("All reviewers agree"));
    }
}
