//! LLM-driven memory consolidation.
//!
//! After each conversation turn, extracts structured information:
//! - `history_entry`: A timestamped summary for the daily conversation log.
//! - `memory_update`: New facts, preferences, or decisions worth remembering
//!   long-term (or `null` if nothing new was learned).
//!
//! This two-phase approach replaces the naive raw-message auto-save with
//! semantic extraction, similar to Nanobot's `save_memory` tool call pattern.

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

/// Run two-phase LLM-driven consolidation on a conversation turn.
///
/// Phase 1: Write a history entry to the Daily memory category.
/// Phase 2: Write a memory update to the Core category (if the LLM identified new facts).
///
/// This function is designed to be called fire-and-forget via `zeroclaw_spawn::spawn!`.
/// Strip channel media markers (e.g. `[IMAGE:/local/path]`, `[DOCUMENT:...]`)
/// that contain local filesystem paths.  These must never be forwarded to
/// upstream model_provider APIs — they would leak local paths and cause API errors.
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

    // Phase 1: Write history entry to Daily category.
    //
    // Gated two ways, independently of the Phase-2 Core dedup (which is
    // governed by `dedup_on_write`):
    //   - `consolidate_daily = false` disables the per-turn Daily write
    //     entirely (Core fact extraction below still runs).
    //   - `daily_dedup = true` (default) skips the write when an exact or
    //     near-identical Daily summary already exists, so ordinary question
    //     turns do not accumulate near-duplicate transient rows on an
    //     append-only backend.
    //
    // In-turn cross-path skip: when THIS turn also produces a Core
    // `memory_update` and the Daily `history_entry` is a near-identical
    // paraphrase of it (same fact, different wording), skip the redundant
    // Daily write and keep only the durable Core row. There was no
    // cross-category dedup before, so such a fact was stored as both a #core
    // and a #daily row and surfaced twice at recall. Gated by the existing
    // `daily_dedup` switch and `dedup_jaccard_threshold`; a Daily-only turn
    // (no Core update) still writes as before.
    if memory_config.consolidate_daily {
        maybe_write_daily_history(
            memory,
            memory_config,
            &result.history_entry,
            result.memory_update.as_deref(),
        )
        .await?;
    }

    // Phase 2: Write memory update to Core category (if present).
    if let Some(ref update) = result.memory_update
        && !update.trim().is_empty()
    {
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
                return Ok(());
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
                    return Ok(());
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

        // Store with importance metadata.
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
    }

    Ok(())
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
        // Recall recent entries similar to this summary; keep only Daily
        // candidates. Recall failures must not drop the write - fall back to
        // inserting (fail-open) so a transient recall error never silently
        // loses history.
        let candidates = match memory.recall(history_entry, 10, None, None, None).await {
            Ok(entries) => dedup::daily_candidates(entries),
            Err(e) => {
                ::zeroclaw_log::record!(
                    DEBUG,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_attrs(::serde_json::json!({"error": format!("{e}")})),
                    "daily dedup recall failed; inserting without dedup"
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
        // With the default gate (daily_dedup on), writing the same summary
        // several times must leave exactly one Daily row.
        let tmp = TempDir::new().unwrap();
        let mem = SqliteMemory::new("test", tmp.path()).unwrap();
        let cfg = MemoryConfig::default();
        assert!(cfg.daily_dedup);

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
        let cfg = MemoryConfig::default();
        assert!(cfg.daily_dedup);

        let core = "Svetlana is the user's mother and grandmother to Ellie and Vitalik";
        let daily = "Svetlana is the user's mother and also grandmother to Ellie and Vitalik";
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
        let cfg = MemoryConfig::default();

        maybe_write_daily_history(
            &mem,
            &cfg,
            "User asked what the weather is like today",
            Some("The user's birthday is February 24, 1990"),
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
}
