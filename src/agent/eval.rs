use serde::{Deserialize, Serialize};

use schemars::JsonSchema;
use zeroclaw_macros::Configurable;

// ── Complexity estimation ───────────────────────────────────────

/// Coarse complexity tier for a user message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComplexityTier {
    /// Short, simple query (greetings, yes/no, lookups).
    Simple,
    /// Typical request — not trivially simple, not deeply complex.
    Standard,
    /// Long or reasoning-heavy request (code, multi-step, analysis).
    Complex,
}

/// Heuristic keywords that signal reasoning complexity.
const REASONING_KEYWORDS: &[&str] = &[
    "explain",
    "why",
    "analyze",
    "compare",
    "design",
    "implement",
    "refactor",
    "debug",
    "optimize",
    "architecture",
    "trade-off",
    "tradeoff",
    "reasoning",
    "step by step",
    "think through",
    "evaluate",
    "critique",
    "pros and cons",
];

/// Estimate the complexity of a user message without an LLM call.
///
/// Rules (applied in order):
/// - **Complex**: message > 200 chars, OR contains a code fence, OR ≥ 2
///   reasoning keywords.
/// - **Simple**: message < 50 chars AND no reasoning keywords.
/// - **Standard**: everything else.
pub fn estimate_complexity(message: &str) -> ComplexityTier {
    let lower = message.to_lowercase();
    let len = message.len();

    let keyword_count = REASONING_KEYWORDS
        .iter()
        .filter(|kw| lower.contains(**kw))
        .count();

    let has_code_fence = message.contains("```");

    if len > 200 || has_code_fence || keyword_count >= 2 {
        return ComplexityTier::Complex;
    }

    if len < 50 && keyword_count == 0 {
        return ComplexityTier::Simple;
    }

    ComplexityTier::Standard
}

// ── Auto-classify config ────────────────────────────────────────

/// Configuration for automatic complexity-based classification.
///
/// When the rule-based classifier in `QueryClassificationConfig` produces no
/// match, the eval layer can fall back to `estimate_complexity` and map the
/// resulting tier to a routing hint.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Configurable)]
#[prefix = "agent.auto-classify"]
pub struct AutoClassifyConfig {
    /// Hint to use for `Simple` complexity tier (e.g. `"fast"`).
    #[serde(default)]
    pub simple_hint: Option<String>,
    /// Hint to use for `Standard` complexity tier.
    #[serde(default)]
    pub standard_hint: Option<String>,
    /// Hint to use for `Complex` complexity tier (e.g. `"reasoning"`).
    #[serde(default)]
    pub complex_hint: Option<String>,
    /// Hint prefix for cost-optimized routing (default: `"cost-optimized"`).
    #[serde(default = "default_cost_optimized_hint")]
    pub cost_optimized_hint: String,
}

fn default_cost_optimized_hint() -> String {
    "cost-optimized".to_string()
}

impl Default for AutoClassifyConfig {
    fn default() -> Self {
        Self {
            simple_hint: None,
            standard_hint: None,
            complex_hint: None,
            cost_optimized_hint: default_cost_optimized_hint(),
        }
    }
}

impl AutoClassifyConfig {
    /// Map a complexity tier to the configured hint, if any.
    pub fn hint_for(&self, tier: ComplexityTier) -> Option<&str> {
        match tier {
            ComplexityTier::Simple => self.simple_hint.as_deref(),
            ComplexityTier::Standard => self.standard_hint.as_deref(),
            ComplexityTier::Complex => self.complex_hint.as_deref(),
        }
    }
}

// ── Post-response eval ──────────────────────────────────────────

/// Configuration for the post-response quality evaluator.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Configurable)]
#[prefix = "agent.eval"]
pub struct EvalConfig {
    /// Enable the eval quality gate.
    #[serde(default)]
    pub enabled: bool,
    /// Minimum quality score (0.0–1.0) to accept a response.
    /// Below this threshold, a retry with a higher-tier model is suggested.
    #[serde(default = "default_min_quality_score")]
    pub min_quality_score: f64,
    /// Maximum retries with escalated models before accepting whatever we get.
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,
}

fn default_min_quality_score() -> f64 {
    0.5
}

fn default_max_retries() -> u32 {
    1
}

impl Default for EvalConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            min_quality_score: default_min_quality_score(),
            max_retries: default_max_retries(),
        }
    }
}

/// Result of evaluating a response against quality heuristics.
#[derive(Debug, Clone)]
pub struct EvalResult {
    /// Aggregate quality score from 0.0 (terrible) to 1.0 (excellent).
    pub score: f64,
    /// Individual check outcomes (for observability).
    pub checks: Vec<EvalCheck>,
    /// If score < threshold, the suggested higher-tier hint for retry.
    pub retry_hint: Option<String>,
}

#[derive(Debug, Clone)]
pub struct EvalCheck {
    pub name: &'static str,
    pub passed: bool,
    pub weight: f64,
}

/// Code-related keywords in user queries.
const CODE_KEYWORDS: &[&str] = &[
    "code",
    "function",
    "implement",
    "class",
    "struct",
    "module",
    "script",
    "program",
    "bug",
    "error",
    "compile",
    "syntax",
    "refactor",
];

/// Evaluate a response against heuristic quality checks. No LLM call.
///
/// Checks:
/// 1. **Non-empty**: response must not be empty.
/// 2. **Not a cop-out**: response must not be just "I don't know" or similar.
/// 3. **Sufficient length**: response length should be proportional to query complexity.
/// 4. **Code presence**: if the query mentions code keywords, the response should
///    contain a code block.
pub fn evaluate_response(
    query: &str,
    response: &str,
    complexity: ComplexityTier,
    auto_classify: Option<&AutoClassifyConfig>,
) -> EvalResult {
    let mut checks = Vec::new();

    // Check 1: Non-empty
    let non_empty = !response.trim().is_empty();
    checks.push(EvalCheck {
        name: "non_empty",
        passed: non_empty,
        weight: 0.3,
    });

    // Check 2: Not a cop-out
    let lower_resp = response.to_lowercase();
    let cop_out_phrases = [
        "i don't know",
        "i'm not sure",
        "i cannot",
        "i can't help",
        "as an ai",
    ];
    let is_cop_out = cop_out_phrases
        .iter()
        .any(|phrase| lower_resp.starts_with(phrase));
    let not_cop_out = !is_cop_out || response.len() > 200; // long responses with caveats are fine
    checks.push(EvalCheck {
        name: "not_cop_out",
        passed: not_cop_out,
        weight: 0.25,
    });

    // Check 3: Sufficient length for complexity
    let min_len = match complexity {
        ComplexityTier::Simple => 5,
        ComplexityTier::Standard => 20,
        ComplexityTier::Complex => 50,
    };
    let sufficient_length = response.len() >= min_len;
    checks.push(EvalCheck {
        name: "sufficient_length",
        passed: sufficient_length,
        weight: 0.2,
    });

    // Check 4: Code presence when expected
    let query_lower = query.to_lowercase();
    let expects_code = CODE_KEYWORDS.iter().any(|kw| query_lower.contains(kw));
    let has_code = response.contains("```") || response.contains("    "); // code block or indented
    let code_check_passed = !expects_code || has_code;
    checks.push(EvalCheck {
        name: "code_presence",
        passed: code_check_passed,
        weight: 0.25,
    });

    // Compute weighted score
    let total_weight: f64 = checks.iter().map(|c| c.weight).sum();
    let earned: f64 = checks.iter().filter(|c| c.passed).map(|c| c.weight).sum();
    let score = if total_weight > 0.0 {
        earned / total_weight
    } else {
        1.0
    };

    // Determine retry hint: if score is low, suggest escalating
    let retry_hint = if score <= default_min_quality_score() {
        // Try to escalate: Simple→Standard→Complex
        let next_tier = match complexity {
            ComplexityTier::Simple => Some(ComplexityTier::Standard),
            ComplexityTier::Standard => Some(ComplexityTier::Complex),
            ComplexityTier::Complex => None, // already at max
        };
        next_tier.and_then(|tier| {
            auto_classify
                .and_then(|ac| ac.hint_for(tier))
                .map(String::from)
        })
    } else {
        None
    };

    EvalResult {
        score,
        checks,
        retry_hint,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── estimate_complexity ─────────────────────────────────────

    #[test]
    fn simple_short_message() {
        assert_eq!(estimate_complexity("hi"), ComplexityTier::Simple);
        assert_eq!(estimate_complexity("hello"), ComplexityTier::Simple);
        assert_eq!(estimate_complexity("yes"), ComplexityTier::Simple);
    }

    #[test]
    fn complex_long_message() {
        let long = "a".repeat(201);
        assert_eq!(estimate_complexity(&long), ComplexityTier::Complex);
    }

    #[test]
    fn complex_code_fence() {
        let msg = "Here is some code:\n```rust\nfn main() {}\n```";
        assert_eq!(estimate_complexity(msg), ComplexityTier::Complex);
    }

    #[test]
    fn complex_multiple_reasoning_keywords() {
        let msg = "Please explain why this design is better and analyze the trade-off";
        assert_eq!(estimate_complexity(msg), ComplexityTier::Complex);
    }

    #[test]
    fn standard_medium_message() {
        // 50+ chars but no code fence, < 2 reasoning keywords
        let msg = "Can you help me find a good restaurant in this area please?";
        assert_eq!(estimate_complexity(msg), ComplexityTier::Standard);
    }

    #[test]
    fn standard_short_with_one_keyword() {
        // < 50 chars but has 1 reasoning keyword → still not Simple
        let msg = "explain this";
        assert_eq!(estimate_complexity(msg), ComplexityTier::Standard);
    }

    // ── auto_classify ───────────────────────────────────────────

    #[test]
    fn auto_classify_maps_tiers_to_hints() {
        let ac = AutoClassifyConfig {
            simple_hint: Some("fast".into()),
            standard_hint: None,
            complex_hint: Some("reasoning".into()),
            ..Default::default()
        };
        assert_eq!(ac.hint_for(ComplexityTier::Simple), Some("fast"));
        assert_eq!(ac.hint_for(ComplexityTier::Standard), None);
        assert_eq!(ac.hint_for(ComplexityTier::Complex), Some("reasoning"));
    }

    // ── evaluate_response ───────────────────────────────────────

    #[test]
    fn empty_response_scores_low() {
        let result = evaluate_response("hello", "", ComplexityTier::Simple, None);
        assert!(result.score <= 0.5, "empty response should score low");
    }

    #[test]
    fn good_response_scores_high() {
        let result = evaluate_response(
            "what is 2+2?",
            "The answer is 4.",
            ComplexityTier::Simple,
            None,
        );
        assert!(
            result.score >= 0.9,
            "good simple response should score high, got {}",
            result.score
        );
    }

    #[test]
    fn cop_out_response_penalized() {
        let result = evaluate_response(
            "explain quantum computing",
            "I don't know much about that.",
            ComplexityTier::Standard,
            None,
        );
        assert!(
            result.score < 1.0,
            "cop-out should be penalized, got {}",
            result.score
        );
    }

    #[test]
    fn code_query_without_code_response_penalized() {
        let result = evaluate_response(
            "write a function to sort an array",
            "You should use a sorting algorithm.",
            ComplexityTier::Standard,
            None,
        );
        // "code_presence" check should fail
        let code_check = result.checks.iter().find(|c| c.name == "code_presence");
        assert!(
            code_check.is_some() && !code_check.unwrap().passed,
            "code check should fail"
        );
    }

    #[test]
    fn retry_hint_escalation() {
        let ac = AutoClassifyConfig {
            simple_hint: Some("fast".into()),
            standard_hint: Some("default".into()),
            complex_hint: Some("reasoning".into()),
            ..Default::default()
        };
        // Empty response for a Simple query → should suggest Standard hint
        let result = evaluate_response("hello", "", ComplexityTier::Simple, Some(&ac));
        assert_eq!(result.retry_hint, Some("default".into()));
    }

    #[test]
    fn no_retry_when_already_complex() {
        let ac = AutoClassifyConfig {
            simple_hint: Some("fast".into()),
            standard_hint: Some("default".into()),
            complex_hint: Some("reasoning".into()),
            ..Default::default()
        };
        // Empty response for Complex → no escalation possible
        let result =
            evaluate_response("explain everything", "", ComplexityTier::Complex, Some(&ac));
        assert_eq!(result.retry_hint, None);
    }

    #[test]
    fn max_retries_defaults() {
        let config = EvalConfig::default();
        assert!(!config.enabled);
        assert_eq!(config.max_retries, 1);
        assert!((config.min_quality_score - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn cost_optimized_hint_default() {
        let config = AutoClassifyConfig::default();
        assert_eq!(config.cost_optimized_hint, "cost-optimized");
    }
}
