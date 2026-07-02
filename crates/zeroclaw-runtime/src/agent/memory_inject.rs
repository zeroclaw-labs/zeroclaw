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
//! Pipeline: recall (per session, key-deduped) -> time decay -> relevance
//! filter -> skip set (autosave keys/content, `*_history` keys, `[IMAGE:`
//! markers, `<tool_result` blocks, optional Conversation-category
//! exclusion) -> budget caps (entry count, per-entry chars, total chars)
//! -> `[Memory context]` wrapper. Exactly one `MemoryRecall` observer
//! event is emitted per render, covering all recalls.

use std::collections::HashSet;
use std::fmt::Write as _;
use std::time::Instant;

use zeroclaw_api::ingress::TurnOrigin;
use zeroclaw_memory::{
    self, MEMORY_CONTEXT_CLOSE, MEMORY_CONTEXT_OPEN, Memory, MemoryCategory, MemoryEntry, decay,
};

use super::loop_::make_query_summary;
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
}

impl Default for MemoryInjectConfig {
    fn default() -> Self {
        Self {
            limit: DEFAULT_RECALL_LIMIT,
            min_relevance_score: 0.0,
            max_entries: DEFAULT_MAX_ENTRIES,
            entry_max_chars: DEFAULT_ENTRY_MAX_CHARS,
            max_total_chars: DEFAULT_MAX_TOTAL_CHARS,
        }
    }
}

/// The resolved injection decision for one turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InjectPolicy {
    /// No injection: nested sub-turns, or a spawn site suppressed it.
    Skip,
    /// Inject via the uniform pipeline.
    Inject {
        /// Drop `MemoryCategory::Conversation` entries. True for scheduled
        /// origins (chat history must not leak into autonomous runs, #5456)
        /// and for turns without a session scope (without a session filter,
        /// other channels' conversations would bleed in).
        exclude_conversation: bool,
    },
}

/// Resolve the injection policy from who initiated the turn.
///
/// The uniform rule set:
/// - `SubTurn` never injects: a nested turn must not re-absorb the parent's
///   memory; the parent states what the child needs in the prompt it writes.
/// - Scheduled origins (`Cron`, `Daemon`) inject with Conversation entries
///   excluded (#5456).
/// - All other origins inject; Conversation entries are excluded when the
///   turn has no session scope.
/// - `suppress` covers per-spawn opt-outs (e.g. a cron job configured with
///   `uses_memory = false`).
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

/// Render the memory-context preamble for one turn: the uniform pipeline
/// over one or more session scopes (multi-session recall is key-deduped in
/// order, mirroring the channel renderer's history-key + sender recall).
///
/// Returns the wrapped block followed by a blank line, or an empty string
/// when nothing qualifies. Recall errors are swallowed (the turn proceeds
/// without memory); exactly one `MemoryRecall` observer event is emitted
/// per call, with `success = false` only when every recall failed.
pub async fn render_memory_context(
    mem: &dyn Memory,
    observer: &dyn Observer,
    user_msg: &str,
    sessions: &[Option<&str>],
    cfg: &MemoryInjectConfig,
    exclude_conversation: bool,
) -> String {
    let backend = mem.name().to_string();
    let query_summary = make_query_summary(user_msg);

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
            .recall(user_msg, cfg.limit, *session_id, None, None)
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
    });

    // Older non-Core memories score lower; Core is evergreen.
    decay::apply_time_decay(&mut entries, decay::DEFAULT_HALF_LIFE_DAYS);

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
            agent_alias: None,
            agent_id: None,
        }
    }

    /// Recall returns the fixture list for the requested session scope;
    /// `fail` simulates a backend error.
    struct FixtureMemory {
        by_session: HashMap<Option<String>, Vec<MemoryEntry>>,
        fail: bool,
    }

    impl FixtureMemory {
        fn with(entries: Vec<MemoryEntry>) -> Self {
            let mut by_session = HashMap::new();
            by_session.insert(None, entries);
            Self {
                by_session,
                fail: false,
            }
        }

        fn failing() -> Self {
            Self {
                by_session: HashMap::new(),
                fail: true,
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
            _limit: usize,
            session_id: Option<&str>,
            _since: Option<&str>,
            _until: Option<&str>,
        ) -> anyhow::Result<Vec<MemoryEntry>> {
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
    }

    impl Observer for RecordingObserver {
        fn record_event(&self, event: &ObserverEvent) {
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
        )
        .await;

        assert!(context.starts_with(MEMORY_CONTEXT_OPEN));
        assert!(context.contains("- user_preference: prefers concise answers"));
        assert!(context.ends_with(&format!("{MEMORY_CONTEXT_CLOSE}\n\n")));
        assert_eq!(observer.recalls.lock().as_slice(), &[(1, true)]);
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
        let context = render_memory_context(&mem, &observer, "query", &[], &cfg, false).await;

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
        let context = render_memory_context(&mem, &observer, "query", &[], &cfg, false).await;

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
        let mem = FixtureMemory {
            by_session,
            fail: false,
        };
        let observer = RecordingObserver::default();

        let context = render_memory_context(
            &mem,
            &observer,
            "query",
            &[Some("history"), Some("sender")],
            &MemoryInjectConfig::default(),
            false,
        )
        .await;

        // First scope wins the duplicate key; both uniques render; one event.
        assert!(context.contains("- shared: from history scope"));
        assert!(!context.contains("from sender scope"));
        assert!(context.contains("- only_first: h"));
        assert!(context.contains("- only_sender: s"));
        assert_eq!(observer.recalls.lock().as_slice(), &[(3, true)]);
    }
}
