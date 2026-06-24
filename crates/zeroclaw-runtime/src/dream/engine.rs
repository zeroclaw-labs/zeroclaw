//! Dream mode engine — periodic memory consolidation and reflective learning.
//!
//! The dream engine runs in two modes:
//!
//! **Local mode (no network calls):** Gathers recent memories, applies time-decay
//! scoring, prunes stale/low-importance entries by age and threshold. Fully offline.
//!
//! **LLM-assisted mode (opt-in):** Additionally calls an LLM to identify patterns,
//! extract insights, and flag semantically stale entries. Requires `dream_mode.model`
//! to be configured.
//!
//! Phases:
//! 1. **Gather** recent daily memories and conversation summaries.
//! 2. **Reflect** via LLM *(optional — skipped when no provider is configured)*.
//! 3. **Consolidate** distilled insights into long-term Core memories *(LLM only)*.
//! 4. **Prune** stale daily memories and low-importance entries *(always runs)*.
//! 5. **Report** an optional summary for the next user interaction.
//!
//! # Design: separate prompt contract, shared provider contract
//!
//! The reflect phase calls `provider.chat_with_system(...)` directly rather than
//! constructing a full `Agent`. This is intentional:
//!
//! - **Different prompt contract:** Dream reflect is a one-shot JSON-extraction
//!   call with no tools, no agent persona, no conversation history, and no
//!   workspace system prompt. Wrapping it in `Agent::from_config` would add
//!   tool dispatch, message-history threading, and persona resolution that
//!   actively conflict with the deterministic JSON-out contract.
//! - **Same provider contract:** The provider is still built through
//!   `zeroclaw_providers::create_routed_provider_with_options` in both the
//!   daemon worker and CLI handler. That means `dream_mode.model` overrides,
//!   `providers.fallback.api_key` / `base_url`, `model_routes`, `reliability`,
//!   and `provider_runtime_options` all flow through the standard resolution
//!   path — only the *prompt* surface differs from a normal agent turn.
//!
//! See the `llm_assisted_path_passes_configured_model_to_provider` test below
//! for the contract assertion that the configured model reaches the provider.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use zeroclaw_api::memory_traits::{Memory, MemoryCategory, MemoryEntry};
use zeroclaw_api::model_provider::ModelProvider as Provider;
use zeroclaw_config::schema::DreamModeConfig;

use super::pending::{DreamPending, StagedInsight};

// Local logging helpers — keep dream-engine call sites readable while still
// using the workspace's `zeroclaw_log::record!` event sink. Accept format-args
// like the tracing crate's macros so existing call sites need no rewriting.
macro_rules! info {
    ($($arg:tt)*) => {
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
            &format!($($arg)*)
        )
    };
}
macro_rules! warn {
    ($($arg:tt)*) => {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
            &format!($($arg)*)
        )
    };
}
macro_rules! debug {
    ($($arg:tt)*) => {
        ::zeroclaw_log::record!(
            DEBUG,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
            &format!($($arg)*)
        )
    };
}
use super::report::DreamReport;

/// Value written into `superseded_by` when a dream cycle retires an entry
/// non-destructively. Any non-null value hides the row from retrieval; this
/// marker makes the origin auditable (and distinguishable from a
/// supersession by a concrete newer entry).
pub(crate) const DREAM_SUPERSEDE_MARKER: &str = "dream:retired";

// ── Dream cycle result ─────────────────────────────────────────

/// Result of a single dream cycle.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DreamCycleResult {
    /// Number of memories gathered for reflection.
    pub gathered_count: usize,
    /// Insights distilled by the LLM reflect phase (empty in local-only mode).
    pub insights: Vec<String>,
    /// Number of stale memories pruned.
    pub pruned_count: usize,
    /// Number of new Core memories created from insights.
    pub consolidated_count: usize,
    /// Summary text for the "While you were away..." report.
    pub report_summary: Option<String>,
    /// Timestamp of the dream cycle.
    pub timestamp: DateTime<Utc>,
    /// Whether the LLM reflect phase was used.
    pub llm_assisted: bool,
}

// ── LLM prompt ─────────────────────────────────────────────────

const DREAM_REFLECT_SYSTEM_PROMPT: &str = r#"You are a memory consolidation engine performing a "dream cycle". Given a collection of recent daily memories and conversation summaries, analyze them and extract:

1. "insights": An array of concise, actionable insights — patterns you notice, recurring topics, preference shifts, important decisions, or lessons learned. Each insight should be a single sentence that stands alone as a useful fact to remember long-term.

2. "stale_keys": An array of memory keys that appear outdated, contradictory, or superseded by newer information. Only include keys you are confident are no longer relevant.

3. "summary": A 1-3 sentence natural language summary of what happened during this period, suitable for a "While you were away..." report.

Respond ONLY with valid JSON:
{"insights": ["...", "..."], "stale_keys": ["..."], "summary": "..."}
Do not include any text outside the JSON object."#;

/// Parsed output from the LLM reflect phase.
#[derive(Debug, Deserialize)]
struct ReflectResult {
    #[serde(default)]
    insights: Vec<String>,
    #[serde(default)]
    stale_keys: Vec<String>,
    #[serde(default)]
    summary: Option<String>,
}

// ── Engine ───────���───────────────────────────────��──────────────

/// Dream mode engine — consolidates memories during idle periods.
pub struct DreamEngine {
    config: DreamModeConfig,
    workspace_dir: PathBuf,
}

impl DreamEngine {
    pub fn new(config: DreamModeConfig, workspace_dir: PathBuf) -> Self {
        Self {
            config,
            workspace_dir,
        }
    }

    /// Run a single dream cycle.
    ///
    /// - When `provider` is `Some`, runs the full LLM-assisted pipeline.
    /// - When `provider` is `None`, runs local-only (prune by age/importance, no insights).
    pub async fn run_cycle(
        &self,
        memory: &dyn Memory,
        provider: Option<&dyn Provider>,
        model: Option<&str>,
    ) -> Result<DreamCycleResult> {
        self.run_cycle_with_options(memory, provider, model, false)
            .await
    }

    /// Run a single dream cycle with explicit option control.
    ///
    /// When `dry_run` is true, the cycle is side-effect free: no memory mutations,
    /// no `dream_pending.json`, no `dream_report.json` written. The returned
    /// `DreamCycleResult` describes what *would* have happened.
    pub async fn run_cycle_with_options(
        &self,
        memory: &dyn Memory,
        provider: Option<&dyn Provider>,
        model: Option<&str>,
        dry_run: bool,
    ) -> Result<DreamCycleResult> {
        info!("Dream cycle started");

        // Phase 1: Gather
        let gathered = self.gather(memory).await?;
        let gathered_count = gathered.len();
        info!("Dream gather: {} memories collected", gathered_count);

        if gathered.is_empty() {
            info!("Dream cycle skipped: no recent memories to consolidate");
            return Ok(DreamCycleResult {
                gathered_count: 0,
                insights: Vec::new(),
                pruned_count: 0,
                consolidated_count: 0,
                report_summary: None,
                timestamp: Utc::now(),
                llm_assisted: false,
            });
        }

        // Phase 2: Reflect (LLM call — only if provider is configured)
        let reflect_result = match (provider, model) {
            (Some(p), Some(m)) => {
                let result = self.reflect(p, m, &gathered).await?;
                info!(
                    "Dream reflect: {} insights, {} stale keys",
                    result.insights.len(),
                    result.stale_keys.len()
                );
                Some(result)
            }
            _ => {
                info!("Dream reflect: skipped (no LLM configured, local-only mode)");
                None
            }
        };

        let llm_assisted = reflect_result.is_some();
        let insights = reflect_result
            .as_ref()
            .map(|r| r.insights.clone())
            .unwrap_or_default();
        let stale_keys = reflect_result
            .as_ref()
            .map(|r| r.stale_keys.clone())
            .unwrap_or_default();
        let summary = reflect_result.as_ref().and_then(|r| r.summary.clone());

        // Dry-run: side-effect free. Return what would have happened without persisting anything.
        if dry_run {
            let pruned_preview =
                self.collect_local_prune_candidates(memory).await?.len() + stale_keys.len();
            info!(
                "Dream cycle complete (dry-run): {} insights, {} would-be-pruned, no writes",
                insights.len(),
                pruned_preview
            );
            return Ok(DreamCycleResult {
                gathered_count,
                insights,
                pruned_count: pruned_preview,
                consolidated_count: 0,
                report_summary: summary,
                timestamp: Utc::now(),
                llm_assisted,
            });
        }

        // In audit mode, stage proposed mutations for review instead of applying.
        if self.config.audit_mode {
            // Preserve unreviewed work: if dream_pending.json already exists,
            // skip writing a new one so a scheduled cycle can't silently
            // clobber pending review surface from an earlier cycle. Surface
            // the skip through a dream_report so the user is still notified.
            match DreamPending::load(&self.workspace_dir) {
                Ok(Some(_existing)) => {
                    info!(
                        "Dream cycle complete (audit): existing dream_pending.json preserved; \
                         skipping new stage to avoid overwriting unreviewed work"
                    );
                    let skip_summary = "Dream cycle skipped: pending review at \
                                        dream_pending.json — run `zeroclaw dream promote` \
                                        or remove the file before the next cycle."
                        .to_string();

                    if self.config.show_report {
                        let report = DreamReport {
                            summary: skip_summary.clone(),
                            insights_count: 0,
                            pruned_count: 0,
                            timestamp: Utc::now(),
                            delivered: false,
                        };
                        if let Err(e) = report.save(&self.workspace_dir) {
                            warn!("Failed to persist dream report on audit-skip: {e}");
                        }
                    }

                    return Ok(DreamCycleResult {
                        gathered_count,
                        insights,
                        pruned_count: 0,
                        consolidated_count: 0,
                        report_summary: Some(skip_summary),
                        timestamp: Utc::now(),
                        llm_assisted,
                    });
                }
                Ok(None) => {} // No existing pending — proceed to stage.
                Err(e) => {
                    warn!("Failed to check existing dream_pending.json: {e}");
                    // Be safe: proceed as if no pending file existed.
                }
            }

            let staged_insights: Vec<StagedInsight> = insights
                .iter()
                .filter(|s| !s.trim().is_empty())
                .map(|s| StagedInsight {
                    content: s.clone(),
                    importance: zeroclaw_memory::importance::compute_importance(
                        s,
                        &MemoryCategory::Core,
                    ),
                })
                .collect();

            // Even in local-only mode, stage the prune candidates for review.
            let mut proposed_prunes = stale_keys.clone();
            proposed_prunes.extend(self.collect_local_prune_candidates(memory).await?);

            let pending = DreamPending {
                insights: staged_insights,
                proposed_prunes,
                timestamp: Utc::now(),
                summary: summary.clone(),
            };
            if let Err(e) = pending.save(&self.workspace_dir) {
                warn!("Failed to persist dream_pending.json: {e}");
            }

            // Also surface a "stage created" report so interactive startup
            // notifies the user there's pending review work.
            if self.config.show_report {
                let report_summary_text = summary.clone().unwrap_or_else(|| {
                    "Dream cycle staged proposed mutations to dream_pending.json. \
                     Run `zeroclaw dream promote` to apply."
                        .to_string()
                });
                let report = DreamReport {
                    summary: report_summary_text,
                    insights_count: pending.insights.len(),
                    pruned_count: pending.proposed_prunes.len(),
                    timestamp: Utc::now(),
                    delivered: false,
                };
                if let Err(e) = report.save(&self.workspace_dir) {
                    warn!("Failed to persist dream report on stage: {e}");
                }
            }

            info!(
                "Dream cycle complete (audit): staged to dream_pending.json, no mutations applied"
            );
            return Ok(DreamCycleResult {
                gathered_count,
                insights,
                pruned_count: 0,
                consolidated_count: 0,
                report_summary: summary,
                timestamp: Utc::now(),
                llm_assisted,
            });
        }

        // Phase 3: Consolidate insights into Core memories (LLM-only)
        let consolidated_count = if llm_assisted {
            let count = self.consolidate(memory, &insights).await?;
            info!("Dream consolidate: {} new Core memories", count);
            count
        } else {
            0
        };

        // Phase 4: Prune stale memories (always runs — local + LLM-identified)
        let pruned_count = self.prune(memory, &stale_keys).await?;
        info!("Dream prune: {} memories removed", pruned_count);

        // Phase 5: Build report
        let report_summary = if llm_assisted {
            summary
        } else {
            // Local-only: generate a simple mechanical summary.
            Some(format!(
                "Pruned {pruned_count} stale memories (age/importance threshold)."
            ))
        };

        let result = DreamCycleResult {
            gathered_count,
            insights,
            pruned_count,
            consolidated_count,
            report_summary: report_summary.clone(),
            timestamp: Utc::now(),
            llm_assisted,
        };

        // Persist report for "While you were away..." display
        if self.config.show_report
            && let Some(ref summary_text) = result.report_summary
        {
            let report = DreamReport {
                summary: summary_text.clone(),
                insights_count: result.consolidated_count,
                pruned_count: result.pruned_count,
                timestamp: result.timestamp,
                delivered: false,
            };
            if let Err(e) = report.save(&self.workspace_dir) {
                warn!("Failed to persist dream report: {e}");
            }
        }

        info!(
            "Dream cycle complete ({}): {} insights, {} pruned",
            if llm_assisted {
                "LLM-assisted"
            } else {
                "local"
            },
            result.consolidated_count,
            result.pruned_count
        );

        Ok(result)
    }

    // ── Phase 1: Gather ���───────────────────────────────────────

    /// Collect recent daily memories and conversation summaries.
    ///
    /// Filters out:
    /// - `MemoryCategory::Conversation` (chat context).
    /// - Anything in the `"dream"` namespace (memories created by previous
    ///   dream cycles — including promoted Core insights). This prevents a
    ///   self-referential feedback loop where the engine re-dreams its own
    ///   outputs.
    async fn gather(&self, memory: &dyn Memory) -> Result<Vec<MemoryEntry>> {
        let since = (Utc::now() - chrono::Duration::hours(24)).to_rfc3339();

        let mut entries = memory
            .recall("", self.config.gather_limit, None, Some(&since), None)
            .await
            .context("dream gather: failed to recall recent memories")?;

        // Exclude Conversation memories to avoid chat context leaking into dreams.
        entries.retain(|e| !matches!(e.category, MemoryCategory::Conversation));

        // Exclude dream-namespaced memories so the engine never reads its own
        // generated artifacts back into the next cycle's reflection input.
        entries.retain(|e| e.namespace != "dream");

        // Cap to configured limit.
        entries.truncate(self.config.gather_limit);

        Ok(entries)
    }

    // ── Phase 2: Reflect ───────────────────────────────────────

    /// Single LLM pass to identify patterns, insights, and stale entries.
    async fn reflect(
        &self,
        provider: &dyn Provider,
        model: &str,
        gathered: &[MemoryEntry],
    ) -> Result<ReflectResult> {
        let mut input = String::with_capacity(gathered.len() * 200);
        for entry in gathered {
            input.push_str(&format!(
                "[{}] ({}) {}: {}\n",
                entry.timestamp, entry.category, entry.key, entry.content
            ));
        }

        let truncated = truncate_utf8(&input, 8000);

        let raw = provider
            .chat_with_system(
                Some(DREAM_REFLECT_SYSTEM_PROMPT),
                &truncated,
                model,
                Some(self.config.temperature),
            )
            .await
            .context("dream reflect: LLM call failed")?;

        parse_reflect_response(&raw)
    }

    // ── Phase 3: Consolidate ───────────────────────────────────

    /// Store distilled insights as Core memories with importance scoring.
    async fn consolidate(&self, memory: &dyn Memory, insights: &[String]) -> Result<usize> {
        let mut stored = 0;

        for insight in insights {
            if insight.trim().is_empty() {
                continue;
            }

            let key = format!("dream_insight_{}", uuid::Uuid::new_v4());
            let importance =
                zeroclaw_memory::importance::compute_importance(insight, &MemoryCategory::Core);

            // Check for conflicts with existing Core memories.
            if let Err(e) = zeroclaw_memory::conflict::check_and_resolve_conflicts(
                memory,
                &key,
                insight,
                &MemoryCategory::Core,
                0.85,
            )
            .await
            {
                debug!("dream consolidate: conflict check skipped: {e}");
            }

            match memory
                .store_with_metadata(
                    &key,
                    insight,
                    MemoryCategory::Core,
                    None,
                    Some("dream"),
                    Some(importance),
                )
                .await
            {
                Ok(()) => stored += 1,
                Err(e) => {
                    warn!("dream consolidate: failed to store insight: {e}");
                }
            }
        }

        Ok(stored)
    }

    // ─��� Phase 4: Prune ─���───────────────────────────────────────

    /// Retire one entry by key. Non-destructive by default: marks the entry
    /// `superseded` (filtered out of retrieval, kept in the store) unless
    /// `hard_prune` is set, in which case it hard-deletes. Returns whether a
    /// row was actually retired (`false` on append-only backends in the
    /// non-destructive mode).
    async fn retire(&self, memory: &dyn Memory, key: &str) -> Result<bool> {
        if self.config.hard_prune {
            memory.forget(key).await
        } else {
            memory.mark_superseded(key, DREAM_SUPERSEDE_MARKER).await
        }
    }

    /// Retire stale daily memories, low-importance entries, and LLM-identified
    /// outdated entries. Non-destructive by default (supersede); opt into hard
    /// deletion via `dream_mode.hard_prune`.
    async fn prune(&self, memory: &dyn Memory, stale_keys: &[String]) -> Result<usize> {
        let mut pruned = 0;
        let mut attempted = 0;

        // Retire LLM-identified stale entries (empty if local-only).
        for key in stale_keys {
            attempted += 1;
            match self.retire(memory, key).await {
                Ok(true) => pruned += 1,
                Ok(false) => {
                    debug!("dream prune: no row retired for key: {key}");
                }
                Err(e) => {
                    debug!("dream prune: failed to retire key {key}: {e}");
                }
            }
        }

        // Retire old Daily memories beyond max_daily_age_days or below importance threshold.
        let cutoff = (Utc::now()
            - chrono::Duration::days(i64::from(self.config.max_daily_age_days)))
        .to_rfc3339();

        match memory.list(Some(&MemoryCategory::Daily), None).await {
            Ok(entries) => {
                for entry in entries {
                    let expired = entry.timestamp.as_str() < cutoff.as_str();
                    let below_threshold = entry
                        .importance
                        .map(|s| s < self.config.prune_threshold)
                        .unwrap_or(false);

                    if expired || below_threshold {
                        attempted += 1;
                        if let Ok(true) = self.retire(memory, &entry.key).await {
                            pruned += 1;
                        }
                    }
                }
            }
            Err(e) => {
                debug!("dream prune: failed to list daily memories: {e}");
            }
        }

        // Backend-aware honesty: in non-destructive mode, a backend that hid
        // nothing despite candidates is append-only / supersession-unaware
        // (markdown, none). Report it instead of silently claiming 0 pruned.
        if !self.config.hard_prune && attempted > 0 && pruned == 0 {
            info!(
                "Dream prune: backend does not support superseding (append-only); \
                 {attempted} candidate(s) left in place, nothing retired"
            );
        }

        Ok(pruned)
    }

    // ── Helpers ────────────────────────────────────────────────

    /// Collect keys that would be pruned locally (for audit mode staging).
    async fn collect_local_prune_candidates(&self, memory: &dyn Memory) -> Result<Vec<String>> {
        let cutoff = (Utc::now()
            - chrono::Duration::days(i64::from(self.config.max_daily_age_days)))
        .to_rfc3339();

        let mut candidates = Vec::new();
        if let Ok(entries) = memory.list(Some(&MemoryCategory::Daily), None).await {
            for entry in entries {
                let expired = entry.timestamp.as_str() < cutoff.as_str();
                let below_threshold = entry
                    .importance
                    .map(|s| s < self.config.prune_threshold)
                    .unwrap_or(false);

                if expired || below_threshold {
                    candidates.push(entry.key.clone());
                }
            }
        }
        Ok(candidates)
    }
}

// ── Helpers ─────��──────────────────────────────────────────────

/// Truncate a string to at most `max_bytes` at a valid UTF-8 char boundary.
fn truncate_utf8(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    let end = s
        .char_indices()
        .map(|(i, _)| i)
        .take_while(|&i| i <= max_bytes)
        .last()
        .unwrap_or(0);
    format!("{}...", &s[..end])
}

/// Parse the LLM's reflect response, with fallback for malformed JSON.
fn parse_reflect_response(raw: &str) -> Result<ReflectResult> {
    let cleaned = raw
        .trim()
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();

    serde_json::from_str(cleaned).map_err(|e| {
        debug!("dream reflect: failed to parse LLM response: {e}");
        debug!("dream reflect: raw response: {raw}");
        anyhow::Error::msg(format!("dream reflect: malformed LLM response: {e}"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_utf8_handles_ascii() {
        assert_eq!(truncate_utf8("hello world", 5), "hello...");
        assert_eq!(truncate_utf8("short", 100), "short");
    }

    #[test]
    fn truncate_utf8_handles_multibyte() {
        let s = "hello \u{1F600} world";
        let result = truncate_utf8(s, 7);
        assert!(result.is_char_boundary(result.len() - 3)); // "..." suffix
    }

    #[test]
    fn parse_reflect_response_valid_json() {
        let raw = r#"{"insights": ["User prefers Rust"], "stale_keys": ["old_key"], "summary": "Quiet day"}"#;
        let result = parse_reflect_response(raw).unwrap();
        assert_eq!(result.insights.len(), 1);
        assert_eq!(result.stale_keys.len(), 1);
        assert_eq!(result.summary.as_deref(), Some("Quiet day"));
    }

    #[test]
    fn parse_reflect_response_code_fenced() {
        let raw = "```json\n{\"insights\": [], \"stale_keys\": [], \"summary\": null}\n```";
        let result = parse_reflect_response(raw).unwrap();
        assert!(result.insights.is_empty());
    }

    #[test]
    fn parse_reflect_response_invalid_returns_error() {
        let raw = "not json at all";
        assert!(parse_reflect_response(raw).is_err());
    }

    #[test]
    fn dream_cycle_result_serializes() {
        let result = DreamCycleResult {
            gathered_count: 10,
            insights: vec!["pattern A".into()],
            pruned_count: 2,
            consolidated_count: 1,
            report_summary: Some("Summary".into()),
            timestamp: Utc::now(),
            llm_assisted: true,
        };
        let json = serde_json::to_string(&result).unwrap();
        assert!(json.contains("pattern A"));
    }

    // ── Mock backends for engine integration tests ─────────────

    use async_trait::async_trait;
    use std::sync::Mutex;
    use zeroclaw_api::attribution::{
        Attributable, MemoryKind, ModelProviderKind, ProviderKind, Role,
    };
    use zeroclaw_api::memory_traits::{Memory, MemoryCategory, MemoryEntry};
    use zeroclaw_api::model_provider::ModelProvider as Provider;

    /// In-memory Memory backend that records every mutating call. Used to
    /// assert no writes happen during dry-run.
    #[derive(Default)]
    struct RecordingMemory {
        entries: Mutex<Vec<MemoryEntry>>,
        stores: Mutex<usize>,
        forgets: Mutex<usize>,
        supersedes: Mutex<usize>,
    }

    impl RecordingMemory {
        fn with_daily_entries(entries: Vec<MemoryEntry>) -> Self {
            Self {
                entries: Mutex::new(entries),
                stores: Mutex::new(0),
                forgets: Mutex::new(0),
                supersedes: Mutex::new(0),
            }
        }
        fn with_entries(entries: Vec<MemoryEntry>) -> Self {
            Self::with_daily_entries(entries)
        }
    }

    impl Attributable for RecordingMemory {
        fn role(&self) -> Role {
            Role::Memory(MemoryKind::InMemory)
        }
        fn alias(&self) -> &str {
            "recording-mock"
        }
    }

    #[async_trait]
    impl Memory for RecordingMemory {
        fn name(&self) -> &str {
            "recording-mock"
        }
        async fn store(
            &self,
            _key: &str,
            _content: &str,
            _category: MemoryCategory,
            _session_id: Option<&str>,
        ) -> anyhow::Result<()> {
            *self.stores.lock().unwrap() += 1;
            Ok(())
        }
        async fn store_with_metadata(
            &self,
            _key: &str,
            _content: &str,
            _category: MemoryCategory,
            _session_id: Option<&str>,
            _namespace: Option<&str>,
            _importance: Option<f64>,
        ) -> anyhow::Result<()> {
            *self.stores.lock().unwrap() += 1;
            Ok(())
        }
        async fn recall(
            &self,
            _query: &str,
            _limit: usize,
            _session_id: Option<&str>,
            _since: Option<&str>,
            _until: Option<&str>,
        ) -> anyhow::Result<Vec<MemoryEntry>> {
            // Return the seeded daily entries so the gather phase has data.
            Ok(self.entries.lock().unwrap().clone())
        }
        async fn get(&self, _key: &str) -> anyhow::Result<Option<MemoryEntry>> {
            Ok(None)
        }
        async fn list(
            &self,
            category: Option<&MemoryCategory>,
            _session_id: Option<&str>,
        ) -> anyhow::Result<Vec<MemoryEntry>> {
            let all = self.entries.lock().unwrap().clone();
            match category {
                Some(c) => Ok(all.into_iter().filter(|e| &e.category == c).collect()),
                None => Ok(all),
            }
        }
        async fn forget(&self, _key: &str) -> anyhow::Result<bool> {
            *self.forgets.lock().unwrap() += 1;
            Ok(true)
        }
        async fn mark_superseded(&self, _key: &str, _superseded_by: &str) -> anyhow::Result<bool> {
            *self.supersedes.lock().unwrap() += 1;
            Ok(true)
        }
        async fn count(&self) -> anyhow::Result<usize> {
            Ok(self.entries.lock().unwrap().len())
        }
        async fn health_check(&self) -> bool {
            true
        }
        async fn store_with_agent(
            &self,
            _key: &str,
            _content: &str,
            _category: MemoryCategory,
            _session_id: Option<&str>,
            _namespace: Option<&str>,
            _importance: Option<f64>,
            _agent_alias: Option<&str>,
        ) -> anyhow::Result<()> {
            *self.stores.lock().unwrap() += 1;
            Ok(())
        }
        async fn forget_for_agent(&self, _key: &str, _agent_alias: &str) -> anyhow::Result<bool> {
            *self.forgets.lock().unwrap() += 1;
            Ok(true)
        }
        async fn recall_for_agents(
            &self,
            _agent_aliases: &[&str],
            _query: &str,
            _limit: usize,
            _session_id: Option<&str>,
            _since: Option<&str>,
            _until: Option<&str>,
        ) -> anyhow::Result<Vec<MemoryEntry>> {
            Ok(self.entries.lock().unwrap().clone())
        }
    }

    /// Provider that returns a fixed JSON reflect response and records the
    /// model name it was called with.
    struct MockProvider {
        response: String,
        called_with_model: Mutex<Option<String>>,
        last_message: Mutex<Option<String>>,
    }

    impl MockProvider {
        fn new(response: &str) -> Self {
            Self {
                response: response.to_string(),
                called_with_model: Mutex::new(None),
                last_message: Mutex::new(None),
            }
        }
    }

    impl Attributable for MockProvider {
        fn role(&self) -> Role {
            Role::Provider(ProviderKind::Model(ModelProviderKind::Anthropic))
        }
        fn alias(&self) -> &str {
            "mock"
        }
    }

    #[async_trait]
    impl Provider for MockProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            message: &str,
            model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            *self.called_with_model.lock().unwrap() = Some(model.to_string());
            *self.last_message.lock().unwrap() = Some(message.to_string());
            Ok(self.response.clone())
        }
    }

    fn make_daily_entry(key: &str, importance: Option<f64>) -> MemoryEntry {
        MemoryEntry {
            id: key.into(),
            key: key.into(),
            content: format!("content for {key}"),
            category: MemoryCategory::Daily,
            timestamp: Utc::now().to_rfc3339(),
            session_id: None,
            score: None,
            namespace: "default".into(),
            importance,
            superseded_by: None,
            agent_alias: None,
            agent_id: None,
        }
    }

    fn make_entry(
        key: &str,
        category: MemoryCategory,
        namespace: &str,
        importance: Option<f64>,
    ) -> MemoryEntry {
        MemoryEntry {
            id: key.into(),
            key: key.into(),
            content: format!("content for {key}"),
            category,
            timestamp: Utc::now().to_rfc3339(),
            session_id: None,
            score: None,
            namespace: namespace.into(),
            importance,
            superseded_by: None,
            agent_alias: None,
            agent_id: None,
        }
    }

    #[tokio::test]
    async fn dry_run_writes_nothing_even_with_reflected_insights() {
        // Set up a workspace dir we can inspect for side-effect files.
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().to_path_buf();

        // Memory backend with a Daily entry that would normally be pruned.
        let memory = RecordingMemory::with_daily_entries(vec![make_daily_entry(
            "old_key",
            Some(0.05), // below default prune_threshold=0.1
        )]);

        // Provider returns a non-empty reflect result that would trigger writes
        // in non-dry-run modes.
        let provider = MockProvider::new(
            r#"{"insights": ["Insight one"], "stale_keys": ["old_key"], "summary": "Eventful day"}"#,
        );

        let config = DreamModeConfig {
            enabled: true,
            audit_mode: true, // even with audit_mode set, dry-run must dominate
            show_report: true,
            ..DreamModeConfig::default()
        };
        let engine = DreamEngine::new(config, workspace.clone());

        let result = engine
            .run_cycle_with_options(&memory, Some(&provider), Some("test-model"), true)
            .await
            .unwrap();

        // Result describes what would have happened.
        assert!(result.llm_assisted);
        assert_eq!(result.insights, vec!["Insight one".to_string()]);
        assert_eq!(result.consolidated_count, 0);
        assert!(result.pruned_count >= 1); // would-be-pruned preview

        // Critical: no files written, no memory mutations.
        assert!(
            !workspace.join("dream_pending.json").exists(),
            "dry-run wrote dream_pending.json"
        );
        assert!(
            !workspace.join("dream_report.json").exists(),
            "dry-run wrote dream_report.json"
        );
        assert_eq!(*memory.stores.lock().unwrap(), 0, "dry-run called store");
        assert_eq!(*memory.forgets.lock().unwrap(), 0, "dry-run called forget");
    }

    #[tokio::test]
    async fn audit_mode_writes_pending_but_not_memory() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().to_path_buf();

        let memory = RecordingMemory::with_daily_entries(vec![make_daily_entry("old_key", None)]);
        let provider =
            MockProvider::new(r#"{"insights": ["A"], "stale_keys": ["old_key"], "summary": "ok"}"#);

        let config = DreamModeConfig {
            enabled: true,
            audit_mode: true,
            show_report: false,
            ..DreamModeConfig::default()
        };
        let engine = DreamEngine::new(config, workspace.clone());

        engine
            .run_cycle_with_options(&memory, Some(&provider), Some("test-model"), false)
            .await
            .unwrap();

        // Audit mode stages but doesn't mutate memory.
        assert!(workspace.join("dream_pending.json").exists());
        assert_eq!(*memory.stores.lock().unwrap(), 0);
        assert_eq!(*memory.forgets.lock().unwrap(), 0);
    }

    #[tokio::test]
    async fn llm_assisted_path_passes_configured_model_to_provider() {
        // Verifies the dream LLM call honors the model passed in (which the
        // daemon/CLI resolve from dream_mode.model). This is the contract test
        // for the intentional "dream is a separate prompt, but uses normal
        // provider config" design.
        let temp = tempfile::tempdir().unwrap();
        let memory = RecordingMemory::with_daily_entries(vec![make_daily_entry("k", None)]);
        let provider = MockProvider::new(r#"{"insights":[],"stale_keys":[],"summary":null}"#);

        let engine = DreamEngine::new(
            DreamModeConfig {
                enabled: true,
                audit_mode: false,
                show_report: false,
                ..DreamModeConfig::default()
            },
            temp.path().to_path_buf(),
        );

        engine
            .run_cycle(&memory, Some(&provider), Some("custom-dream-model-xyz"))
            .await
            .unwrap();

        assert_eq!(
            provider.called_with_model.lock().unwrap().as_deref(),
            Some("custom-dream-model-xyz")
        );
    }

    #[tokio::test]
    async fn local_only_path_does_not_call_provider() {
        let temp = tempfile::tempdir().unwrap();
        let memory = RecordingMemory::with_daily_entries(vec![make_daily_entry("k", None)]);
        let provider = MockProvider::new("should-never-be-returned");

        let engine = DreamEngine::new(
            DreamModeConfig {
                enabled: true,
                audit_mode: false,
                show_report: false,
                ..DreamModeConfig::default()
            },
            temp.path().to_path_buf(),
        );

        let result = engine.run_cycle(&memory, None, None).await.unwrap();

        assert!(!result.llm_assisted);
        assert!(result.insights.is_empty());
        assert!(provider.called_with_model.lock().unwrap().is_none());
    }

    // ── New blocker coverage (Audacity88 re-review 2026-05-20) ──

    /// A second audit-mode cycle must not silently clobber the first cycle's
    /// pending review surface. The first cycle's proposed insights must still
    /// be loadable from dream_pending.json after the second cycle runs.
    #[tokio::test]
    async fn second_audit_cycle_preserves_unreviewed_pending() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().to_path_buf();

        let memory = RecordingMemory::with_daily_entries(vec![make_daily_entry("k1", None)]);
        let provider_first = MockProvider::new(
            r#"{"insights":["FIRST_INSIGHT"],"stale_keys":[],"summary":"first cycle"}"#,
        );

        let config = DreamModeConfig {
            enabled: true,
            audit_mode: true,
            show_report: true,
            ..DreamModeConfig::default()
        };
        let engine = DreamEngine::new(config.clone(), workspace.clone());

        engine
            .run_cycle(&memory, Some(&provider_first), Some("m"))
            .await
            .unwrap();

        let pending_after_first = crate::dream::pending::DreamPending::load(&workspace)
            .unwrap()
            .unwrap();
        assert!(
            pending_after_first
                .insights
                .iter()
                .any(|i| i.content == "FIRST_INSIGHT"),
            "first cycle must stage FIRST_INSIGHT"
        );

        // Second cycle with different insight. The pending file must still
        // surface the first cycle's work — either by skipping the write,
        // appending+deduping, or otherwise preserving the unreviewed content.
        let provider_second = MockProvider::new(
            r#"{"insights":["SECOND_INSIGHT"],"stale_keys":[],"summary":"second cycle"}"#,
        );
        engine
            .run_cycle(&memory, Some(&provider_second), Some("m"))
            .await
            .unwrap();

        let pending_after_second = crate::dream::pending::DreamPending::load(&workspace)
            .unwrap()
            .unwrap();
        assert!(
            pending_after_second
                .insights
                .iter()
                .any(|i| i.content == "FIRST_INSIGHT"),
            "second cycle must not drop FIRST_INSIGHT from the unreviewed pending file"
        );
    }

    /// Memories with `namespace == "dream"` (consolidated/promoted from
    /// previous cycles) must not flow back into the next cycle's reflect
    /// input. Otherwise the dream pipeline would loop on its own outputs.
    #[tokio::test]
    async fn gather_excludes_dream_namespace() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().to_path_buf();

        let memory = RecordingMemory::with_entries(vec![
            make_entry("user_pref_x", MemoryCategory::Daily, "default", None),
            make_entry("dream_insight_y", MemoryCategory::Core, "dream", None),
        ]);
        let provider = MockProvider::new(r#"{"insights":[],"stale_keys":[],"summary":null}"#);

        let config = DreamModeConfig {
            enabled: true,
            audit_mode: false,
            show_report: false,
            ..DreamModeConfig::default()
        };
        let engine = DreamEngine::new(config, workspace);

        engine
            .run_cycle(&memory, Some(&provider), Some("m"))
            .await
            .unwrap();

        let llm_input = provider
            .last_message
            .lock()
            .unwrap()
            .clone()
            .expect("provider must be invoked");
        assert!(
            llm_input.contains("user_pref_x"),
            "user/project memory should reach reflect input"
        );
        assert!(
            !llm_input.contains("dream_insight_y"),
            "dream-namespaced memory must be filtered out of reflect input"
        );
    }

    // ── Non-destructive prune (supersede vs hard-delete) ───────────

    /// By default (`hard_prune = false`) the prune phase retires entries via
    /// `mark_superseded` (reversible, raw episode kept) and never hard-deletes.
    #[tokio::test]
    async fn prune_supersedes_by_default_does_not_delete() {
        let temp = tempfile::tempdir().unwrap();
        let memory =
            RecordingMemory::with_daily_entries(vec![make_daily_entry("old_key", Some(0.05))]);
        let provider =
            MockProvider::new(r#"{"insights":[],"stale_keys":["old_key"],"summary":null}"#);

        let config = DreamModeConfig {
            enabled: true,
            audit_mode: false, // run consolidate/prune directly (not staged)
            show_report: false,
            hard_prune: false, // default — non-destructive
            ..DreamModeConfig::default()
        };
        let engine = DreamEngine::new(config, temp.path().to_path_buf());
        engine
            .run_cycle(&memory, Some(&provider), Some("m"))
            .await
            .unwrap();

        assert!(
            *memory.supersedes.lock().unwrap() >= 1,
            "default prune must supersede (non-destructive)"
        );
        assert_eq!(
            *memory.forgets.lock().unwrap(),
            0,
            "default prune must never hard-delete"
        );
    }

    /// With `hard_prune = true` the operator opts into destructive deletion.
    #[tokio::test]
    async fn prune_hard_deletes_when_opted_in() {
        let temp = tempfile::tempdir().unwrap();
        let memory =
            RecordingMemory::with_daily_entries(vec![make_daily_entry("old_key", Some(0.05))]);
        let provider =
            MockProvider::new(r#"{"insights":[],"stale_keys":["old_key"],"summary":null}"#);

        let config = DreamModeConfig {
            enabled: true,
            audit_mode: false,
            show_report: false,
            hard_prune: true, // opt into hard delete
            ..DreamModeConfig::default()
        };
        let engine = DreamEngine::new(config, temp.path().to_path_buf());
        engine
            .run_cycle(&memory, Some(&provider), Some("m"))
            .await
            .unwrap();

        assert!(
            *memory.forgets.lock().unwrap() >= 1,
            "hard_prune must hard-delete"
        );
        assert_eq!(
            *memory.supersedes.lock().unwrap(),
            0,
            "hard_prune must not supersede"
        );
    }
}
