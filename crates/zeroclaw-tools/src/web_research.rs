//! Web research delegate tool.
//!
//! The main agent asks a question; a bounded sub-agent loop does
//! search → fetch → distill against the configured `[web_search]` backend and
//! returns a written summary with a mandatory `Sources:` list. Raw
//! search-engine result text never reaches the primary context window — only
//! the distilled answer does.
//!
//! # Why this is a self-contained loop
//!
//! `zeroclaw-runtime` owns the full turn engine (`run_tool_call_loop`), but
//! this crate cannot depend on runtime — runtime depends on tool
//! implementations (see `crate::i18n` module docs). The two tools being scoped
//! (`web_search_tool`, `web_fetch`) live here, so the delegate lives here too
//! and drives the provider directly. `run_tool_call_loop` also returns only a
//! `String`, with no tool-call trace, so the deterministic source-harvesting
//! guarantee below would not be expressible through it.
//!
//! # Bounds
//!
//! Every run is capped on two independent axes — [`ResearchBounds`] — and both
//! degrade to a best-effort partial answer rather than an error: whatever was
//! gathered is still distilled and returned with its sources. The wall clock
//! bounds nested tool calls as well as provider calls, so a slow page cannot
//! outlive the run.
//!
//! # Runtime seams
//!
//! Two things a sub-agent needs live in `zeroclaw-runtime` and arrive here as
//! injected values rather than as a dependency edge: the spend budget and cost
//! ledger ([`SubAgentMeter`]) and the alias-resolved model binding
//! ([`SubAgentBinding`]). Both are supplied at registration; neither has a
//! production default.
//!
//! # Trust
//!
//! The fetch ledger, not the model's prose, decides what counts as a source. A
//! URL is recorded only after its fetch succeeds, and the final `Sources:`
//! section is rebuilt from that ledger on every run.

use async_trait::async_trait;
use regex::Regex;
use serde_json::json;
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};
use zeroclaw_api::model_provider::{
    ChatMessage, ChatRequest, ChatResponse, ModelProvider, TokenUsage,
};
use zeroclaw_api::tool::{Tool, ToolOutput, ToolResult, ToolSpec};
use zeroclaw_config::policy::{SecurityPolicy, ToolOperation};
use zeroclaw_config::schema::Config;
use zeroclaw_providers::{ModelProviderRuntimeOptions, ProviderDispatch};

/// Tool name, used for registration, policy lookup, and auto-approve matching.
pub const NAME: &str = "web_research";

/// The tools a research sub-agent may ever be scoped to. Both are read-only,
/// which is what lets the delegate run under a readonly risk profile.
pub const SCOPED_TOOL_NAMES: [&str; 2] = ["web_search_tool", "web_fetch"];

/// Maximum tool calls one research run may execute. Counted per executed call
/// (not per model round) because each call is what costs a network round-trip.
const MAX_TOOL_CALLS: usize = 8;

/// Hard wall-clock ceiling for one research run.
const WALL_CLOCK_SECS: u64 = 180;

/// Budget for the final "wrap up now" call after the loop stops. Separate from
/// the main budget so a run that stopped *because* it timed out can still
/// produce a partial summary.
const WRAPUP_SECS: u64 = 45;

/// Cap on how much of a single tool result is fed back into the sub-agent's
/// history, so one huge page cannot blow the sub-context.
const MAX_TOOL_RESULT_CHARS: usize = 12_000;

static URL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"https?://[^\s\)\]\},"'`<>]+"#).expect("static URL regex is valid")
});

/// The two independent limits on a research run.
#[derive(Debug, Clone, Copy)]
pub struct ResearchBounds {
    pub max_tool_calls: usize,
    pub wall_clock: Duration,
}

impl Default for ResearchBounds {
    fn default() -> Self {
        Self {
            max_tool_calls: MAX_TOOL_CALLS,
            wall_clock: Duration::from_secs(WALL_CLOCK_SECS),
        }
    }
}

/// Runtime-owned metering seam for the sub-agent's model calls.
///
/// The cost tracker, its pricing table, and the shared spend budget all live in
/// `zeroclaw-runtime`, which this crate cannot depend on (runtime depends on
/// tool implementations). Registration therefore injects the real
/// implementation as a trait object, exactly as it injects the scoped tool
/// handles. Every nested provider call goes through [`metered_chat`], so there
/// is no path to the model that skips this seam.
pub trait SubAgentMeter: Send + Sync {
    /// Enforce the shared tool-loop spend budget *before* a model call.
    ///
    /// `Err` stops the research run — the sub-agent must not be able to spend
    /// past a limit the main loop would have respected.
    fn enforce_budget(&self) -> Result<(), String>;

    /// Record the usage a model call reported, *after* it returns.
    fn record_usage(&self, usage: &TokenUsage);
}

/// Why the research loop stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopReason {
    /// The sub-agent answered without requesting more tools.
    Completed,
    /// The tool-call budget was exhausted.
    MaxToolCalls,
    /// The wall-clock budget was exhausted.
    Timeout,
    /// The provider failed; whatever was gathered is still returned.
    ProviderError(String),
    /// The shared spend budget was exhausted, so no further model call was
    /// made. Whatever was gathered is still returned.
    BudgetExceeded(String),
    /// The provider cannot do native tool calls, so a single deterministic
    /// search/fetch + distill pass ran instead of an agentic loop.
    SinglePass,
}

impl StopReason {
    /// True when the run ended early and the summary is best-effort partial.
    #[must_use]
    pub fn is_partial(&self) -> bool {
        matches!(
            self,
            Self::MaxToolCalls | Self::Timeout | Self::ProviderError(_) | Self::BudgetExceeded(_)
        )
    }

    /// Stable machine-readable token for this outcome.
    ///
    /// Goes into the partial preamble the main agent reads, so a caller can
    /// branch on *why* a briefing is incomplete without parsing prose.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::MaxToolCalls => "max_tool_calls",
            Self::Timeout => "timeout",
            Self::ProviderError(_) => "provider_error",
            Self::BudgetExceeded(_) => "budget_exceeded",
            Self::SinglePass => "single_pass",
        }
    }
}

/// Result of one research run.
#[derive(Debug, Clone)]
pub struct ResearchOutcome {
    pub summary: String,
    /// Pages actually retrieved. Ground truth for "we read this".
    pub sources: Vec<String>,
    /// Fetches that were attempted and did not succeed — SSRF-rejected,
    /// non-2xx, transport failures. Never cited as a source; used only to
    /// annotate a model citation that names one of them.
    pub failed_fetches: Vec<String>,
    pub tool_calls_used: usize,
    pub stop_reason: StopReason,
}

/// Tracks the URLs a run may legitimately cite.
///
/// `fetched` is ground truth — a page the sub-agent actually retrieved, so an
/// entry lands here only *after* the tool reports success. `attempted_failed`
/// holds URLs whose fetch was tried and failed; recording those as sources
/// would let an SSRF rejection or a 404 masquerade as a read page and make an
/// empty run look sourced.
#[derive(Debug, Default)]
struct SourceLedger {
    fetched: Vec<String>,
    attempted_failed: Vec<String>,
}

impl SourceLedger {
    /// Record a page as retrieved. Callers must only reach this after the
    /// fetch tool reported success.
    fn record_fetched(&mut self, url: &str) {
        push_unique(&mut self.fetched, url.trim());
    }

    /// Record a fetch that was attempted and did not succeed.
    fn record_failed_fetch(&mut self, url: &str) {
        push_unique(&mut self.attempted_failed, url.trim());
    }
}

fn push_unique(list: &mut Vec<String>, value: &str) {
    let value = normalize_url(value);
    if !value.is_empty() && !list.iter().any(|existing| existing == &value) {
        list.push(value);
    }
}

/// Trim the trailing punctuation that sentence context glues onto a URL, so a
/// citation and a ledger entry for the same page compare equal.
fn normalize_url(value: &str) -> String {
    value
        .trim()
        .trim_end_matches(['.', ',', ';'])
        .trim()
        .to_string()
}

/// Build the sub-agent's system prompt.
///
/// Pure and public to the crate so the prompt contract (decomposition guidance
/// plus the non-negotiable `Sources:` mandate) is directly testable.
fn build_system_prompt(bounds: ResearchBounds, has_start_url: bool) -> String {
    let mut prompt = String::from(
        "You are a focused web-research sub-agent. Your only job is to answer the \
         user's research question from live web sources and return a written \
         briefing. You are not conversing with the user; you produce one final \
         answer.\n\n\
         ## How to search\n\n\
         - Decompose the question into several small, specific queries. Multiple \
         narrow searches beat one long stuffed query — search engines match \
         keywords, not sentences.\n\
         - Start broad to find the authoritative sources, then search again with \
         the precise terms, names, versions, or error strings you discovered.\n\
         - When a search result looks authoritative, fetch the page to read it. \
         Do not summarize from search snippets alone when the full page is \
         available.\n\
         - Prefer primary sources (official docs, specs, release notes, the \
         project's own repository) over aggregators and SEO content.\n\
         - If results contradict each other, say so explicitly and cite both.\n\n",
    );

    if has_start_url {
        prompt.push_str(
            "## Starting point\n\n\
             The user supplied a starting URL. Fetch and read it first, then \
             search only if it does not fully answer the question.\n\n",
        );
    }

    prompt.push_str(&format!(
        "## Budget\n\n\
         You may make at most {} tool calls, and the whole run is capped at {} \
         seconds. Spend them deliberately. When the budget runs out you will be \
         asked to summarize immediately with whatever you have, so do not save \
         your conclusions for a final step that may never come.\n\n\
         ## Required output format\n\n\
         Write a direct, self-contained briefing that answers the question. Do \
         not describe your search process. Then end your reply with a section \
         that begins with the exact line `Sources:` followed by one `- <url>` \
         line per page you actually used.\n\n\
         Including the `Sources:` section is mandatory. Never cite a URL you did \
         not actually retrieve. If you could not find an answer, say so plainly \
         and still list whatever you consulted.",
        bounds.max_tool_calls,
        bounds.wall_clock.as_secs()
    ));

    prompt
}

/// Split a model-written briefing into `(body, cited_urls)`.
///
/// Everything from the model's own `Sources:` line onward is removed from the
/// body and mined for URLs. The model's section is never passed through
/// verbatim: it is a *claim* about what was read, and only the fetch ledger
/// knows what actually was.
fn split_model_sources(summary: &str) -> (String, Vec<String>) {
    let summary = summary.trim();
    let Some(index) = summary
        .lines()
        .position(|line| line.trim_start().to_lowercase().starts_with("sources:"))
    else {
        return (summary.to_string(), Vec::new());
    };

    let body: Vec<&str> = summary.lines().take(index).collect();
    let tail: Vec<&str> = summary.lines().skip(index).collect();

    let mut cited = Vec::new();
    for m in URL_RE.find_iter(&tail.join("\n")) {
        push_unique(&mut cited, m.as_str());
    }

    (body.join("\n").trim_end().to_string(), cited)
}

/// Rebuild the briefing's `Sources:` section from the fetch ledger.
///
/// The ledger is ground truth for "retrieved". A URL the model cited that the
/// ledger does not know is not dropped (that would hide a hallucinated or
/// unreachable citation) and not silently promoted to a source either — it is
/// listed under an explicit `Model-cited (unverified):` heading. Citations
/// naming a fetch that was attempted and failed are annotated as such, which is
/// exactly the case that used to masquerade as a retrieved page.
fn rebuild_sources_section(
    summary: &str,
    retrieved: &[String],
    failed_fetches: &[String],
) -> String {
    let (body, cited) = split_model_sources(summary);

    let mut out = body;
    if !out.is_empty() {
        out.push_str("\n\n");
    }

    out.push_str("Sources:\n");
    if retrieved.is_empty() {
        out.push_str("- (none retrieved)");
    } else {
        for (index, url) in retrieved.iter().enumerate() {
            if index > 0 {
                out.push('\n');
            }
            out.push_str("- ");
            out.push_str(url);
        }
    }

    let unverified: Vec<&String> = cited
        .iter()
        .filter(|url| !retrieved.iter().any(|got| got == *url))
        .collect();
    if !unverified.is_empty() {
        out.push_str("\n\nModel-cited (unverified):\n");
        for (index, url) in unverified.iter().enumerate() {
            if index > 0 {
                out.push('\n');
            }
            out.push_str("- ");
            out.push_str(url);
            if failed_fetches.iter().any(|failed| &failed == url) {
                out.push_str(" (fetch failed)");
            }
        }
    }

    out
}

/// Preamble marking a briefing as incomplete, and why.
///
/// Bracketed marker in the *output* text, matching the repo's degraded-result
/// convention (`[Results truncated: ...]`, `[output truncated]`). It has to
/// live in the output rather than in `ToolResult::error` because the dispatcher
/// forwards only `output` to the model on a successful call and replaces it
/// wholesale with `Error: {error}` on a failed one — a briefing returned as a
/// failure would be discarded unread.
fn partial_notice(stop_reason: &StopReason) -> Option<String> {
    let detail = match stop_reason {
        StopReason::MaxToolCalls => "the research sub-agent hit its tool-call budget",
        StopReason::Timeout => "the research sub-agent hit its time budget",
        StopReason::ProviderError(_) => "the research sub-agent's model call failed",
        StopReason::BudgetExceeded(_) => "the research sub-agent hit the shared spend budget",
        StopReason::Completed | StopReason::SinglePass => return None,
    };
    Some(format!(
        "[partial: outcome={}; {detail}; the briefing below is based on what it \
         gathered so far]",
        stop_reason.code()
    ))
}

/// Render an outcome into the text the main agent receives.
fn render_outcome(outcome: &ResearchOutcome) -> String {
    let body = rebuild_sources_section(&outcome.summary, &outcome.sources, &outcome.failed_fetches);
    match partial_notice(&outcome.stop_reason) {
        Some(notice) => format!("{notice}\n\n{body}"),
        None => body,
    }
}

fn truncate_tool_result(text: &str) -> String {
    if text.chars().count() <= MAX_TOOL_RESULT_CHARS {
        return text.to_string();
    }
    let mut out: String = text.chars().take(MAX_TOOL_RESULT_CHARS).collect();
    out.push_str("\n... [truncated]");
    out
}

/// Everything a research run needs that is fixed for its whole duration.
///
/// Bundled so the loop, the wrap-up, and the single-pass fallback all take the
/// same context and cannot accidentally be handed a provider without its meter
/// or a tool set without its policy.
struct ResearchCtx<'a> {
    provider: &'a dyn ModelProvider,
    model: &'a str,
    temperature: Option<f64>,
    meter: &'a dyn SubAgentMeter,
    tools: &'a [Arc<dyn Tool>],
    security: &'a SecurityPolicy,
}

/// Result of one nested tool call.
enum ScopedCall {
    /// Text to feed back to the sub-agent as an observation.
    Observed(String),
    /// The run's wall clock expired before or during the call.
    DeadlineExpired,
}

/// Execute one sub-agent tool call and fold its result into the ledger.
///
/// `remaining` is the run's whole remaining wall clock, not a per-tool timeout:
/// a nested `web_fetch` can be configured with a long timeout of its own, and
/// eight of those would run far past the research deadline if each were allowed
/// its full budget.
async fn execute_scoped_call(
    ctx: &ResearchCtx<'_>,
    name: &str,
    args: &serde_json::Value,
    ledger: &mut SourceLedger,
    remaining: Duration,
) -> ScopedCall {
    // The nested handles are subject to the active profile's denylist exactly
    // as a registered tool is. Registration already filters the scope; this is
    // the execution-side half of the same decision, so a scope assembled
    // anywhere else cannot reach a tool the operator excluded.
    if ctx.security.is_tool_excluded(name) {
        return ScopedCall::Observed(format!(
            "Error: tool '{name}' is excluded by the active security policy and \
             cannot be used by the research sub-agent."
        ));
    }

    let Some(tool) = ctx.tools.iter().find(|t| t.name() == name) else {
        return ScopedCall::Observed(format!(
            "Error: tool '{name}' is not available to the research sub-agent. \
             Available: {}",
            ctx.tools
                .iter()
                .map(|t| t.name())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    };

    if remaining.is_zero() {
        return ScopedCall::DeadlineExpired;
    }

    let fetch_url = (name == "web_fetch")
        .then(|| args.get("url").and_then(|v| v.as_str()))
        .flatten();

    let Ok(executed) = tokio::time::timeout(remaining, tool.execute(args.clone())).await else {
        // An expired fetch is an attempt, not a retrieval.
        if let Some(url) = fetch_url {
            ledger.record_failed_fetch(url);
        }
        return ScopedCall::DeadlineExpired;
    };

    let observation = match executed {
        Ok(result) => {
            let text = result.output.to_string();
            if result.success {
                // Only a successful fetch is a source. Recording the target up
                // front would let an SSRF rejection or a 404 be cited as a page
                // the sub-agent read.
                if let Some(url) = fetch_url {
                    ledger.record_fetched(url);
                }
                truncate_tool_result(&text)
            } else {
                if let Some(url) = fetch_url {
                    ledger.record_failed_fetch(url);
                }
                format!(
                    "Error from {name}: {}",
                    result.error.unwrap_or_else(|| "unknown error".to_string())
                )
            }
        }
        Err(e) => {
            if let Some(url) = fetch_url {
                ledger.record_failed_fetch(url);
            }
            format!("Error from {name}: {e}")
        }
    };

    ScopedCall::Observed(observation)
}

/// Wall clock left before `deadline`, or `None` once it has passed.
fn remaining_before(deadline: Instant) -> Option<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|d| !d.is_zero())
}

/// Outcome of one metered model call.
enum MeteredChat {
    Response(Box<ChatResponse>),
    BudgetExceeded(String),
    ProviderError(String),
    Timeout,
}

/// One nested model call, metered on both sides.
///
/// The shared spend budget is enforced *before* the request goes out and the
/// usage the response reports is recorded *after* it comes back, so a research
/// run bills against the same budget the main agent loop does and cannot spend
/// past a limit that would have stopped the main loop.
async fn metered_chat(
    ctx: &ResearchCtx<'_>,
    messages: &[ChatMessage],
    tools: Option<&[ToolSpec]>,
    budget: Duration,
) -> MeteredChat {
    if let Err(error) = ctx.meter.enforce_budget() {
        return MeteredChat::BudgetExceeded(error);
    }

    let response = tokio::time::timeout(
        budget,
        ProviderDispatch::from_ref(ctx.provider).chat(
            ChatRequest {
                messages,
                tools,
                thinking: None,
            },
            ctx.model,
            ctx.temperature,
        ),
    )
    .await;

    match response {
        Err(_elapsed) => MeteredChat::Timeout,
        Ok(Err(e)) => MeteredChat::ProviderError(e.to_string()),
        Ok(Ok(chat)) => {
            if let Some(usage) = chat.usage.as_ref() {
                ctx.meter.record_usage(usage);
            }
            MeteredChat::Response(Box::new(chat))
        }
    }
}

/// Ask the model for a final briefing using only the history gathered so far.
async fn request_wrapup(ctx: &ResearchCtx<'_>, history: &[ChatMessage]) -> Option<String> {
    let mut messages = history.to_vec();
    messages.push(ChatMessage::user(
        "Your research budget is now exhausted. Stop searching and write the \
         final briefing immediately, using only what you have already gathered. \
         Remember to end with the mandatory `Sources:` section listing the pages \
         you actually retrieved.",
    ));

    match metered_chat(ctx, &messages, None, Duration::from_secs(WRAPUP_SECS)).await {
        MeteredChat::Response(chat) => {
            let text = chat.text_or_empty().trim().to_string();
            (!text.is_empty()).then_some(text)
        }
        // A wrap-up that cannot run leaves the caller with whatever sources
        // were gathered; the partial preamble already says the run was cut.
        MeteredChat::BudgetExceeded(_) | MeteredChat::ProviderError(_) | MeteredChat::Timeout => {
            None
        }
    }
}

/// Drive the bounded search → fetch → distill loop.
///
/// Takes the provider, meter, and tool set by reference so tests can inject
/// stubs and exercise the bounding logic without a network or a real model.
async fn run_research_loop(
    ctx: &ResearchCtx<'_>,
    question: &str,
    start_url: Option<&str>,
    bounds: ResearchBounds,
) -> ResearchOutcome {
    let specs: Vec<ToolSpec> = ctx.tools.iter().map(|t| t.spec()).collect();
    let mut ledger = SourceLedger::default();

    let mut history = vec![
        ChatMessage::system(build_system_prompt(bounds, start_url.is_some())),
        ChatMessage::user(match start_url {
            Some(url) => format!("Research question: {question}\n\nStart from this page: {url}"),
            None => format!("Research question: {question}"),
        }),
    ];

    let deadline = Instant::now() + bounds.wall_clock;
    let mut tool_calls_used = 0usize;
    let mut summary: Option<String> = None;
    let stop_reason;

    loop {
        let Some(remaining) = remaining_before(deadline) else {
            stop_reason = StopReason::Timeout;
            break;
        };

        let chat = match metered_chat(ctx, &history, Some(&specs), remaining).await {
            MeteredChat::Timeout => {
                stop_reason = StopReason::Timeout;
                break;
            }
            MeteredChat::ProviderError(e) => {
                stop_reason = StopReason::ProviderError(e);
                break;
            }
            MeteredChat::BudgetExceeded(e) => {
                stop_reason = StopReason::BudgetExceeded(e);
                break;
            }
            MeteredChat::Response(chat) => chat,
        };

        if !chat.has_tool_calls() {
            summary = Some(chat.text_or_empty().trim().to_string());
            stop_reason = StopReason::Completed;
            break;
        }

        // Keep the assistant's narration in history for continuity. Tool
        // results come back as user-role observations rather than native
        // `tool` messages: `ChatRequest` carries only `[ChatMessage]`, which
        // structurally cannot round-trip a `tool_call_id`, and an unpaired
        // `tool` message is rejected by several provider APIs.
        let narration = chat.text_or_empty().trim().to_string();
        if !narration.is_empty() {
            history.push(ChatMessage::assistant(narration));
        }

        let mut batch_stop: Option<StopReason> = None;
        for call in &chat.tool_calls {
            if tool_calls_used >= bounds.max_tool_calls {
                batch_stop = Some(StopReason::MaxToolCalls);
                break;
            }

            // Recomputed per call rather than once per batch: a call gets only
            // the time the run has left, and a batch whose earlier calls used
            // it all is abandoned rather than run to completion.
            let remaining = remaining_before(deadline).unwrap_or(Duration::ZERO);

            tool_calls_used += 1;

            let args: serde_json::Value =
                serde_json::from_str(&call.arguments).unwrap_or_else(|_| json!({}));
            match execute_scoped_call(ctx, &call.name, &args, &mut ledger, remaining).await {
                ScopedCall::Observed(observation) => {
                    history.push(ChatMessage::user(format!(
                        "Result of {}({}):\n{observation}",
                        call.name, args
                    )));
                }
                ScopedCall::DeadlineExpired => {
                    batch_stop = Some(StopReason::Timeout);
                    break;
                }
            }
        }

        if let Some(reason) = batch_stop {
            stop_reason = reason;
            break;
        }
    }

    // Any early stop still owes the caller a best-effort briefing.
    if summary.is_none() && stop_reason.is_partial() {
        summary = request_wrapup(ctx, &history).await;
    }

    ResearchOutcome {
        summary: summary.unwrap_or_default(),
        sources: ledger.fetched,
        failed_fetches: ledger.attempted_failed,
        tool_calls_used,
        stop_reason,
    }
}

/// Single deterministic search/fetch + distill pass.
///
/// Used when the provider cannot make native tool calls. Without this,
/// `chat`'s prompt-guided fallback returns prose with **no** parsed tool calls
/// (see `ModelProvider::chat`), so the agentic loop would return a confident,
/// sourceless answer — strictly worse than the raw search tool it replaces.
async fn run_single_pass_research(
    ctx: &ResearchCtx<'_>,
    question: &str,
    start_url: Option<&str>,
    bounds: ResearchBounds,
) -> ResearchOutcome {
    let mut ledger = SourceLedger::default();
    let mut tool_calls_used = 0usize;
    let deadline = Instant::now() + bounds.wall_clock;

    let (name, args) = match start_url {
        Some(url) => ("web_fetch", json!({ "url": url })),
        None => ("web_search_tool", json!({ "query": question })),
    };

    tool_calls_used += 1;
    let gathered = match execute_scoped_call(
        ctx,
        name,
        &args,
        &mut ledger,
        remaining_before(deadline).unwrap_or_default(),
    )
    .await
    {
        ScopedCall::Observed(text) => text,
        ScopedCall::DeadlineExpired => {
            return ResearchOutcome {
                summary: String::new(),
                sources: ledger.fetched,
                failed_fetches: ledger.attempted_failed,
                tool_calls_used,
                stop_reason: StopReason::Timeout,
            };
        }
    };

    let messages = vec![
        ChatMessage::system(
            "You are a web-research sub-agent. Answer the question using only the \
             retrieved material below. Do not invent facts or URLs. End your reply \
             with a section beginning with the exact line `Sources:` listing the \
             URLs you used.",
        ),
        ChatMessage::user(format!(
            "Research question: {question}\n\nRetrieved material:\n{gathered}"
        )),
    ];

    let wrapup_budget = remaining_before(deadline)
        .unwrap_or_default()
        .min(Duration::from_secs(WRAPUP_SECS));

    let (summary, stop_reason) = match metered_chat(ctx, &messages, None, wrapup_budget).await {
        MeteredChat::Response(chat) => (
            chat.text_or_empty().trim().to_string(),
            StopReason::SinglePass,
        ),
        MeteredChat::ProviderError(e) => (String::new(), StopReason::ProviderError(e)),
        MeteredChat::BudgetExceeded(e) => (String::new(), StopReason::BudgetExceeded(e)),
        MeteredChat::Timeout => (String::new(), StopReason::Timeout),
    };

    ResearchOutcome {
        summary,
        sources: ledger.fetched,
        failed_fetches: ledger.attempted_failed,
        tool_calls_used,
        stop_reason,
    }
}

/// The model binding a research sub-agent thinks with.
///
/// Carries the canonical `(config, family, alias)` triple rather than a
/// pre-built provider so construction goes through the same alias-aware factory
/// the main agent loop uses. Building from family alone would resolve every run
/// against the synthetic `"default"` alias, which silently loses Azure
/// resource/deployment routing, OAuth-based families (Qwen, MiniMax), and every
/// `requires_openai_auth` alias.
pub struct SubAgentBinding {
    pub config: Arc<Config>,
    pub family: String,
    pub alias: String,
    pub model: String,
    pub temperature: Option<f64>,
    pub api_key: Option<String>,
    pub runtime_options: ModelProviderRuntimeOptions,
}

/// Delegate tool that answers a research question from the live web.
pub struct WebResearchTool {
    security: Arc<SecurityPolicy>,
    /// Tools the research sub-agent may call. Deliberately search + fetch
    /// only: no shell, no writes, and no local-filesystem readers (a hostile
    /// page could otherwise steer the sub-agent into reading local files and
    /// exfiltrating them through a fetch URL). Narrowed further by the active
    /// profile's denylist at assembly, and re-checked per call.
    scoped_tools: Vec<Arc<dyn Tool>>,
    binding: SubAgentBinding,
    meter: Arc<dyn SubAgentMeter>,
    bounds: ResearchBounds,
}

impl WebResearchTool {
    /// `meter` is required rather than defaulted: an unmetered sub-agent would
    /// spend against no budget at all, so production registration has to pass
    /// the runtime's real one.
    pub fn new(
        security: Arc<SecurityPolicy>,
        scoped_tools: Vec<Arc<dyn Tool>>,
        binding: SubAgentBinding,
        meter: Arc<dyn SubAgentMeter>,
    ) -> Self {
        Self {
            security,
            scoped_tools,
            binding,
            meter,
            bounds: ResearchBounds::default(),
        }
    }

    /// Names of the tools the sub-agent may call, for diagnostics and tests.
    #[must_use]
    pub fn scoped_tool_names(&self) -> Vec<&str> {
        self.scoped_tools.iter().map(|t| t.name()).collect()
    }

    /// Build the sub-agent's provider through the canonical alias-aware
    /// factory — the same one the main agent loop's provider comes from.
    ///
    /// The alias and the typed config entry both have to reach the dispatch:
    /// building from the family alone resolves against a synthetic `"default"`
    /// alias and a defaulted config entry, which drops Azure resource and
    /// deployment routing, the OAuth-based families, and every
    /// `requires_openai_auth` alias.
    fn build_provider(&self) -> anyhow::Result<Box<dyn ModelProvider>> {
        zeroclaw_providers::create_model_provider_for_alias(
            &self.binding.config,
            &self.binding.family,
            &self.binding.alias,
            self.binding.api_key.as_deref(),
            &self.binding.runtime_options,
        )
    }
}

#[async_trait]
impl Tool for WebResearchTool {
    fn name(&self) -> &str {
        NAME
    }

    fn description(&self) -> &str {
        "Research a question on the live web and return a written briefing with a \
         Sources list. Runs a bounded search-and-read sub-agent, so raw search \
         results never enter this conversation. Use this instead of searching \
         directly; ask a full question, not keywords. Approving this call also \
         covers the read-only searches and page fetches the sub-agent makes \
         inside it; they are not approved separately."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "question": {
                    "type": "string",
                    "description": "The research question, phrased as a full question \
                                    rather than search keywords. Include any context \
                                    that constrains a good answer (versions, dates, \
                                    platform)."
                },
                "url": {
                    "type": "string",
                    "description": "Optional starting page. When given, the sub-agent \
                                    reads this page first and only searches if it does \
                                    not fully answer the question."
                }
            },
            "required": ["question"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        // Read, not Act. Both tools the sub-agent can reach are read-only, and
        // the documented `readonly` autonomy level explicitly permits web
        // search — gating this as an action would leave a readonly agent with
        // no web access at all now that raw search is scoped behind it.
        if let Err(error) = self
            .security
            .enforce_tool_operation(ToolOperation::Read, NAME)
        {
            return Ok(ToolResult::err(error));
        }

        let question = match args.get("question").and_then(|v| v.as_str()) {
            Some(q) if !q.trim().is_empty() => q.trim(),
            _ => {
                return Ok(ToolResult::err(
                    "Missing or empty required parameter: question",
                ));
            }
        };

        let start_url = args
            .get("url")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|u| !u.is_empty());

        // An empty scope means the profile excluded every research tool. Say so
        // rather than running a sub-agent that can only hallucinate.
        if self.scoped_tools.is_empty() {
            return Ok(ToolResult::err(
                "web_research has no research tools available; enable [web_search] \
                 and/or [web_fetch], and check that the active profile does not \
                 exclude web_search_tool and web_fetch",
            ));
        }

        let provider: Box<dyn ModelProvider> = match self.build_provider() {
            Ok(p) => p,
            Err(e) => {
                return Ok(ToolResult::err(format!(
                    "Failed to create model_provider: {e}"
                )));
            }
        };

        let ctx = ResearchCtx {
            provider: &*provider,
            model: &self.binding.model,
            temperature: self.binding.temperature,
            meter: self.meter.as_ref(),
            tools: &self.scoped_tools,
            security: &self.security,
        };

        let outcome = if provider.supports_native_tools() {
            run_research_loop(&ctx, question, start_url, self.bounds).await
        } else {
            run_single_pass_research(&ctx, question, start_url, self.bounds).await
        };

        // A run that gathered nothing AND produced nothing is a failure, not an
        // empty briefing — surface it so the agent can react.
        if outcome.summary.trim().is_empty() && outcome.sources.is_empty() {
            let detail = match &outcome.stop_reason {
                StopReason::ProviderError(e) => format!("model call failed: {e}"),
                StopReason::BudgetExceeded(e) => format!("spend budget exhausted: {e}"),
                StopReason::Timeout => "the research run timed out".to_string(),
                other => format!("the research run ended without a result ({other:?})"),
            };
            return Ok(ToolResult::err(format!(
                "web_research produced no answer: {detail}"
            )));
        }

        Ok(deliver(&outcome))
    }
}

/// Wrap a finished run in the result shape the dispatcher actually forwards.
///
/// Always a success, including for a truncated run. `ToolResult::partial` would
/// lose the briefing: the runtime dispatcher forwards `output` to the model only
/// on a successful result and substitutes `Error: {error}` otherwise, so a
/// partial marked as a failure is delivered as an error string with the
/// gathered answer discarded. The degradation travels in the output instead —
/// a `[partial: outcome=...]` preamble, matching how other tools mark truncated
/// output — with the same outcome mirrored into the structured payload for
/// SOP capture and data-flow surfaces.
fn deliver(outcome: &ResearchOutcome) -> ToolResult {
    ToolResult::ok(ToolOutput::json_with_text(
        json!({
            "outcome": outcome.stop_reason.code(),
            "partial": outcome.stop_reason.is_partial(),
            "tool_calls_used": outcome.tool_calls_used,
            "sources": outcome.sources,
            "failed_fetches": outcome.failed_fetches,
        }),
        render_outcome(outcome),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use zeroclaw_api::model_provider::{ChatResponse, ToolCall};

    // ── Prompt contract ──────────────────────────────────────────────

    #[test]
    fn system_prompt_mandates_a_sources_section() {
        let prompt = build_system_prompt(ResearchBounds::default(), false);
        assert!(
            prompt.contains("`Sources:`"),
            "prompt must name the Sources section verbatim: {prompt}"
        );
        assert!(
            prompt.contains("mandatory"),
            "prompt must state the Sources section is mandatory: {prompt}"
        );
        assert!(
            prompt.contains("Never cite a URL you did not actually retrieve"),
            "prompt must forbid fabricated citations: {prompt}"
        );
    }

    #[test]
    fn system_prompt_teaches_query_decomposition() {
        let prompt = build_system_prompt(ResearchBounds::default(), false);
        assert!(
            prompt.contains("Decompose the question into several small, specific queries"),
            "prompt must teach decomposition: {prompt}"
        );
        assert!(
            prompt.contains(
                "Multiple \
         narrow searches beat one long stuffed query"
            ) || prompt.contains("narrow searches beat one long stuffed query"),
            "prompt must state that several narrow searches beat one stuffed query: {prompt}"
        );
    }

    #[test]
    fn system_prompt_states_the_actual_bounds() {
        let bounds = ResearchBounds {
            max_tool_calls: 3,
            wall_clock: Duration::from_secs(42),
        };
        let prompt = build_system_prompt(bounds, false);
        assert!(prompt.contains("at most 3 tool calls"), "{prompt}");
        assert!(prompt.contains("capped at 42"), "{prompt}");
    }

    #[test]
    fn system_prompt_mentions_the_start_url_only_when_one_is_given() {
        assert!(!build_system_prompt(ResearchBounds::default(), false).contains("Starting point"));
        assert!(build_system_prompt(ResearchBounds::default(), true).contains("Starting point"));
    }

    // ── Sources rebuilt from the ledger ──────────────────────────────

    #[test]
    fn sources_section_is_appended_when_the_model_omitted_it() {
        let out = rebuild_sources_section("The answer is 42.", &["https://a.example".into()], &[]);
        assert!(out.contains("Sources:"), "{out}");
        assert!(out.contains("- https://a.example"), "{out}");
    }

    /// The model's own `Sources:` section is a claim, not evidence. It must be
    /// replaced by the ledger, never passed through.
    #[test]
    fn model_written_sources_section_is_replaced_by_the_ledger() {
        let summary = "The answer is 42.\n\nSources:\n- https://model.example";
        let out = rebuild_sources_section(summary, &["https://fetched.example".into()], &[]);

        assert!(
            out.contains("Sources:\n- https://fetched.example"),
            "the ledger must supply the Sources section: {out}"
        );
        assert!(
            out.contains("The answer is 42."),
            "the briefing body must survive: {out}"
        );
        assert!(
            !out.contains("Sources:\n- https://model.example"),
            "the model's section must not be kept verbatim: {out}"
        );
    }

    /// A cited URL the ledger never retrieved is neither promoted to a source
    /// nor silently dropped.
    #[test]
    fn model_cited_urls_outside_the_ledger_land_in_an_unverified_subsection() {
        let summary = "Body.\n\nSources:\n- https://real.example\n- https://invented.example";
        let out = rebuild_sources_section(summary, &["https://real.example".into()], &[]);

        let sources_at = out.find("Sources:").expect("Sources section");
        let unverified_at = out
            .find("Model-cited (unverified):")
            .expect("unverified subsection");
        assert!(sources_at < unverified_at, "retrieved sources come first");

        let verified = &out[sources_at..unverified_at];
        assert!(verified.contains("https://real.example"), "{out}");
        assert!(
            !verified.contains("https://invented.example"),
            "an uncorroborated citation must not appear as a source: {out}"
        );
        assert!(
            out[unverified_at..].contains("https://invented.example"),
            "{out}"
        );
    }

    /// The diagnostics list earns its keep here: a citation naming a fetch that
    /// was attempted and failed is labelled, so a page that was never actually
    /// read cannot read as one that was.
    #[test]
    fn a_cited_failed_fetch_is_annotated_as_such() {
        let summary = "Body.\n\nSources:\n- https://blocked.example";
        let out = rebuild_sources_section(summary, &[], &["https://blocked.example".into()]);

        assert!(out.contains("Model-cited (unverified):"), "{out}");
        assert!(
            out.contains("https://blocked.example (fetch failed)"),
            "{out}"
        );
        assert!(out.contains("- (none retrieved)"), "{out}");
    }

    #[test]
    fn sources_section_is_present_even_with_no_sources() {
        let out = rebuild_sources_section("Nothing found.", &[], &[]);
        assert!(out.contains("Sources:"), "{out}");
        assert!(out.contains("(none retrieved)"), "{out}");
        assert!(
            !out.contains("Model-cited"),
            "no citations means no unverified subsection: {out}"
        );
    }

    #[test]
    fn a_citation_matching_the_ledger_is_not_duplicated_as_unverified() {
        let summary = "Body.\n\nSources:\n- https://a.example/p.";
        let out = rebuild_sources_section(summary, &["https://a.example/p".into()], &[]);
        assert!(
            !out.contains("Model-cited"),
            "trailing punctuation must not defeat ledger matching: {out}"
        );
    }

    // ── Source ledger ────────────────────────────────────────────────

    #[test]
    fn ledger_separates_retrieved_pages_from_failed_attempts() {
        let mut ledger = SourceLedger::default();
        ledger.record_fetched("https://fetched.example/page");
        ledger.record_failed_fetch("https://blocked.example/page");

        assert_eq!(ledger.fetched, vec!["https://fetched.example/page"]);
        assert_eq!(
            ledger.attempted_failed,
            vec!["https://blocked.example/page"]
        );
    }

    #[test]
    fn ledger_deduplicates() {
        let mut ledger = SourceLedger::default();
        ledger.record_fetched("https://a.example");
        ledger.record_fetched("https://a.example");
        assert_eq!(ledger.fetched.len(), 1);
    }

    // ── Stub tools + provider ────────────────────────────────────────

    struct StubTool {
        name: &'static str,
        schema: serde_json::Value,
        output: String,
        succeeds: bool,
        delay: Duration,
        calls: Arc<Mutex<Vec<serde_json::Value>>>,
    }

    impl StubTool {
        fn new(name: &'static str, schema: serde_json::Value, output: &str) -> Self {
            Self {
                name,
                schema,
                output: output.to_string(),
                succeeds: true,
                delay: Duration::ZERO,
                calls: Arc::new(Mutex::new(Vec::new())),
            }
        }

        fn failing(mut self) -> Self {
            self.succeeds = false;
            self
        }

        fn slow(mut self, delay: Duration) -> Self {
            self.delay = delay;
            self
        }
    }

    #[async_trait]
    impl Tool for StubTool {
        fn name(&self) -> &str {
            self.name
        }
        fn description(&self) -> &str {
            "stub"
        }
        fn parameters_schema(&self) -> serde_json::Value {
            self.schema.clone()
        }
        async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
            self.calls.lock().expect("stub lock").push(args);
            if !self.delay.is_zero() {
                tokio::time::sleep(self.delay).await;
            }
            if self.succeeds {
                Ok(ToolResult::ok(self.output.clone()))
            } else {
                Ok(ToolResult::err(self.output.clone()))
            }
        }
    }

    zeroclaw_api::mock_tool_attribution!(StubTool);

    // ── Metering stubs ───────────────────────────────────────────────

    /// Inert meter. Test-only: production registration must inject the
    /// runtime's real one, which is why `WebResearchTool::new` has no default.
    struct InertMeter;

    impl SubAgentMeter for InertMeter {
        fn enforce_budget(&self) -> Result<(), String> {
            Ok(())
        }
        fn record_usage(&self, _usage: &TokenUsage) {}
    }

    /// Counts budget checks and recorded usage, and can refuse after N calls.
    #[derive(Default)]
    struct RecordingMeter {
        checks: AtomicUsize,
        allow_first: usize,
        recorded: Mutex<Vec<(u64, u64)>>,
    }

    impl RecordingMeter {
        fn unlimited() -> Self {
            Self {
                allow_first: usize::MAX,
                ..Self::default()
            }
        }

        fn allowing(allow_first: usize) -> Self {
            Self {
                allow_first,
                ..Self::default()
            }
        }

        fn checks(&self) -> usize {
            self.checks.load(Ordering::SeqCst)
        }

        fn recorded(&self) -> Vec<(u64, u64)> {
            self.recorded.lock().expect("meter lock").clone()
        }
    }

    impl SubAgentMeter for RecordingMeter {
        fn enforce_budget(&self) -> Result<(), String> {
            let seen = self.checks.fetch_add(1, Ordering::SeqCst);
            if seen < self.allow_first {
                Ok(())
            } else {
                Err("Budget exceeded: $5.00 of $5.00 Daily limit".to_string())
            }
        }
        fn record_usage(&self, usage: &TokenUsage) {
            self.recorded.lock().expect("meter lock").push((
                usage.input_tokens.unwrap_or(0),
                usage.output_tokens.unwrap_or(0),
            ));
        }
    }

    fn usage(input: u64, output: u64) -> TokenUsage {
        TokenUsage {
            input_tokens: Some(input),
            output_tokens: Some(output),
            cached_input_tokens: None,
        }
    }

    // ── Context builder ──────────────────────────────────────────────

    /// Default research context: no policy exclusions, inert meter.
    fn ctx<'a>(
        provider: &'a dyn ModelProvider,
        tools: &'a [Arc<dyn Tool>],
        meter: &'a dyn SubAgentMeter,
        security: &'a SecurityPolicy,
    ) -> ResearchCtx<'a> {
        ResearchCtx {
            provider,
            model: "m",
            temperature: None,
            meter,
            tools,
            security,
        }
    }

    /// `ModelProvider` requires `Attributable`; the tool macro cannot supply it
    /// for a non-`Tool` type, so the provider stubs get a shared hand impl.
    macro_rules! stub_provider_attribution {
        ($($ty:ty),+ $(,)?) => {
            $(
                impl zeroclaw_api::attribution::Attributable for $ty {
                    fn role(&self) -> zeroclaw_api::attribution::Role {
                        zeroclaw_api::attribution::Role::Provider(
                            zeroclaw_api::attribution::ProviderKind::Model(
                                zeroclaw_api::attribution::ModelProviderKind::OpenRouter,
                            ),
                        )
                    }
                    fn alias(&self) -> &str {
                        "stub"
                    }
                }
            )+
        };
    }
    stub_provider_attribution!(AlwaysToolCallsProvider, ScriptedProvider);

    fn search_stub(output: &str) -> Arc<StubTool> {
        Arc::new(StubTool::new(
            "web_search_tool",
            json!({"type": "object", "properties": {"query": {"type": "string"}}}),
            output,
        ))
    }

    fn fetch_stub(output: &str) -> Arc<StubTool> {
        Arc::new(StubTool::new(
            "web_fetch",
            json!({"type": "object", "properties": {"url": {"type": "string"}}}),
            output,
        ))
    }

    fn tool_call(id: &str, name: &str, args: serde_json::Value) -> ToolCall {
        ToolCall {
            id: id.to_string(),
            name: name.to_string(),
            arguments: args.to_string(),
            extra_content: None,
        }
    }

    fn text_response(text: &str) -> ChatResponse {
        ChatResponse {
            text: Some(text.to_string()),
            tool_calls: Vec::new(),
            usage: None,
            reasoning_content: None,
        }
    }

    /// Never stops asking for tools — exercises the bounding logic.
    struct AlwaysToolCallsProvider {
        rounds: AtomicUsize,
    }

    #[async_trait]
    impl ModelProvider for AlwaysToolCallsProvider {
        fn supports_native_tools(&self) -> bool {
            true
        }
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            Ok(String::new())
        }
        async fn chat(
            &self,
            request: ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<ChatResponse> {
            // No tools offered => this is the wrap-up call.
            if request.tools.is_none() {
                return Ok(text_response("Best effort briefing."));
            }
            let round = self.rounds.fetch_add(1, Ordering::SeqCst);
            Ok(ChatResponse {
                text: Some(format!("round {round}")),
                tool_calls: vec![tool_call(
                    &format!("c{round}"),
                    "web_search_tool",
                    json!({"query": format!("q{round}")}),
                )],
                usage: None,
                reasoning_content: None,
            })
        }
    }

    /// Replays a fixed script of responses, one per `chat` call.
    struct ScriptedProvider {
        script: Mutex<std::collections::VecDeque<ChatResponse>>,
        native: bool,
        seen_tool_specs: Mutex<Vec<Vec<String>>>,
    }

    impl ScriptedProvider {
        fn new(script: Vec<ChatResponse>, native: bool) -> Self {
            Self {
                script: Mutex::new(script.into()),
                native,
                seen_tool_specs: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl ModelProvider for ScriptedProvider {
        fn supports_native_tools(&self) -> bool {
            self.native
        }
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            Ok(String::new())
        }
        async fn chat(
            &self,
            request: ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<ChatResponse> {
            self.seen_tool_specs.lock().expect("spec lock").push(
                request
                    .tools
                    .unwrap_or(&[])
                    .iter()
                    .map(|s| s.name.clone())
                    .collect(),
            );
            self.script
                .lock()
                .expect("script lock")
                .pop_front()
                .ok_or_else(|| anyhow::Error::msg("script exhausted"))
        }
    }

    // ── Loop bounds ──────────────────────────────────────────────────

    #[tokio::test]
    async fn loop_stops_at_the_tool_call_budget_and_returns_a_partial() {
        let search = search_stub("result https://found.example/doc");
        let tools: Vec<Arc<dyn Tool>> = vec![search.clone()];
        let provider = AlwaysToolCallsProvider {
            rounds: AtomicUsize::new(0),
        };
        let meter = InertMeter;
        let policy = SecurityPolicy::default();
        let bounds = ResearchBounds {
            max_tool_calls: 3,
            wall_clock: Duration::from_secs(30),
        };

        let outcome = run_research_loop(
            &ctx(&provider, &tools, &meter, &policy),
            "why is the sky blue?",
            None,
            bounds,
        )
        .await;

        assert_eq!(outcome.stop_reason, StopReason::MaxToolCalls);
        assert_eq!(
            outcome.tool_calls_used, 3,
            "must execute exactly the budgeted number of tool calls"
        );
        assert_eq!(
            search.calls.lock().expect("stub lock").len(),
            3,
            "the underlying tool must not be invoked past the budget"
        );
        assert!(
            outcome.stop_reason.is_partial(),
            "budget exhaustion is a partial result"
        );
        assert_eq!(outcome.summary, "Best effort briefing.");
    }

    /// A search-only run retrieved nothing, so it has no sources. The briefing
    /// still comes back, marked partial.
    #[tokio::test]
    async fn tool_call_budget_exhaustion_renders_a_partial_with_no_retrieved_sources() {
        let tools: Vec<Arc<dyn Tool>> = vec![search_stub("see https://found.example/doc")];
        let provider = AlwaysToolCallsProvider {
            rounds: AtomicUsize::new(0),
        };
        let meter = InertMeter;
        let policy = SecurityPolicy::default();
        let outcome = run_research_loop(
            &ctx(&provider, &tools, &meter, &policy),
            "q",
            None,
            ResearchBounds {
                max_tool_calls: 2,
                wall_clock: Duration::from_secs(30),
            },
        )
        .await;

        let rendered = render_outcome(&outcome);
        assert!(rendered.contains("[partial:"), "{rendered}");
        assert!(rendered.contains("outcome=max_tool_calls"), "{rendered}");
        assert!(rendered.contains("Sources:"), "{rendered}");
        assert!(
            rendered.contains("(none retrieved)"),
            "a URL merely seen in search-result text was never retrieved and \
             must not be presented as a source: {rendered}"
        );
    }

    #[tokio::test]
    async fn zero_wall_clock_budget_stops_before_any_provider_call() {
        let search = search_stub("unused");
        let tools: Vec<Arc<dyn Tool>> = vec![search.clone()];
        let provider = AlwaysToolCallsProvider {
            rounds: AtomicUsize::new(0),
        };
        let meter = InertMeter;
        let policy = SecurityPolicy::default();

        let outcome = run_research_loop(
            &ctx(&provider, &tools, &meter, &policy),
            "q",
            None,
            ResearchBounds {
                max_tool_calls: 8,
                wall_clock: Duration::ZERO,
            },
        )
        .await;

        assert_eq!(outcome.stop_reason, StopReason::Timeout);
        assert_eq!(outcome.tool_calls_used, 0);
        assert!(search.calls.lock().expect("stub lock").is_empty());
    }

    #[tokio::test]
    async fn loop_completes_when_the_model_answers_without_tools() {
        let provider = ScriptedProvider::new(
            vec![
                ChatResponse {
                    text: Some("looking".into()),
                    tool_calls: vec![tool_call(
                        "c1",
                        "web_fetch",
                        json!({"url": "https://a.example/p"}),
                    )],
                    usage: None,
                    reasoning_content: None,
                },
                text_response("Done.\n\nSources:\n- https://a.example/p"),
            ],
            true,
        );
        let tools: Vec<Arc<dyn Tool>> = vec![fetch_stub("page body")];
        let meter = InertMeter;
        let policy = SecurityPolicy::default();

        let outcome = run_research_loop(
            &ctx(&provider, &tools, &meter, &policy),
            "q",
            None,
            ResearchBounds::default(),
        )
        .await;

        assert_eq!(outcome.stop_reason, StopReason::Completed);
        assert_eq!(outcome.tool_calls_used, 1);
        assert_eq!(outcome.sources, vec!["https://a.example/p"]);
        assert!(!outcome.stop_reason.is_partial());
    }

    #[tokio::test]
    async fn loop_offers_only_the_scoped_tools_to_the_model() {
        let provider = ScriptedProvider::new(vec![text_response("done")], true);
        let tools: Vec<Arc<dyn Tool>> = vec![search_stub("r"), fetch_stub("p")];
        let meter = InertMeter;
        let policy = SecurityPolicy::default();

        let _ = run_research_loop(
            &ctx(&provider, &tools, &meter, &policy),
            "q",
            None,
            ResearchBounds::default(),
        )
        .await;

        let seen = provider.seen_tool_specs.lock().expect("spec lock").clone();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0], vec!["web_search_tool", "web_fetch"]);
    }

    #[tokio::test]
    async fn unknown_tool_calls_are_reported_without_aborting_the_run() {
        let provider = ScriptedProvider::new(
            vec![
                ChatResponse {
                    text: None,
                    tool_calls: vec![tool_call("c1", "shell", json!({"command": "rm -rf /"}))],
                    usage: None,
                    reasoning_content: None,
                },
                text_response("Could not use shell."),
            ],
            true,
        );
        let tools: Vec<Arc<dyn Tool>> = vec![search_stub("r")];
        let meter = InertMeter;
        let policy = SecurityPolicy::default();

        let outcome = run_research_loop(
            &ctx(&provider, &tools, &meter, &policy),
            "q",
            None,
            ResearchBounds::default(),
        )
        .await;

        assert_eq!(outcome.stop_reason, StopReason::Completed);
        assert_eq!(
            outcome.tool_calls_used, 1,
            "a rejected call still consumes budget, so a loop of bad names cannot spin"
        );
    }

    #[tokio::test]
    async fn provider_error_returns_a_partial_rather_than_failing_the_run() {
        let provider = ScriptedProvider::new(
            vec![ChatResponse {
                text: None,
                tool_calls: vec![tool_call(
                    "c1",
                    "web_fetch",
                    json!({"url": "https://a.example/p"}),
                )],
                usage: None,
                reasoning_content: None,
            }],
            true,
        );
        let tools: Vec<Arc<dyn Tool>> = vec![fetch_stub("body")];
        let meter = InertMeter;
        let policy = SecurityPolicy::default();

        let outcome = run_research_loop(
            &ctx(&provider, &tools, &meter, &policy),
            "q",
            None,
            ResearchBounds::default(),
        )
        .await;

        assert!(matches!(outcome.stop_reason, StopReason::ProviderError(_)));
        assert_eq!(
            outcome.sources,
            vec!["https://a.example/p"],
            "sources gathered before the failure must survive"
        );
        assert!(render_outcome(&outcome).contains("Sources:"));
    }

    // ── Policy scope (nested calls honor the denylist) ───────────────

    /// A tool the profile excludes must be unreachable from inside the
    /// sub-agent, even when a scope containing it is handed in. Registration
    /// filters the scope; this is the execution-side half of the same gate.
    #[tokio::test]
    async fn an_excluded_tool_is_refused_at_the_nested_call_boundary() {
        let fetch = fetch_stub("secret page body");
        let tools: Vec<Arc<dyn Tool>> = vec![search_stub("r"), fetch.clone()];
        let provider = ScriptedProvider::new(
            vec![
                ChatResponse {
                    text: None,
                    tool_calls: vec![tool_call(
                        "c1",
                        "web_fetch",
                        json!({"url": "https://a.example/p"}),
                    )],
                    usage: None,
                    reasoning_content: None,
                },
                text_response("Could not fetch."),
            ],
            true,
        );
        let meter = InertMeter;
        let policy = SecurityPolicy {
            excluded_tools: Some(vec!["web_fetch".into()]),
            ..SecurityPolicy::default()
        };

        let outcome = run_research_loop(
            &ctx(&provider, &tools, &meter, &policy),
            "q",
            None,
            ResearchBounds::default(),
        )
        .await;

        assert!(
            fetch.calls.lock().expect("stub lock").is_empty(),
            "an excluded tool must never be executed"
        );
        assert!(
            outcome.sources.is_empty(),
            "a refused call retrieves nothing"
        );
        assert_eq!(outcome.stop_reason, StopReason::Completed);
    }

    #[tokio::test]
    async fn a_non_excluded_tool_still_runs() {
        let fetch = fetch_stub("page body");
        let tools: Vec<Arc<dyn Tool>> = vec![fetch.clone()];
        let provider = ScriptedProvider::new(
            vec![
                ChatResponse {
                    text: None,
                    tool_calls: vec![tool_call(
                        "c1",
                        "web_fetch",
                        json!({"url": "https://a.example/p"}),
                    )],
                    usage: None,
                    reasoning_content: None,
                },
                text_response("Read it."),
            ],
            true,
        );
        let meter = InertMeter;
        let policy = SecurityPolicy {
            excluded_tools: Some(vec!["shell".into()]),
            ..SecurityPolicy::default()
        };

        let outcome = run_research_loop(
            &ctx(&provider, &tools, &meter, &policy),
            "q",
            None,
            ResearchBounds::default(),
        )
        .await;

        assert_eq!(
            fetch.calls.lock().expect("stub lock").len(),
            1,
            "positive control: an unexcluded tool must still execute"
        );
        assert_eq!(outcome.sources, vec!["https://a.example/p"]);
    }

    // ── Metering ─────────────────────────────────────────────────────

    /// Every nested model call is preceded by a budget check and followed by a
    /// usage record. Two provider calls here: the tool round and the answer.
    #[tokio::test]
    async fn every_nested_model_call_is_metered() {
        let provider = ScriptedProvider::new(
            vec![
                ChatResponse {
                    text: None,
                    tool_calls: vec![tool_call(
                        "c1",
                        "web_fetch",
                        json!({"url": "https://a.example/p"}),
                    )],
                    usage: Some(usage(100, 20)),
                    reasoning_content: None,
                },
                ChatResponse {
                    text: Some("Done.".into()),
                    tool_calls: Vec::new(),
                    usage: Some(usage(300, 40)),
                    reasoning_content: None,
                },
            ],
            true,
        );
        let tools: Vec<Arc<dyn Tool>> = vec![fetch_stub("body")];
        let meter = RecordingMeter::unlimited();
        let policy = SecurityPolicy::default();

        let outcome = run_research_loop(
            &ctx(&provider, &tools, &meter, &policy),
            "q",
            None,
            ResearchBounds::default(),
        )
        .await;

        assert_eq!(outcome.stop_reason, StopReason::Completed);
        assert_eq!(
            meter.checks(),
            2,
            "the budget must be enforced before each of the two model calls"
        );
        assert_eq!(
            meter.recorded(),
            vec![(100, 20), (300, 40)],
            "usage from every model call must be recorded"
        );
    }

    /// Budget exhaustion stops the loop before the provider is called, and the
    /// wrap-up call is refused too, so the run cannot spend past the limit.
    #[tokio::test]
    async fn budget_exhaustion_stops_the_loop_before_any_provider_call() {
        let provider = ScriptedProvider::new(vec![text_response("never reached")], true);
        let tools: Vec<Arc<dyn Tool>> = vec![search_stub("r")];
        let meter = RecordingMeter::allowing(0);
        let policy = SecurityPolicy::default();

        let outcome = run_research_loop(
            &ctx(&provider, &tools, &meter, &policy),
            "q",
            None,
            ResearchBounds::default(),
        )
        .await;

        assert!(
            matches!(outcome.stop_reason, StopReason::BudgetExceeded(_)),
            "got {:?}",
            outcome.stop_reason
        );
        assert!(
            outcome.stop_reason.is_partial(),
            "a budget stop is a partial result"
        );
        assert_eq!(outcome.tool_calls_used, 0);
        assert!(
            provider
                .seen_tool_specs
                .lock()
                .expect("spec lock")
                .is_empty(),
            "the provider must not be called once the budget is exhausted"
        );
    }

    /// The budget is re-checked every round, not just at entry.
    #[tokio::test]
    async fn budget_exhaustion_mid_run_stops_further_model_calls() {
        let provider = ScriptedProvider::new(
            vec![
                ChatResponse {
                    text: None,
                    tool_calls: vec![tool_call(
                        "c1",
                        "web_fetch",
                        json!({"url": "https://a.example/p"}),
                    )],
                    usage: Some(usage(10, 5)),
                    reasoning_content: None,
                },
                text_response("never reached"),
            ],
            true,
        );
        let tools: Vec<Arc<dyn Tool>> = vec![fetch_stub("body")];
        // One model call allowed; the second round and the wrap-up are refused.
        let meter = RecordingMeter::allowing(1);
        let policy = SecurityPolicy::default();

        let outcome = run_research_loop(
            &ctx(&provider, &tools, &meter, &policy),
            "q",
            None,
            ResearchBounds::default(),
        )
        .await;

        assert!(
            matches!(outcome.stop_reason, StopReason::BudgetExceeded(_)),
            "got {:?}",
            outcome.stop_reason
        );
        assert_eq!(
            provider.seen_tool_specs.lock().expect("spec lock").len(),
            1,
            "exactly one model call must have gone out"
        );
        assert_eq!(
            outcome.sources,
            vec!["https://a.example/p"],
            "work done before the budget ran out survives"
        );
    }

    // ── Ledger records only successful fetches ───────────────────────

    /// An SSRF rejection, a 404, or any other failed fetch is an attempt, not a
    /// source. Recording it up front would let an empty run look sourced.
    #[tokio::test]
    async fn a_failed_fetch_is_not_recorded_as_a_source() {
        let failing_fetch: Arc<dyn Tool> = Arc::new(
            StubTool::new(
                "web_fetch",
                json!({"type": "object", "properties": {"url": {"type": "string"}}}),
                "blocked: private host",
            )
            .failing(),
        );
        let tools: Vec<Arc<dyn Tool>> = vec![failing_fetch];
        let provider = ScriptedProvider::new(
            vec![
                ChatResponse {
                    text: None,
                    tool_calls: vec![tool_call(
                        "c1",
                        "web_fetch",
                        json!({"url": "https://blocked.example/p"}),
                    )],
                    usage: None,
                    reasoning_content: None,
                },
                text_response("Nothing.\n\nSources:\n- https://blocked.example/p"),
            ],
            true,
        );
        let meter = InertMeter;
        let policy = SecurityPolicy::default();

        let outcome = run_research_loop(
            &ctx(&provider, &tools, &meter, &policy),
            "q",
            None,
            ResearchBounds::default(),
        )
        .await;

        assert!(
            outcome.sources.is_empty(),
            "a failed fetch must not become a source, got {:?}",
            outcome.sources
        );
        assert_eq!(
            outcome.failed_fetches,
            vec!["https://blocked.example/p"],
            "the attempt is kept for diagnostics"
        );

        let rendered = render_outcome(&outcome);
        assert!(rendered.contains("- (none retrieved)"), "{rendered}");
        assert!(
            rendered.contains("https://blocked.example/p (fetch failed)"),
            "a citation of the failed fetch must be labelled: {rendered}"
        );
    }

    #[tokio::test]
    async fn a_successful_fetch_is_recorded_as_a_source() {
        let tools: Vec<Arc<dyn Tool>> = vec![fetch_stub("page body")];
        let provider = ScriptedProvider::new(
            vec![
                ChatResponse {
                    text: None,
                    tool_calls: vec![tool_call(
                        "c1",
                        "web_fetch",
                        json!({"url": "https://ok.example/p"}),
                    )],
                    usage: None,
                    reasoning_content: None,
                },
                text_response("Read it."),
            ],
            true,
        );
        let meter = InertMeter;
        let policy = SecurityPolicy::default();

        let outcome = run_research_loop(
            &ctx(&provider, &tools, &meter, &policy),
            "q",
            None,
            ResearchBounds::default(),
        )
        .await;

        assert_eq!(
            outcome.sources,
            vec!["https://ok.example/p"],
            "positive control: a successful fetch is a source"
        );
        assert!(outcome.failed_fetches.is_empty());
    }

    // ── Nested tool calls are bounded by the run deadline ────────────

    /// A nested tool with a long timeout of its own must not outlive the run.
    /// The tool is slower than the whole wall clock, so the call is abandoned
    /// and the run reports the existing Timeout outcome.
    #[tokio::test]
    async fn a_nested_tool_call_cannot_outlive_the_run_deadline() {
        let slow_fetch: Arc<dyn Tool> = Arc::new(
            StubTool::new(
                "web_fetch",
                json!({"type": "object", "properties": {"url": {"type": "string"}}}),
                "never returned",
            )
            .slow(Duration::from_secs(60)),
        );
        let tools: Vec<Arc<dyn Tool>> = vec![slow_fetch];
        let provider = ScriptedProvider::new(
            vec![ChatResponse {
                text: None,
                tool_calls: vec![tool_call(
                    "c1",
                    "web_fetch",
                    json!({"url": "https://slow.example/p"}),
                )],
                usage: None,
                reasoning_content: None,
            }],
            true,
        );
        let meter = InertMeter;
        let policy = SecurityPolicy::default();

        let started = Instant::now();
        let outcome = run_research_loop(
            &ctx(&provider, &tools, &meter, &policy),
            "q",
            None,
            ResearchBounds {
                max_tool_calls: 8,
                wall_clock: Duration::from_millis(150),
            },
        )
        .await;

        assert_eq!(outcome.stop_reason, StopReason::Timeout);
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "the run must not wait out the tool's own timeout, took {:?}",
            started.elapsed()
        );
        assert!(
            outcome.sources.is_empty(),
            "an abandoned fetch retrieved nothing"
        );
        assert_eq!(
            outcome.failed_fetches,
            vec!["https://slow.example/p"],
            "the abandoned attempt is kept for diagnostics"
        );
    }

    /// The zero-budget case, pinned directly: once the run has no time left, a
    /// nested call is refused without invoking the tool at all. This is what
    /// stops a batch whose earlier calls consumed the whole wall clock from
    /// running its remaining calls.
    #[tokio::test]
    async fn a_nested_call_with_no_remaining_budget_never_invokes_the_tool() {
        let fetch = fetch_stub("page body");
        let tools: Vec<Arc<dyn Tool>> = vec![fetch.clone()];
        let provider = ScriptedProvider::new(Vec::new(), true);
        let meter = InertMeter;
        let policy = SecurityPolicy::default();
        let mut ledger = SourceLedger::default();

        let outcome = execute_scoped_call(
            &ctx(&provider, &tools, &meter, &policy),
            "web_fetch",
            &json!({"url": "https://a.example/p"}),
            &mut ledger,
            Duration::ZERO,
        )
        .await;

        assert!(
            matches!(outcome, ScopedCall::DeadlineExpired),
            "a zero budget must expire the call"
        );
        assert!(
            fetch.calls.lock().expect("stub lock").is_empty(),
            "the tool must not be invoked with no time left"
        );
        assert!(ledger.fetched.is_empty());
    }

    /// Positive control for the budget plumbing: a call with time left runs.
    #[tokio::test]
    async fn a_nested_call_with_remaining_budget_invokes_the_tool() {
        let fetch = fetch_stub("page body");
        let tools: Vec<Arc<dyn Tool>> = vec![fetch.clone()];
        let provider = ScriptedProvider::new(Vec::new(), true);
        let meter = InertMeter;
        let policy = SecurityPolicy::default();
        let mut ledger = SourceLedger::default();

        let outcome = execute_scoped_call(
            &ctx(&provider, &tools, &meter, &policy),
            "web_fetch",
            &json!({"url": "https://a.example/p"}),
            &mut ledger,
            Duration::from_secs(30),
        )
        .await;

        assert!(matches!(outcome, ScopedCall::Observed(_)));
        assert_eq!(fetch.calls.lock().expect("stub lock").len(), 1);
        assert_eq!(ledger.fetched, vec!["https://a.example/p"]);
    }

    /// Once the deadline passes mid-batch, the remaining calls in that batch
    /// are not dispatched.
    #[tokio::test]
    async fn an_expired_deadline_abandons_the_rest_of_the_batch() {
        let slow_fetch = Arc::new(
            StubTool::new(
                "web_fetch",
                json!({"type": "object", "properties": {"url": {"type": "string"}}}),
                "slow",
            )
            .slow(Duration::from_secs(60)),
        );
        let search = search_stub("should never run");
        let tools: Vec<Arc<dyn Tool>> = vec![slow_fetch.clone(), search.clone()];
        let provider = ScriptedProvider::new(
            vec![ChatResponse {
                text: None,
                tool_calls: vec![
                    tool_call("c1", "web_fetch", json!({"url": "https://slow.example/p"})),
                    tool_call("c2", "web_search_tool", json!({"query": "after"})),
                ],
                usage: None,
                reasoning_content: None,
            }],
            true,
        );
        let meter = InertMeter;
        let policy = SecurityPolicy::default();

        let outcome = run_research_loop(
            &ctx(&provider, &tools, &meter, &policy),
            "q",
            None,
            ResearchBounds {
                max_tool_calls: 8,
                wall_clock: Duration::from_millis(150),
            },
        )
        .await;

        assert_eq!(outcome.stop_reason, StopReason::Timeout);
        assert_eq!(
            slow_fetch.calls.lock().expect("stub lock").len(),
            1,
            "the first call ran and was abandoned"
        );
        assert!(
            search.calls.lock().expect("stub lock").is_empty(),
            "the rest of the batch must not be dispatched after the deadline"
        );
    }

    // ── Non-native providers ─────────────────────────────────────────

    #[tokio::test]
    async fn single_pass_runs_a_real_search_for_non_native_providers() {
        let search = search_stub("https://found.example/doc — a page");
        let tools: Vec<Arc<dyn Tool>> = vec![search.clone()];
        let provider = ScriptedProvider::new(vec![text_response("Distilled answer.")], false);
        let meter = RecordingMeter::unlimited();
        let policy = SecurityPolicy::default();

        let outcome = run_single_pass_research(
            &ctx(&provider, &tools, &meter, &policy),
            "why?",
            None,
            ResearchBounds::default(),
        )
        .await;

        assert_eq!(outcome.stop_reason, StopReason::SinglePass);
        assert_eq!(
            search.calls.lock().expect("stub lock")[0]["query"],
            "why?",
            "the question must be searched verbatim"
        );
        assert!(
            outcome.sources.is_empty(),
            "a search retrieves no page, so it yields no source: {:?}",
            outcome.sources
        );
        assert!(render_outcome(&outcome).contains("Sources:"));
        assert_eq!(
            meter.checks(),
            1,
            "the single-pass model call must be metered too"
        );
    }

    #[tokio::test]
    async fn single_pass_starts_from_the_supplied_url() {
        let fetch = fetch_stub("page body");
        let tools: Vec<Arc<dyn Tool>> = vec![search_stub("unused"), fetch.clone()];
        let provider = ScriptedProvider::new(vec![text_response("Answer.")], false);
        let meter = InertMeter;
        let policy = SecurityPolicy::default();

        let outcome = run_single_pass_research(
            &ctx(&provider, &tools, &meter, &policy),
            "what?",
            Some("https://start.example/page"),
            ResearchBounds::default(),
        )
        .await;

        assert_eq!(
            fetch.calls.lock().expect("stub lock")[0]["url"],
            "https://start.example/page"
        );
        assert_eq!(outcome.sources, vec!["https://start.example/page"]);
    }

    #[tokio::test]
    async fn single_pass_refuses_to_spend_past_the_budget() {
        let tools: Vec<Arc<dyn Tool>> = vec![search_stub("r")];
        let provider = ScriptedProvider::new(vec![text_response("never reached")], false);
        let meter = RecordingMeter::allowing(0);
        let policy = SecurityPolicy::default();

        let outcome = run_single_pass_research(
            &ctx(&provider, &tools, &meter, &policy),
            "q",
            None,
            ResearchBounds::default(),
        )
        .await;

        assert!(
            matches!(outcome.stop_reason, StopReason::BudgetExceeded(_)),
            "got {:?}",
            outcome.stop_reason
        );
        assert!(
            provider
                .seen_tool_specs
                .lock()
                .expect("spec lock")
                .is_empty(),
            "no model call may go out once the budget is exhausted"
        );
    }

    // ── Tool surface ─────────────────────────────────────────────────

    fn test_binding() -> SubAgentBinding {
        SubAgentBinding {
            config: Arc::new(Config::default()),
            family: "openrouter".to_string(),
            alias: "default".to_string(),
            model: "test-model".to_string(),
            temperature: None,
            api_key: None,
            runtime_options: ModelProviderRuntimeOptions::default(),
        }
    }

    fn test_tool(scoped: Vec<Arc<dyn Tool>>) -> WebResearchTool {
        WebResearchTool::new(
            Arc::new(SecurityPolicy::default()),
            scoped,
            test_binding(),
            Arc::new(InertMeter),
        )
    }

    #[test]
    fn tool_metadata_declares_question_and_optional_url() {
        let tool = test_tool(vec![search_stub("r")]);
        assert_eq!(tool.name(), "web_research");

        let schema = tool.parameters_schema();
        assert!(schema["properties"]["question"].is_object());
        assert!(schema["properties"]["url"].is_object());
        let required = schema["required"].as_array().expect("required array");
        assert_eq!(
            required,
            &vec![json!("question")],
            "only question is required"
        );
    }

    /// The sub-agent's provider must be built for the alias it was bound to,
    /// not for the synthetic `"default"` alias the family-only factory pins.
    /// The alias is what carries the typed entry through dispatch — Azure
    /// resource/deployment routing, the OAuth-based families, and every
    /// `requires_openai_auth` entry all resolve from it.
    #[test]
    fn the_provider_is_built_for_the_bound_alias() {
        let mut config = Config::default();
        config
            .providers
            .models
            .ensure("openrouter", "research")
            .expect("openrouter is a known family")
            .model = Some("test-model".to_string());

        let tool = WebResearchTool::new(
            Arc::new(SecurityPolicy::default()),
            vec![search_stub("r")],
            SubAgentBinding {
                config: Arc::new(config),
                family: "openrouter".to_string(),
                alias: "research".to_string(),
                model: "test-model".to_string(),
                temperature: None,
                api_key: Some("test-key".to_string()),
                runtime_options: ModelProviderRuntimeOptions::default(),
            },
            Arc::new(InertMeter),
        );

        let provider = tool.build_provider().expect("provider construction");
        assert_eq!(
            zeroclaw_api::attribution::Attributable::alias(provider.as_ref()),
            "research",
            "the provider must be built for the bound alias, not \"default\""
        );
    }

    /// Nested read-only calls are covered by the outer approval rather than
    /// prompting separately, so the description has to say so.
    #[test]
    fn tool_description_documents_the_nested_approval_scope() {
        let tool = test_tool(vec![search_stub("r")]);
        let description = tool.description();
        assert!(
            description.contains("Approving this call also covers"),
            "description must state that the outer approval covers nested \
             calls: {description}"
        );
    }

    #[tokio::test]
    async fn execute_rejects_a_missing_question() {
        let tool = test_tool(vec![search_stub("r")]);
        let result = tool.execute(json!({})).await.expect("execute");
        assert!(!result.success);
        assert!(result.error.expect("error").contains("question"));
    }

    #[tokio::test]
    async fn execute_rejects_a_blank_question() {
        let tool = test_tool(vec![search_stub("r")]);
        let result = tool
            .execute(json!({"question": "   "}))
            .await
            .expect("execute");
        assert!(!result.success);
    }

    #[tokio::test]
    async fn execute_reports_when_no_research_tools_are_available() {
        let tool = test_tool(vec![]);
        let result = tool
            .execute(json!({"question": "anything"}))
            .await
            .expect("execute");
        assert!(!result.success);
        assert!(result.error.expect("error").contains("no research tools"));
    }

    /// The documented `readonly` autonomy level permits web search, and raw
    /// search is now reachable only through this delegate — so a readonly
    /// policy must not reject the tool at its own gate. It reaches the
    /// scope/provider checks like any other call.
    #[tokio::test]
    async fn execute_is_not_rejected_by_a_readonly_policy() {
        let readonly = Arc::new(SecurityPolicy {
            autonomy: zeroclaw_config::policy::AutonomyLevel::ReadOnly,
            ..SecurityPolicy::default()
        });
        assert!(!readonly.can_act(), "precondition: the policy is read-only");
        let tool = WebResearchTool::new(readonly, vec![], test_binding(), Arc::new(InertMeter));

        let result = tool
            .execute(json!({"question": "anything"}))
            .await
            .expect("execute");
        let error = result.error.expect("error");
        assert!(
            !error.contains("read-only mode"),
            "readonly must not block web_research: {error}"
        );
        assert!(
            error.contains("no research tools"),
            "the run must proceed to the scope check: {error}"
        );
    }

    #[test]
    fn scoped_tool_names_excludes_shell_and_write_tools() {
        let tool = test_tool(vec![search_stub("r"), fetch_stub("p")]);
        let names = tool.scoped_tool_names();
        assert_eq!(names, vec!["web_search_tool", "web_fetch"]);
        assert!(!names.contains(&"shell"));
        assert!(!names.contains(&"file_write"));
    }

    // ── Dispatcher-visible delivery of a partial briefing ────────────

    /// The runtime dispatcher forwards `output` to the model only when
    /// `success` is true; on a failed result it substitutes `Error: {error}`
    /// and discards the output entirely. A partial briefing therefore has to
    /// come back as a success carrying its own `[partial: ...]` marker, or the
    /// model never sees the work that was done.
    #[test]
    fn a_partial_outcome_is_delivered_as_a_successful_result() {
        let outcome = ResearchOutcome {
            summary: "Half an answer.".to_string(),
            sources: vec!["https://a.example/p".to_string()],
            failed_fetches: Vec::new(),
            tool_calls_used: 3,
            stop_reason: StopReason::Timeout,
        };

        let result = deliver(&outcome);

        assert!(
            result.success,
            "a partial briefing must be a successful result or the dispatcher \
             throws its output away"
        );
        assert!(
            result.error.is_none(),
            "an error field would make the dispatcher substitute Error: {{...}}"
        );
        let text = result.output.as_str();
        assert!(text.contains("[partial:"), "{text}");
        assert!(text.contains("outcome=timeout"), "{text}");
        assert!(text.contains("Half an answer."), "{text}");
        assert!(text.contains("https://a.example/p"), "{text}");
    }

    /// The structured payload mirrors the outcome for SOP capture and
    /// data-flow surfaces; the model itself reads only the text.
    #[test]
    fn delivery_carries_a_machine_readable_outcome_alongside_the_text() {
        let outcome = ResearchOutcome {
            summary: "Answer.".to_string(),
            sources: vec!["https://a.example/p".to_string()],
            failed_fetches: vec!["https://blocked.example".to_string()],
            tool_calls_used: 4,
            stop_reason: StopReason::MaxToolCalls,
        };

        let result = deliver(&outcome);
        let data = result.output.data().expect("structured payload");
        assert_eq!(data["outcome"], "max_tool_calls");
        assert_eq!(data["partial"], true);
        assert_eq!(data["tool_calls_used"], 4);
        assert_eq!(data["sources"][0], "https://a.example/p");
        assert_eq!(data["failed_fetches"][0], "https://blocked.example");
    }

    #[test]
    fn a_completed_outcome_is_delivered_as_a_success_with_no_marker() {
        let outcome = ResearchOutcome {
            summary: "A full answer.".to_string(),
            sources: vec!["https://a.example/p".to_string()],
            failed_fetches: Vec::new(),
            tool_calls_used: 2,
            stop_reason: StopReason::Completed,
        };

        let result = deliver(&outcome);
        assert!(result.success);
        assert!(!result.output.as_str().contains("[partial:"));
        assert_eq!(
            result.output.data().expect("structured payload")["partial"],
            false
        );
    }

    #[test]
    fn a_completed_outcome_carries_no_partial_marker() {
        let outcome = ResearchOutcome {
            summary: "A full answer.".to_string(),
            sources: vec!["https://a.example/p".to_string()],
            failed_fetches: Vec::new(),
            tool_calls_used: 2,
            stop_reason: StopReason::Completed,
        };
        let rendered = render_outcome(&outcome);
        assert!(!rendered.contains("[partial:"), "{rendered}");
    }

    #[test]
    fn every_partial_stop_reason_has_a_notice_and_a_code() {
        for reason in [
            StopReason::MaxToolCalls,
            StopReason::Timeout,
            StopReason::ProviderError("boom".into()),
            StopReason::BudgetExceeded("over".into()),
        ] {
            assert!(reason.is_partial(), "{reason:?} must be partial");
            let notice = partial_notice(&reason).expect("partial reasons carry a notice");
            assert!(
                notice.contains(&format!("outcome={}", reason.code())),
                "{notice}"
            );
        }
        for reason in [StopReason::Completed, StopReason::SinglePass] {
            assert!(!reason.is_partial(), "{reason:?} must not be partial");
            assert!(partial_notice(&reason).is_none());
        }
    }
}
