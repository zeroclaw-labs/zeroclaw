//! Automated fix-apply from review findings.
//!
//! Takes a [`ConsensusReport`] from the review pipeline and generates
//! structured fix instructions that can be applied by the agent's tool
//! execution surface (file_edit, apply_patch, shell).
//!
//! ## Workflow
//!
//! ```text
//! ConsensusReport ──▸ FixPlan ──▸ FixInstruction[] ──▸ Agent applies each
//!                                      │
//!                     ┌────────────────┘
//!                     ├─ FileEdit { path, old, new }
//!                     ├─ ShellCommand { cmd }
//!                     └─ ApplyPatch { diff }
//! ```
//!
//! The fix instructions are structured so the agent loop can apply them
//! without additional LLM inference for simple, mechanical fixes.

use serde::{Deserialize, Serialize};

use super::traits::{ConsensusReport, ReviewFinding, ReviewVerdict, Severity};

// ── Fix instruction types ────────────────────────────────────────

/// A single fix instruction derived from a review finding.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FixInstruction {
    /// Replace text in a file (deterministic, no LLM needed).
    FileEdit {
        file_path: String,
        line_start: Option<usize>,
        line_end: Option<usize>,
        description: String,
        suggested_replacement: Option<String>,
    },
    /// Run a shell command (e.g. `cargo fmt`, `eslint --fix`).
    ShellCommand {
        command: String,
        description: String,
    },
    /// Apply a unified diff patch.
    ApplyPatch { patch: String, description: String },
    /// Requires LLM inference to generate the fix.
    LlmAssisted {
        prompt: String,
        file_path: Option<String>,
        description: String,
    },
}

// ── Fix plan ─────────────────────────────────────────────────────

/// A plan of fix instructions derived from review findings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FixPlan {
    /// Ordered list of fix instructions.
    pub instructions: Vec<FixInstruction>,
    /// Summary of what will be fixed.
    pub summary: String,
    /// Number of findings that could not be auto-fixed.
    pub deferred_count: usize,
}

impl FixPlan {
    /// Whether there are any instructions to apply.
    pub fn is_empty(&self) -> bool {
        self.instructions.is_empty()
    }

    /// Count of fix instructions.
    pub fn len(&self) -> usize {
        self.instructions.len()
    }

    /// Count of instructions that require LLM assistance.
    pub fn llm_assisted_count(&self) -> usize {
        self.instructions
            .iter()
            .filter(|i| matches!(i, FixInstruction::LlmAssisted { .. }))
            .count()
    }
}

// ── Fix plan generator ──────────────────────────────────────────

/// Generate a fix plan from a consensus report.
///
/// Converts review findings into actionable fix instructions:
///
/// - Findings with concrete suggestions → `FileEdit` or `LlmAssisted`
/// - Style/formatting findings → `ShellCommand` (formatter)
/// - Complex findings → `LlmAssisted` with structured prompt
///
/// Findings below `min_severity` are deferred (not included).
pub fn generate_fix_plan(report: &ConsensusReport, min_severity: Severity) -> FixPlan {
    if report.verdict == ReviewVerdict::Approve {
        return FixPlan {
            instructions: Vec::new(),
            summary: "All reviewers approved — no fixes needed.".into(),
            deferred_count: 0,
        };
    }

    let mut instructions = Vec::new();
    let mut deferred_count = 0;

    for finding in &report.merged_findings {
        if finding.severity < min_severity {
            deferred_count += 1;
            continue;
        }

        let instruction = finding_to_instruction(finding);
        instructions.push(instruction);
    }

    let summary = format!(
        "Generated {} fix instruction(s) from {} finding(s) ({} deferred below {} severity).",
        instructions.len(),
        report.merged_findings.len(),
        deferred_count,
        min_severity.label(),
    );

    FixPlan {
        instructions,
        summary,
        deferred_count,
    }
}

/// Convert a single review finding into a fix instruction.
fn finding_to_instruction(finding: &ReviewFinding) -> FixInstruction {
    let category = finding.category.as_str();

    // Style/formatting → shell command (formatter)
    if matches!(category, "style" | "formatting" | "format") {
        return FixInstruction::ShellCommand {
            command: detect_formatter(finding),
            description: finding.description.clone(),
        };
    }

    // Lint warnings with file path → LLM-assisted with context
    if let Some(ref path) = finding.file_path {
        if let Some(ref suggestion) = finding.suggestion {
            // Has concrete suggestion → file edit
            return FixInstruction::FileEdit {
                file_path: path.clone(),
                line_start: finding.line_range.map(|(s, _)| s),
                line_end: finding.line_range.map(|(_, e)| e),
                description: finding.description.clone(),
                suggested_replacement: Some(suggestion.clone()),
            };
        }

        // No concrete suggestion → LLM generates fix
        let prompt = build_finding_fix_prompt(finding);
        return FixInstruction::LlmAssisted {
            prompt,
            file_path: Some(path.clone()),
            description: finding.description.clone(),
        };
    }

    // Project-wide finding → LLM-assisted
    let prompt = build_finding_fix_prompt(finding);
    FixInstruction::LlmAssisted {
        prompt,
        file_path: None,
        description: finding.description.clone(),
    }
}

/// Detect the appropriate formatter command based on file extension.
fn detect_formatter(finding: &ReviewFinding) -> String {
    let ext = finding
        .file_path
        .as_deref()
        .and_then(|p| p.rsplit_once('.').map(|(_, e)| e))
        .unwrap_or("");

    match ext {
        "rs" => "cargo fmt --all".to_string(),
        "ts" | "tsx" | "js" | "jsx" => "npx prettier --write .".to_string(),
        "py" => "python -m black .".to_string(),
        "go" => "gofmt -w .".to_string(),
        _ => "cargo fmt --all".to_string(), // default for this Rust project
    }
}

/// Build a structured prompt for an LLM to fix a specific finding.
fn build_finding_fix_prompt(finding: &ReviewFinding) -> String {
    let mut prompt = String::new();

    prompt.push_str(&format!(
        "Fix the following {} severity {} issue:\n\n",
        finding.severity.label(),
        finding.category,
    ));

    prompt.push_str(&format!("**Issue**: {}\n", finding.description));

    if let Some(ref path) = finding.file_path {
        prompt.push_str(&format!("**File**: `{}`\n", path));
    }
    if let Some((start, end)) = finding.line_range {
        prompt.push_str(&format!("**Lines**: {}-{}\n", start, end));
    }
    if let Some(ref suggestion) = finding.suggestion {
        prompt.push_str(&format!("**Reviewer suggestion**: {}\n", suggestion));
    }

    prompt.push_str("\nGenerate a minimal, targeted fix. Do not change unrelated code.\n");

    prompt
}

// ── Tests ────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coding::traits::{ReviewFinding, ReviewVerdict};

    fn make_finding(
        severity: Severity,
        category: &str,
        file: Option<&str>,
        suggestion: Option<&str>,
    ) -> ReviewFinding {
        ReviewFinding {
            severity,
            file_path: file.map(String::from),
            line_range: file.map(|_| (10, 20)),
            category: category.into(),
            description: format!("{} issue", category),
            suggestion: suggestion.map(String::from),
        }
    }

    fn make_report(findings: Vec<ReviewFinding>, verdict: ReviewVerdict) -> ConsensusReport {
        ConsensusReport {
            reviews: vec![],
            merged_findings: findings,
            verdict,
            summary: "Test".into(),
        }
    }

    #[test]
    fn generate_fix_plan_approve_returns_empty() {
        let report = make_report(vec![], ReviewVerdict::Approve);
        let plan = generate_fix_plan(&report, Severity::High);
        assert!(plan.is_empty());
        assert_eq!(plan.deferred_count, 0);
    }

    #[test]
    fn generate_fix_plan_filters_by_severity() {
        let report = make_report(
            vec![
                make_finding(Severity::Critical, "security", Some("src/lib.rs"), None),
                make_finding(Severity::Low, "style", Some("src/lib.rs"), None),
            ],
            ReviewVerdict::RequestChanges,
        );

        let plan = generate_fix_plan(&report, Severity::High);
        assert_eq!(plan.len(), 1);
        assert_eq!(plan.deferred_count, 1);
    }

    #[test]
    fn finding_with_suggestion_becomes_file_edit() {
        let finding = make_finding(
            Severity::High,
            "correctness",
            Some("src/main.rs"),
            Some("Use `bail!` instead of `panic!`"),
        );

        let instruction = finding_to_instruction(&finding);
        assert!(matches!(instruction, FixInstruction::FileEdit { .. }));
    }

    #[test]
    fn finding_without_suggestion_becomes_llm_assisted() {
        let finding = make_finding(Severity::High, "security", Some("src/main.rs"), None);

        let instruction = finding_to_instruction(&finding);
        assert!(matches!(instruction, FixInstruction::LlmAssisted { .. }));
    }

    #[test]
    fn style_finding_becomes_shell_command() {
        let finding = make_finding(Severity::Medium, "style", Some("src/main.rs"), None);

        let instruction = finding_to_instruction(&finding);
        match instruction {
            FixInstruction::ShellCommand { command, .. } => {
                assert!(command.contains("fmt"));
            }
            _ => panic!("Expected ShellCommand for style finding"),
        }
    }

    #[test]
    fn project_wide_finding_becomes_llm_assisted() {
        let finding = make_finding(Severity::High, "architecture", None, None);

        let instruction = finding_to_instruction(&finding);
        match instruction {
            FixInstruction::LlmAssisted { file_path, .. } => {
                assert!(file_path.is_none());
            }
            _ => panic!("Expected LlmAssisted for project-wide finding"),
        }
    }

    #[test]
    fn fix_plan_counts() {
        let report = make_report(
            vec![
                make_finding(Severity::Critical, "security", Some("a.rs"), None),
                make_finding(Severity::High, "style", Some("b.rs"), None),
                make_finding(Severity::High, "correctness", None, None),
            ],
            ReviewVerdict::RequestChanges,
        );

        let plan = generate_fix_plan(&report, Severity::High);
        assert_eq!(plan.len(), 3);
        assert!(plan.llm_assisted_count() >= 1);
    }

    #[test]
    fn detect_formatter_by_extension() {
        let rs_finding = make_finding(Severity::Low, "style", Some("src/main.rs"), None);
        assert!(detect_formatter(&rs_finding).contains("cargo fmt"));

        let ts_finding = make_finding(Severity::Low, "style", Some("src/app.tsx"), None);
        assert!(detect_formatter(&ts_finding).contains("prettier"));

        let py_finding = make_finding(Severity::Low, "style", Some("app.py"), None);
        assert!(detect_formatter(&py_finding).contains("black"));
    }

    #[test]
    fn build_finding_fix_prompt_structure() {
        let finding = make_finding(
            Severity::Critical,
            "security",
            Some("src/auth.rs"),
            Some("Parameterize the query"),
        );

        let prompt = build_finding_fix_prompt(&finding);
        assert!(prompt.contains("CRITICAL"));
        assert!(prompt.contains("security"));
        assert!(prompt.contains("src/auth.rs"));
        assert!(prompt.contains("Parameterize the query"));
        assert!(prompt.contains("minimal, targeted fix"));
    }
}
