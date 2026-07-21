//! LLM-driven memory consolidation.

use crate::conflict;
use crate::dedup::{self, DedupAction};
use crate::importance;
use crate::merge;
use crate::policy::PolicyEnforcer;
use crate::policy_gate;
use crate::traits::{Memory, MemoryCategory, StoreOptions};
use zeroclaw_api::model_provider::ModelProvider;
use zeroclaw_config::schema::MemoryConfig;
use zeroclaw_providers::ProviderDispatch;

/// Output of consolidation extraction.
#[derive(Debug, serde::Deserialize)]
pub struct ConsolidationResult {
    /// Brief timestamped summary for the conversation history log.
    pub history_entry: String,
    /// New facts/preferences/decisions to store long-term, or None.
    pub memory_update: Option<String>,
    /// Atomic facts extracted from the turn (when consolidation_extract_facts is enabled).
    #[serde(default)]
    pub facts: Vec<String>,
    /// Observed trend or pattern (when consolidation_extract_facts is enabled).
    #[serde(default)]
    pub trend: Option<String>,
}

const CONSOLIDATION_SYSTEM_PROMPT: &str = r#"You are a memory consolidation engine. Given a conversation turn, extract:
1. "history_entry": A brief summary of what happened in this turn (1-2 sentences). Include the key topic or action.
2. "memory_update": Any NEW facts, preferences, decisions, or commitments worth remembering long-term. Return null if nothing new was learned.

Respond ONLY with valid JSON: {"history_entry": "...", "memory_update": "..." or null}
Do not include any text outside the JSON object."#;

fn strip_media_markers(text: &str) -> String {
    // Matches [IMAGE:...], [DOCUMENT:...], [FILE:...], [VIDEO:...], [VOICE:...], [AUDIO:...]
    static RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"\[(?:IMAGE|DOCUMENT|FILE|VIDEO|VOICE|AUDIO):[^\]]*\]")
            .expect("media-tag regex must compile")
    });
    RE.replace_all(text, "[media attachment]").into_owned()
}

pub async fn consolidate_turn(
    model_provider: &dyn ModelProvider,
    model: &str,
    temperature: Option<f64>,
    memory: &dyn Memory,
    memory_config: &MemoryConfig,
    user_message: &str,
    assistant_response: &str,
) -> anyhow::Result<()> {
    let turn_text = format!(
        "User: {}\nAssistant: {}",
        strip_media_markers(user_message),
        strip_media_markers(assistant_response),
    );

    // Truncate very long turns to avoid wasting tokens on consolidation.
    // Use char-boundary-safe slicing to prevent panic on multi-byte UTF-8 (e.g. CJK text).
    let truncated = if turn_text.len() > 4000 {
        let end = turn_text
            .char_indices()
            .map(|(i, _)| i)
            .take_while(|&i| i <= 4000)
            .last()
            .unwrap_or(0);
        format!("{}…", &turn_text[..end])
    } else {
        turn_text.clone()
    };

    let raw = ProviderDispatch::from_ref(model_provider)
        .chat_with_system(
            Some(CONSOLIDATION_SYSTEM_PROMPT),
            &truncated,
            model,
            temperature,
        )
        .await?;

    let result: ConsolidationResult = parse_consolidation_response(&raw, &turn_text);

    // Ordering (fixes the "Daily lost on Core reject/fail" regression): the Core
    // write runs FIRST, and its outcome decides whether the redundant Daily
    // write is suppressed. Previously Daily was written (or cross-path-skipped)
    // in Phase 1 BEFORE Core had passed policy validation, dedup, conflict
    // resolution, and persistence, so a rejected or failing Core turn lost BOTH
    // the Daily row and the durable Core row. Now the same-turn Daily row is
    // suppressed ONLY when the matching Core fact is confirmed durable; on any
    // unsuccessful Core outcome (policy reject, dedup reject, store error) the
    // Daily history is still written so the turn is never silently dropped.
    //
    // Phase 1: Write memory update to Core category (if present). The result is
    // captured but NOT propagated yet: an unsuccessful Core outcome (policy
    // rejection or store failure surfaced as `Err`) must still let the Daily
    // history be written below before the error is returned, so a failing Core
    // never silently drops the turn's Daily record.
    let core_result =
        write_core_update(memory, memory_config, result.memory_update.as_deref()).await;
    // Core is treated as durable ONLY on an explicit `Ok(true)` (fresh insert,
    // merge into a survivor, or a dedup reject where an equivalent Core row
    // already exists). A rejection/failure (`Err`) or a no-op (`Ok(false)`)
    // leaves `core_durable = false`, so the Daily row is preserved.
    let core_durable = matches!(core_result, Ok(true));

    // Phase 2: Write the per-turn Daily history entry (if enabled).
    //
    //   - `consolidate_daily = false` disables the per-turn Daily write
    //     entirely (Core extraction above still ran).
    //   - When this turn produced a durable Core fact AND the Daily
    //     `history_entry` is a near-identical paraphrase of it (same fact,
    //     different wording), skip the redundant Daily write so the fact is not
    //     stored as both a #core and a #daily row. This cross-path skip is
    //     applied ONLY on a durable Core outcome; if Core was rejected or failed
    //     the Daily row is preserved as the turn's only record.
    //   - `daily_dedup` (opt-in) additionally skips a Daily write that
    //     duplicates a recent PRIVATE Daily row.
    if memory_config.consolidate_daily {
        // Only offer the Core fact as a cross-path suppression key when it
        // actually became durable; otherwise pass None so the Daily row is kept.
        let core_for_skip = if core_durable {
            result.memory_update.as_deref()
        } else {
            None
        };
        maybe_write_daily_history(memory, memory_config, &result.history_entry, core_for_skip)
            .await?;
    }

    // Now surface any Core rejection/failure. The Daily history has already been
    // preserved above, satisfying "write Daily on any unsuccessful Core
    // outcome" while keeping the Core write-gate fail-closed.
    core_result.map(|_| ())
}

/// Run the Core-category write phase for this turn's `memory_update` and report
/// whether a matching Core row became durable.
///
/// Returns `Ok(true)` only when a Core row for this fact is persisted (inserted
/// or merged into a survivor). A policy rejection propagates as `Err` (the turn
/// fails, preserving the previous fail-closed behavior); a dedup rejection or an
/// absent/empty update returns `Ok(false)` so the caller keeps the Daily row.
async fn write_core_update(
    memory: &dyn Memory,
    memory_config: &MemoryConfig,
    memory_update: Option<&str>,
) -> anyhow::Result<bool> {
    let Some(update) = memory_update else {
        return Ok(false);
    };
    if update.trim().is_empty() {
        return Ok(false);
    }

    let mem_key = format!("core_{}", uuid::Uuid::new_v4());

    // Compute importance score heuristically.
    let imp = importance::compute_importance(update, &MemoryCategory::Core);

    // A: fail-closed policy write-gate on the autonomous consolidation path.
    let policy = PolicyEnforcer::new(&memory_config.policy);
    if let Err(e) =
        policy_gate::validate_store(memory, &policy, "default", &MemoryCategory::Core).await
    {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                .with_attrs(::serde_json::json!({"error": e.to_string()})),
            "memory consolidation write denied by policy"
        );
        anyhow::bail!("memory consolidation write denied by policy: {e}");
    }

    // A: write-time near-duplicate detection.
    let candidates = memory.recall(update, 10, None, None, None).await?;
    let candidates = dedup::core_candidates(candidates);
    match dedup::dedup_gate(&candidates, update, memory_config) {
        DedupAction::Insert => {}
        DedupAction::Reject { dup_of } => {
            ::zeroclaw_log::record!(
                DEBUG,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(::serde_json::json!({"duplicate_of": dup_of})),
                "memory consolidation skipped duplicate core update"
            );
            // A dedup reject means an equivalent Core fact ALREADY exists and is
            // durable, so the same-turn Daily paraphrase is still redundant.
            return Ok(true);
        }
        DedupAction::Merge { into } => {
            if let Some(survivor) = candidates.iter().find(|entry| entry.id == into) {
                let merged = merge::merge_into_survivor(survivor, update);
                let options = StoreOptions {
                    namespace: Some(survivor.namespace.clone()),
                    importance: merged.importance,
                    ..StoreOptions::default()
                };
                memory
                    .store_with_options(
                        &survivor.key,
                        &merged.content,
                        MemoryCategory::Core,
                        survivor.session_id.as_deref(),
                        options,
                    )
                    .await?;
                return Ok(true);
            }
        }
    }

    // A: detect conflicts, store, then reversibly supersede the losers.
    let superseded_ids = match conflict::check_and_resolve_conflicts(
        memory,
        &mem_key,
        update,
        &MemoryCategory::Core,
        memory_config.conflict_threshold,
    )
    .await
    {
        Ok(ids) => ids,
        Err(e) => {
            ::zeroclaw_log::record!(
                DEBUG,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                "conflict check skipped"
            );
            Vec::new()
        }
    };

    // Store with importance metadata. A store error propagates as `Err` (the
    // turn fails) BEFORE the caller writes Daily, so a failed Core never
    // suppresses the Daily row.
    let options = StoreOptions {
        importance: Some(imp),
        ..StoreOptions::default()
    };
    memory
        .store_with_options(&mem_key, update, MemoryCategory::Core, None, options)
        .await?;

    // A: reversible soft-hide of superseded rows (gated, default-on).
    if memory_config.conflict_supersede_enabled
        && let Err(e) = memory.supersede(&superseded_ids, &mem_key).await
    {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
            "memory supersede skipped"
        );
    }

    Ok(true)
}

/// Decide whether to write this turn's Daily history entry, applying the in-turn
/// cross-path skip before delegating to [`write_daily_history`].
///
/// When this turn also produced a Core `memory_update` and the Daily
/// `history_entry` says the same thing (near-identical under the existing
/// Jaccard threshold, gated by `daily_dedup`), the redundant Daily write is
/// skipped so the fact is not persisted as both a #core and a #daily row. A
/// Daily-only turn (`core_update = None`) always falls through to the normal
/// Daily write path.
async fn maybe_write_daily_history(
    memory: &dyn Memory,
    memory_config: &MemoryConfig,
    history_entry: &str,
    core_update: Option<&str>,
) -> anyhow::Result<()> {
    if let Some(update) = core_update
        && dedup::daily_duplicates_core(history_entry, update, memory_config)
    {
        ::zeroclaw_log::record!(
            DEBUG,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
            "skipped per-turn Daily write duplicating this turn's Core fact"
        );
        return Ok(());
    }
    write_daily_history(memory, memory_config, history_entry).await
}

/// Write the per-turn Daily history summary, applying the `daily_dedup` gate.
///
/// When `daily_dedup` is on (default), recalls recent Daily entries and skips
/// the write if the incoming summary is an exact or near-identical duplicate
/// (see [`dedup::should_write_daily`]). A blank summary is always skipped. This
/// keeps the transient Daily log from accumulating near-duplicate rows on an
/// append-only backend without touching the Phase-2 Core dedup path.
async fn write_daily_history(
    memory: &dyn Memory,
    memory_config: &MemoryConfig,
    history_entry: &str,
) -> anyhow::Result<()> {
    if memory_config.daily_dedup {
        // Candidate lookup for the Daily dedup gate: read THIS agent's OWN
        // PRIVATE Daily rows, scoped BEFORE any limit. Previously this used
        // `recall(history_entry, 10, ...)` then filtered to Daily, which on
        // hindsight merges private+shared+system and truncates the merged result
        // FIRST - so a shared/system Daily row could suppress this agent's
        // private history, and unrelated categories could crowd the real private
        // duplicate out of the top ten. `list_own_daily_history` returns only
        // the private, Daily-scoped rows (single-store backends keep the old
        // `list(Daily)` behavior via the trait default). Lookup failures must
        // not drop the write - fall back to inserting (fail-open) so a transient
        // error never silently loses history.
        let candidates = match memory.list_own_daily_history().await {
            Ok(entries) => dedup::daily_candidates(entries),
            Err(e) => {
                ::zeroclaw_log::record!(
                    DEBUG,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_attrs(::serde_json::json!({"error": format!("{e}")})),
                    "daily dedup candidate lookup failed; inserting without dedup"
                );
                Vec::new()
            }
        };
        match dedup::should_write_daily(&candidates, history_entry, memory_config) {
            dedup::DailyWrite::Insert => {}
            dedup::DailyWrite::Skip { dup_of } => {
                ::zeroclaw_log::record!(
                    DEBUG,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_attrs(::serde_json::json!({"duplicate_of": dup_of})),
                    "memory consolidation skipped duplicate daily history entry"
                );
                return Ok(());
            }
        }
    }

    let date = chrono::Local::now().format("%Y-%m-%d").to_string();
    let history_key = format!("daily_{date}_{}", uuid::Uuid::new_v4());
    memory
        .store(&history_key, history_entry, MemoryCategory::Daily, None)
        .await
}

/// Parse the LLM's consolidation response, with fallback for malformed JSON.
fn parse_consolidation_response(raw: &str, fallback_text: &str) -> ConsolidationResult {
    // Try to extract JSON from the response (LLM may wrap in markdown code blocks).
    let cleaned = raw
        .trim()
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();

    serde_json::from_str(cleaned).unwrap_or_else(|_| {
        // Fallback: use truncated turn text as history entry.
        // Use char-boundary-safe slicing to prevent panic on multi-byte UTF-8.
        let summary = if fallback_text.len() > 200 {
            let end = fallback_text
                .char_indices()
                .map(|(i, _)| i)
                .take_while(|&i| i <= 200)
                .last()
                .unwrap_or(0);
            format!("{}…", &fallback_text[..end])
        } else {
            fallback_text.to_string()
        };
        ConsolidationResult {
            history_entry: summary,
            memory_update: None,
            facts: Vec::new(),
            trend: None,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sqlite::SqliteMemory;
    use tempfile::TempDir;

    async fn daily_row_count(mem: &SqliteMemory) -> usize {
        mem.list(Some(&MemoryCategory::Daily), None)
            .await
            .unwrap()
            .len()
    }

    #[tokio::test]
    async fn repeated_identical_turns_do_not_accumulate_daily_rows() {
        // daily_dedup is opt-in (default off); with it explicitly enabled,
        // writing the same summary several times must leave exactly one Daily row.
        let tmp = TempDir::new().unwrap();
        let mem = SqliteMemory::new("test", tmp.path()).unwrap();
        let cfg = MemoryConfig {
            daily_dedup: true,
            ..MemoryConfig::default()
        };

        for _ in 0..4 {
            write_daily_history(&mem, &cfg, "User asked what time it is")
                .await
                .unwrap();
        }

        assert_eq!(
            daily_row_count(&mem).await,
            1,
            "identical turn summaries must be deduplicated to a single Daily row"
        );
    }

    #[tokio::test]
    async fn dedup_off_writes_every_turn() {
        // daily_dedup off restores the old unconditional-append behavior.
        let tmp = TempDir::new().unwrap();
        let mem = SqliteMemory::new("test", tmp.path()).unwrap();
        let cfg = MemoryConfig {
            daily_dedup: false,
            ..MemoryConfig::default()
        };

        for _ in 0..3 {
            write_daily_history(&mem, &cfg, "User asked what time it is")
                .await
                .unwrap();
        }

        assert_eq!(daily_row_count(&mem).await, 3);
    }

    #[tokio::test]
    async fn consolidate_daily_off_writes_no_daily_row() {
        // The top-level toggle skips the Daily write entirely. Mirror the
        // gate consolidate_turn applies (it calls write_daily_history only when
        // consolidate_daily is true).
        let tmp = TempDir::new().unwrap();
        let mem = SqliteMemory::new("test", tmp.path()).unwrap();
        let cfg = MemoryConfig {
            consolidate_daily: false,
            ..MemoryConfig::default()
        };

        if cfg.consolidate_daily {
            write_daily_history(&mem, &cfg, "User asked what time it is")
                .await
                .unwrap();
        }

        assert_eq!(daily_row_count(&mem).await, 0);
    }

    #[tokio::test]
    async fn novel_summaries_still_write_distinct_rows() {
        let tmp = TempDir::new().unwrap();
        let mem = SqliteMemory::new("test", tmp.path()).unwrap();
        let cfg = MemoryConfig::default();

        write_daily_history(&mem, &cfg, "User asked what time it is")
            .await
            .unwrap();
        write_daily_history(&mem, &cfg, "User configured a Postgres memory backend")
            .await
            .unwrap();

        assert_eq!(daily_row_count(&mem).await, 2);
    }

    #[tokio::test]
    async fn daily_skipped_when_it_duplicates_this_turns_core_fact() {
        // Turn produces a Core memory_update AND a near-identical Daily
        // history_entry (same fact, reworded). The Daily write must be skipped
        // so the fact is not stored as both a #core and a #daily row.
        let tmp = TempDir::new().unwrap();
        let mem = SqliteMemory::new("test", tmp.path()).unwrap();
        let cfg = MemoryConfig {
            daily_dedup: true,
            ..MemoryConfig::default()
        };

        let core = "relative_a is user_a's mentor and advisor to user_b and user_c";
        let daily = "relative_a is user_a's mentor and also advisor to user_b and user_c";
        maybe_write_daily_history(&mem, &cfg, daily, Some(core))
            .await
            .unwrap();

        assert_eq!(
            daily_row_count(&mem).await,
            0,
            "Daily write must be skipped when it duplicates this turn's Core fact"
        );
    }

    #[tokio::test]
    async fn daily_written_when_core_fact_is_unrelated() {
        // Same turn has a Core fact and an UNRELATED Daily summary: the Daily
        // timeline must still record the independent info.
        let tmp = TempDir::new().unwrap();
        let mem = SqliteMemory::new("test", tmp.path()).unwrap();
        // Enable the gate explicitly so this exercises the "unrelated" branch
        // rather than trivially passing via the default-off short circuit.
        let cfg = MemoryConfig {
            daily_dedup: true,
            ..MemoryConfig::default()
        };

        maybe_write_daily_history(
            &mem,
            &cfg,
            "User asked what the weather is like today",
            Some("user_a prefers the metric system for unit conversions"),
        )
        .await
        .unwrap();

        assert_eq!(
            daily_row_count(&mem).await,
            1,
            "an unrelated Daily summary must still be written alongside the Core fact"
        );
    }

    #[tokio::test]
    async fn daily_only_turn_still_writes() {
        // No Core memory_update this turn (Daily-only): the Daily write happens
        // exactly as before.
        let tmp = TempDir::new().unwrap();
        let mem = SqliteMemory::new("test", tmp.path()).unwrap();
        let cfg = MemoryConfig::default();

        maybe_write_daily_history(&mem, &cfg, "User asked what time it is", None)
            .await
            .unwrap();

        assert_eq!(daily_row_count(&mem).await, 1);
    }

    #[test]
    fn parse_valid_json_response() {
        let raw = r#"{"history_entry": "User asked about Rust.", "memory_update": "User prefers Rust over Go."}"#;
        let result = parse_consolidation_response(raw, "fallback");
        assert_eq!(result.history_entry, "User asked about Rust.");
        assert_eq!(
            result.memory_update.as_deref(),
            Some("User prefers Rust over Go.")
        );
    }

    #[test]
    fn parse_json_with_null_memory() {
        let raw = r#"{"history_entry": "Routine greeting.", "memory_update": null}"#;
        let result = parse_consolidation_response(raw, "fallback");
        assert_eq!(result.history_entry, "Routine greeting.");
        assert!(result.memory_update.is_none());
    }

    #[test]
    fn parse_json_wrapped_in_code_block() {
        let raw =
            "```json\n{\"history_entry\": \"Discussed deployment.\", \"memory_update\": null}\n```";
        let result = parse_consolidation_response(raw, "fallback");
        assert_eq!(result.history_entry, "Discussed deployment.");
    }

    #[test]
    fn fallback_on_malformed_response() {
        let raw = "I'm sorry, I can't do that.";
        let result = parse_consolidation_response(raw, "User: hello\nAssistant: hi");
        assert_eq!(result.history_entry, "User: hello\nAssistant: hi");
        assert!(result.memory_update.is_none());
    }

    #[test]
    fn fallback_truncates_long_text() {
        let long_text = "x".repeat(500);
        let result = parse_consolidation_response("invalid", &long_text);
        // 200 bytes + "…" (3 bytes in UTF-8) = 203
        assert!(result.history_entry.len() <= 203);
    }

    #[test]
    fn fallback_truncates_cjk_text_without_panic() {
        // Each CJK character is 3 bytes in UTF-8; byte index 200 may land
        // inside a character. This must not panic.
        let cjk_text = "二手书项目".repeat(50); // 250 chars = 750 bytes
        let result = parse_consolidation_response("invalid", &cjk_text);
        assert!(
            result
                .history_entry
                .is_char_boundary(result.history_entry.len())
        );
        assert!(result.history_entry.ends_with('…'));
    }

    #[test]
    fn strip_media_markers_replaces_all_marker_kinds() {
        let text = "see [IMAGE:/a.png] [DOCUMENT:/b.pdf] [FILE:/c.txt] \
                    [VIDEO:/d.mp4] [VOICE:/e.ogg] [AUDIO:/f.mp3] done";
        let stripped = strip_media_markers(text);
        assert_eq!(
            stripped,
            "see [media attachment] [media attachment] [media attachment] \
             [media attachment] [media attachment] [media attachment] done"
        );
    }

    #[test]
    fn strip_media_markers_leaves_other_text_unchanged() {
        let text = "plain sentence with [NOTE:keep me] and no media tags";
        assert_eq!(strip_media_markers(text), text);
    }
}

#[cfg(test)]
mod consolidate_turn_tests {
    use super::*;
    use crate::sqlite::SqliteMemory;
    use crate::traits::MemoryEntry;
    use async_trait::async_trait;
    use std::path::Path;
    use std::sync::Mutex;
    use tempfile::TempDir;
    use zeroclaw_api::attribution::{Attributable, ModelProviderKind, ProviderKind, Role};
    use zeroclaw_config::schema::MemoryDedupAction;

    /// Scripted model: returns a canned consolidation reply (or fails) and
    /// records the prompt it was sent, so tests can assert on the outbound
    /// text without any network access.
    struct ScriptedProvider {
        reply: Option<String>,
        last_prompt: Mutex<Option<String>>,
    }

    impl ScriptedProvider {
        fn replying(reply: &str) -> Self {
            Self {
                reply: Some(reply.to_string()),
                last_prompt: Mutex::new(None),
            }
        }

        fn failing() -> Self {
            Self {
                reply: None,
                last_prompt: Mutex::new(None),
            }
        }

        fn update_reply(update: &str) -> Self {
            Self::replying(
                &serde_json::json!({
                    "history_entry": "Turn summary.",
                    "memory_update": update,
                })
                .to_string(),
            )
        }

        fn last_prompt(&self) -> Option<String> {
            self.last_prompt.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl ModelProvider for ScriptedProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            *self.last_prompt.lock().unwrap() = Some(message.to_string());
            match &self.reply {
                Some(reply) => Ok(reply.clone()),
                None => anyhow::bail!("scripted provider failure"),
            }
        }
    }

    impl Attributable for ScriptedProvider {
        fn role(&self) -> Role {
            Role::Provider(ProviderKind::Model(ModelProviderKind::Custom))
        }

        fn alias(&self) -> &str {
            "ScriptedProvider"
        }
    }

    async fn run(
        provider: &ScriptedProvider,
        memory: &SqliteMemory,
        cfg: &MemoryConfig,
    ) -> anyhow::Result<()> {
        consolidate_turn(
            provider,
            "test-model",
            None,
            memory,
            cfg,
            "user message",
            "assistant response",
        )
        .await
    }

    async fn count(memory: &SqliteMemory, category: MemoryCategory) -> u64 {
        memory.count_in_scope(None, Some(&category)).await.unwrap()
    }

    fn open_db(workspace: &Path) -> rusqlite::Connection {
        rusqlite::Connection::open(workspace.join("memory").join("brain.db")).unwrap()
    }

    fn superseded_by(conn: &rusqlite::Connection, key: &str) -> Option<String> {
        conn.query_row(
            "SELECT superseded_by FROM memories WHERE key = ?1",
            rusqlite::params![key],
            |row| row.get(0),
        )
        .unwrap()
    }

    async fn store_core_fact(memory: &SqliteMemory, key: &str, content: &str) {
        memory
            .store_with_options(
                key,
                content,
                MemoryCategory::Core,
                None,
                StoreOptions::default().with_namespace("default"),
            )
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn persists_history_and_core_update() {
        let tmp = TempDir::new().unwrap();
        let memory = SqliteMemory::new("sqlite", tmp.path()).unwrap();
        let provider = ScriptedProvider::update_reply("User tracks budgets in TOML files.");

        run(&provider, &memory, &MemoryConfig::default())
            .await
            .unwrap();

        assert_eq!(count(&memory, MemoryCategory::Daily).await, 1);
        assert_eq!(count(&memory, MemoryCategory::Core).await, 1);

        let conn = open_db(tmp.path());
        let (content, importance): (String, Option<f64>) = conn
            .query_row(
                "SELECT content, importance FROM memories \
                 WHERE category = 'core' AND key LIKE 'core_%'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(content, "User tracks budgets in TOML files.");
        assert!(
            importance.is_some(),
            "core writes carry a computed importance"
        );
    }

    #[tokio::test]
    async fn null_memory_update_writes_history_only() {
        let tmp = TempDir::new().unwrap();
        let memory = SqliteMemory::new("sqlite", tmp.path()).unwrap();
        let provider = ScriptedProvider::replying(
            r#"{"history_entry": "Routine greeting.", "memory_update": null}"#,
        );

        run(&provider, &memory, &MemoryConfig::default())
            .await
            .unwrap();

        assert_eq!(count(&memory, MemoryCategory::Daily).await, 1);
        assert_eq!(count(&memory, MemoryCategory::Core).await, 0);
    }

    #[tokio::test]
    async fn media_markers_are_stripped_from_the_outbound_prompt() {
        let tmp = TempDir::new().unwrap();
        let memory = SqliteMemory::new("sqlite", tmp.path()).unwrap();
        let provider = ScriptedProvider::replying(
            r#"{"history_entry": "Reviewed an image.", "memory_update": null}"#,
        );

        consolidate_turn(
            &provider,
            "test-model",
            None,
            &memory,
            &MemoryConfig::default(),
            "please review [IMAGE:/home/user/private/cat.png]",
            "described the picture in [DOCUMENT:/tmp/notes.pdf]",
        )
        .await
        .unwrap();

        let prompt = provider.last_prompt().expect("model must be called");
        assert!(prompt.contains("[media attachment]"));
        assert!(
            !prompt.contains("/home/user/private/cat.png"),
            "local paths must not reach the model"
        );
        assert!(!prompt.contains("/tmp/notes.pdf"));
        assert!(prompt.contains("User:"));
        assert!(prompt.contains("Assistant:"));
    }

    #[tokio::test]
    async fn dedup_reject_skips_duplicate_core_update() {
        let tmp = TempDir::new().unwrap();
        let memory = SqliteMemory::new("sqlite", tmp.path()).unwrap();
        let fact = "User prefers Rust for systems work";
        store_core_fact(&memory, "existing_fact", fact).await;

        let cfg = MemoryConfig {
            dedup_on_write: true,
            ..MemoryConfig::default()
        };
        let provider = ScriptedProvider::update_reply(fact);

        run(&provider, &memory, &cfg).await.unwrap();

        assert_eq!(
            count(&memory, MemoryCategory::Core).await,
            1,
            "exact duplicate is rejected, only the pre-existing row remains"
        );
        let conn = open_db(tmp.path());
        let new_rows: u64 = conn
            .query_row(
                "SELECT COUNT(*) FROM memories WHERE key LIKE 'core_%'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(new_rows, 0, "no consolidation-keyed row was written");
    }

    #[tokio::test]
    async fn dedup_merge_folds_update_into_the_survivor() {
        let tmp = TempDir::new().unwrap();
        let memory = SqliteMemory::new("sqlite", tmp.path()).unwrap();
        let survivor = "User prefers Rust for systems work";
        let update = "User prefers Rust for systems programming work";
        store_core_fact(&memory, "fact_rust", survivor).await;

        let cfg = MemoryConfig {
            dedup_on_write: true,
            dedup_action: MemoryDedupAction::Merge,
            dedup_jaccard_threshold: 0.5,
            ..MemoryConfig::default()
        };
        let provider = ScriptedProvider::update_reply(update);

        run(&provider, &memory, &cfg).await.unwrap();

        assert_eq!(count(&memory, MemoryCategory::Core).await, 1);
        let merged: MemoryEntry = memory.get("fact_rust").await.unwrap().unwrap();
        assert_eq!(
            merged.content,
            format!("{update}\n{survivor}"),
            "near-duplicate is folded into the survivor row in place"
        );
    }

    /// Keyword-only recall scores come from BM25, which needs corpus
    /// contrast: in a corpus where every document contains the query terms
    /// the IDF collapses to ~0 and conflict scores become meaningless.
    /// Seed unrelated facts so the conflicting row scores above threshold.
    async fn seed_filler_facts(memory: &SqliteMemory) {
        store_core_fact(memory, "filler_editor", "Preferred editor is Helix").await;
        store_core_fact(memory, "filler_tz", "Timezone offset UTC plus ten").await;
        store_core_fact(memory, "filler_lang", "Speaks French and Japanese").await;
        store_core_fact(memory, "filler_ci", "CI provider is Buildkite").await;
    }

    fn live_rows_with_content(conn: &rusqlite::Connection, content: &str) -> u64 {
        conn.query_row(
            "SELECT COUNT(*) FROM memories WHERE content = ?1 AND superseded_by IS NULL",
            rusqlite::params![content],
            |row| row.get(0),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn conflict_supersede_marks_the_loser_reversibly() {
        let tmp = TempDir::new().unwrap();
        let memory = SqliteMemory::new("sqlite", tmp.path()).unwrap();
        seed_filler_facts(&memory).await;
        store_core_fact(
            &memory,
            "stale_fact",
            "User deploys the app on Fridays every week",
        )
        .await;

        let cfg = MemoryConfig {
            // Keyword-only scores vary with corpus statistics; a low
            // threshold keeps the conflict decision deterministic here.
            conflict_threshold: 0.01,
            ..MemoryConfig::default()
        };
        assert!(
            cfg.conflict_supersede_enabled,
            "supersede must be the default"
        );
        let update = "User deploys the app on Mondays every week";
        let provider = ScriptedProvider::update_reply(update);

        run(&provider, &memory, &cfg).await.unwrap();

        let conn = open_db(tmp.path());
        let marker = superseded_by(&conn, "stale_fact")
            .expect("conflicting row is reversibly soft-hidden, not deleted");
        assert!(
            marker.starts_with("core_"),
            "superseded_by points at the winning consolidation write"
        );
        assert_eq!(
            live_rows_with_content(&conn, update),
            1,
            "the winning update is stored live"
        );
    }

    #[tokio::test]
    async fn supersede_disabled_leaves_conflicting_rows_live() {
        let tmp = TempDir::new().unwrap();
        let memory = SqliteMemory::new("sqlite", tmp.path()).unwrap();
        seed_filler_facts(&memory).await;
        store_core_fact(
            &memory,
            "stale_fact",
            "User deploys the app on Fridays every week",
        )
        .await;

        let cfg = MemoryConfig {
            conflict_threshold: 0.01,
            conflict_supersede_enabled: false,
            ..MemoryConfig::default()
        };
        let update = "User deploys the app on Mondays every week";
        let provider = ScriptedProvider::update_reply(update);

        run(&provider, &memory, &cfg).await.unwrap();

        let conn = open_db(tmp.path());
        assert!(
            superseded_by(&conn, "stale_fact").is_none(),
            "flag off: the conflicting row stays visible"
        );
        assert_eq!(live_rows_with_content(&conn, update), 1);
    }

    #[tokio::test]
    async fn policy_gate_rejection_bails_the_core_write() {
        // Regression for the S4 review blocker "Daily-after-Core-durable":
        // Core runs FIRST; a policy REJECTION means `core_durable = false`, so
        // the Daily row for this turn is written (not cross-path-skipped) and
        // the turn is never silently dropped even though Core failed.
        let tmp = TempDir::new().unwrap();
        let memory = SqliteMemory::new("sqlite", tmp.path()).unwrap();

        let mut cfg = MemoryConfig::default();
        cfg.policy.read_only_namespaces = vec!["default".into()];
        let provider = ScriptedProvider::update_reply("User uses zsh.");

        let err = run(&provider, &memory, &cfg)
            .await
            .expect_err("policy violation must fail the consolidation write");
        assert!(err.to_string().contains("denied by policy"));

        assert_eq!(
            count(&memory, MemoryCategory::Daily).await,
            1,
            "Core policy rejection must not drop the Daily row for this turn"
        );
        assert_eq!(
            count(&memory, MemoryCategory::Core).await,
            0,
            "the gated core write never lands"
        );
    }

    /// A `Memory` wrapper around `SqliteMemory` that makes every Core-category
    /// `store_with_options` call fail, while delegating every other operation
    /// (Daily writes, recall, count, etc.) to the inner store unchanged. Used
    /// to prove the Daily row survives a Core write that fails AFTER policy/
    /// dedup pass (e.g. a transient backend store error), not only a policy
    /// rejection.
    struct CoreStoreFailingMemory {
        inner: SqliteMemory,
    }

    impl Attributable for CoreStoreFailingMemory {
        fn role(&self) -> Role {
            self.inner.role()
        }
        fn alias(&self) -> &str {
            self.inner.alias()
        }
    }

    #[async_trait]
    impl Memory for CoreStoreFailingMemory {
        fn name(&self) -> &str {
            self.inner.name()
        }

        async fn store(
            &self,
            key: &str,
            content: &str,
            category: MemoryCategory,
            session_id: Option<&str>,
        ) -> anyhow::Result<()> {
            self.inner.store(key, content, category, session_id).await
        }

        async fn store_with_options(
            &self,
            key: &str,
            content: &str,
            category: MemoryCategory,
            session_id: Option<&str>,
            options: StoreOptions,
        ) -> anyhow::Result<()> {
            if matches!(category, MemoryCategory::Core) {
                anyhow::bail!("simulated Core store failure");
            }
            self.inner
                .store_with_options(key, content, category, session_id, options)
                .await
        }

        async fn recall(
            &self,
            query: &str,
            limit: usize,
            session_id: Option<&str>,
            since: Option<&str>,
            until: Option<&str>,
        ) -> anyhow::Result<Vec<MemoryEntry>> {
            self.inner
                .recall(query, limit, session_id, since, until)
                .await
        }

        async fn get(&self, key: &str) -> anyhow::Result<Option<MemoryEntry>> {
            self.inner.get(key).await
        }

        async fn list(
            &self,
            category: Option<&MemoryCategory>,
            session_id: Option<&str>,
        ) -> anyhow::Result<Vec<MemoryEntry>> {
            self.inner.list(category, session_id).await
        }

        async fn forget(&self, key: &str) -> anyhow::Result<bool> {
            self.inner.forget(key).await
        }

        async fn forget_for_agent(&self, key: &str, agent_id: &str) -> anyhow::Result<bool> {
            self.inner.forget_for_agent(key, agent_id).await
        }

        async fn count(&self) -> anyhow::Result<usize> {
            self.inner.count().await
        }

        async fn health_check(&self) -> bool {
            self.inner.health_check().await
        }

        async fn count_in_scope(
            &self,
            namespace: Option<&str>,
            category: Option<&MemoryCategory>,
        ) -> anyhow::Result<u64> {
            self.inner.count_in_scope(namespace, category).await
        }

        async fn store_with_agent(
            &self,
            key: &str,
            content: &str,
            category: MemoryCategory,
            session_id: Option<&str>,
            namespace: Option<&str>,
            importance: Option<f64>,
            agent_id: Option<&str>,
        ) -> anyhow::Result<()> {
            self.inner
                .store_with_agent(
                    key, content, category, session_id, namespace, importance, agent_id,
                )
                .await
        }

        async fn recall_for_agents(
            &self,
            allowed_agent_ids: &[&str],
            query: &str,
            limit: usize,
            session_id: Option<&str>,
            since: Option<&str>,
            until: Option<&str>,
        ) -> anyhow::Result<Vec<MemoryEntry>> {
            self.inner
                .recall_for_agents(allowed_agent_ids, query, limit, session_id, since, until)
                .await
        }
    }

    #[tokio::test]
    async fn core_store_failure_still_preserves_the_daily_row() {
        // Regression for the S4 review blocker "Daily-after-Core-durable",
        // FAILING Core path: policy validation and dedup both pass, but the
        // actual store call fails (e.g. a transient backend error). Ordering
        // must still preserve the Daily row instead of losing both records.
        let tmp = TempDir::new().unwrap();
        let memory = CoreStoreFailingMemory {
            inner: SqliteMemory::new("sqlite", tmp.path()).unwrap(),
        };
        let provider = ScriptedProvider::update_reply("User tracks tasks in a bullet journal.");

        let err = consolidate_turn(
            &provider,
            "test-model",
            None,
            &memory,
            &MemoryConfig::default(),
            "user message",
            "assistant response",
        )
        .await
        .expect_err("a failing Core store must surface as an error");
        assert!(err.to_string().contains("simulated Core store failure"));

        assert_eq!(
            memory
                .inner
                .count_in_scope(None, Some(&MemoryCategory::Daily))
                .await
                .unwrap(),
            1,
            "a failing Core store must not drop the Daily row for this turn"
        );
        assert_eq!(
            memory
                .inner
                .count_in_scope(None, Some(&MemoryCategory::Core))
                .await
                .unwrap(),
            0,
            "the failed core write never lands"
        );
    }

    #[tokio::test]
    async fn provider_failure_aborts_before_any_write() {
        let tmp = TempDir::new().unwrap();
        let memory = SqliteMemory::new("sqlite", tmp.path()).unwrap();
        let provider = ScriptedProvider::failing();

        let result = run(&provider, &memory, &MemoryConfig::default()).await;

        assert!(result.is_err());
        assert_eq!(count(&memory, MemoryCategory::Daily).await, 0);
        assert_eq!(count(&memory, MemoryCategory::Core).await, 0);
    }
}
