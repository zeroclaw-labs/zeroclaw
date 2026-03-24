//! Memory promotion engine: short-term → long-term (Core) + ontology.
//!
//! After each conversation turn, this module:
//! 1. Classifies the interaction category (chat, document, coding, etc.)
//! 2. Builds a structured memory entry with metadata (time, place, counterpart, action)
//! 3. Stores the structured entry to Core memory (long-term)
//! 4. Creates corresponding ontology objects, links, and action logs
//!
//! This is code-enforced promotion, not LLM-instruction-dependent.

use crate::memory::{InteractionCategory, Memory, MemoryCategory};
use crate::ontology::types::ActorKind;
use crate::ontology::OntologyRepo;
use serde_json::json;
use std::sync::Arc;

/// Metadata extracted from a conversation turn for structured storage.
#[derive(Debug, Clone)]
pub(super) struct TurnMetadata {
    /// When the turn occurred (ISO 8601 local time)
    pub timestamp: String,
    /// Where the interaction happened (channel name, device, etc.)
    pub location: String,
    /// The counterpart / other party in the interaction
    pub counterpart: String,
    /// Classified interaction category
    pub category: InteractionCategory,
    /// Summary of the action/interaction
    pub action_summary: String,
}

/// Build structured metadata from a conversation turn.
pub(super) fn extract_turn_metadata(
    user_msg: &str,
    assistant_resp: &str,
    channel: &str,
    sender: &str,
    tool_hints: &[&str],
) -> TurnMetadata {
    let now = chrono::Local::now();
    let category = InteractionCategory::classify(user_msg, tool_hints);

    // Build action summary: concise description of what happened
    let action_summary = build_action_summary(user_msg, assistant_resp, &category);

    TurnMetadata {
        timestamp: now.format("%Y-%m-%d %H:%M:%S %Z").to_string(),
        location: if channel.is_empty() {
            "local".to_string()
        } else {
            channel.to_string()
        },
        counterpart: if sender.is_empty() {
            "user".to_string()
        } else {
            sender.to_string()
        },
        category,
        action_summary,
    }
}

/// Build a concise action summary from the turn content.
fn build_action_summary(
    user_msg: &str,
    assistant_resp: &str,
    category: &InteractionCategory,
) -> String {
    let user_preview = truncate(user_msg, 100);
    let resp_preview = truncate(assistant_resp, 100);
    let cat_label = match category {
        InteractionCategory::Chat => "대화",
        InteractionCategory::Document => "문서작업",
        InteractionCategory::Music => "음악작업",
        InteractionCategory::Image => "이미지작업",
        InteractionCategory::Translation => "통역/번역",
        InteractionCategory::Coding => "코딩",
        InteractionCategory::Search => "검색",
        InteractionCategory::General => "일반",
    };
    format!("[{cat_label}] 질문: {user_preview} → 응답: {resp_preview}")
}

/// Build a structured memory sentence with all metadata for long-term storage.
fn build_structured_memory_content(
    meta: &TurnMetadata,
    user_msg: &str,
    assistant_resp: &str,
) -> String {
    let user_preview = truncate(user_msg, 300);
    let resp_preview = truncate(assistant_resp, 300);
    format!(
        "[{category}] 시간: {time} | 장소: {location} | 상대방: {counterpart} | \
         행위: {action}\n\
         사용자: {user_preview}\n\
         응답: {resp_preview}",
        category = meta.category,
        time = meta.timestamp,
        location = meta.location,
        counterpart = meta.counterpart,
        action = meta.action_summary,
        user_preview = user_preview,
        resp_preview = resp_preview,
    )
}

/// Promote a conversation turn to long-term Core memory with structured metadata.
///
/// This creates a structured memory entry tagged with the interaction category,
/// time, place, counterpart, and action summary.
pub(super) async fn promote_to_core_memory(
    mem: &Arc<dyn Memory>,
    user_msg: &str,
    assistant_resp: &str,
    meta: &TurnMetadata,
) {
    let content = build_structured_memory_content(meta, user_msg, assistant_resp);
    let key = format!(
        "promoted_{category}_{id}",
        category = meta.category,
        id = uuid::Uuid::new_v4()
    );
    let _ = mem.store(&key, &content, MemoryCategory::Core, None).await;
    tracing::debug!(key, category = %meta.category, "Promoted turn to Core memory");
}

/// Reflect a conversation turn into the ontology layer.
///
/// Creates/updates ontology objects and links based on the interaction:
/// - Context object for the interaction session
/// - Action log with when/where/who/what metadata
/// - Links between the context and the counterpart
pub(super) fn reflect_to_ontology(
    repo: &OntologyRepo,
    user_msg: &str,
    assistant_resp: &str,
    meta: &TurnMetadata,
    owner_user_id: &str,
) {
    // 1. Ensure a Context object for this interaction session
    let context_title = format!(
        "{category} interaction at {time}",
        category = meta.category,
        time = meta.timestamp,
    );
    let context_props = json!({
        "interaction_category": meta.category.to_string(),
        "location": meta.location,
        "counterpart": meta.counterpart,
        "timestamp": meta.timestamp,
        "action_summary": meta.action_summary,
        "user_message_preview": truncate(user_msg, 200),
        "assistant_response_preview": truncate(assistant_resp, 200),
    });

    let context_id = match repo.create_object(
        "Context",
        Some(&context_title),
        &context_props,
        owner_user_id,
    ) {
        Ok(id) => id,
        Err(e) => {
            tracing::warn!("Failed to create ontology Context object: {e}");
            return;
        }
    };

    // 2. Ensure a Contact/User object for the counterpart
    if !meta.counterpart.is_empty() && meta.counterpart != "user" {
        let contact_props = json!({
            "last_seen": meta.timestamp,
            "last_channel": meta.location,
        });
        if let Ok(contact_id) =
            repo.ensure_object("Contact", &meta.counterpart, &contact_props, owner_user_id)
        {
            // Link: Context -> related_to -> Contact
            let _ = repo.create_link("related_to", context_id, contact_id, None);
        }
    }

    // 3. Create category-specific ontology objects
    match meta.category {
        InteractionCategory::Document => {
            let doc_props = json!({
                "interaction_type": "document_work",
                "timestamp": meta.timestamp,
                "location": meta.location,
                "summary": truncate(user_msg, 200),
            });
            if let Ok(doc_id) = repo.create_object(
                "Document",
                Some(&truncate(user_msg, 80)),
                &doc_props,
                owner_user_id,
            ) {
                let _ = repo.create_link("related_to", context_id, doc_id, None);
            }
        }
        InteractionCategory::Coding => {
            let task_props = json!({
                "interaction_type": "coding",
                "timestamp": meta.timestamp,
                "location": meta.location,
                "status": "completed",
                "summary": truncate(user_msg, 200),
            });
            if let Ok(task_id) = repo.create_object(
                "Task",
                Some(&truncate(user_msg, 80)),
                &task_props,
                owner_user_id,
            ) {
                let _ = repo.create_link("related_to", context_id, task_id, None);
            }
        }
        InteractionCategory::Translation => {
            let doc_props = json!({
                "interaction_type": "translation",
                "timestamp": meta.timestamp,
                "location": meta.location,
                "summary": truncate(user_msg, 200),
            });
            if let Ok(doc_id) = repo.create_object(
                "Document",
                Some(&format!("Translation: {}", truncate(user_msg, 60))),
                &doc_props,
                owner_user_id,
            ) {
                let _ = repo.create_link("related_to", context_id, doc_id, None);
            }
        }
        _ => {
            // For Chat, Music, Image, Search, General — the Context object is sufficient
        }
    }

    // 4. Log the action with full 5W1H metadata
    let action_type = match meta.category {
        InteractionCategory::Chat => "SendMessage",
        InteractionCategory::Document | InteractionCategory::Translation => "ReadDocument",
        InteractionCategory::Coding => "RunCommand",
        InteractionCategory::Search => "WebSearch",
        InteractionCategory::Music | InteractionCategory::Image | InteractionCategory::General => {
            "RecordDecision"
        }
    };

    let action_params = json!({
        "interaction_category": meta.category.to_string(),
        "user_message": truncate(user_msg, 300),
        "assistant_response": truncate(assistant_resp, 300),
        "counterpart": meta.counterpart,
    });

    let occurred_at = chrono::Local::now()
        .format("%Y-%m-%dT%H:%M:%S%:z")
        .to_string();
    if let Ok(action_id) = repo.insert_action_pending(
        action_type,
        owner_user_id,
        &ActorKind::Agent,
        Some(context_id),
        &[],
        &action_params,
        Some(&meta.location),
        None,
        Some(&occurred_at),
        Some(&meta.location),
        "",
    ) {
        // Mark the action as completed immediately (since the turn already happened)
        let result = json!({
            "status": "completed",
            "category": meta.category.to_string(),
        });
        let _ = repo.complete_action(action_id, &result);
    }

    tracing::debug!(
        category = %meta.category,
        context_id,
        "Reflected turn to ontology"
    );
}

/// Run the full promotion pipeline: short-term → Core memory + ontology.
///
/// Called after each conversation turn to ensure immediate promotion and sync.
pub(super) async fn promote_turn(
    mem: &Arc<dyn Memory>,
    ontology_repo: Option<&OntologyRepo>,
    user_msg: &str,
    assistant_resp: &str,
    channel: &str,
    sender: &str,
    tool_hints: &[&str],
    owner_user_id: &str,
) {
    // 1. Extract metadata
    let meta = extract_turn_metadata(user_msg, assistant_resp, channel, sender, tool_hints);

    // 2. Promote to Core memory (long-term) with structured content
    promote_to_core_memory(mem, user_msg, assistant_resp, &meta).await;

    // 3. Reflect to ontology (create objects, links, actions)
    if let Some(repo) = ontology_repo {
        reflect_to_ontology(repo, user_msg, assistant_resp, &meta, owner_user_id);
    }
}

/// Extract tool names used during a conversation turn from history messages.
pub(super) fn extract_tool_hints_from_history(
    messages: &[crate::providers::ChatMessage],
) -> Vec<String> {
    static TOOL_NAME_RE: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r#"<tool_result name="([^"]+)">"#).unwrap());

    let mut tools = Vec::new();
    for msg in messages {
        for cap in TOOL_NAME_RE.captures_iter(&msg.content) {
            tools.push(cap[1].to_string());
        }
    }
    tools
}

fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_chars).collect();
        format!("{truncated}...")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_turn_metadata_classifies_coding() {
        let meta = extract_turn_metadata(
            "help me write a Rust function",
            "Here's the function...",
            "cli",
            "user_a",
            &[],
        );
        assert_eq!(meta.category, InteractionCategory::Coding);
        assert_eq!(meta.counterpart, "user_a");
        assert_eq!(meta.location, "cli");
    }

    #[test]
    fn extract_turn_metadata_classifies_from_tools() {
        let meta =
            extract_turn_metadata("hello there", "hi!", "telegram", "alice", &["web_search"]);
        assert_eq!(meta.category, InteractionCategory::Search);
    }

    #[test]
    fn build_structured_memory_content_includes_all_fields() {
        let meta = TurnMetadata {
            timestamp: "2026-03-24 14:00:00 KST".to_string(),
            location: "telegram".to_string(),
            counterpart: "alice".to_string(),
            category: InteractionCategory::Chat,
            action_summary: "[대화] 질문: hello → 응답: hi".to_string(),
        };
        let content = build_structured_memory_content(&meta, "hello", "hi");
        assert!(content.contains("시간:"));
        assert!(content.contains("장소:"));
        assert!(content.contains("상대방:"));
        assert!(content.contains("행위:"));
        assert!(content.contains("telegram"));
        assert!(content.contains("alice"));
    }

    #[test]
    fn extract_tool_hints_from_history_finds_tools() {
        let msgs = vec![crate::providers::ChatMessage {
            role: "user".to_string(),
            content: r#"<tool_result name="shell">output</tool_result>"#.to_string(),
        }];
        let hints = extract_tool_hints_from_history(&msgs);
        assert_eq!(hints, vec!["shell".to_string()]);
    }

    #[test]
    fn truncate_handles_short_strings() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("hello world", 5), "hello...");
    }
}
