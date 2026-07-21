//! LLM-driven memory consolidation.

use crate::classify;
use crate::conflict;
use crate::dedup::{self, DedupAction};
use crate::importance;
use crate::merge;
use crate::policy::PolicyEnforcer;
use crate::policy_gate;
use crate::traits::{
    Memory, MemoryCategory, MemoryEntry, MemoryKind, SemanticSubtype, StoreOptions,
};
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
    /// Semantic subtype hint for the primary Core memory update (used only when
    /// `memory.types.enabled`; parsed via `classify::kind_of_core`).
    #[serde(default)]
    pub kind: Option<String>,
}

const CONSOLIDATION_SYSTEM_PROMPT: &str = r#"You are a memory consolidation engine. Given a conversation turn, extract:
1. "history_entry": A brief summary of what happened in this turn (1-2 sentences). Include the key topic or action.
2. "memory_update": Any NEW facts, preferences, decisions, or commitments worth remembering long-term. Return null if nothing new was learned.

Respond ONLY with valid JSON: {"history_entry": "...", "memory_update": "..." or null}
Do not include any text outside the JSON object."#;

/// Extended prompt used only when `consolidation_extract_facts` is enabled: also
/// asks for a semantic `kind`, atomic `facts`, and a cross-turn `trend`.
const CONSOLIDATION_TYPED_SYSTEM_PROMPT: &str = r#"You are a memory consolidation engine. Given a conversation turn, extract:
1. "history_entry": A brief summary of what happened in this turn (1-2 sentences). Include the key topic or action.
2. "memory_update": Any NEW facts, preferences, decisions, or commitments worth remembering long-term. Return null if nothing new was learned.
3. "kind": The memory_update subtype: "preference", "fact", "decision", or "entity". Use "fact" if unsure. Return null when memory_update is null.
4. "facts": Atomic durable facts from this turn. Return an empty array when there are none. Do not include procedures or how-to workflows.
5. "trend": A stable cross-turn trend or pattern, or null when none is visible.

Reusable procedures, how-to workflows, and repeated operating steps belong in skills, not persistent memory records. Do not store them as memory_update or facts unless they are also stable semantic facts.

Respond ONLY with valid JSON: {"history_entry": "...", "memory_update": "..." or null, "kind": "preference" | "fact" | "decision" | "entity" | null, "facts": ["..."], "trend": "..." or null}
Do not include any text outside the JSON object."#;

const MAX_TYPED_FACTS_PER_TURN: usize = 5;

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

    // Typed memory must ask for a subtype even when atomic-fact extraction stays
    // disabled. Reusing the extended schema keeps the model contract explicit.
    let system_prompt = if memory_config.types.enabled || memory_config.consolidation_extract_facts
    {
        CONSOLIDATION_TYPED_SYSTEM_PROMPT
    } else {
        CONSOLIDATION_SYSTEM_PROMPT
    };

    let raw = ProviderDispatch::from_ref(model_provider)
        .chat_with_system(Some(system_prompt), &truncated, model, temperature)
        .await?;

    let result: ConsolidationResult = parse_consolidation_response(&raw, &turn_text);

    // Phase 1: Write history entry to Daily category. The gated arm tags the
    // write Episodic; the default arm keeps the exact legacy store call so the
    // flags-off write path is unchanged.
    let date = chrono::Local::now().format("%Y-%m-%d").to_string();
    let history_key = format!("daily_{date}_{}", uuid::Uuid::new_v4());
    if memory_config.types.enabled {
        let history_options = StoreOptions {
            kind: Some(MemoryKind::Episodic),
            ..StoreOptions::default()
        };
        memory
            .store_with_options(
                &history_key,
                &result.history_entry,
                MemoryCategory::Daily,
                None,
                history_options,
            )
            .await?;
    } else {
        memory
            .store(
                &history_key,
                &result.history_entry,
                MemoryCategory::Daily,
                None,
            )
            .await?;
    }

    // Phase 2: extract atomic typed facts (gated; default off). Runs before
    // the primary Core update because that path exits early when the update
    // dedups or merges into a survivor, and a duplicate primary update must
    // not silently suppress this turn's fact writes. An exact overlap belongs
    // to the primary update so its more specific subtype wins.
    if memory_config.consolidation_extract_facts {
        store_typed_facts(
            memory,
            memory_config,
            &result,
            result.memory_update.as_deref(),
        )
        .await?;
    }

    // Phase 3: Write memory update to Core category (if present).
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
                        kind: survivor.kind.clone().or_else(|| {
                            memory_config
                                .types
                                .enabled
                                .then(|| classify::kind_of_core(&result))
                        }),
                        pinned: survivor.pinned,
                        tenant_id: survivor.tenant_id.clone(),
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

        // Store with importance metadata (+ kind when types are enabled).
        let options = StoreOptions {
            importance: Some(imp),
            kind: memory_config
                .types
                .enabled
                .then(|| classify::kind_of_core(&result)),
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

/// Store atomic facts extracted from a turn as individual Core memories,
/// reusing the same policy/dedup/merge/supersede path as the core update.
/// Only called when `consolidation_extract_facts` is enabled; kind tagging
/// additionally requires `memory.types.enabled`.
async fn store_typed_facts(
    memory: &dyn Memory,
    memory_config: &MemoryConfig,
    result: &ConsolidationResult,
    primary_update: Option<&str>,
) -> anyhow::Result<()> {
    let fact_kind = memory_config
        .types
        .enabled
        .then_some(MemoryKind::Semantic(SemanticSubtype::Fact));
    let policy = PolicyEnforcer::new(&memory_config.policy);

    for fact in result
        .facts
        .iter()
        .filter_map(|fact| {
            let trimmed = fact.trim();
            let overlaps_primary = primary_update.is_some_and(|primary| {
                fact_overlaps_primary_update(trimmed, primary, memory_config)
            });
            (!trimmed.is_empty() && !overlaps_primary).then_some(trimmed)
        })
        .take(MAX_TYPED_FACTS_PER_TURN)
    {
        // Same fail-closed policy write-gate as the primary core update; each
        // fact is an autonomous Core write.
        if let Err(e) =
            policy_gate::validate_store(memory, &policy, "default", &MemoryCategory::Core).await
        {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"error": e.to_string()})),
                "memory fact write denied by policy"
            );
            anyhow::bail!("memory fact write denied by policy: {e}");
        }

        let mem_key = format!("core_fact_{}", uuid::Uuid::new_v4());
        let imp = importance::compute_importance(fact, &MemoryCategory::Core);

        let candidates = memory.recall(fact, 10, None, None, None).await?;
        let candidates = dedup::core_candidates(candidates);
        match dedup::dedup_gate(&candidates, fact, memory_config) {
            DedupAction::Insert => {}
            DedupAction::Reject { dup_of } => {
                ::zeroclaw_log::record!(
                    DEBUG,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_attrs(::serde_json::json!({"duplicate_of": dup_of})),
                    "memory fact skipped as duplicate"
                );
                continue;
            }
            DedupAction::Merge { into } => {
                if let Some(survivor) = candidates.iter().find(|entry| entry.id == into) {
                    let merged = merge::merge_into_survivor(survivor, fact);
                    memory
                        .store_with_options(
                            &survivor.key,
                            &merged.content,
                            MemoryCategory::Core,
                            survivor.session_id.as_deref(),
                            StoreOptions {
                                namespace: Some(survivor.namespace.clone()),
                                importance: merged.importance,
                                kind: survivor.kind.clone().or_else(|| fact_kind.clone()),
                                pinned: survivor.pinned,
                                tenant_id: survivor.tenant_id.clone(),
                            },
                        )
                        .await?;
                    continue;
                }
            }
        }

        let superseded_ids = match conflict::check_and_resolve_conflicts(
            memory,
            &mem_key,
            fact,
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

        memory
            .store_with_options(
                &mem_key,
                fact,
                MemoryCategory::Core,
                None,
                StoreOptions {
                    importance: Some(imp),
                    kind: fact_kind.clone(),
                    ..StoreOptions::default()
                },
            )
            .await?;

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

fn fact_overlaps_primary_update(
    fact: &str,
    primary_update: &str,
    memory_config: &MemoryConfig,
) -> bool {
    let primary = primary_update.trim();
    if primary.is_empty() {
        return false;
    }
    if primary == fact {
        return true;
    }
    if !memory_config.dedup_on_write {
        return false;
    }

    let candidate = MemoryEntry {
        id: "primary_update_candidate".into(),
        key: "primary_update_candidate".into(),
        content: primary.to_string(),
        category: MemoryCategory::Core,
        timestamp: "now".into(),
        session_id: None,
        score: None,
        namespace: "default".into(),
        importance: None,
        superseded_by: None,
        kind: None,
        pinned: false,
        tenant_id: None,
        agent_alias: None,
        agent_id: None,
    };

    !matches!(
        dedup::dedup_gate(&[candidate], fact, memory_config),
        DedupAction::Insert
    )
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
            kind: None,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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

    use crate::agent_scoped::AgentScopedMemory;
    use crate::audit::AuditedMemory;
    use crate::sqlite::SqliteMemory;
    use crate::traits::MemoryEntry;
    use async_trait::async_trait;
    use parking_lot::Mutex;
    use std::sync::Arc;
    use zeroclaw_api::model_provider::ChatMessage;
    use zeroclaw_config::schema::{MemoryDedupAction, MemoryPolicyConfig, MemoryTypesConfig};

    #[derive(Debug, Clone)]
    enum RecordedWrite {
        Store {
            key: String,
            content: String,
            category: MemoryCategory,
        },
        StoreWithOptions {
            key: String,
            content: String,
            category: MemoryCategory,
            options: StoreOptions,
        },
    }

    impl RecordedWrite {
        fn key(&self) -> &str {
            match self {
                Self::Store { key, .. } | Self::StoreWithOptions { key, .. } => key,
            }
        }
    }

    #[derive(Default)]
    struct RecordingMemory {
        writes: Mutex<Vec<RecordedWrite>>,
        /// Entries returned from every `recall` call (empty by default).
        recall_entries: Mutex<Vec<MemoryEntry>>,
    }

    #[async_trait]
    impl Memory for RecordingMemory {
        fn name(&self) -> &str {
            "recording"
        }

        async fn store(
            &self,
            key: &str,
            content: &str,
            category: MemoryCategory,
            _session_id: Option<&str>,
        ) -> anyhow::Result<()> {
            self.writes.lock().push(RecordedWrite::Store {
                key: key.to_string(),
                content: content.to_string(),
                category,
            });
            Ok(())
        }

        async fn store_with_options(
            &self,
            key: &str,
            content: &str,
            category: MemoryCategory,
            _session_id: Option<&str>,
            options: StoreOptions,
        ) -> anyhow::Result<()> {
            self.writes.lock().push(RecordedWrite::StoreWithOptions {
                key: key.to_string(),
                content: content.to_string(),
                category,
                options,
            });
            Ok(())
        }

        async fn store_with_agent(
            &self,
            key: &str,
            content: &str,
            category: MemoryCategory,
            _session_id: Option<&str>,
            _namespace: Option<&str>,
            _importance: Option<f64>,
            _agent_id: Option<&str>,
        ) -> anyhow::Result<()> {
            self.writes.lock().push(RecordedWrite::Store {
                key: key.to_string(),
                content: content.to_string(),
                category,
            });
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
            Ok(self.recall_entries.lock().clone())
        }

        async fn recall_for_agents(
            &self,
            _allowed_agent_ids: &[&str],
            _query: &str,
            _limit: usize,
            _session_id: Option<&str>,
            _since: Option<&str>,
            _until: Option<&str>,
        ) -> anyhow::Result<Vec<MemoryEntry>> {
            Ok(Vec::new())
        }

        async fn get(&self, _key: &str) -> anyhow::Result<Option<MemoryEntry>> {
            Ok(None)
        }

        async fn list(
            &self,
            _category: Option<&MemoryCategory>,
            _session_id: Option<&str>,
        ) -> anyhow::Result<Vec<MemoryEntry>> {
            Ok(Vec::new())
        }

        async fn forget(&self, _key: &str) -> anyhow::Result<bool> {
            Ok(false)
        }

        async fn forget_for_agent(&self, _key: &str, _agent_id: &str) -> anyhow::Result<bool> {
            Ok(false)
        }

        async fn count(&self) -> anyhow::Result<usize> {
            Ok(0)
        }

        async fn health_check(&self) -> bool {
            true
        }
    }

    impl ::zeroclaw_api::attribution::Attributable for RecordingMemory {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Memory(
                ::zeroclaw_api::attribution::MemoryKind::InMemory,
            )
        }
        fn alias(&self) -> &str {
            "RecordingMemory"
        }
    }

    struct ScriptedProvider {
        response: &'static str,
        system_prompts: Mutex<Vec<String>>,
    }

    impl ScriptedProvider {
        fn new(response: &'static str) -> Self {
            Self {
                response,
                system_prompts: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl ModelProvider for ScriptedProvider {
        async fn chat_with_system(
            &self,
            system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            self.system_prompts
                .lock()
                .push(system_prompt.unwrap_or_default().to_string());
            Ok(self.response.to_string())
        }

        async fn chat_with_history(
            &self,
            _messages: &[ChatMessage],
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            Ok(self.response.to_string())
        }
    }

    impl ::zeroclaw_api::attribution::Attributable for ScriptedProvider {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Provider(
                ::zeroclaw_api::attribution::ProviderKind::Model(
                    ::zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }
        fn alias(&self) -> &str {
            "ScriptedProvider"
        }
    }

    /// A response that also carries the typed fields, as a compliant model
    /// would produce them under the typed prompt (and as a non-compliant model
    /// could produce them under the default prompt).
    const TYPED_RESPONSE: &str = r#"{"history_entry": "Discussed rollout strategy.", "memory_update": "Use staged rollout for deploys.", "kind": "decision", "facts": ["Deploys use a staged rollout", "Rollbacks swap the previous binary back", "   "], "trend": null}"#;

    async fn run_consolidation(
        provider: &ScriptedProvider,
        memory: &dyn Memory,
        config: &MemoryConfig,
    ) -> anyhow::Result<()> {
        consolidate_turn(
            provider,
            "test-model",
            None,
            memory,
            config,
            "How do we deploy?",
            "We use a staged rollout.",
        )
        .await
    }

    #[tokio::test]
    async fn flags_off_consolidation_keeps_master_write_shape() {
        let provider = ScriptedProvider::new(TYPED_RESPONSE);
        let memory = RecordingMemory::default();
        let config = MemoryConfig::default();

        run_consolidation(&provider, &memory, &config)
            .await
            .unwrap();

        // Exactly one provider call, with the unchanged default prompt.
        let prompts = provider.system_prompts.lock();
        assert_eq!(prompts.len(), 1);
        assert_eq!(prompts[0], CONSOLIDATION_SYSTEM_PROMPT);

        // Exactly two writes: the Daily history entry through the plain
        // legacy store call, and the Core update with kind unset -- even
        // though the model response carried kind/facts fields.
        let writes = memory.writes.lock();
        assert_eq!(writes.len(), 2);
        match &writes[0] {
            RecordedWrite::Store {
                key,
                content,
                category,
            } => {
                assert!(key.starts_with("daily_"));
                assert_eq!(content, "Discussed rollout strategy.");
                assert_eq!(*category, MemoryCategory::Daily);
            }
            other => panic!("expected plain store for the Daily entry, got {other:?}"),
        }
        match &writes[1] {
            RecordedWrite::StoreWithOptions {
                key,
                content,
                category,
                options,
            } => {
                assert!(key.starts_with("core_"));
                assert!(!key.starts_with("core_fact_"));
                assert_eq!(content, "Use staged rollout for deploys.");
                assert_eq!(*category, MemoryCategory::Core);
                assert_eq!(options.kind, None);
                assert!(options.importance.is_some());
                assert_eq!(options.namespace, None);
                assert!(!options.pinned);
                assert_eq!(options.tenant_id, None);
            }
            other => panic!("expected store_with_options for the Core update, got {other:?}"),
        }
        assert!(!writes.iter().any(|w| w.key().starts_with("core_fact_")));
    }

    #[tokio::test]
    async fn types_enabled_tags_daily_episodic_and_core_kind() {
        let provider = ScriptedProvider::new(TYPED_RESPONSE);
        let memory = RecordingMemory::default();
        let config = MemoryConfig {
            types: MemoryTypesConfig { enabled: true },
            ..MemoryConfig::default()
        };

        run_consolidation(&provider, &memory, &config)
            .await
            .unwrap();

        let prompts = provider.system_prompts.lock();
        assert_eq!(prompts.as_slice(), [CONSOLIDATION_TYPED_SYSTEM_PROMPT]);

        let writes = memory.writes.lock();
        assert_eq!(writes.len(), 2);
        match &writes[0] {
            RecordedWrite::StoreWithOptions { key, options, .. } => {
                assert!(key.starts_with("daily_"));
                assert_eq!(options.kind, Some(MemoryKind::Episodic));
            }
            other => panic!("expected store_with_options for the Daily entry, got {other:?}"),
        }
        match &writes[1] {
            RecordedWrite::StoreWithOptions { key, options, .. } => {
                assert!(key.starts_with("core_"));
                assert_eq!(
                    options.kind,
                    Some(MemoryKind::Semantic(SemanticSubtype::Decision))
                );
            }
            other => panic!("expected store_with_options for the Core update, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn typed_consolidation_preserves_kind_and_agent_through_scoped_sqlite() {
        let tmp = tempfile::TempDir::new().unwrap();
        let inner = Arc::new(SqliteMemory::new("test", tmp.path()).unwrap());
        let alpha = inner.ensure_agent_uuid("alpha").await.unwrap();
        let scoped = AgentScopedMemory::new(inner.clone(), &alpha, Vec::<String>::new());
        let provider = ScriptedProvider::new(TYPED_RESPONSE);
        let config = MemoryConfig {
            types: MemoryTypesConfig { enabled: true },
            ..MemoryConfig::default()
        };

        run_consolidation(&provider, &scoped, &config)
            .await
            .unwrap();

        let entries = inner.list(None, None).await.unwrap();
        let daily = entries
            .iter()
            .find(|entry| entry.key.starts_with("daily_"))
            .expect("daily consolidation row");
        let core = entries
            .iter()
            .find(|entry| entry.key.starts_with("core_"))
            .expect("core consolidation row");
        assert_eq!(daily.agent_id.as_deref(), Some(alpha.as_str()));
        assert_eq!(core.agent_id.as_deref(), Some(alpha.as_str()));
        assert_eq!(daily.kind, Some(MemoryKind::Episodic));
        assert_eq!(
            core.kind,
            Some(MemoryKind::Semantic(SemanticSubtype::Decision))
        );
    }

    #[tokio::test]
    async fn typed_consolidation_survives_audit_decorator_in_the_stack() {
        // Composed-stack regression: scope wrapper over the audit decorator
        // over real SQLite. The scoped store forwards through
        // `store_with_options_and_agent`, so the decorator must forward the
        // full typed surface (not inherit the failing trait default) while
        // still recording the writes in its audit trail.
        let tmp = tempfile::TempDir::new().unwrap();
        let sqlite = SqliteMemory::new("test", tmp.path()).unwrap();
        let audited = Arc::new(AuditedMemory::new(sqlite, tmp.path()).unwrap());
        let alpha = audited.ensure_agent_uuid("alpha").await.unwrap();
        let scoped = AgentScopedMemory::new(audited.clone(), &alpha, Vec::<String>::new());
        let provider = ScriptedProvider::new(TYPED_RESPONSE);
        let config = MemoryConfig {
            types: MemoryTypesConfig { enabled: true },
            ..MemoryConfig::default()
        };

        run_consolidation(&provider, &scoped, &config)
            .await
            .unwrap();

        let entries = audited.list(None, None).await.unwrap();
        let daily = entries
            .iter()
            .find(|entry| entry.key.starts_with("daily_"))
            .expect("daily consolidation row");
        let core = entries
            .iter()
            .find(|entry| entry.key.starts_with("core_"))
            .expect("core consolidation row");
        assert_eq!(daily.agent_id.as_deref(), Some(alpha.as_str()));
        assert_eq!(core.agent_id.as_deref(), Some(alpha.as_str()));
        assert_eq!(daily.kind, Some(MemoryKind::Episodic));
        assert_eq!(
            core.kind,
            Some(MemoryKind::Semantic(SemanticSubtype::Decision))
        );
        let audited_stores = audited.audit_count().expect("audit trail must be readable");
        assert!(
            audited_stores >= 2,
            "audit trail must record the consolidation stores, got {audited_stores}"
        );
    }

    #[tokio::test]
    async fn primary_update_wins_over_an_exact_overlapping_fact() {
        const OVERLAPPING_RESPONSE: &str = r#"{"history_entry":"h","memory_update":"Use staged rollout","kind":"decision","facts":["Use staged rollout"]}"#;
        let provider = ScriptedProvider::new(OVERLAPPING_RESPONSE);
        let memory = RecordingMemory::default();
        let config = MemoryConfig {
            consolidation_extract_facts: true,
            types: MemoryTypesConfig { enabled: true },
            ..MemoryConfig::default()
        };

        run_consolidation(&provider, &memory, &config)
            .await
            .unwrap();

        let writes = memory.writes.lock();
        assert!(
            !writes
                .iter()
                .any(|write| write.key().starts_with("core_fact_")),
            "the generic fact must not preempt the primary decision"
        );
        let options = writes
            .iter()
            .find_map(|write| match write {
                RecordedWrite::StoreWithOptions { key, options, .. }
                    if key.starts_with("core_") =>
                {
                    Some(options)
                }
                _ => None,
            })
            .expect("primary Core write");
        assert_eq!(
            options.kind,
            Some(MemoryKind::Semantic(SemanticSubtype::Decision))
        );
    }

    #[tokio::test]
    async fn primary_update_wins_over_a_semantically_overlapping_fact() {
        const OVERLAPPING_RESPONSE: &str = r#"{"history_entry":"h","memory_update":"Use staged rollout for deploys","kind":"decision","facts":["Use staged rollout deploys","Rollbacks keep the previous binary"]}"#;
        let provider = ScriptedProvider::new(OVERLAPPING_RESPONSE);
        let memory = RecordingMemory::default();
        let config = MemoryConfig {
            consolidation_extract_facts: true,
            dedup_on_write: true,
            dedup_jaccard_threshold: 0.5,
            types: MemoryTypesConfig { enabled: true },
            ..MemoryConfig::default()
        };

        run_consolidation(&provider, &memory, &config)
            .await
            .unwrap();

        let writes = memory.writes.lock();
        let fact_writes: Vec<_> = writes
            .iter()
            .filter_map(|write| match write {
                RecordedWrite::StoreWithOptions { key, content, .. }
                    if key.starts_with("core_fact_") =>
                {
                    Some(content.as_str())
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            fact_writes,
            vec!["Rollbacks keep the previous binary"],
            "the overlapping paraphrased fact must be claimed by the primary update, while unrelated facts still land"
        );
        let options = writes
            .iter()
            .find_map(|write| match write {
                RecordedWrite::StoreWithOptions { key, options, .. }
                    if key.starts_with("core_") && !key.starts_with("core_fact_") =>
                {
                    Some(options)
                }
                _ => None,
            })
            .expect("primary Core write");
        assert_eq!(
            options.kind,
            Some(MemoryKind::Semantic(SemanticSubtype::Decision))
        );
    }

    #[tokio::test]
    async fn fact_merge_preserves_survivor_kind_pinned_and_tenant() {
        const FACT_RESPONSE: &str =
            r#"{"history_entry":"h","memory_update":null,"facts":["Use staged rollout"]}"#;
        let provider = ScriptedProvider::new(FACT_RESPONSE);
        let memory = RecordingMemory::default();
        memory.recall_entries.lock().push(MemoryEntry {
            id: "survivor".into(),
            key: "core_survivor".into(),
            content: "Use staged rollout".into(),
            category: MemoryCategory::Core,
            timestamp: "now".into(),
            session_id: Some("session-1".into()),
            score: None,
            namespace: "operations".into(),
            importance: Some(0.8),
            superseded_by: None,
            kind: Some(MemoryKind::Semantic(SemanticSubtype::Preference)),
            pinned: true,
            tenant_id: Some("tenant-a".into()),
            agent_alias: None,
            agent_id: None,
        });
        let config = MemoryConfig {
            consolidation_extract_facts: true,
            dedup_on_write: true,
            dedup_action: MemoryDedupAction::Merge,
            types: MemoryTypesConfig { enabled: true },
            ..MemoryConfig::default()
        };

        run_consolidation(&provider, &memory, &config)
            .await
            .unwrap();

        let writes = memory.writes.lock();
        let options = writes
            .iter()
            .find_map(|write| match write {
                RecordedWrite::StoreWithOptions { key, options, .. } if key == "core_survivor" => {
                    Some(options)
                }
                _ => None,
            })
            .expect("merged survivor write");
        assert_eq!(options.namespace.as_deref(), Some("operations"));
        assert_eq!(
            options.kind,
            Some(MemoryKind::Semantic(SemanticSubtype::Preference))
        );
        assert!(options.pinned);
        assert_eq!(options.tenant_id.as_deref(), Some("tenant-a"));
    }

    #[tokio::test]
    async fn extract_facts_stores_atomic_typed_facts() {
        let provider = ScriptedProvider::new(TYPED_RESPONSE);
        let memory = RecordingMemory::default();
        let config = MemoryConfig {
            consolidation_extract_facts: true,
            types: MemoryTypesConfig { enabled: true },
            ..MemoryConfig::default()
        };

        run_consolidation(&provider, &memory, &config)
            .await
            .unwrap();

        // The typed prompt replaces the default one.
        let prompts = provider.system_prompts.lock();
        assert_eq!(prompts.len(), 1);
        assert_eq!(prompts[0], CONSOLIDATION_TYPED_SYSTEM_PROMPT);

        // Daily + Core + the two non-blank facts (the whitespace-only third
        // fact is filtered out).
        let writes = memory.writes.lock();
        let fact_writes: Vec<_> = writes
            .iter()
            .filter_map(|w| match w {
                RecordedWrite::StoreWithOptions {
                    key,
                    content,
                    category,
                    options,
                } if key.starts_with("core_fact_") => Some((content, category, options)),
                _ => None,
            })
            .collect();
        assert_eq!(writes.len(), 4);
        assert_eq!(fact_writes.len(), 2);
        assert_eq!(fact_writes[0].0, "Deploys use a staged rollout");
        assert_eq!(fact_writes[1].0, "Rollbacks swap the previous binary back");
        for (_, category, options) in fact_writes {
            assert_eq!(*category, MemoryCategory::Core);
            assert_eq!(
                options.kind,
                Some(MemoryKind::Semantic(SemanticSubtype::Fact))
            );
            assert!(options.importance.is_some());
        }
    }

    #[tokio::test]
    async fn extract_facts_caps_per_turn_and_respects_types_gate() {
        const MANY_FACTS: &str = r#"{"history_entry": "h", "memory_update": null, "facts": ["f1", "f2", "f3", "f4", "f5", "f6", "f7"]}"#;
        let provider = ScriptedProvider::new(MANY_FACTS);
        let memory = RecordingMemory::default();
        // extract_facts on, types off: fact writes land but stay untyped.
        let config = MemoryConfig {
            consolidation_extract_facts: true,
            ..MemoryConfig::default()
        };

        run_consolidation(&provider, &memory, &config)
            .await
            .unwrap();

        let writes = memory.writes.lock();
        let fact_writes: Vec<_> = writes
            .iter()
            .filter_map(|w| match w {
                RecordedWrite::StoreWithOptions { key, options, .. }
                    if key.starts_with("core_fact_") =>
                {
                    Some(options)
                }
                _ => None,
            })
            .collect();
        assert_eq!(fact_writes.len(), MAX_TYPED_FACTS_PER_TURN);
        for options in fact_writes {
            assert_eq!(options.kind, None);
        }
    }

    #[tokio::test]
    async fn typed_facts_respect_policy_write_gate() {
        const FACTS_ONLY: &str =
            r#"{"history_entry": "h", "memory_update": null, "facts": ["f1", "f2"]}"#;
        let provider = ScriptedProvider::new(FACTS_ONLY);
        let memory = RecordingMemory::default();
        let config = MemoryConfig {
            consolidation_extract_facts: true,
            policy: MemoryPolicyConfig {
                read_only_namespaces: vec!["default".into()],
                ..MemoryPolicyConfig::default()
            },
            ..MemoryConfig::default()
        };

        let err = run_consolidation(&provider, &memory, &config)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("denied by policy"));

        // The Daily history entry landed before the gate; no fact rows did.
        let writes = memory.writes.lock();
        assert_eq!(writes.len(), 1);
        assert!(writes[0].key().starts_with("daily_"));
    }

    #[tokio::test]
    async fn duplicate_core_update_does_not_suppress_fact_writes() {
        // Regression for phase ordering: the primary Core update path exits
        // early when the update dedup-rejects against an existing row. Fact
        // extraction runs before that path, so a duplicate primary update
        // must not silently drop this turn's fact writes.
        let provider = ScriptedProvider::new(TYPED_RESPONSE);
        let memory = RecordingMemory::default();
        memory.recall_entries.lock().push(MemoryEntry {
            id: "existing".into(),
            key: "core_existing".into(),
            // Exact duplicate of TYPED_RESPONSE's memory_update.
            content: "Use staged rollout for deploys.".into(),
            category: MemoryCategory::Core,
            timestamp: "now".into(),
            session_id: None,
            score: None,
            namespace: "default".into(),
            importance: Some(0.7),
            superseded_by: None,
            kind: None,
            pinned: false,
            tenant_id: None,
            agent_alias: None,
            agent_id: None,
        });
        let config = MemoryConfig {
            consolidation_extract_facts: true,
            dedup_on_write: true,
            ..MemoryConfig::default()
        };

        run_consolidation(&provider, &memory, &config)
            .await
            .unwrap();

        let writes = memory.writes.lock();
        // Both facts still land even though the primary update was rejected
        // as a duplicate; no plain core_ update row is written.
        let fact_count = writes
            .iter()
            .filter(|w| w.key().starts_with("core_fact_"))
            .count();
        assert_eq!(fact_count, 2);
        assert!(
            !writes
                .iter()
                .any(|w| w.key().starts_with("core_") && !w.key().starts_with("core_fact_"))
        );
    }

    #[tokio::test]
    async fn flags_off_sqlite_kind_column_stays_null() {
        let tmp = tempfile::TempDir::new().unwrap();
        let sqlite = crate::sqlite::SqliteMemory::new("test", tmp.path()).unwrap();
        let provider = ScriptedProvider::new(TYPED_RESPONSE);
        let config = MemoryConfig::default();

        run_consolidation(&provider, &sqlite, &config)
            .await
            .unwrap();

        let entries = sqlite.list(None, None).await.unwrap();
        assert!(!entries.is_empty());
        for entry in &entries {
            assert_eq!(
                entry.kind, None,
                "fresh install row {} got a kind",
                entry.key
            );
            assert!(!entry.key.starts_with("core_fact_"));
        }
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
            "the history entry precedes the gate and is kept"
        );
        assert_eq!(
            count(&memory, MemoryCategory::Core).await,
            0,
            "the gated core write never lands"
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
