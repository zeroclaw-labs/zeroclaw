//! Advisor Strategy — higher-intelligence model consulted at critical moments.
//!
//! ## Pattern origin
//!
//! Adapted from Anthropic's "Advisor Strategy"
//! (<https://claude.com/blog/the-advisor-strategy>) and the `advisor-opus`
//! Claude Code plugin (<https://github.com/Kimjaechol/advisor-opus>). The
//! core idea: pair a fast executor with a higher-intelligence advisor and
//! consult the advisor at a small number of critical checkpoints.
//!
//! ## MoA-specific mapping
//!
//! - **Executor** = on-device Gemma 4 SLM (see `src/gatekeeper/router.rs`).
//!   In this phase the executor for tool-requiring tasks is still the
//!   existing LLM agent loop (`src/agent`); SLM-as-full-executor with
//!   prompt-guided tool calling is a follow-up track.
//! - **Advisor** = the user's highest-tier LLM (Claude Opus 4.7 / GPT-5.4 /
//!   Gemini 4.1 Pro). The advisor uses the same Provider trait and the
//!   same user-key → operator-key (2.2×) routing as every other LLM call.
//!
//! ## Three checkpoints
//!
//! 1. **Plan** — before execution. Advisor produces a structured plan
//!    (end state / critical path / risks / first move).
//! 2. **Review** — after execution, before returning to user. Advisor
//!    audits the result for correctness, architecture, security, silent
//!    failures.
//! 3. **Advise** — when the executor is stuck or needs a direction pivot.
//!    Ad-hoc consultation, returned verbatim.
//!
//! ## Budget
//!
//! Target: 2 advisor calls per complex task (Plan + Review). Max 4 with
//! Advise retries. Trivial (`TaskCategory::Simple`) tasks skip the
//! advisor entirely — the executor SLM handles them directly. Policy is
//! encoded in [`AdvisorPolicy::for_category`].

pub mod prompts;
pub mod slm_executor;
pub mod types;

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::gatekeeper::router::TaskCategory;
use crate::providers::Provider;

// Some of these are only consumed by callers we haven't wired yet
// (WS gatekeeper, richer telemetry). Keep the re-export set stable so
// downstream consumers don't have to touch this line as integrations land.
#[allow(unused_imports)]
pub use slm_executor::{RunOutcome as SlmRunOutcome, SlmExecutor};
#[allow(unused_imports)]
pub use types::{AdvisorCheckpoint, AdvisorRequest, PlanOutput, ReviewOutput, ReviewVerdict, TaskKind};

/// Invocation policy for a given [`TaskCategory`].
///
/// Drives whether the advisor is called at each of the three checkpoints.
/// The mapping is intentionally small and explicit rather than model-driven
/// — cost-predictable for the user, debuggable for maintainers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdvisorPolicy {
    /// Whether to call the advisor with a PLAN request before execution.
    pub plan: bool,
    /// Whether to call the advisor with a REVIEW request after execution
    /// and before returning the result to the user.
    pub review: bool,
    /// Whether to consult the advisor if the executor reports a stuck /
    /// pivot state mid-execution. Off for most categories — only
    /// tool-heavy multi-step work (coding, specialized tooling) benefits.
    pub pivot: bool,
}

impl AdvisorPolicy {
    /// Pick the policy for a gatekeeper-classified category.
    ///
    /// - `Simple`: trivial; advisor adds latency and cost for no gain.
    /// - `Medium`: tool-assisted local response; review only.
    /// - `Complex`: cloud LLM reasoning needed; plan + review.
    /// - `Specialized`: tool-heavy domain work; plan + review + pivot.
    #[must_use]
    pub const fn for_category(category: TaskCategory) -> Self {
        match category {
            TaskCategory::Simple => Self { plan: false, review: false, pivot: false },
            TaskCategory::Medium => Self { plan: false, review: true, pivot: false },
            TaskCategory::Complex => Self { plan: true, review: true, pivot: false },
            TaskCategory::Specialized => Self { plan: true, review: true, pivot: true },
        }
    }

    /// Whether any advisor call is required.
    #[must_use]
    pub const fn any(self) -> bool {
        self.plan || self.review || self.pivot
    }
}

/// Handle to the configured advisor LLM.
///
/// Not a trait — a thin wrapper around a [`Provider`] that knows its
/// model id and temperature. Constructed at gateway boot from
/// [`crate::config::AdvisorConfig`] (see `src/config/schema.rs`), stored
/// on [`crate::gateway::AppState`], and shared across every chat handler
/// via `Arc`.
pub struct AdvisorClient {
    provider: Arc<dyn Provider>,
    model: String,
    temperature: f64,
    timeout: Duration,
}

impl AdvisorClient {
    /// Construct a client around a provider and its top-tier model.
    #[must_use]
    pub fn new(provider: Arc<dyn Provider>, model: impl Into<String>, temperature: f64) -> Self {
        Self {
            provider,
            model: model.into(),
            temperature,
            timeout: Duration::from_secs(60),
        }
    }

    /// Override the per-call timeout. Default is 60s, which covers
    /// Opus-class PLAN requests (~20s) with generous headroom.
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Advisor model id in use (for logs and response metadata).
    #[must_use]
    pub fn model(&self) -> &str {
        &self.model
    }

    /// Ask the advisor for a pre-execution plan.
    pub async fn plan(&self, request: &AdvisorRequest<'_>) -> Result<PlanOutput> {
        let (system, user) = prompts::build_plan_prompt(request);
        let reply = self.chat(&system, &user).await?;
        PlanOutput::parse(&reply).context("parse advisor PLAN response")
    }

    /// Ask the advisor to review the executor's result before it ships
    /// back to the user. Returns a structured verdict — the caller
    /// decides whether to revise or pass through.
    pub async fn review(&self, request: &AdvisorRequest<'_>) -> Result<ReviewOutput> {
        let (system, user) = prompts::build_review_prompt(request);
        let reply = self.chat(&system, &user).await?;
        ReviewOutput::parse(&reply).context("parse advisor REVIEW response")
    }

    /// Ad-hoc strategic advice for stuck / pivot situations. Returned
    /// verbatim — callers forward to their executor without filtering.
    pub async fn advise(&self, request: &AdvisorRequest<'_>) -> Result<String> {
        let (system, user) = prompts::build_advise_prompt(request);
        self.chat(&system, &user).await
    }

    async fn chat(&self, system: &str, user: &str) -> Result<String> {
        let fut = self
            .provider
            .chat_with_system(Some(system), user, &self.model, self.temperature);
        tokio::time::timeout(self.timeout, fut)
            .await
            .map_err(|_| anyhow::anyhow!("advisor call timed out after {:?}", self.timeout))?
    }
}

/// Pick the highest-tier model id for a given provider family.
///
/// Used by the gateway to auto-populate `AdvisorConfig.model` when the
/// config has not been overridden by the user. Deliberately conservative
/// — if the provider is not in the known list, returns `None` and the
/// caller can decide whether to fall back to the default chat model
/// or disable the advisor.
#[must_use]
pub fn top_tier_model_for(provider: &str) -> Option<&'static str> {
    match provider.to_ascii_lowercase().as_str() {
        "anthropic" | "claude" => Some("claude-opus-4-7"),
        "openai" | "gpt" => Some("gpt-5.4"),
        "gemini" | "google" | "google-gemini" => Some("gemini-4.1-pro"),
        "deepseek" => Some("deepseek-r1-pro"),
        "groq" => Some("llama-4-70b-versatile"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_skips_simple_tasks() {
        let p = AdvisorPolicy::for_category(TaskCategory::Simple);
        assert!(!p.any(), "Simple tasks must never invoke the advisor");
    }

    #[test]
    fn policy_always_reviews_non_simple_tasks() {
        for cat in [TaskCategory::Medium, TaskCategory::Complex, TaskCategory::Specialized] {
            let p = AdvisorPolicy::for_category(cat);
            assert!(p.review, "{cat:?} must at minimum review before returning");
        }
    }

    #[test]
    fn policy_plans_for_complex_and_specialized() {
        assert!(AdvisorPolicy::for_category(TaskCategory::Complex).plan);
        assert!(AdvisorPolicy::for_category(TaskCategory::Specialized).plan);
        assert!(!AdvisorPolicy::for_category(TaskCategory::Medium).plan);
    }

    #[test]
    fn policy_allows_pivot_only_for_specialized() {
        assert!(!AdvisorPolicy::for_category(TaskCategory::Simple).pivot);
        assert!(!AdvisorPolicy::for_category(TaskCategory::Medium).pivot);
        assert!(!AdvisorPolicy::for_category(TaskCategory::Complex).pivot);
        assert!(AdvisorPolicy::for_category(TaskCategory::Specialized).pivot);
    }

    #[test]
    fn top_tier_lookup_handles_common_aliases() {
        assert_eq!(top_tier_model_for("anthropic"), Some("claude-opus-4-7"));
        assert_eq!(top_tier_model_for("Claude"), Some("claude-opus-4-7"));
        assert_eq!(top_tier_model_for("openai"), Some("gpt-5.4"));
        assert_eq!(top_tier_model_for("Google-Gemini"), Some("gemini-4.1-pro"));
        assert_eq!(top_tier_model_for("random-unknown"), None);
    }
}
