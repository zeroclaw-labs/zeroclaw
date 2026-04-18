//! Structured payloads for advisor invocations.
//!
//! The advisor is prompted to reply with JSON so parsing is deterministic
//! instead of regex-over-prose. Callers see typed structs and route on
//! them directly (revise-or-pass for Review, execute-plan for Plan,
//! forward-verbatim for Advise).

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};

use crate::gatekeeper::router::TaskCategory;

/// The three advisor checkpoints defined by the Advisor Strategy.
///
/// Used for logs and observability events — the actual prompt template
/// is selected by calling the matching [`crate::advisor::AdvisorClient`]
/// method, not by threading this enum through the call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AdvisorCheckpoint {
    /// Before the executor starts — strategic plan.
    Plan,
    /// After the executor finishes — correctness / architecture review.
    Review,
    /// Mid-execution pivot or stuck signal — ad-hoc guidance.
    Advise,
}

impl AdvisorCheckpoint {
    #[must_use]
    pub const fn label(&self) -> &'static str {
        match self {
            Self::Plan => "plan",
            Self::Review => "review",
            Self::Advise => "advise",
        }
    }
}

/// Functional family of a user request, used for advisor policy routing.
///
/// Distinct from [`TaskCategory`] (which is about SLM confidence) — two
/// different messages can both be `TaskCategory::Complex` yet need very
/// different advisor attention (a code refactor vs. an essay draft). The
/// gatekeeper's tool hint + message content together map to this enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskKind {
    /// Short Q&A, greeting, chit-chat. Advisor skipped.
    DailyChat,
    /// Writing or editing long-form prose. Plan + review.
    DocumentWork,
    /// Source-code generation, refactoring, debugging. Plan + review + pivot.
    Coding,
    /// Research / reasoning / analysis without tool use. Plan + review.
    Analysis,
    /// Anything the gatekeeper flagged as specialized (legal RAG, voice,
    /// etc.) — plan + review + pivot.
    Specialized,
    /// Fallback bucket when the request doesn't match any known family.
    Other,
}

impl TaskKind {
    /// Infer from the gatekeeper's classification + message body.
    ///
    /// Cheap heuristic — runs on the executor path and must stay fast.
    /// If the gatekeeper reported a specific `tool_needed`, we trust it
    /// as the dominant signal; otherwise the message body drives the
    /// guess.
    #[must_use]
    pub fn infer(category: TaskCategory, tool_hint: Option<&str>, message: &str) -> Self {
        if let Some(tool) = tool_hint {
            let lower = tool.to_ascii_lowercase();
            if lower.contains("code") || lower.contains("shell") || lower.contains("file") {
                return Self::Coding;
            }
            if lower.contains("legal") || lower.contains("voice") || lower.contains("rag") {
                return Self::Specialized;
            }
            if lower.contains("document") || lower.contains("write") || lower.contains("draft") {
                return Self::DocumentWork;
            }
        }
        let lower = message.to_ascii_lowercase();
        const CODING_HINTS: &[&str] = &[
            "refactor", "debug", "implement", "function", "class", "bug",
            "compile", "rust", "python", "javascript", "코드", "구현",
            "디버깅", "리팩터",
        ];
        const DOC_HINTS: &[&str] = &[
            "essay", "document", "write", "draft", "report", "summary",
            "문서", "보고서", "작성", "초안",
        ];
        const ANALYSIS_HINTS: &[&str] = &[
            "analyze", "compare", "evaluate", "reason", "explain why",
            "분석", "비교", "평가", "판단",
        ];
        if CODING_HINTS.iter().any(|h| lower.contains(h)) {
            return Self::Coding;
        }
        if DOC_HINTS.iter().any(|h| lower.contains(h)) {
            return Self::DocumentWork;
        }
        if ANALYSIS_HINTS.iter().any(|h| lower.contains(h)) {
            return Self::Analysis;
        }
        match category {
            TaskCategory::Simple => Self::DailyChat,
            TaskCategory::Specialized => Self::Specialized,
            _ => Self::Other,
        }
    }

    #[must_use]
    pub const fn label(&self) -> &'static str {
        match self {
            Self::DailyChat => "daily_chat",
            Self::DocumentWork => "document_work",
            Self::Coding => "coding",
            Self::Analysis => "analysis",
            Self::Specialized => "specialized",
            Self::Other => "other",
        }
    }
}

/// Executor-assembled context packet sent to every advisor call.
///
/// Mirrors the three-step context format the Claude Code plugin uses
/// (task summary → what you know → recent raw output). The advisor
/// starts fresh with no conversation history, so the executor bears
/// responsibility for compressing what matters into `context` and
/// leaving verbatim tool output in `recent_output`.
#[derive(Debug, Clone)]
pub struct AdvisorRequest<'a> {
    /// 1–2 sentence statement of what the user is trying to accomplish.
    pub task_summary: &'a str,
    /// Bullet-style compressed background — codebase shape, decisions
    /// already made, constraints discovered. Keep short.
    pub background: &'a str,
    /// Last 3–5 tool results, copied verbatim — the advisor needs the
    /// raw text, not a summary. Empty string is acceptable.
    pub recent_output: &'a str,
    /// Concrete question or current situation the advisor should act on.
    pub question: &'a str,
    /// Functional family of the task (derived at call time for telemetry).
    pub kind: TaskKind,
}

/// Structured plan returned by `AdvisorClient::plan`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanOutput {
    /// What "done" looks like — single sentence.
    pub end_state: String,
    /// Ordered steps from current state to end state.
    pub critical_path: Vec<String>,
    /// Known risks with mitigations. Empty is allowed when the task is
    /// genuinely low-risk; the advisor is instructed not to invent risks.
    #[serde(default)]
    pub risks: Vec<String>,
    /// Single best next action — where the executor should start.
    pub first_move: String,
    /// Tools the executor should reach for. Advisor is prompted to
    /// favour the `smart_search` cascade (free web → Perplexity → retry)
    /// over raw `web_search` / `perplexity_search` whenever a retrieval
    /// step is needed, so the executor gets resilient fallback without
    /// having to orchestrate the tiers itself.
    #[serde(default)]
    pub suggested_tools: Vec<String>,
}

impl PlanOutput {
    /// Parse a raw advisor reply. Accepts either a fenced JSON block
    /// (```json ... ```), a bare JSON object, or a structured markdown
    /// response that includes the required sections.
    pub fn parse(raw: &str) -> Result<Self> {
        if let Some(parsed) = try_parse_json::<Self>(raw) {
            return Ok(parsed);
        }
        // Markdown fallback — rare but the advisor occasionally drops JSON
        // fences under tight rate limits. Extract headed sections.
        let end_state = extract_section(raw, &["end state", "end_state"])
            .ok_or_else(|| anyhow!("advisor plan missing `End State` section"))?;
        let first_move = extract_section(raw, &["first move", "first_move"])
            .ok_or_else(|| anyhow!("advisor plan missing `First Move` section"))?;
        let critical_path = extract_bullet_list(raw, &["critical path", "critical_path", "plan", "steps"]);
        let risks = extract_bullet_list(raw, &["risks", "risk"]);
        let suggested_tools = extract_bullet_list(raw, &["suggested tools", "suggested_tools", "tools"]);
        Ok(Self {
            end_state,
            critical_path,
            risks,
            first_move,
            suggested_tools,
        })
    }
}

/// Review verdict severity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReviewVerdict {
    /// Ship as-is — no changes needed.
    Pass,
    /// Correctable issues found — executor should revise before returning.
    RevisionNeeded,
    /// Cannot ship — a blocker (security, correctness, scope) was found.
    Block,
}

impl ReviewVerdict {
    #[must_use]
    pub const fn requires_revision(self) -> bool {
        matches!(self, Self::RevisionNeeded | Self::Block)
    }
}

/// Structured review returned by `AdvisorClient::review`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewOutput {
    pub verdict: ReviewVerdict,
    /// Bug / logic / edge-case concerns in the executor's result.
    #[serde(default)]
    pub correctness_issues: Vec<String>,
    /// Design-level concerns (coupling, scalability). Empty is allowed.
    #[serde(default)]
    pub architecture_concerns: Vec<String>,
    /// Security flags (injection risk, data exposure, unsafe defaults).
    #[serde(default)]
    pub security_flags: Vec<String>,
    /// Silent failures — swallowed errors, missing validation.
    #[serde(default)]
    pub silent_failures: Vec<String>,
    /// Concise summary (under 60 words) the executor can relay to the
    /// user when revision is not needed.
    #[serde(default)]
    pub summary: String,
}

impl ReviewOutput {
    pub fn parse(raw: &str) -> Result<Self> {
        if let Some(parsed) = try_parse_json::<Self>(raw) {
            return Ok(parsed);
        }
        // Markdown fallback — extract by headings.
        let verdict_raw = extract_section(raw, &["verdict"]).unwrap_or_default();
        let verdict = match verdict_raw.to_ascii_lowercase().trim() {
            "pass" | "ok" | "ship" => ReviewVerdict::Pass,
            "block" | "blocker" | "reject" => ReviewVerdict::Block,
            _ => ReviewVerdict::RevisionNeeded,
        };
        Ok(Self {
            verdict,
            correctness_issues: extract_bullet_list(raw, &["correctness", "bugs"]),
            architecture_concerns: extract_bullet_list(raw, &["architecture", "design"]),
            security_flags: extract_bullet_list(raw, &["security"]),
            silent_failures: extract_bullet_list(raw, &["silent", "silent failures"]),
            summary: extract_section(raw, &["summary"]).unwrap_or_default(),
        })
    }

    /// Whether the executor must revise before returning.
    #[must_use]
    pub fn needs_revision(&self) -> bool {
        self.verdict.requires_revision()
    }
}

// ── Parsing helpers ────────────────────────────────────────────────

fn try_parse_json<T: for<'de> Deserialize<'de>>(raw: &str) -> Option<T> {
    // Fast path: the whole reply IS a JSON object.
    if let Ok(parsed) = serde_json::from_str::<T>(raw.trim()) {
        return Some(parsed);
    }
    // Strip ```json … ``` fences.
    if let Some(stripped) = strip_code_fence(raw) {
        if let Ok(parsed) = serde_json::from_str::<T>(stripped.trim()) {
            return Some(parsed);
        }
    }
    // Best-effort: find the first {...} block.
    if let Some(block) = extract_first_json_object(raw) {
        if let Ok(parsed) = serde_json::from_str::<T>(block.trim()) {
            return Some(parsed);
        }
    }
    None
}

fn strip_code_fence(raw: &str) -> Option<&str> {
    let start = raw.find("```").map(|i| i + 3)?;
    let after_lang = raw[start..].find('\n').map(|n| start + n + 1)?;
    let end = raw[after_lang..].find("```").map(|e| after_lang + e)?;
    Some(&raw[after_lang..end])
}

fn extract_first_json_object(raw: &str) -> Option<&str> {
    let start = raw.find('{')?;
    let mut depth = 0i32;
    let mut end = None;
    for (i, ch) in raw[start..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    end = Some(start + i + 1);
                    break;
                }
            }
            _ => {}
        }
    }
    end.map(|e| &raw[start..e])
}

fn extract_section(raw: &str, headings: &[&str]) -> Option<String> {
    let lower = raw.to_ascii_lowercase();
    for heading in headings {
        let hl = heading.to_ascii_lowercase();
        for needle in [
            format!("## {hl}"),
            format!("### {hl}"),
            format!("**{hl}**"),
            format!("{hl}:"),
        ] {
            if let Some(pos) = lower.find(&needle) {
                let after = &raw[pos + needle.len()..];
                let end = after
                    .find("\n## ")
                    .or_else(|| after.find("\n### "))
                    .unwrap_or(after.len());
                let body = after[..end].trim();
                if !body.is_empty() {
                    return Some(body.lines().next().unwrap_or(body).trim().to_string());
                }
            }
        }
    }
    None
}

fn extract_bullet_list(raw: &str, headings: &[&str]) -> Vec<String> {
    let lower = raw.to_ascii_lowercase();
    for heading in headings {
        let hl = heading.to_ascii_lowercase();
        for needle in [format!("## {hl}"), format!("### {hl}")] {
            if let Some(pos) = lower.find(&needle) {
                let after = &raw[pos + needle.len()..];
                let end = after
                    .find("\n## ")
                    .or_else(|| after.find("\n### "))
                    .unwrap_or(after.len());
                let body = &after[..end];
                let mut out = Vec::new();
                for line in body.lines() {
                    let trimmed = line.trim_start();
                    if let Some(rest) = trimmed
                        .strip_prefix("- ")
                        .or_else(|| trimmed.strip_prefix("* "))
                        .or_else(|| {
                            // "1. step"
                            trimmed
                                .split_once('.')
                                .filter(|(n, _)| n.chars().all(|c| c.is_ascii_digit()))
                                .map(|(_, rest)| rest.trim_start())
                        })
                    {
                        let item = rest.trim();
                        if !item.is_empty() {
                            out.push(item.to_string());
                        }
                    }
                }
                if !out.is_empty() {
                    return out;
                }
            }
        }
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_parses_pure_json() {
        let raw = r#"{"end_state":"done","critical_path":["a","b"],"risks":["r1"],"first_move":"x"}"#;
        let plan = PlanOutput::parse(raw).unwrap();
        assert_eq!(plan.end_state, "done");
        assert_eq!(plan.critical_path, vec!["a", "b"]);
        assert_eq!(plan.first_move, "x");
    }

    #[test]
    fn plan_parses_fenced_json() {
        let raw = "```json\n{\"end_state\":\"x\",\"critical_path\":[],\"first_move\":\"y\"}\n```";
        let plan = PlanOutput::parse(raw).unwrap();
        assert_eq!(plan.end_state, "x");
        assert_eq!(plan.first_move, "y");
    }

    #[test]
    fn review_parses_verdict_enum() {
        let raw = r#"{"verdict":"pass","summary":"LGTM"}"#;
        let r = ReviewOutput::parse(raw).unwrap();
        assert_eq!(r.verdict, ReviewVerdict::Pass);
        assert!(!r.needs_revision());
    }

    #[test]
    fn review_flags_revision_needed_for_blockers() {
        let raw = r#"{"verdict":"block","correctness_issues":["off-by-one"]}"#;
        let r = ReviewOutput::parse(raw).unwrap();
        assert_eq!(r.verdict, ReviewVerdict::Block);
        assert!(r.needs_revision());
    }

    #[test]
    fn task_kind_infers_coding_from_message() {
        let k = TaskKind::infer(TaskCategory::Complex, None, "please refactor this rust function");
        assert_eq!(k, TaskKind::Coding);
    }

    #[test]
    fn task_kind_infers_document_work() {
        let k = TaskKind::infer(TaskCategory::Complex, None, "초안 문서 작성해줘");
        assert_eq!(k, TaskKind::DocumentWork);
    }

    #[test]
    fn task_kind_respects_tool_hint() {
        let k = TaskKind::infer(TaskCategory::Medium, Some("legal_rag"), "아무 질문");
        assert_eq!(k, TaskKind::Specialized);
    }

    #[test]
    fn checkpoint_label_round_trips() {
        assert_eq!(AdvisorCheckpoint::Plan.label(), "plan");
        assert_eq!(AdvisorCheckpoint::Review.label(), "review");
        assert_eq!(AdvisorCheckpoint::Advise.label(), "advise");
    }
}
