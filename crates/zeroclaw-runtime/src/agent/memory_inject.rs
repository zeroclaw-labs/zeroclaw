//! Unified memory-context injection: ONE policy and ONE renderer for the
//! memory preamble, applied at the turn engine.
//!
//! Replaces the per-path inline renderers (the three `loop_` sites, the
//! channel orchestrator's budgeted renderer, the agent-direct loader, and
//! the cron/daemon pre-prompt blocks). The injection decision is keyed on
//! [`TurnOrigin`] (who initiated the turn), resolved by
//! [`resolve_inject_policy`]; the pipeline in [`render_memory_context`] is
//! the uniform superset of the legacy renderers' behavior. Divergences from
//! any individual legacy path are deliberate and documented on the PR that
//! wires this module (uniform rigour: a per-path difference is an error
//! unless documented).
//!
//! Pipeline: recall (per session, key-deduped) -> time decay OR the gated
//! rerank stage (`rerank_enabled`: over-fetched candidate pool, then
//! recency/importance blend + duplicate collapse + optional MMR + trim,
//! replacing decay on that arm) -> relevance filter -> skip set (autosave
//! keys/content, `*_history` keys, `[IMAGE:` markers, `<tool_result`
//! blocks, optional Conversation-category exclusion) -> budget caps (entry
//! count, per-entry chars, total chars) -> `[Memory context]` wrapper.
//! Exactly one `MemoryRecall` observer event is emitted per render,
//! covering all recalls.

use std::collections::HashSet;
use std::fmt::Write as _;
use std::time::Instant;

use zeroclaw_api::ingress::TurnOrigin;
use zeroclaw_memory::{
    self, MEMORY_CONTEXT_CLOSE, MEMORY_CONTEXT_OPEN, Memory, MemoryCategory, MemoryEntry, decay,
    rerank,
};

use super::loop_::make_query_summary;
use crate::agent::TurnMeta;
use crate::observability::{Observer, ObserverEvent};
use crate::util::truncate_with_ellipsis;

/// Default recall limit per session (all legacy paths used 5).
pub const DEFAULT_RECALL_LIMIT: usize = 5;
/// Default cap on rendered entries (from the channel renderer's budget).
pub const DEFAULT_MAX_ENTRIES: usize = 4;
/// Default per-entry character cap before ellipsis truncation.
pub const DEFAULT_ENTRY_MAX_CHARS: usize = 800;
/// Default total character budget for the rendered block.
pub const DEFAULT_MAX_TOTAL_CHARS: usize = 4_000;

/// The stable, per-agent-config half of the injection policy. Callers build
/// it from the agent's resolved memory config; the per-turn half (origin,
/// session scope, spawn-site suppression) arrives via
/// [`resolve_inject_policy`]'s arguments.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MemoryInjectConfig {
    /// Entries requested from each `recall` call.
    pub limit: usize,
    /// Entries scoring below this (post-decay) are dropped; entries without
    /// a score (non-vector backends) are kept. Callers supply the agent's
    /// configured `memory.min_relevance_score`.
    pub min_relevance_score: f64,
    /// Cap on rendered entries.
    pub max_entries: usize,
    /// Per-entry character cap (ellipsis-truncated beyond it).
    pub entry_max_chars: usize,
    /// Total character budget for the block.
    pub max_total_chars: usize,
    /// Gate for the rerank stage. Off (the default) keeps the pipeline on
    /// the time-decay arm, byte-identical to the pre-rerank renderer.
    pub rerank_enabled: bool,
    /// Candidate pool multiplier over `limit` when reranking: the stage
    /// over-fetches so blend/dedup/MMR have a pool to trim back down.
    pub candidate_multiplier: usize,
    /// Rerank stage parameters (strategy, threshold, blend weights, floor,
    /// final trim). Only consulted when `rerank_enabled` is set.
    pub rerank: rerank::RerankConfig,
}

impl Default for MemoryInjectConfig {
    fn default() -> Self {
        Self {
            limit: DEFAULT_RECALL_LIMIT,
            min_relevance_score: 0.0,
            max_entries: DEFAULT_MAX_ENTRIES,
            entry_max_chars: DEFAULT_ENTRY_MAX_CHARS,
            max_total_chars: DEFAULT_MAX_TOTAL_CHARS,
            rerank_enabled: false,
            candidate_multiplier: 1,
            rerank: rerank::RerankConfig::disabled(DEFAULT_RECALL_LIMIT, 0.0),
        }
    }
}

impl MemoryInjectConfig {
    /// Build the injection config from the agent's resolved memory config
    /// plus the effective recall limit. Threads the relevance floor and the
    /// rerank stage settings; budgets stay at the renderer defaults.
    #[must_use]
    pub fn from_memory_config(
        memory: &zeroclaw_config::schema::MemoryConfig,
        limit: usize,
    ) -> Self {
        let limit = if memory.rerank_enabled {
            rerank::bounded_final_limit(limit)
        } else {
            limit
        };
        Self {
            limit,
            min_relevance_score: memory.min_relevance_score,
            rerank_enabled: memory.rerank_enabled,
            candidate_multiplier: memory.candidate_multiplier.max(1),
            rerank: build_rerank_config(memory, limit),
            ..Default::default()
        }
    }
}

/// Materialize the rerank stage config from the canonical memory config.
/// An unknown `rerank_strategy` falls back to no advanced rerank (the blend,
/// duplicate collapse, and trim still run) with a once-per-process warning.
fn build_rerank_config(
    memory: &zeroclaw_config::schema::MemoryConfig,
    final_limit: usize,
) -> rerank::RerankConfig {
    let strategy = match memory.rerank_strategy.trim().to_ascii_lowercase().as_str() {
        "mmr" => rerank::RerankStrategy::Mmr {
            lambda: memory.mmr_lambda,
        },
        "none" | "" => rerank::RerankStrategy::None,
        other => {
            static WARN_ONCE: std::sync::Once = std::sync::Once::new();
            WARN_ONCE.call_once(|| {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({
                            "rerank_strategy": other,
                        })),
                    "unknown memory rerank_strategy; falling back to no advanced rerank"
                );
            });
            rerank::RerankStrategy::None
        }
    };

    // Cap the candidate pool at the requested over-fetch (limit * multiplier),
    // bounded so a mis-sized config or an over-returning backend cannot feed an
    // unbounded pool into the rerank scans. `candidate_multiplier` is validated
    // to `1..=MAX_MEMORY_RERANK_CANDIDATE_MULTIPLIER` at config load.
    let final_limit = rerank::bounded_final_limit(final_limit);
    let candidate_pool_cap = final_limit
        .max(1)
        .saturating_mul(memory.candidate_multiplier.max(1));

    rerank::RerankConfig {
        strategy,
        threshold: memory.rerank_threshold,
        importance_weight: memory.importance_weight,
        recency_weight: memory.recency_weight,
        min_relevance_score: memory.min_relevance_score,
        final_limit,
        candidate_pool_cap: rerank::bounded_pool_cap(final_limit, candidate_pool_cap),
    }
}

/// The per-turn memory half carried on `ToolLoop`: the handle plus the
/// recall inputs only the entry point knows. Entry points that own a memory
/// pass `Some`; nested sub-turn sites pass `None`.
pub struct TurnMemory<'a> {
    /// The agent's memory backend for this turn.
    pub handle: &'a dyn Memory,
    /// The RAW user message to recall against (before any timestamp or
    /// site-added prefix reaches the history message).
    pub query: String,
    /// Session scopes to recall from (multi-scope recalls are key-deduped
    /// in order). Empty means one unscoped recall.
    pub sessions: Vec<Option<String>>,
    /// Spawn-site opt-out (e.g. a cron job with `uses_memory = false`).
    pub suppress: bool,
    /// The stable config half (limits, relevance floor, budgets).
    pub cfg: MemoryInjectConfig,
}

/// The resolved injection decision for one turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InjectPolicy {
    /// No injection: nested sub-turns, or a spawn site suppressed it.
    Skip,
    /// Inject via the uniform pipeline.
    Inject {
        /// Drop `MemoryCategory::Conversation` entries. True for scheduled
        /// origins (chat history must not leak into autonomous runs,
        /// and for turns without a session scope (without a session filter,
        /// other channels' conversations would bleed in).
        exclude_conversation: bool,
    },
}

#[must_use]
pub fn resolve_inject_policy(
    origin: TurnOrigin,
    has_session: bool,
    suppress: bool,
) -> InjectPolicy {
    if suppress {
        return InjectPolicy::Skip;
    }
    match origin {
        TurnOrigin::SubTurn => InjectPolicy::Skip,
        TurnOrigin::Cron | TurnOrigin::Daemon => InjectPolicy::Inject {
            exclude_conversation: true,
        },
        TurnOrigin::Interactive | TurnOrigin::Channel | TurnOrigin::AgentDirect => {
            InjectPolicy::Inject {
                exclude_conversation: !has_session,
            }
        }
    }
}

/// The uniform skip set: entries no path wants in a preamble. Union of the
/// legacy renderers' filters (the channel renderer's set was the widest).
fn should_skip_entry(key: &str, content: &str) -> bool {
    if zeroclaw_memory::is_assistant_autosave_key(key) {
        return true;
    }
    // Raw per-turn user messages: re-injecting them makes each recalled
    // entry embed all prior generations, growing exponentially. Consolidated
    // knowledge is already promoted to Core/Daily entries.
    if zeroclaw_memory::is_user_autosave_key(key) {
        return true;
    }
    if zeroclaw_memory::should_skip_autosave_content(content) {
        return true;
    }
    // Channel history blobs are session transcripts, not knowledge.
    if key.trim().to_ascii_lowercase().ends_with("_history") {
        return true;
    }
    // Image markers would duplicate the image block already in the request.
    if content.contains("[IMAGE:") {
        return true;
    }
    // Orphan tool_result blocks recalled into a fresh session present the
    // model with a result that has no preceding call.
    if content.contains("<tool_result") {
        return true;
    }
    false
}

pub async fn render_memory_context(
    mem: &dyn Memory,
    observer: &dyn Observer,
    user_msg: &str,
    sessions: &[Option<&str>],
    cfg: &MemoryInjectConfig,
    exclude_conversation: bool,
    turn: TurnMeta<'_>,
) -> String {
    let backend = mem.name().to_string();
    let query_summary = make_query_summary(user_msg);

    // Over-fetch a larger candidate pool only when reranking is enabled;
    // otherwise recall exactly `limit` so the default path is unchanged.
    let recall_limit = if cfg.rerank_enabled {
        let requested_pool = cfg
            .limit
            .saturating_mul(cfg.candidate_multiplier)
            .max(cfg.limit)
            .max(cfg.rerank.candidate_pool_cap);
        rerank::bounded_pool_cap(cfg.limit.max(cfg.rerank.final_limit), requested_pool)
    } else {
        cfg.limit
    };

    let start = Instant::now();
    let mut entries: Vec<MemoryEntry> = Vec::new();
    let mut seen_keys: HashSet<String> = HashSet::new();
    let mut any_ok = false;
    // A turn with no session scopes still recalls once, unscoped.
    let scopes: &[Option<&str>] = if sessions.is_empty() {
        &[None]
    } else {
        sessions
    };
    for session_id in scopes {
        match mem
            .recall(user_msg, recall_limit, *session_id, None, None)
            .await
        {
            Ok(recalled) => {
                any_ok = true;
                for entry in recalled {
                    if seen_keys.insert(entry.key.clone()) {
                        entries.push(entry);
                    }
                }
            }
            Err(_) => {
                // Swallowed: memory failure must not fail the turn.
            }
        }
    }
    let duration = start.elapsed();

    observer.record_event(&ObserverEvent::MemoryRecall {
        query_summary,
        duration,
        num_entries: entries.len(),
        backend,
        success: any_ok,
        channel: Some(turn.channel_name.to_string()),
        agent_alias: turn.agent_alias.map(str::to_string),
        turn_id: Some(turn.turn_id.to_string()),
    });

    if cfg.rerank_enabled {
        // Relevance plane: blend (retrieval + importance + recency), collapse
        // near-duplicates, optionally diversify via MMR, apply the floor, and
        // trim back to the recall limit. The blend's recency factor subsumes
        // time decay, so this arm replaces it; the budget loop below still
        // caps the rendered block. The eligibility predicate mirrors the
        // renderer's skip set so ineligible rows are dropped before the trim,
        // not after, letting over-fetched candidates fill the freed slots.
        entries = rerank::run(entries, &cfg.rerank, |entry| {
            if exclude_conversation && matches!(entry.category, MemoryCategory::Conversation) {
                return false;
            }
            !should_skip_entry(&entry.key, &entry.content)
        });
    } else {
        // Older non-Core memories score lower; Core is evergreen.
        decay::apply_time_decay(&mut entries, decay::DEFAULT_HALF_LIFE_DAYS);
    }

    let mut context = String::new();
    let mut included = 0usize;
    let mut used_chars = 0usize;

    for entry in entries.iter().filter(|e| match e.score {
        Some(score) => score >= cfg.min_relevance_score,
        None => true, // keep unscored entries (non-vector backends)
    }) {
        if included >= cfg.max_entries {
            break;
        }
        if exclude_conversation && matches!(entry.category, MemoryCategory::Conversation) {
            continue;
        }
        if should_skip_entry(&entry.key, &entry.content) {
            continue;
        }

        let content = if entry.content.chars().count() > cfg.entry_max_chars {
            truncate_with_ellipsis(&entry.content, cfg.entry_max_chars)
        } else {
            entry.content.clone()
        };

        let mut line = String::new();
        let _ = writeln!(line, "- {}: {}", entry.key, content);
        let line_chars = line.chars().count();
        if used_chars + line_chars > cfg.max_total_chars {
            break;
        }

        if included == 0 {
            context.push_str(MEMORY_CONTEXT_OPEN);
            context.push('\n');
        }
        context.push_str(&line);
        used_chars += line_chars;
        included += 1;
    }

    if included > 0 {
        context.push_str(MEMORY_CONTEXT_CLOSE);
        context.push_str("\n\n");
    }

    context
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use parking_lot::Mutex;
    use std::collections::HashMap;

    fn entry(
        key: &str,
        content: &str,
        category: MemoryCategory,
        score: Option<f64>,
    ) -> MemoryEntry {
        MemoryEntry {
            id: key.to_string(),
            key: key.to_string(),
            content: content.to_string(),
            category,
            // A current timestamp keeps time decay a no-op in tests.
            timestamp: chrono::Utc::now().to_rfc3339(),
            session_id: None,
            score,
            namespace: "default".into(),
            importance: None,
            superseded_by: None,
            kind: None,
            pinned: false,
            tenant_id: None,
            agent_alias: None,
            agent_id: None,
        }
    }

    /// Recall returns the fixture list for the requested session scope;
    /// `fail` simulates a backend error. `recalls` counts backend recalls
    /// for decorator-composition tests, while requested limits are recorded
    /// so rerank tests can assert the over-fetch gating.
    struct FixtureMemory {
        by_session: HashMap<Option<String>, Vec<MemoryEntry>>,
        fail: bool,
        recalls: std::sync::atomic::AtomicUsize,
        recorded_limits: Mutex<Vec<usize>>,
    }

    impl FixtureMemory {
        fn with(entries: Vec<MemoryEntry>) -> Self {
            let mut by_session = HashMap::new();
            by_session.insert(None, entries);
            Self::with_sessions(by_session)
        }

        fn with_sessions(by_session: HashMap<Option<String>, Vec<MemoryEntry>>) -> Self {
            Self {
                by_session,
                fail: false,
                recalls: std::sync::atomic::AtomicUsize::new(0),
                recorded_limits: Mutex::new(Vec::new()),
            }
        }

        fn failing() -> Self {
            Self {
                by_session: HashMap::new(),
                fail: true,
                recalls: std::sync::atomic::AtomicUsize::new(0),
                recorded_limits: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl Memory for FixtureMemory {
        async fn store(
            &self,
            _key: &str,
            _content: &str,
            _category: MemoryCategory,
            _session_id: Option<&str>,
        ) -> anyhow::Result<()> {
            Ok(())
        }

        async fn recall(
            &self,
            _query: &str,
            limit: usize,
            session_id: Option<&str>,
            _since: Option<&str>,
            _until: Option<&str>,
        ) -> anyhow::Result<Vec<MemoryEntry>> {
            self.recalls
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.recorded_limits.lock().push(limit);
            if self.fail {
                anyhow::bail!("backend down");
            }
            Ok(self
                .by_session
                .get(&session_id.map(str::to_string))
                .cloned()
                .unwrap_or_default())
        }

        async fn get(&self, _key: &str) -> anyhow::Result<Option<MemoryEntry>> {
            Ok(None)
        }

        async fn list(
            &self,
            _category: Option<&MemoryCategory>,
            _session_id: Option<&str>,
        ) -> anyhow::Result<Vec<MemoryEntry>> {
            Ok(vec![])
        }

        async fn forget(&self, _key: &str) -> anyhow::Result<bool> {
            Ok(true)
        }

        async fn forget_for_agent(&self, _key: &str, _agent_id: &str) -> anyhow::Result<bool> {
            Ok(true)
        }

        async fn count(&self) -> anyhow::Result<usize> {
            Ok(0)
        }

        async fn health_check(&self) -> bool {
            true
        }

        fn name(&self) -> &str {
            "fixture"
        }

        async fn store_with_agent(
            &self,
            _key: &str,
            _content: &str,
            _category: MemoryCategory,
            _session_id: Option<&str>,
            _namespace: Option<&str>,
            _importance: Option<f64>,
            _agent_id: Option<&str>,
        ) -> anyhow::Result<()> {
            Ok(())
        }

        async fn recall_for_agents(
            &self,
            _allowed_agent_ids: &[&str],
            query: &str,
            limit: usize,
            session_id: Option<&str>,
            since: Option<&str>,
            until: Option<&str>,
        ) -> anyhow::Result<Vec<MemoryEntry>> {
            self.recall(query, limit, session_id, since, until).await
        }
    }

    impl ::zeroclaw_api::attribution::Attributable for FixtureMemory {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Memory(
                ::zeroclaw_api::attribution::MemoryKind::InMemory,
            )
        }
        fn alias(&self) -> &str {
            "FixtureMemory"
        }
    }

    /// Records MemoryRecall events: (num_entries, success).
    #[derive(Default)]
    struct RecordingObserver {
        recalls: Mutex<Vec<(usize, bool)>>,
        events: Mutex<Vec<ObserverEvent>>,
    }

    impl Observer for RecordingObserver {
        fn record_event(&self, event: &ObserverEvent) {
            self.events.lock().push(event.clone());
            if let ObserverEvent::MemoryRecall {
                num_entries,
                success,
                ..
            } = event
            {
                self.recalls.lock().push((*num_entries, *success));
            }
        }

        fn record_metric(&self, _metric: &zeroclaw_api::observability_traits::ObserverMetric) {}

        fn name(&self) -> &str {
            "recording"
        }

        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
    }

    // ── policy ────────────────────────────────────────────────────────────

    #[test]
    fn policy_sub_turn_never_injects() {
        assert_eq!(
            resolve_inject_policy(TurnOrigin::SubTurn, true, false),
            InjectPolicy::Skip
        );
        assert_eq!(
            resolve_inject_policy(TurnOrigin::SubTurn, false, false),
            InjectPolicy::Skip
        );
    }

    #[test]
    fn policy_scheduled_origins_always_exclude_conversation() {
        for origin in [TurnOrigin::Cron, TurnOrigin::Daemon] {
            for has_session in [true, false] {
                assert_eq!(
                    resolve_inject_policy(origin, has_session, false),
                    InjectPolicy::Inject {
                        exclude_conversation: true
                    }
                );
            }
        }
    }

    #[test]
    fn policy_user_facing_origins_key_exclusion_on_session_scope() {
        for origin in [
            TurnOrigin::Interactive,
            TurnOrigin::Channel,
            TurnOrigin::AgentDirect,
        ] {
            assert_eq!(
                resolve_inject_policy(origin, true, false),
                InjectPolicy::Inject {
                    exclude_conversation: false
                }
            );
            assert_eq!(
                resolve_inject_policy(origin, false, false),
                InjectPolicy::Inject {
                    exclude_conversation: true
                }
            );
        }
    }

    #[test]
    fn policy_suppress_overrides_every_origin() {
        for origin in [
            TurnOrigin::Interactive,
            TurnOrigin::Channel,
            TurnOrigin::Cron,
            TurnOrigin::Daemon,
            TurnOrigin::AgentDirect,
            TurnOrigin::SubTurn,
        ] {
            assert_eq!(
                resolve_inject_policy(origin, true, true),
                InjectPolicy::Skip
            );
        }
    }

    // ── renderer ──────────────────────────────────────────────────────────

    #[tokio::test]
    async fn renders_wrapped_block_and_emits_one_recall_event() {
        let mem = FixtureMemory::with(vec![entry(
            "user_preference",
            "prefers concise answers",
            MemoryCategory::Core,
            Some(0.9),
        )]);
        let observer = RecordingObserver::default();

        let context = render_memory_context(
            &mem,
            &observer,
            "how should I answer",
            &[],
            &MemoryInjectConfig::default(),
            false,
            TurnMeta {
                parent_agent_alias: None,
                agent_alias: None,
                turn_id: "t",
                channel_name: "test",
            },
        )
        .await;

        assert!(context.starts_with(MEMORY_CONTEXT_OPEN));
        assert!(context.contains("- user_preference: prefers concise answers"));
        assert!(context.ends_with(&format!("{MEMORY_CONTEXT_CLOSE}\n\n")));
        assert_eq!(observer.recalls.lock().as_slice(), &[(1, true)]);
    }

    #[tokio::test]
    async fn recall_event_carries_the_turn_correlation_triple() {
        let mem = FixtureMemory::with(vec![entry(
            "user_preference",
            "prefers concise answers",
            MemoryCategory::Core,
            Some(0.9),
        )]);
        let observer = RecordingObserver::default();

        let _ = render_memory_context(
            &mem,
            &observer,
            "how should I answer",
            &[],
            &MemoryInjectConfig::default(),
            false,
            TurnMeta {
                parent_agent_alias: None,
                agent_alias: Some("default"),
                turn_id: "turn-42",
                channel_name: "cli",
            },
        )
        .await;

        let events = observer.events.lock();
        match events
            .iter()
            .find(|e| matches!(e, ObserverEvent::MemoryRecall { .. }))
            .expect("render_memory_context must emit MemoryRecall")
        {
            ObserverEvent::MemoryRecall {
                channel,
                agent_alias,
                turn_id,
                ..
            } => {
                assert_eq!(channel.as_deref(), Some("cli"));
                assert_eq!(agent_alias.as_deref(), Some("default"));
                assert_eq!(turn_id.as_deref(), Some("turn-42"));
            }
            _ => unreachable!(),
        }
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e, ObserverEvent::MemoryRecall { .. }))
                .count(),
            1,
            "exactly one recall event per turn — the #8619 contract"
        );
    }

    #[tokio::test]
    async fn empty_recall_renders_nothing_but_still_emits_the_event() {
        let mem = FixtureMemory::with(vec![]);
        let observer = RecordingObserver::default();

        let context = render_memory_context(
            &mem,
            &observer,
            "anything",
            &[],
            &MemoryInjectConfig::default(),
            false,
            TurnMeta {
                parent_agent_alias: None,
                agent_alias: None,
                turn_id: "t",
                channel_name: "test",
            },
        )
        .await;

        assert!(context.is_empty());
        assert_eq!(observer.recalls.lock().as_slice(), &[(0, true)]);
    }

    #[tokio::test]
    async fn recall_failure_is_swallowed_and_reported_unsuccessful() {
        let mem = FixtureMemory::failing();
        let observer = RecordingObserver::default();

        let context = render_memory_context(
            &mem,
            &observer,
            "anything",
            &[],
            &MemoryInjectConfig::default(),
            false,
            TurnMeta {
                parent_agent_alias: None,
                agent_alias: None,
                turn_id: "t",
                channel_name: "test",
            },
        )
        .await;

        assert!(context.is_empty());
        assert_eq!(observer.recalls.lock().as_slice(), &[(0, false)]);
    }

    #[tokio::test]
    async fn exclude_conversation_drops_only_conversation_entries() {
        let mem = FixtureMemory::with(vec![
            entry(
                "chat",
                "said hi earlier",
                MemoryCategory::Conversation,
                None,
            ),
            entry("fact", "server is prod-3", MemoryCategory::Core, None),
        ]);
        let observer = RecordingObserver::default();

        let context = render_memory_context(
            &mem,
            &observer,
            "query",
            &[],
            &MemoryInjectConfig::default(),
            true,
            TurnMeta {
                parent_agent_alias: None,
                agent_alias: None,
                turn_id: "t",
                channel_name: "test",
            },
        )
        .await;

        assert!(!context.contains("said hi earlier"));
        assert!(context.contains("- fact: server is prod-3"));
    }

    #[tokio::test]
    async fn uniform_skip_set_filters_poisoned_entries() {
        let mem = FixtureMemory::with(vec![
            entry(
                "matrix_room_history",
                "transcript blob",
                MemoryCategory::Core,
                None,
            ),
            entry("photo", "see [IMAGE:abc123]", MemoryCategory::Core, None),
            entry(
                "stale_tool",
                "<tool_result>old output</tool_result>",
                MemoryCategory::Core,
                None,
            ),
            entry("keeper", "real knowledge", MemoryCategory::Core, None),
        ]);
        let observer = RecordingObserver::default();

        let context = render_memory_context(
            &mem,
            &observer,
            "query",
            &[],
            &MemoryInjectConfig::default(),
            false,
            TurnMeta {
                parent_agent_alias: None,
                agent_alias: None,
                turn_id: "t",
                channel_name: "test",
            },
        )
        .await;

        assert!(!context.contains("transcript blob"));
        assert!(!context.contains("[IMAGE:"));
        assert!(!context.contains("<tool_result"));
        assert!(context.contains("- keeper: real knowledge"));
    }

    #[tokio::test]
    async fn relevance_filter_drops_low_scores_and_keeps_unscored() {
        let mem = FixtureMemory::with(vec![
            entry("low", "barely related", MemoryCategory::Core, Some(0.1)),
            entry("high", "very related", MemoryCategory::Core, Some(0.9)),
            entry("unscored", "keyword backend", MemoryCategory::Core, None),
        ]);
        let observer = RecordingObserver::default();

        let cfg = MemoryInjectConfig {
            min_relevance_score: 0.5,
            ..Default::default()
        };
        let context = render_memory_context(
            &mem,
            &observer,
            "query",
            &[],
            &cfg,
            false,
            TurnMeta {
                parent_agent_alias: None,
                agent_alias: None,
                turn_id: "t",
                channel_name: "test",
            },
        )
        .await;

        assert!(!context.contains("barely related"));
        assert!(context.contains("- high: very related"));
        assert!(context.contains("- unscored: keyword backend"));
    }

    #[tokio::test]
    async fn budgets_cap_entry_count_and_truncate_long_content() {
        let long = "x".repeat(900);
        let mem = FixtureMemory::with(vec![
            entry("a", &long, MemoryCategory::Core, None),
            entry("b", "two", MemoryCategory::Core, None),
            entry("c", "three", MemoryCategory::Core, None),
            entry("d", "four", MemoryCategory::Core, None),
            entry("e", "five", MemoryCategory::Core, None),
        ]);
        let observer = RecordingObserver::default();

        let context = render_memory_context(
            &mem,
            &observer,
            "query",
            &[],
            &MemoryInjectConfig::default(),
            false,
            TurnMeta {
                parent_agent_alias: None,
                agent_alias: None,
                turn_id: "t",
                channel_name: "test",
            },
        )
        .await;

        // Entry cap: 4 of the 5 render.
        assert!(context.contains("- d: four"));
        assert!(!context.contains("- e: five"));
        // Per-entry cap: the 900-char content is ellipsis-truncated.
        assert!(context.contains("..."));
        assert!(!context.contains(&long));
    }

    #[tokio::test]
    async fn total_budget_stops_before_overflowing() {
        let chunk = "y".repeat(700);
        let mem = FixtureMemory::with(vec![
            entry("a", &chunk, MemoryCategory::Core, None),
            entry("b", &chunk, MemoryCategory::Core, None),
            entry("c", &chunk, MemoryCategory::Core, None),
        ]);
        let observer = RecordingObserver::default();

        let cfg = MemoryInjectConfig {
            max_total_chars: 1_500,
            ..Default::default()
        };
        let context = render_memory_context(
            &mem,
            &observer,
            "query",
            &[],
            &cfg,
            false,
            TurnMeta {
                parent_agent_alias: None,
                agent_alias: None,
                turn_id: "t",
                channel_name: "test",
            },
        )
        .await;

        assert!(context.contains("- a: "));
        assert!(context.contains("- b: "));
        assert!(!context.contains("- c: "));
    }

    #[tokio::test]
    async fn multi_session_recall_dedups_by_key_in_order() {
        let mut by_session = HashMap::new();
        by_session.insert(
            Some("history".to_string()),
            vec![
                entry("shared", "from history scope", MemoryCategory::Core, None),
                entry("only_first", "h", MemoryCategory::Core, None),
            ],
        );
        by_session.insert(
            Some("sender".to_string()),
            vec![
                entry("shared", "from sender scope", MemoryCategory::Core, None),
                entry("only_sender", "s", MemoryCategory::Core, None),
            ],
        );
        let mem = FixtureMemory::with_sessions(by_session);
        let observer = RecordingObserver::default();

        let context = render_memory_context(
            &mem,
            &observer,
            "query",
            &[Some("history"), Some("sender")],
            &MemoryInjectConfig::default(),
            false,
            TurnMeta {
                parent_agent_alias: None,
                agent_alias: None,
                turn_id: "t",
                channel_name: "test",
            },
        )
        .await;

        // First scope wins the duplicate key; both uniques render; one event.
        assert!(context.contains("- shared: from history scope"));
        assert!(!context.contains("from sender scope"));
        assert!(context.contains("- only_first: h"));
        assert!(context.contains("- only_sender: s"));
        assert_eq!(observer.recalls.lock().as_slice(), &[(3, true)]);
    }

    /// Injection-path smoke for the retrieval decorator: when the turn's
    /// memory handle is pipeline-wrapped (as `create_memory_for_agent` now
    /// builds it), each renderer recall reaches the scoped backend and the
    /// rendered block remains unchanged.
    #[tokio::test]
    async fn recall_routes_through_retrieval_pipeline_decorator() {
        let fixture = std::sync::Arc::new(FixtureMemory::with(vec![entry(
            "fact",
            "server is prod-3",
            MemoryCategory::Core,
            Some(0.9),
        )]));
        let pipeline = zeroclaw_memory::RetrievalPipeline::new(
            fixture.clone() as std::sync::Arc<dyn Memory>,
            zeroclaw_memory::RetrievalConfig::default(),
        );
        let observer = RecordingObserver::default();

        let first = render_memory_context(
            &pipeline,
            &observer,
            "which server",
            &[],
            &MemoryInjectConfig::default(),
            false,
            TurnMeta {
                parent_agent_alias: None,
                agent_alias: None,
                turn_id: "t",
                channel_name: "test",
            },
        )
        .await;
        let second = render_memory_context(
            &pipeline,
            &observer,
            "which server",
            &[],
            &MemoryInjectConfig::default(),
            false,
            TurnMeta {
                parent_agent_alias: None,
                agent_alias: None,
                turn_id: "t",
                channel_name: "test",
            },
        )
        .await;

        assert!(first.contains("- fact: server is prod-3"));
        assert_eq!(
            first, second,
            "direct backend recall must render identically"
        );
        assert_eq!(
            fixture.recalls.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "each render must reach the backend through the decorator"
        );
        assert_eq!(
            observer.recalls.lock().as_slice(),
            &[(1, true), (1, true)],
            "both renders emit a MemoryRecall event"
        );
    }

    // -- rerank stage + injection goldens --------------------------------

    fn scored_entry(
        key: &str,
        content: &str,
        category: MemoryCategory,
        score: Option<f64>,
        importance: Option<f64>,
        timestamp: chrono::DateTime<chrono::Utc>,
    ) -> MemoryEntry {
        MemoryEntry {
            timestamp: timestamp.to_rfc3339(),
            importance,
            ..entry(key, content, category, score)
        }
    }

    /// The golden fixture corpus: a near-duplicate pair (single-entry pools
    /// cannot distinguish the rerank arm), a Conversation entry, a stale
    /// scored entry old enough for time decay to drop it, and an autosave
    /// key the skip set filters on every arm. Recall order is fixture order.
    fn golden_corpus() -> Vec<MemoryEntry> {
        let now = chrono::Utc::now();
        vec![
            scored_entry(
                "alpha",
                "deploy target is prod-cluster-3",
                MemoryCategory::Core,
                Some(0.9),
                Some(0.8),
                now,
            ),
            scored_entry(
                "alpha_dup",
                "deploy target is prod-cluster-3 for staging",
                MemoryCategory::Core,
                Some(0.85),
                Some(0.1),
                now,
            ),
            scored_entry(
                "beta",
                "team standup moved to 0930",
                MemoryCategory::Daily,
                Some(0.7),
                Some(0.5),
                now,
            ),
            scored_entry(
                "chat",
                "user said hello",
                MemoryCategory::Conversation,
                None,
                None,
                now,
            ),
            scored_entry(
                "stale",
                "quarterly report workflow uses legacy tool",
                MemoryCategory::Daily,
                Some(0.6),
                Some(0.4),
                now - chrono::Duration::days(60),
            ),
            scored_entry(
                "user_msg_9",
                "raw autosave user message",
                MemoryCategory::Core,
                Some(0.99),
                None,
                now,
            ),
        ]
    }

    /// Resolve the injection policy for `origin` and render against it,
    /// mirroring the engine's policy-then-render wiring.
    async fn render_for_origin(
        origin: TurnOrigin,
        has_session: bool,
        suppress: bool,
        mem: &FixtureMemory,
        cfg: &MemoryInjectConfig,
    ) -> String {
        match resolve_inject_policy(origin, has_session, suppress) {
            InjectPolicy::Skip => String::new(),
            InjectPolicy::Inject {
                exclude_conversation,
            } => {
                render_memory_context(
                    mem,
                    &RecordingObserver::default(),
                    "query",
                    &[],
                    cfg,
                    exclude_conversation,
                    TurnMeta {
                        parent_agent_alias: None,
                        agent_alias: None,
                        turn_id: "t",
                        channel_name: "test",
                    },
                )
                .await
            }
        }
    }

    // Flags-off goldens: captured from the pre-rerank renderer's behavior on
    // the corpus (decay drops `stale`, the skip set drops `user_msg_9`, the
    // near-duplicate pair renders twice, recall order is preserved). Later
    // pipeline stages re-prove against these.
    const GOLDEN_FULL: &str = "[Memory context]\n\
        - alpha: deploy target is prod-cluster-3\n\
        - alpha_dup: deploy target is prod-cluster-3 for staging\n\
        - beta: team standup moved to 0930\n\
        - chat: user said hello\n\
        [/Memory context]\n\n";
    const GOLDEN_NO_CONVERSATION: &str = "[Memory context]\n\
        - alpha: deploy target is prod-cluster-3\n\
        - alpha_dup: deploy target is prod-cluster-3 for staging\n\
        - beta: team standup moved to 0930\n\
        [/Memory context]\n\n";
    const GOLDEN_TIGHT_BUDGET: &str = "[Memory context]\n\
        - alpha: deploy target is prod-cluster-3\n\
        - alpha_dup: deploy target is prod-cluster-3 for staging\n\
        [/Memory context]\n\n";
    // Rerank arm on the same corpus: the blend re-sorts, MMR demotes the
    // near-duplicate to the tail, the trim drops the unscored Conversation
    // entry, and `stale` survives (the blend's recency factor replaces the
    // decay drop). Divergence from the flags-off goldens is the point.
    const GOLDEN_RERANK: &str = "[Memory context]\n\
        - alpha: deploy target is prod-cluster-3\n\
        - beta: team standup moved to 0930\n\
        - stale: quarterly report workflow uses legacy tool\n\
        - alpha_dup: deploy target is prod-cluster-3 for staging\n\
        [/Memory context]\n\n";
    // Conversation exclusion is an eligibility boundary, so it runs before
    // duplicate collapse and MMR. Once the ineligible rows are removed, this
    // fixture is below the advanced-strategy threshold and keeps blend order.
    const GOLDEN_RERANK_NO_CONVERSATION: &str = "[Memory context]\n\
        - alpha: deploy target is prod-cluster-3\n\
        - alpha_dup: deploy target is prod-cluster-3 for staging\n\
        - beta: team standup moved to 0930\n\
        - stale: quarterly report workflow uses legacy tool\n\
        [/Memory context]\n\n";

    #[tokio::test]
    async fn injection_goldens_across_origin_budget_and_flags() {
        let flags_off = MemoryInjectConfig::from_memory_config(
            &zeroclaw_config::schema::MemoryConfig::default(),
            DEFAULT_RECALL_LIMIT,
        );
        let rerank_on = MemoryInjectConfig::from_memory_config(
            &zeroclaw_config::schema::MemoryConfig {
                rerank_enabled: true,
                rerank_strategy: "mmr".into(),
                ..Default::default()
            },
            DEFAULT_RECALL_LIMIT,
        );

        let cases: Vec<(&str, TurnOrigin, bool, bool, MemoryInjectConfig, &str)> = vec![
            (
                "interactive_scoped",
                TurnOrigin::Interactive,
                true,
                false,
                flags_off,
                GOLDEN_FULL,
            ),
            (
                "channel_unscoped_excludes_conversation",
                TurnOrigin::Channel,
                false,
                false,
                flags_off,
                GOLDEN_NO_CONVERSATION,
            ),
            (
                "cron_excludes_conversation",
                TurnOrigin::Cron,
                true,
                false,
                flags_off,
                GOLDEN_NO_CONVERSATION,
            ),
            (
                "daemon_excludes_conversation",
                TurnOrigin::Daemon,
                false,
                false,
                flags_off,
                GOLDEN_NO_CONVERSATION,
            ),
            (
                "sub_turn_never_injects",
                TurnOrigin::SubTurn,
                true,
                false,
                flags_off,
                "",
            ),
            // The cron `uses_memory = false` spawn-site opt-out arrives here
            // as `suppress`, so the whole render is skipped.
            (
                "cron_uses_memory_false_suppresses",
                TurnOrigin::Cron,
                true,
                true,
                flags_off,
                "",
            ),
            (
                "tight_entry_budget",
                TurnOrigin::Interactive,
                true,
                false,
                MemoryInjectConfig {
                    max_entries: 2,
                    ..flags_off
                },
                GOLDEN_TIGHT_BUDGET,
            ),
            (
                "tight_total_char_budget",
                TurnOrigin::Interactive,
                true,
                false,
                MemoryInjectConfig {
                    max_total_chars: 100,
                    ..flags_off
                },
                GOLDEN_TIGHT_BUDGET,
            ),
            (
                "rerank_on_interactive",
                TurnOrigin::Interactive,
                true,
                false,
                rerank_on,
                GOLDEN_RERANK,
            ),
            (
                "rerank_on_cron",
                TurnOrigin::Cron,
                true,
                false,
                rerank_on,
                GOLDEN_RERANK_NO_CONVERSATION,
            ),
        ];

        for (name, origin, has_session, suppress, cfg, want) in cases {
            let mem = FixtureMemory::with(golden_corpus());
            let got = render_for_origin(origin, has_session, suppress, &mem, &cfg).await;
            assert_eq!(got, want, "golden mismatch for case {name}");
        }
    }

    #[tokio::test]
    async fn rerank_on_mmr_drops_near_duplicate_and_reorders() {
        let now = chrono::Utc::now();
        let pool = vec![
            scored_entry(
                "a",
                "rust memory scoring pipeline",
                MemoryCategory::Daily,
                Some(0.95),
                None,
                now,
            ),
            scored_entry(
                "b",
                "rust memory scoring pipeline latency",
                MemoryCategory::Daily,
                Some(0.93),
                None,
                now,
            ),
            scored_entry(
                "c",
                "garden tomatoes irrigation calendar",
                MemoryCategory::Daily,
                Some(0.75),
                None,
                now,
            ),
        ];

        // Flags off: the near-duplicate pair renders untouched, recall order.
        let mem = FixtureMemory::with(pool.clone());
        let off = MemoryInjectConfig {
            min_relevance_score: 0.4,
            ..Default::default()
        };
        let context = render_memory_context(
            &mem,
            &RecordingObserver::default(),
            "query",
            &[],
            &off,
            false,
            TurnMeta {
                parent_agent_alias: None,
                agent_alias: None,
                turn_id: "t",
                channel_name: "test",
            },
        )
        .await;
        assert!(context.contains("- a: "));
        assert!(context.contains("- b: "));
        assert!(context.contains("- c: "));

        // Rerank on with a 2-slot trim: MMR demotes the near-duplicate below
        // the unrelated entry and the trim drops it.
        let mem = FixtureMemory::with(pool);
        let on = MemoryInjectConfig {
            limit: 2,
            min_relevance_score: 0.4,
            rerank_enabled: true,
            candidate_multiplier: 3,
            rerank: rerank::RerankConfig {
                strategy: rerank::RerankStrategy::Mmr { lambda: 0.5 },
                threshold: 2,
                importance_weight: 0.2,
                recency_weight: 0.1,
                min_relevance_score: 0.4,
                final_limit: 2,
                candidate_pool_cap: 6,
            },
            ..Default::default()
        };
        let context = render_memory_context(
            &mem,
            &RecordingObserver::default(),
            "query",
            &[],
            &on,
            false,
            TurnMeta {
                parent_agent_alias: None,
                agent_alias: None,
                turn_id: "t",
                channel_name: "test",
            },
        )
        .await;
        let a_pos = context.find("- a: ").expect("top entry renders");
        let c_pos = context.find("- c: ").expect("diverse entry renders");
        assert!(a_pos < c_pos, "relevance leader stays first");
        assert!(
            !context.contains("- b: "),
            "near-duplicate must be demoted out of the trimmed set"
        );
    }

    #[tokio::test]
    async fn rerank_gates_candidate_over_fetch() {
        // On: recall over-fetches limit * candidate_multiplier.
        let mem = FixtureMemory::with(vec![]);
        let cfg = MemoryInjectConfig {
            limit: 5,
            rerank_enabled: true,
            candidate_multiplier: 4,
            ..Default::default()
        };
        render_memory_context(
            &mem,
            &RecordingObserver::default(),
            "q",
            &[],
            &cfg,
            false,
            TurnMeta {
                parent_agent_alias: None,
                agent_alias: None,
                turn_id: "t",
                channel_name: "test",
            },
        )
        .await;
        assert_eq!(mem.recorded_limits.lock().as_slice(), &[20]);

        // Off: the multiplier is inert; recall requests exactly `limit`.
        let mem = FixtureMemory::with(vec![]);
        let cfg = MemoryInjectConfig {
            limit: 5,
            rerank_enabled: false,
            candidate_multiplier: 4,
            ..Default::default()
        };
        render_memory_context(
            &mem,
            &RecordingObserver::default(),
            "q",
            &[],
            &cfg,
            false,
            TurnMeta {
                parent_agent_alias: None,
                agent_alias: None,
                turn_id: "t",
                channel_name: "test",
            },
        )
        .await;
        assert_eq!(mem.recorded_limits.lock().as_slice(), &[5]);
    }

    #[tokio::test]
    async fn rerank_bounds_unlimited_recall_sentinel_before_backend_and_scan() {
        let now = chrono::Utc::now();
        let mut pool: Vec<MemoryEntry> = (0..rerank::MAX_CANDIDATE_POOL)
            .map(|i| {
                scored_entry(
                    &format!("bounded_{i:04}"),
                    &format!("unique bounded topic {i} marker_{i}"),
                    MemoryCategory::Daily,
                    Some(0.2 + (i as f64 / 10_000.0)),
                    Some(0.0),
                    now,
                )
            })
            .collect();
        pool.push(scored_entry(
            "out_of_bound_winner",
            "tail candidate should never enter rerank scans",
            MemoryCategory::Core,
            Some(1.0),
            Some(1.0),
            now,
        ));

        let mem = FixtureMemory::with(pool);
        let mut memory = zeroclaw_config::schema::MemoryConfig {
            rerank_enabled: true,
            candidate_multiplier: 20,
            min_relevance_score: 0.0,
            ..Default::default()
        };
        memory.rerank_strategy = "none".into();
        let cfg = MemoryInjectConfig::from_memory_config(&memory, usize::MAX);

        assert_eq!(cfg.limit, rerank::MAX_CANDIDATE_POOL);
        assert_eq!(cfg.rerank.final_limit, rerank::MAX_CANDIDATE_POOL);
        assert_eq!(cfg.rerank.candidate_pool_cap, rerank::MAX_CANDIDATE_POOL);

        let context = render_memory_context(
            &mem,
            &RecordingObserver::default(),
            "query",
            &[],
            &cfg,
            false,
            TurnMeta {
                parent_agent_alias: None,
                agent_alias: None,
                turn_id: "t",
                channel_name: "test",
            },
        )
        .await;

        assert_eq!(
            mem.recorded_limits.lock().as_slice(),
            &[rerank::MAX_CANDIDATE_POOL],
            "unlimited profile sentinel must be capped before backend recall"
        );
        assert!(
            !context.contains("out_of_bound_winner"),
            "over-returned tail candidate must be truncated before rerank work"
        );
        assert_eq!(
            context.matches("\n- ").count(),
            DEFAULT_MAX_ENTRIES,
            "renderer entry budget still applies after bounded rerank"
        );
    }

    #[tokio::test]
    async fn rerank_output_still_flows_through_budget_caps() {
        let now = chrono::Utc::now();
        let pool: Vec<MemoryEntry> = (0..6)
            .map(|i| {
                scored_entry(
                    &format!("k{i}"),
                    &format!("distinct topic number {i} zebra{i}"),
                    MemoryCategory::Daily,
                    Some(0.9 - i as f64 * 0.05),
                    None,
                    now,
                )
            })
            .collect();
        let mem = FixtureMemory::with(pool);
        let cfg = MemoryInjectConfig {
            limit: 6,
            rerank_enabled: true,
            candidate_multiplier: 2,
            rerank: rerank::RerankConfig::disabled(6, 0.0),
            ..Default::default()
        };
        let context = render_memory_context(
            &mem,
            &RecordingObserver::default(),
            "query",
            &[],
            &cfg,
            false,
            TurnMeta {
                parent_agent_alias: None,
                agent_alias: None,
                turn_id: "t",
                channel_name: "test",
            },
        )
        .await;
        // Rerank returned 6 candidates; the entry-count budget still caps
        // the rendered block at the default 4.
        assert_eq!(context.matches("\n- ").count(), DEFAULT_MAX_ENTRIES);
    }

    #[test]
    fn from_memory_config_threads_rerank_settings() {
        let mut memory = zeroclaw_config::schema::MemoryConfig::default();
        let cfg = MemoryInjectConfig::from_memory_config(&memory, 7);
        assert_eq!(cfg.limit, 7);
        assert!((cfg.min_relevance_score - memory.min_relevance_score).abs() < f64::EPSILON);
        assert!(!cfg.rerank_enabled);
        assert_eq!(cfg.candidate_multiplier, memory.candidate_multiplier);
        assert_eq!(cfg.rerank.strategy, rerank::RerankStrategy::None);
        assert_eq!(cfg.rerank.threshold, memory.rerank_threshold);
        assert_eq!(cfg.rerank.final_limit, 7);

        // Strategy parsing is case- and whitespace-tolerant.
        memory.rerank_enabled = true;
        memory.rerank_strategy = " MMR ".into();
        memory.mmr_lambda = 0.6;
        let cfg = MemoryInjectConfig::from_memory_config(&memory, 5);
        assert!(cfg.rerank_enabled);
        assert_eq!(
            cfg.rerank.strategy,
            rerank::RerankStrategy::Mmr { lambda: 0.6 }
        );

        // Unknown strategies fall back to no advanced rerank.
        memory.rerank_strategy = "llm_judge".into();
        let cfg = MemoryInjectConfig::from_memory_config(&memory, 5);
        assert_eq!(cfg.rerank.strategy, rerank::RerankStrategy::None);

        // A zero multiplier is clamped so over-fetch never zeroes recall.
        memory.candidate_multiplier = 0;
        let cfg = MemoryInjectConfig::from_memory_config(&memory, 5);
        assert_eq!(cfg.candidate_multiplier, 1);
    }

    /// Regression: a high-scoring but render-ineligible entry must not consume
    /// a final-selection slot. With a five-entry budget and five eligible facts
    /// plus one ineligible top-scored row, the pre-trim eligibility filter lets
    /// all five eligible facts render instead of only four.
    #[tokio::test]
    async fn rerank_eligibility_filter_frees_slots_for_valid_candidates() {
        let now = chrono::Utc::now();
        let pool = vec![
            // Ineligible (`_history` key) yet outranks every fact.
            scored_entry(
                "channel_history",
                "raw transcript blob",
                MemoryCategory::Core,
                Some(0.99),
                Some(1.0),
                now,
            ),
            scored_entry(
                "fact_a",
                "alpha apples",
                MemoryCategory::Core,
                Some(0.9),
                Some(0.5),
                now,
            ),
            scored_entry(
                "fact_b",
                "beta bananas",
                MemoryCategory::Daily,
                Some(0.8),
                Some(0.5),
                now,
            ),
            scored_entry(
                "fact_c",
                "gamma grapes",
                MemoryCategory::Daily,
                Some(0.7),
                Some(0.5),
                now,
            ),
            scored_entry(
                "fact_d",
                "delta dates",
                MemoryCategory::Daily,
                Some(0.6),
                Some(0.5),
                now,
            ),
            scored_entry(
                "fact_e",
                "epsilon elderberry",
                MemoryCategory::Daily,
                Some(0.5),
                Some(0.5),
                now,
            ),
        ];
        let mem = FixtureMemory::with(pool);
        let cfg = MemoryInjectConfig {
            limit: 5,
            min_relevance_score: 0.0,
            max_entries: 5,
            rerank_enabled: true,
            candidate_multiplier: 2,
            rerank: rerank::RerankConfig {
                strategy: rerank::RerankStrategy::None,
                threshold: usize::MAX,
                importance_weight: 0.2,
                recency_weight: 0.1,
                min_relevance_score: 0.0,
                final_limit: 5,
                candidate_pool_cap: 10,
            },
            ..Default::default()
        };

        let context = render_memory_context(
            &mem,
            &RecordingObserver::default(),
            "query",
            &[],
            &cfg,
            false,
            TurnMeta {
                parent_agent_alias: None,
                agent_alias: None,
                turn_id: "t",
                channel_name: "test",
            },
        )
        .await;

        assert_eq!(
            context.matches("\n- ").count(),
            5,
            "all five eligible facts fill the budget"
        );
        assert!(
            !context.contains("channel_history"),
            "ineligible row dropped"
        );
        assert!(context.contains("- fact_e: epsilon elderberry"));
    }

    /// Composed score-domain regression: current SQLite recall owns BM25
    /// calibration, while this module owns the blend and relevance floor.
    /// Exercise the stock no-embedding backend through the real renderer so
    /// those two layers cannot silently drift back onto incompatible scales.
    #[tokio::test]
    async fn noop_sqlite_bm25_recall_stays_ranked_through_rerank_injection() {
        let tmp = tempfile::TempDir::new().unwrap();
        let memory = zeroclaw_memory::SqliteMemory::new("rerank-bm25", tmp.path()).unwrap();
        assert_eq!(
            memory.embedder_dimensions(),
            0,
            "the regression must exercise the BM25-only recall path"
        );

        memory
            .store(
                "best_match",
                "orbital vault orbital vault passphrase",
                MemoryCategory::Core,
                None,
            )
            .await
            .unwrap();
        memory
            .store(
                "supporting_match",
                "orbital vault maintenance schedule",
                MemoryCategory::Core,
                None,
            )
            .await
            .unwrap();

        let recalled = memory
            .recall("orbital vault", 20, None, None, None)
            .await
            .unwrap();
        assert_eq!(recalled.len(), 2);
        assert!(
            recalled.iter().all(|entry| {
                entry
                    .score
                    .is_some_and(|score| (0.0..=1.0).contains(&score))
            }),
            "the recall boundary must normalize every BM25 score"
        );
        assert!(
            recalled[0].score > recalled[1].score,
            "term-frequency relevance must remain distinguishable"
        );

        let config = zeroclaw_config::schema::MemoryConfig {
            rerank_enabled: true,
            rerank_strategy: "none".into(),
            min_relevance_score: 0.4,
            ..Default::default()
        };
        let inject = MemoryInjectConfig::from_memory_config(&config, 5);
        let context = render_memory_context(
            &memory,
            &RecordingObserver::default(),
            "orbital vault",
            &[],
            &inject,
            false,
            TurnMeta {
                parent_agent_alias: None,
                agent_alias: None,
                turn_id: "t",
                channel_name: "test",
            },
        )
        .await;

        let best = context
            .find("- best_match: ")
            .expect("the relevance leader survives the configured floor");
        let supporting = context
            .find("- supporting_match: ")
            .expect("the weaker relevant entry survives the configured floor");
        assert!(best < supporting, "BM25 relevance order survives blending");
    }
}
