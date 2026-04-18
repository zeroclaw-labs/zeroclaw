//! Bridge between the coding sandbox and the multi-model review pipeline.
//!
//! Connects review findings to sandbox fix actions, enabling automated
//! review-driven iteration:
//!
//! 1. Sandbox produces a code diff after the Implement phase
//! 2. Bridge sends the diff through the `ReviewPipeline`
//! 3. Pipeline returns a `ConsensusReport` with findings
//! 4. Bridge converts actionable findings into sandbox fix prompts
//! 5. Sandbox applies fixes and re-runs validation
//!
//! This closes the loop: code → review → fix → re-review → converge.

use super::pipeline::{PipelineConfig, ReviewPipeline};
use super::traits::{ConsensusReport, ReviewContext, ReviewFinding, ReviewVerdict, Severity};
use crate::sandbox::{ErrorClass, SandboxAction};

/// Maximum number of review-fix iterations before stopping.
const MAX_REVIEW_ITERATIONS: usize = 3;

/// Minimum severity level to trigger an automatic fix.
const AUTO_FIX_MIN_SEVERITY: Severity = Severity::High;

// ── Bridge configuration ─────────────────────────────────────────

/// Configuration for the sandbox ↔ review bridge.
#[derive(Debug, Clone)]
pub struct SandboxReviewBridgeConfig {
    /// Review pipeline configuration.
    pub pipeline: PipelineConfig,
    /// Maximum review-fix iterations.
    pub max_iterations: usize,
    /// Minimum severity that triggers automatic fix.
    pub auto_fix_severity: Severity,
    /// Architecture context to pass to reviewers.
    pub architecture_context: String,
}

impl Default for SandboxReviewBridgeConfig {
    fn default() -> Self {
        Self {
            pipeline: PipelineConfig::default(),
            max_iterations: MAX_REVIEW_ITERATIONS,
            auto_fix_severity: AUTO_FIX_MIN_SEVERITY,
            architecture_context: String::new(),
        }
    }
}

// ── Bridge ───────────────────────────────────────────────────────

/// Bridges the coding sandbox and the review pipeline.
///
/// Used during the sandbox's Validate phase to run automated code
/// review and feed findings back as fix actions.
pub struct SandboxReviewBridge {
    pipeline: ReviewPipeline,
    config: SandboxReviewBridgeConfig,
}

impl SandboxReviewBridge {
    /// Create a new bridge from configuration.
    pub fn new(config: SandboxReviewBridgeConfig) -> Self {
        let pipeline = ReviewPipeline::from_config(&config.pipeline);
        Self { pipeline, config }
    }

    /// Whether the bridge has any reviewers configured.
    pub fn is_active(&self) -> bool {
        !self.pipeline.is_empty()
    }

    /// Run the review pipeline on a code diff and return actionable findings.
    ///
    /// Returns `Ok(None)` if all reviewers approve (no fixes needed).
    /// Returns `Ok(Some(actions))` with fix actions for findings at or
    /// above the configured severity threshold.
    pub async fn review_diff(
        &self,
        diff: &str,
        changed_files: &[String],
        title: &str,
    ) -> anyhow::Result<Option<ReviewFixPlan>> {
        if !self.is_active() {
            return Ok(None);
        }

        let ctx = ReviewContext {
            diff: diff.to_string(),
            changed_files: changed_files.to_vec(),
            architecture_context: self.config.architecture_context.clone(),
            title: title.to_string(),
            description: String::new(),
            prior_reviews: Vec::new(),
        };

        let report = self.pipeline.run(&ctx).await?;

        tracing::info!(
            verdict = report.verdict.label(),
            findings = report.merged_findings.len(),
            "Sandbox review bridge: review complete"
        );

        if report.verdict == ReviewVerdict::Approve {
            return Ok(None);
        }

        let actionable: Vec<&ReviewFinding> = report
            .merged_findings
            .iter()
            .filter(|f| f.severity >= self.config.auto_fix_severity)
            .collect();

        if actionable.is_empty() {
            return Ok(None);
        }

        let actions: Vec<ReviewFixAction> = actionable
            .into_iter()
            .map(|f| ReviewFixAction {
                file_path: f.file_path.clone(),
                line_range: f.line_range,
                category: f.category.clone(),
                description: f.description.clone(),
                suggestion: f.suggestion.clone(),
                severity: f.severity,
            })
            .collect();

        Ok(Some(ReviewFixPlan {
            consensus: report,
            actions,
        }))
    }

    /// Convert review findings into sandbox-compatible fix actions.
    ///
    /// Maps review categories to sandbox error classes for consistent
    /// handling by the sandbox's fix-apply loop.
    pub fn to_sandbox_actions(plan: &ReviewFixPlan) -> Vec<SandboxAction> {
        plan.actions
            .iter()
            .map(|action| {
                let error_class = match action.category.as_str() {
                    "security" => ErrorClass::Runtime,
                    "architecture" => ErrorClass::Lint,
                    "correctness" => ErrorClass::Runtime,
                    "efficiency" => ErrorClass::Lint,
                    "style" | "formatting" => ErrorClass::Lint,
                    "type" | "type_error" => ErrorClass::Type,
                    _ => ErrorClass::Lint,
                };

                let severity_score = match action.severity {
                    Severity::Critical => 95,
                    Severity::High => 75,
                    Severity::Medium => 50,
                    Severity::Low => 25,
                };

                let suggestion = action
                    .suggestion
                    .clone()
                    .unwrap_or_else(|| action.description.clone());

                let context = match &action.file_path {
                    Some(path) => match action.line_range {
                        Some((start, end)) => {
                            format!("Review finding in {}:{}-{}", path, start, end)
                        }
                        None => format!("Review finding in {}", path),
                    },
                    None => "Review finding (project-wide)".to_string(),
                };

                SandboxAction::NeedsFix {
                    error_class,
                    severity: severity_score,
                    context,
                    suggestion,
                }
            })
            .collect()
    }

    /// Build a combined fix prompt from all review findings.
    ///
    /// Produces a structured prompt that an LLM can use to generate
    /// targeted fixes for all actionable review findings at once.
    pub fn build_review_fix_prompt(plan: &ReviewFixPlan) -> String {
        let mut prompt = String::new();
        prompt.push_str("## Code Review Findings — Automated Fix Request\n\n");
        prompt.push_str(&format!(
            "**Verdict**: {}\n\n",
            plan.consensus.verdict.label()
        ));
        prompt.push_str(&format!(
            "{} actionable finding(s) require fixes:\n\n",
            plan.actions.len()
        ));

        for (i, action) in plan.actions.iter().enumerate() {
            prompt.push_str(&format!(
                "### Finding {} — {} ({})\n\n",
                i + 1,
                action.category,
                action.severity.label()
            ));

            if let Some(ref path) = action.file_path {
                prompt.push_str(&format!("**File**: `{}`", path));
                if let Some((start, end)) = action.line_range {
                    prompt.push_str(&format!(" (lines {}-{})", start, end));
                }
                prompt.push('\n');
            }

            prompt.push_str(&format!("**Issue**: {}\n", action.description));

            if let Some(ref suggestion) = action.suggestion {
                prompt.push_str(&format!("**Suggested fix**: {}\n", suggestion));
            }

            prompt.push('\n');
        }

        prompt.push_str("Generate minimal, targeted fixes for each finding above. ");
        prompt.push_str("Do not modify code unrelated to these findings.\n");

        prompt
    }

    /// Maximum configured review-fix iterations.
    pub fn max_iterations(&self) -> usize {
        self.config.max_iterations
    }
}

// ── Review fix plan ─────────────────────────────────────────────

/// Plan containing review findings and derived fix actions.
#[derive(Debug, Clone)]
pub struct ReviewFixPlan {
    /// The full consensus report from the review pipeline.
    pub consensus: ConsensusReport,
    /// Actionable findings that need automated fixes.
    pub actions: Vec<ReviewFixAction>,
}

/// A single actionable finding from the review pipeline.
#[derive(Debug, Clone)]
pub struct ReviewFixAction {
    /// File path, if the finding is file-specific.
    pub file_path: Option<String>,
    /// Line range within the file.
    pub line_range: Option<(usize, usize)>,
    /// Category of the finding (e.g. "security", "architecture").
    pub category: String,
    /// Description of the issue.
    pub description: String,
    /// Suggested fix from the reviewer.
    pub suggestion: Option<String>,
    /// Severity level.
    pub severity: Severity,
}

// ── Tests ────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coding::traits::{ReviewFinding, ReviewVerdict};

    fn make_finding(severity: Severity, category: &str, suggestion: Option<&str>) -> ReviewFinding {
        ReviewFinding {
            severity,
            file_path: Some("src/main.rs".into()),
            line_range: Some((10, 20)),
            category: category.into(),
            description: format!("{} issue in {}", severity.label(), category),
            suggestion: suggestion.map(String::from),
        }
    }

    fn make_plan(findings: Vec<ReviewFinding>, verdict: ReviewVerdict) -> ReviewFixPlan {
        let actions: Vec<ReviewFixAction> = findings
            .iter()
            .map(|f| ReviewFixAction {
                file_path: f.file_path.clone(),
                line_range: f.line_range,
                category: f.category.clone(),
                description: f.description.clone(),
                suggestion: f.suggestion.clone(),
                severity: f.severity,
            })
            .collect();

        ReviewFixPlan {
            consensus: ConsensusReport {
                reviews: vec![],
                merged_findings: findings,
                verdict,
                summary: "Test report".into(),
            },
            actions,
        }
    }

    #[test]
    fn to_sandbox_actions_maps_severity() {
        let plan = make_plan(
            vec![
                make_finding(Severity::Critical, "security", Some("Fix SQL injection")),
                make_finding(Severity::High, "architecture", None),
            ],
            ReviewVerdict::RequestChanges,
        );

        let actions = SandboxReviewBridge::to_sandbox_actions(&plan);
        assert_eq!(actions.len(), 2);

        match &actions[0] {
            SandboxAction::NeedsFix { severity, .. } => assert_eq!(*severity, 95),
            _ => panic!("Expected NeedsFix"),
        }

        match &actions[1] {
            SandboxAction::NeedsFix { severity, .. } => assert_eq!(*severity, 75),
            _ => panic!("Expected NeedsFix"),
        }
    }

    #[test]
    fn to_sandbox_actions_maps_error_class() {
        let plan = make_plan(
            vec![
                make_finding(Severity::High, "security", None),
                make_finding(Severity::High, "type_error", None),
                make_finding(Severity::High, "style", None),
            ],
            ReviewVerdict::RequestChanges,
        );

        let actions = SandboxReviewBridge::to_sandbox_actions(&plan);
        assert_eq!(actions.len(), 3);

        match &actions[0] {
            SandboxAction::NeedsFix { error_class, .. } => {
                assert_eq!(*error_class, ErrorClass::Runtime);
            }
            _ => panic!("Expected NeedsFix"),
        }
        match &actions[1] {
            SandboxAction::NeedsFix { error_class, .. } => {
                assert_eq!(*error_class, ErrorClass::Type);
            }
            _ => panic!("Expected NeedsFix"),
        }
        match &actions[2] {
            SandboxAction::NeedsFix { error_class, .. } => {
                assert_eq!(*error_class, ErrorClass::Lint);
            }
            _ => panic!("Expected NeedsFix"),
        }
    }

    #[test]
    fn build_review_fix_prompt_includes_all_findings() {
        let plan = make_plan(
            vec![
                make_finding(
                    Severity::Critical,
                    "security",
                    Some("Use parameterized queries"),
                ),
                make_finding(Severity::High, "efficiency", None),
            ],
            ReviewVerdict::RequestChanges,
        );

        let prompt = SandboxReviewBridge::build_review_fix_prompt(&plan);
        assert!(prompt.contains("Code Review Findings"));
        assert!(prompt.contains("2 actionable finding(s)"));
        assert!(prompt.contains("security"));
        assert!(prompt.contains("efficiency"));
        assert!(prompt.contains("src/main.rs"));
        assert!(prompt.contains("Use parameterized queries"));
        assert!(prompt.contains("REQUEST_CHANGES"));
    }

    #[test]
    fn bridge_inactive_when_no_reviewers() {
        let config = SandboxReviewBridgeConfig::default();
        let bridge = SandboxReviewBridge::new(config);
        assert!(!bridge.is_active());
    }

    #[test]
    fn bridge_max_iterations_from_config() {
        let config = SandboxReviewBridgeConfig {
            max_iterations: 5,
            ..Default::default()
        };
        let bridge = SandboxReviewBridge::new(config);
        assert_eq!(bridge.max_iterations(), 5);
    }
}
