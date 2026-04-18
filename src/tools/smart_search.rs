//! `SmartSearchTool` — cascade wrapper over the free web search and
//! the paid Perplexity AI search.
//!
//! # Why this exists
//!
//! Per product spec (2026-04-18): "먼저 무료 웹검색도구 → 유료 ai웹검색
//! perplexity 순서로 도구를 사용하도록 해서 어떠한 경우에도 답변할 수 없는
//! 경우가 발생하지 않도록 검색결과가 없다는 사실이 확실해질때까지 3~4회까지
//! 반복해서 다양하게 검색을 시도하도록."
//!
//! Translation of the cascade rule:
//!
//! 1. Try the **free** MoA web search first — cheap, covers most queries.
//! 2. Escalate to **Perplexity AI** when:
//!    - The free tier returned no / thin results, OR
//!    - The query is high-complexity (legal / coding / science / research)
//!      where authoritative synthesis is worth the paid call, OR
//!    - The caller set `force_premium: true`.
//! 3. If both tiers come back thin, **reformulate the query** and retry.
//!    Up to `max_attempts` (default 4, hard cap 6) distinct reformulations.
//! 4. Only if *every* reformulation against *every* available tier returns
//!    nothing substantial do we declare "no results". Partial results from
//!    earlier attempts are still returned so the executor SLM has
//!    *something* to build on.
//!
//! # Integration
//!
//! The tool registers under the name `smart_search` so the agent loop
//! (or a SLM executor, in Phase 2 of the advisor rollout) can call it
//! like any other tool. The response carries a `trace` array showing
//! which tier answered which attempt — useful for the advisor REVIEW
//! checkpoint to judge whether the search was adequate.

use std::collections::HashSet;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;

use super::traits::{Tool, ToolResult};

/// Hard upper bound on attempts regardless of caller request — prevents
/// a pathological caller from burning Perplexity credits in an infinite
/// loop.
const HARD_ATTEMPT_CAP: usize = 6;

/// Default number of attempts when the caller doesn't specify.
const DEFAULT_ATTEMPTS: usize = 4;

/// Minimum non-error output character count we consider "sufficient".
/// Below this the result is usually a single placeholder ("no results
/// found") and we want to retry with a reformulated query.
const SUFFICIENCY_THRESHOLD_CHARS: usize = 500;

/// Cascade search tool: free web search → Perplexity AI → reformulate.
///
/// Holds `Arc<dyn Tool>` handles to the underlying tools rather than
/// concrete types so the caller can swap implementations for testing
/// (e.g., inject a deterministic fake search tool).
pub struct SmartSearchTool {
    free: Arc<dyn Tool>,
    perplexity: Option<Arc<dyn Tool>>,
}

impl SmartSearchTool {
    /// Build with both tiers. When `perplexity` is `None` the cascade
    /// silently degrades to free-only — the tool still works, just
    /// without the premium escalation.
    #[must_use]
    pub fn new(free: Arc<dyn Tool>, perplexity: Option<Arc<dyn Tool>>) -> Self {
        Self { free, perplexity }
    }
}

#[async_trait]
impl Tool for SmartSearchTool {
    fn name(&self) -> &str {
        "smart_search"
    }

    fn description(&self) -> &str {
        "Cascade web search. Tries the free MoA web search first; escalates \
         to Perplexity AI when results are thin or the query is high-complexity \
         (legal, coding, science, research). Reformulates and retries up to \
         4 times across both tiers before concluding no results exist. Always \
         returns at least a trace of what was tried — prefer this over the \
         individual `web_search` / `perplexity_search` tools when you just \
         want reliable search."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "The search query. Include specific terms; the cascade will reformulate automatically if thin."
                },
                "force_premium": {
                    "type": "boolean",
                    "description": "Skip the free tier and go straight to Perplexity AI. Default false.",
                    "default": false
                },
                "max_attempts": {
                    "type": "integer",
                    "description": "Max reformulation attempts across both tiers. Default 4, hard cap 6.",
                    "minimum": 1,
                    "maximum": 6
                },
                "domain_hint": {
                    "type": "string",
                    "description": "Optional domain hint for reformulation (e.g. 'legal', 'coding', 'medicine'). Biases the query variants toward authoritative sources."
                }
            },
            "required": ["query"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let query = args["query"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("smart_search: `query` is required"))?
            .trim();
        if query.is_empty() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("smart_search: empty query".into()),
            });
        }
        let force_premium = args["force_premium"].as_bool().unwrap_or(false);
        let max_attempts = args["max_attempts"]
            .as_u64()
            .map(|n| (n as usize).clamp(1, HARD_ATTEMPT_CAP))
            .unwrap_or(DEFAULT_ATTEMPTS);
        let domain_hint = args["domain_hint"].as_str().unwrap_or("");
        let complex_topic = is_complex_topic(query, domain_hint);

        let variants = build_query_variants(query, domain_hint, max_attempts);
        let mut trace: Vec<String> = Vec::new();
        let mut accumulated: Vec<(&'static str, String)> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();

        for (i, q) in variants.iter().enumerate() {
            if !seen.insert(q.clone()) {
                continue;
            }
            let attempt = i + 1;

            // Tier 1: free — unless caller forced premium.
            if !force_premium {
                match self.free.execute(json!({ "query": q })).await {
                    Ok(res) if is_substantive(&res) => {
                        trace.push(format!(
                            "#{attempt} free({chars}c) \"{q}\"",
                            chars = res.output.len(),
                            q = truncate_for_trace(q)
                        ));
                        let ok_now = !complex_topic
                            && is_sufficient(&res.output, SUFFICIENCY_THRESHOLD_CHARS);
                        accumulated.push(("free", res.output));
                        if ok_now {
                            return Ok(finalize(&accumulated, &trace, true));
                        }
                    }
                    Ok(res) => {
                        trace.push(format!(
                            "#{attempt} free(empty{note}) \"{q}\"",
                            note = res
                                .error
                                .as_deref()
                                .map(|e| format!(": {e}"))
                                .unwrap_or_default(),
                            q = truncate_for_trace(q)
                        ));
                    }
                    Err(e) => {
                        trace.push(format!(
                            "#{attempt} free(err: {e}) \"{q}\"",
                            q = truncate_for_trace(q)
                        ));
                    }
                }
            }

            // Tier 2: Perplexity — when available and justified.
            if let Some(perp) = &self.perplexity {
                let perp_args = json!({ "query": q, "num_results": 5 });
                match perp.execute(perp_args).await {
                    Ok(res) if is_substantive(&res) => {
                        trace.push(format!(
                            "#{attempt} perplexity({chars}c) \"{q}\"",
                            chars = res.output.len(),
                            q = truncate_for_trace(q)
                        ));
                        let ok_now = is_sufficient(&res.output, SUFFICIENCY_THRESHOLD_CHARS);
                        accumulated.push(("perplexity", res.output));
                        if ok_now {
                            return Ok(finalize(&accumulated, &trace, true));
                        }
                    }
                    Ok(res) => {
                        trace.push(format!(
                            "#{attempt} perplexity(empty{note}) \"{q}\"",
                            note = res
                                .error
                                .as_deref()
                                .map(|e| format!(": {e}"))
                                .unwrap_or_default(),
                            q = truncate_for_trace(q)
                        ));
                    }
                    Err(e) => {
                        trace.push(format!(
                            "#{attempt} perplexity(err: {e}) \"{q}\"",
                            q = truncate_for_trace(q)
                        ));
                    }
                }
            }
        }

        if accumulated.is_empty() {
            // Cascade confirmed "no results" after exhausting the matrix.
            Ok(ToolResult {
                success: false,
                output: format!(
                    "No search results after {attempts} reformulation attempts \
                     across free{plus_perplexity} tier{s}. Trace:\n{trace}\n\n\
                     Conclusion: the requested information does not appear to \
                     be indexed in any configured search backend — consider \
                     answering from general knowledge or asking the user to \
                     provide a source URL.",
                    attempts = variants.len(),
                    plus_perplexity = if self.perplexity.is_some() {
                        " + Perplexity"
                    } else {
                        ""
                    },
                    s = if self.perplexity.is_some() { "s" } else { "" },
                    trace = trace.join("\n"),
                ),
                error: Some("smart_search: cascade exhausted, no substantive results".into()),
            })
        } else {
            // At least some results — return the accumulated block, even if
            // individually they didn't cross the sufficiency threshold.
            Ok(finalize(&accumulated, &trace, false))
        }
    }
}

/// A `ToolResult` counts as substantive when it's marked successful AND
/// has a non-empty, non-whitespace output. Some tools return `success=true`
/// with an empty string when no matches were found — we treat that as empty.
fn is_substantive(res: &ToolResult) -> bool {
    res.success && !res.output.trim().is_empty()
}

/// Whether a single tier's output meets the "ship this answer" bar.
/// Heuristic: at least `min_chars` of non-whitespace output, which
/// covers "≥3 result snippets" in practice for both backends' JSON
/// output formats.
fn is_sufficient(output: &str, min_chars: usize) -> bool {
    output.split_whitespace().map(str::len).sum::<usize>() >= min_chars
}

/// Assemble the final cascade output with per-attempt trace and the
/// per-tier results concatenated in the order they were accumulated.
fn finalize(results: &[(&'static str, String)], trace: &[String], sufficient: bool) -> ToolResult {
    let status = if sufficient {
        "sufficient"
    } else {
        "partial (cascade exhausted, returning accumulated results)"
    };
    let mut out = String::new();
    out.push_str(&format!("[smart_search cascade — {status}]\n"));
    if !trace.is_empty() {
        out.push_str("Trace:\n");
        for line in trace {
            out.push_str(&format!("  {line}\n"));
        }
        out.push('\n');
    }
    for (idx, (tier, body)) in results.iter().enumerate() {
        out.push_str(&format!("── Result {} ({tier}) ──\n", idx + 1));
        out.push_str(body);
        if !body.ends_with('\n') {
            out.push('\n');
        }
        out.push('\n');
    }
    ToolResult {
        success: true,
        output: out,
        error: None,
    }
}

/// Heuristic: does this query look like a topic where Perplexity's
/// synthesis adds disproportionate value over a generic SERP? Matches
/// legal / coding / science / medical / research terms in both English
/// and Korean.
fn is_complex_topic(query: &str, domain_hint: &str) -> bool {
    let hint = domain_hint.to_ascii_lowercase();
    const COMPLEX_HINTS: &[&str] = &[
        "legal", "law", "coding", "programming", "science", "research",
        "medical", "medicine", "engineering", "math", "mathematics",
        "physics", "chemistry", "biology", "patent",
    ];
    if COMPLEX_HINTS.iter().any(|h| hint.contains(h)) {
        return true;
    }
    let lower = query.to_ascii_lowercase();
    const QUERY_HINTS: &[&str] = &[
        // English
        "statute", "ruling", "precedent", "lawsuit", "patent", "citation",
        "compile error", "stack trace", "exception", "kernel panic",
        "peer reviewed", "clinical trial", "RFC", "ISO ", "IEEE",
        // Korean
        "판례", "법률", "조항", "판결", "소송", "특허", "등기부", "약관",
        "컴파일", "스택트레이스", "예외", "런타임", "커널",
        "임상시험", "논문", "심사",
    ];
    QUERY_HINTS.iter().any(|h| lower.contains(&h.to_ascii_lowercase()))
}

/// Build up to `max_attempts` distinct query variants for the cascade.
///
/// Variants target different retrieval strategies:
///  1. Original — baseline.
///  2. Authoritative — `"X" site:.edu OR site:.gov OR site:.org` bias.
///  3. Research form — prefix with "research paper"/"원문" for academic corpora.
///  4. Question form — reshape as "what is X" / "X 란" to catch Q&A pages.
///  5. Simpler — strip stopwords and short connectors.
///  6. Domain-hinted — only when `domain_hint` provided.
///
/// Duplicates are de-duped downstream; this just provides candidates.
fn build_query_variants(query: &str, domain_hint: &str, max_attempts: usize) -> Vec<String> {
    let mut variants: Vec<String> = Vec::new();
    variants.push(query.to_string());

    if max_attempts >= 2 {
        variants.push(format!(
            "{query} site:.edu OR site:.gov OR site:.org"
        ));
    }
    if max_attempts >= 3 {
        variants.push(format!("research paper {query}"));
    }
    if max_attempts >= 4 {
        let lower = query.to_ascii_lowercase();
        let question_form = if lower.starts_with("what ")
            || lower.starts_with("how ")
            || lower.ends_with('?')
        {
            query.to_string()
        } else {
            format!("what is {query}?")
        };
        variants.push(question_form);
    }
    if max_attempts >= 5 {
        let simpler = simplify_query(query);
        if !simpler.is_empty() && simpler != query {
            variants.push(simpler);
        }
    }
    if max_attempts >= 6 && !domain_hint.trim().is_empty() {
        variants.push(format!("{domain_hint} {query}"));
    }
    variants.truncate(max_attempts);
    variants
}

/// Drop short connectors / stopwords to produce a keywords-only variant.
fn simplify_query(q: &str) -> String {
    const STOP: &[&str] = &[
        "the", "a", "an", "is", "are", "of", "in", "on", "to", "for", "and",
        "or", "but", "with", "about", "about",
        // Korean particles that rarely help search
        "은", "는", "이", "가", "을", "를", "와", "과", "에", "에서",
    ];
    q.split_whitespace()
        .filter(|w| {
            let lw = w.to_ascii_lowercase();
            !STOP.iter().any(|s| lw == *s || lw.ends_with(s))
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Keep trace lines readable — cap per-query length.
fn truncate_for_trace(q: &str) -> String {
    const MAX: usize = 80;
    if q.chars().count() <= MAX {
        q.to_string()
    } else {
        let taken: String = q.chars().take(MAX).collect();
        format!("{taken}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Stub tool whose replies are preset per call index. Panics on
    /// over-execution so tests can assert exact invocation counts.
    struct StubTool {
        stub_name: &'static str,
        calls: AtomicUsize,
        replies: Vec<ToolResult>,
    }

    impl StubTool {
        fn new(name: &'static str, replies: Vec<ToolResult>) -> Arc<Self> {
            Arc::new(Self {
                stub_name: name,
                calls: AtomicUsize::new(0),
                replies,
            })
        }
        fn call_count(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl Tool for StubTool {
        fn name(&self) -> &str {
            self.stub_name
        }
        fn description(&self) -> &str {
            "stub"
        }
        fn parameters_schema(&self) -> serde_json::Value {
            json!({})
        }
        async fn execute(&self, _args: serde_json::Value) -> anyhow::Result<ToolResult> {
            let i = self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(self.replies.get(i).cloned().unwrap_or_else(|| ToolResult {
                success: false,
                output: String::new(),
                error: Some("stub exhausted".into()),
            }))
        }
    }

    fn result_ok(chars: usize) -> ToolResult {
        ToolResult {
            success: true,
            output: "x".repeat(chars),
            error: None,
        }
    }

    fn result_empty() -> ToolResult {
        ToolResult {
            success: true,
            output: String::new(),
            error: None,
        }
    }

    #[tokio::test]
    async fn free_tier_sufficient_stops_cascade() {
        let free = StubTool::new("web_search", vec![result_ok(600)]);
        let perp = StubTool::new("perplexity_search", vec![]);
        let tool = SmartSearchTool::new(free.clone(), Some(perp.clone()));
        let res = tool.execute(json!({"query": "hello world"})).await.unwrap();
        assert!(res.success);
        assert_eq!(free.call_count(), 1, "free tier should answer alone");
        assert_eq!(perp.call_count(), 0, "perplexity should not be called");
    }

    #[tokio::test]
    async fn complex_topic_always_consults_perplexity() {
        let free = StubTool::new("web_search", vec![result_ok(600)]);
        let perp = StubTool::new("perplexity_search", vec![result_ok(700)]);
        let tool = SmartSearchTool::new(free.clone(), Some(perp.clone()));
        let res = tool
            .execute(json!({"query": "대법원 판례 분석"}))
            .await
            .unwrap();
        assert!(res.success);
        assert!(perp.call_count() >= 1, "Korean 판례 must trigger perplexity");
    }

    #[tokio::test]
    async fn force_premium_skips_free_tier() {
        let free = StubTool::new("web_search", vec![result_ok(600)]);
        let perp = StubTool::new("perplexity_search", vec![result_ok(600)]);
        let tool = SmartSearchTool::new(free.clone(), Some(perp.clone()));
        let _ = tool
            .execute(json!({"query": "hello", "force_premium": true}))
            .await
            .unwrap();
        assert_eq!(free.call_count(), 0);
        assert_eq!(perp.call_count(), 1);
    }

    #[tokio::test]
    async fn cascade_exhausts_after_max_attempts() {
        // All tiers return empty → cascade exhausts and reports failure.
        let free = StubTool::new("web_search", vec![result_empty(); 6]);
        let perp = StubTool::new("perplexity_search", vec![result_empty(); 6]);
        let tool = SmartSearchTool::new(free.clone(), Some(perp.clone()));
        let res = tool
            .execute(json!({"query": "xyzzy nonexistent", "max_attempts": 3}))
            .await
            .unwrap();
        assert!(!res.success);
        assert!(res.error.as_deref().unwrap_or("").contains("cascade exhausted"));
        assert_eq!(free.call_count(), 3);
        assert_eq!(perp.call_count(), 3);
    }

    #[tokio::test]
    async fn no_perplexity_falls_back_to_free_only() {
        let free = StubTool::new("web_search", vec![result_ok(600)]);
        let tool = SmartSearchTool::new(free.clone(), None);
        let res = tool
            .execute(json!({"query": "simple query"}))
            .await
            .unwrap();
        assert!(res.success);
        assert_eq!(free.call_count(), 1);
    }

    #[tokio::test]
    async fn max_attempts_clamped_to_hard_cap() {
        let free = StubTool::new("web_search", vec![result_empty(); 20]);
        let tool = SmartSearchTool::new(free.clone(), None);
        let _ = tool
            .execute(json!({"query": "nope", "max_attempts": 100}))
            .await
            .unwrap();
        assert!(free.call_count() <= HARD_ATTEMPT_CAP);
    }

    #[tokio::test]
    async fn empty_query_is_rejected() {
        let free = StubTool::new("web_search", vec![]);
        let tool = SmartSearchTool::new(free.clone(), None);
        let res = tool.execute(json!({"query": ""})).await.unwrap();
        assert!(!res.success);
        assert!(res.error.as_deref().unwrap_or("").contains("empty query"));
        assert_eq!(free.call_count(), 0);
    }

    #[tokio::test]
    async fn partial_results_returned_when_threshold_missed() {
        // Both tiers return thin (< 500 chars) but non-empty output —
        // cascade exhausts attempts but still surfaces what it found.
        let free = StubTool::new(
            "web_search",
            vec![result_ok(100), result_ok(100), result_ok(100), result_ok(100)],
        );
        let tool = SmartSearchTool::new(free.clone(), None);
        let res = tool
            .execute(json!({"query": "sparse topic"}))
            .await
            .unwrap();
        assert!(res.success, "partial results should still succeed");
        assert!(res.output.contains("partial"));
    }

    #[test]
    fn complex_topic_detection_english_hints() {
        assert!(is_complex_topic("compile error E0425", ""));
        assert!(is_complex_topic("peer reviewed paper", ""));
        assert!(is_complex_topic("RFC 2119", ""));
    }

    #[test]
    fn complex_topic_detection_korean_hints() {
        assert!(is_complex_topic("대법원 판례", ""));
        assert!(is_complex_topic("임상시험 결과", ""));
        assert!(!is_complex_topic("오늘 서울 날씨", ""));
    }

    #[test]
    fn complex_topic_detection_via_domain_hint() {
        assert!(is_complex_topic("anything at all", "legal"));
        assert!(is_complex_topic("anything at all", "MEDICAL"));
        assert!(!is_complex_topic("anything at all", "weather"));
    }

    #[test]
    fn query_variants_respect_max_attempts() {
        let v = build_query_variants("test query", "", 2);
        assert_eq!(v.len(), 2);
        // Can be ≤ 6 when simplification produces a duplicate of the
        // original — we still cap at max_attempts but don't pad with
        // noise if a variant would be redundant.
        let v = build_query_variants("test query", "legal", 6);
        assert!(
            v.len() >= 5 && v.len() <= 6,
            "expected 5-6 variants, got {}",
            v.len()
        );
    }

    #[test]
    fn simplify_query_strips_stopwords() {
        assert_eq!(simplify_query("what is the meaning of life"), "what meaning life");
        // Korean particles get trimmed too
        let k = simplify_query("서울 의 날씨 는 어때");
        assert!(!k.is_empty());
    }
}
