use crate::memory::{self, Memory};
use crate::ontology::OntologyRepo;
use crate::providers::ChatMessage;
use std::collections::HashSet;
use std::fmt::Write;

/// Maximum number of long-term memory entries to recall per message.
const MAX_RECALL_ENTRIES: usize = 100;

/// Maximum number of ontology objects to search per message.
const MAX_ONTOLOGY_ENTRIES: usize = 100;

/// Maximum cross-search enrichment entries (prevents runaway queries).
const MAX_CROSS_SEARCH_ENTRIES: usize = 20;

/// Build context preamble by searching both long-term memory and ontology
/// for relevant entries, with **bidirectional cross-referencing**.
///
/// This is the unified ACE Layer 2 implementation:
/// - Phase 0: Essential profile + user instructions (always loaded)
/// - Phase 1: Primary memory recall (RAG vector+keyword search)
/// - Phase 2: Primary ontology search (graph relationships)
/// - Phase 3: Cross-search ontology → memory enrichment
/// - Phase 4: Cross-search memory → ontology enrichment
/// - Layer 3: Budget guard (trim if total > budget)
///
/// Returns `AceContextResult` with context string and optional trimmed notice.
pub(super) async fn build_context(
    mem: &dyn Memory,
    user_msg: &str,
    min_relevance_score: f64,
    session_id: Option<&str>,
    ontology: Option<&OntologyRepo>,
    ace_budget_chars: usize,
) -> AceContextResult {
    let mut context = String::with_capacity(8192);
    let mut seen_memory_keys = HashSet::new();
    let mut cross_search_keywords = Vec::new();
    let mut section_boundaries: Vec<(usize, SectionPriority)> = Vec::new();

    // ── Phase 0: Essential profile + instructions (ALWAYS loaded) ──
    let profile_block = build_profile_context(mem).await;
    if !profile_block.is_empty() {
        let start = context.len();
        context.push_str(&profile_block);
        context.push('\n');
        section_boundaries.push((start, SectionPriority::Essential));
    }

    // ── Phase 1 (ACE Layer 2a): Primary memory recall ──
    if let Ok(entries) = mem.recall(user_msg, MAX_RECALL_ENTRIES, session_id).await {
        let relevant: Vec<_> = entries
            .iter()
            .filter(|e| match e.score {
                Some(score) => score >= min_relevance_score,
                None => true,
            })
            .collect();

        if !relevant.is_empty() {
            let start = context.len();
            context.push_str("[Memory context]\n");
            let header_only_len = context.len();
            for entry in &relevant {
                if memory::is_assistant_autosave_key(&entry.key) {
                    continue;
                }
                seen_memory_keys.insert(entry.key.clone());
                // Track recall frequency — higher count = higher priority in future RAG
                let _ = mem.track_recall(&entry.key).await;
                let ts_hint = format_short_timestamp(&entry.timestamp);
                let _ = writeln!(context, "- {}:{} {}", entry.key, ts_hint, entry.content);
                extract_cross_search_keywords(&entry.content, &mut cross_search_keywords);
            }
            if context.len() == header_only_len {
                // All entries were autosave — remove the header
                context.truncate(start);
            } else {
                context.push('\n');
                section_boundaries.push((start, SectionPriority::RagMemory));
            }
        }
    }

    // ── Phase 2 (ACE Layer 2b): Primary ontology search ──
    let mut ontology_cross_keywords = Vec::new();
    if let Some(repo) = ontology {
        let owner = session_id.unwrap_or("cli_interactive");
        if let Ok(objects) = repo.search_objects(owner, None, user_msg, MAX_ONTOLOGY_ENTRIES) {
            if !objects.is_empty() {
                let start = context.len();
                context.push_str("[Ontology context]\n");
                for obj in &objects {
                    let title = obj.title.as_deref().unwrap_or("(untitled)");
                    let props = if obj.properties.is_null()
                        || obj.properties.as_object().is_some_and(|m| m.is_empty())
                    {
                        String::new()
                    } else {
                        obj.properties.to_string()
                    };
                    if props.is_empty() {
                        let _ = writeln!(context, "- {title}");
                    } else {
                        let _ = writeln!(context, "- {title}: {props}");
                    }
                    if let Some(t) = obj.title.as_deref() {
                        ontology_cross_keywords.push(t.to_string());
                    }
                    extract_cross_search_keywords_from_json(
                        &obj.properties,
                        &mut ontology_cross_keywords,
                    );
                }
                context.push('\n');
                section_boundaries.push((start, SectionPriority::Ontology));
            }
        }

        // ── Phase 3 (ACE Layer 2c): Cross-search — ontology → memory ──
        if !ontology_cross_keywords.is_empty() {
            let cross_query = ontology_cross_keywords
                .iter()
                .take(5)
                .cloned()
                .collect::<Vec<_>>()
                .join(" ");

            if let Ok(enriched) = mem
                .recall(&cross_query, MAX_CROSS_SEARCH_ENTRIES, session_id)
                .await
            {
                let new_entries: Vec<_> = enriched
                    .iter()
                    .filter(|e| {
                        e.score.unwrap_or(1.0) >= min_relevance_score
                            && !memory::is_assistant_autosave_key(&e.key)
                            && !seen_memory_keys.contains(&e.key)
                    })
                    .collect();

                if !new_entries.is_empty() {
                    let start = context.len();
                    context.push_str("[Cross-referenced memories (from ontology context)]\n");
                    for entry in &new_entries {
                        seen_memory_keys.insert(entry.key.clone());
                        let _ = writeln!(context, "- {}: {}", entry.key, entry.content);
                    }
                    context.push('\n');
                    section_boundaries.push((start, SectionPriority::CrossSearch));
                }
            }
        }

        // ── Phase 4: Cross-search — memory → ontology ──
        if !cross_search_keywords.is_empty() {
            let cross_query = cross_search_keywords
                .iter()
                .take(5)
                .cloned()
                .collect::<Vec<_>>()
                .join(" ");

            if let Ok(enriched_objects) =
                repo.search_objects(owner, None, &cross_query, MAX_CROSS_SEARCH_ENTRIES)
            {
                let new_objects: Vec<_> = enriched_objects
                    .iter()
                    .filter(|o| {
                        let title = o.title.as_deref().unwrap_or("");
                        !ontology_cross_keywords.contains(&title.to_string())
                    })
                    .collect();

                if !new_objects.is_empty() {
                    let start = context.len();
                    context.push_str("[Cross-referenced relationships (from memory context)]\n");
                    for obj in &new_objects {
                        let title = obj.title.as_deref().unwrap_or("(untitled)");
                        let props = if obj.properties.is_null()
                            || obj.properties.as_object().is_some_and(|m| m.is_empty())
                        {
                            String::new()
                        } else {
                            obj.properties.to_string()
                        };
                        if props.is_empty() {
                            let _ = writeln!(context, "- {title}");
                        } else {
                            let _ = writeln!(context, "- {title}: {props}");
                        }
                    }
                    context.push('\n');
                    section_boundaries.push((start, SectionPriority::CrossSearch));
                }
            }
        }
    }

    // ── ACE Layer 3: Budget guard ──
    let total_chars = context.chars().count();
    let trimmed_notice = if ace_budget_chars > 0 && total_chars > ace_budget_chars {
        // Trim from lowest priority sections first
        let mut trimmed_summaries = Vec::new();
        let mut new_context = context.clone();

        // Sort by priority (lowest first = trim first)
        let mut boundaries_sorted: Vec<_> = section_boundaries
            .iter()
            .filter(|(_, p)| p.is_trimmable())
            .collect();
        boundaries_sorted.sort_by_key(|(_, p)| p.rank());

        for (_, priority) in &boundaries_sorted {
            if new_context.chars().count() <= ace_budget_chars {
                break;
            }
            // Find and summarize the section to trim
            let label = match priority {
                SectionPriority::CrossSearch => "[Cross-referenced",
                SectionPriority::RagMemory => "[Memory context",
                SectionPriority::Ontology => "[Ontology context",
                SectionPriority::Essential => continue,
            };
            if let Some(pos) = new_context.find(label) {
                // Extract first content line for the notice
                let section_text = &new_context[pos..];
                let first_content = section_text
                    .lines()
                    .skip(1)
                    .find(|l| l.trim().len() > 5)
                    .unwrap_or("(내용)")
                    .trim();
                let summary: String = first_content.chars().take(60).collect();
                trimmed_summaries.push(format!("  • {summary}..."));

                // Remove the section
                if let Some(end) = section_text.find("\n\n") {
                    let end_pos = pos + end + 2;
                    new_context = format!("{}{}", &new_context[..pos], &new_context[end_pos..]);
                }
            }
        }

        if !trimmed_summaries.is_empty() {
            context = new_context;
            Some(format!(
                "\n💡 아래 과거 기억이 저장되어 있지만 컨텍스트 예산 초과로 \
                 이번 대화에는 포함되지 않았습니다:\n{}\n\
                 추가로 검색해드릴까요? (\"기억 검색해줘\"라고 말씀해주세요)",
                trimmed_summaries.join("\n")
            ))
        } else {
            None
        }
    } else {
        None
    };

    AceContextResult {
        context,
        trimmed_memories_notice: trimmed_notice,
    }
}

/// Extract time, place, and person keywords from memory content
/// for cross-referencing with ontology.
fn extract_cross_search_keywords(content: &str, keywords: &mut Vec<String>) {
    // Look for structured metadata patterns in promoted memories:
    // [category] 시간: ... | 장소: ... | 상대방: ... | 행위: ...
    for line in content.lines() {
        let line = line.trim();

        // Extract Korean metadata fields
        for prefix in &["시간:", "장소:", "상대방:", "행위:"] {
            if let Some(pos) = line.find(prefix) {
                let after = &line[pos + prefix.len()..];
                let value = after.split('|').next().unwrap_or(after).trim();
                if !value.is_empty() && value != "unknown" && value != "user" {
                    keywords.push(value.to_string());
                }
            }
        }

        // Extract English metadata fields (for non-Korean content)
        for prefix in &["time:", "location:", "counterpart:", "action:"] {
            if let Some(pos) = line.find(prefix) {
                let after = &line[pos + prefix.len()..];
                let value = after.split('|').next().unwrap_or(after).trim();
                if !value.is_empty() && value != "unknown" && value != "user" {
                    keywords.push(value.to_string());
                }
            }
        }
    }
}

/// Extract keywords from ontology object JSON properties.
fn extract_cross_search_keywords_from_json(props: &serde_json::Value, keywords: &mut Vec<String>) {
    if let Some(obj) = props.as_object() {
        for (key, value) in obj {
            // Focus on identity/temporal/spatial fields
            if matches!(
                key.as_str(),
                "name"
                    | "location"
                    | "time"
                    | "date"
                    | "counterpart"
                    | "channel"
                    | "category"
                    | "topic"
                    | "subject"
            ) {
                if let Some(s) = value.as_str() {
                    if !s.is_empty() && s.len() < 100 {
                        keywords.push(s.to_string());
                    }
                }
            }
        }
    }
}

/// Build hardware datasheet context from RAG when peripherals are enabled.
/// Includes pin-alias lookup (e.g. "red_led" → 13) when query matches, plus retrieved chunks.
pub(super) fn build_hardware_context(
    rag: &crate::rag::HardwareRag,
    user_msg: &str,
    boards: &[String],
    chunk_limit: usize,
) -> String {
    if rag.is_empty() || boards.is_empty() {
        return String::new();
    }

    let mut context = String::new();

    // Pin aliases: when user says "red led", inject "red_led: 13" for matching boards
    let pin_ctx = rag.pin_alias_context(user_msg, boards);
    if !pin_ctx.is_empty() {
        context.push_str(&pin_ctx);
    }

    let chunks = rag.retrieve(user_msg, boards, chunk_limit);
    if chunks.is_empty() && pin_ctx.is_empty() {
        return String::new();
    }

    if !chunks.is_empty() {
        context.push_str("[Hardware documentation]\n");
    }
    for chunk in chunks {
        let board_tag = chunk.board.as_deref().unwrap_or("generic");
        let _ = writeln!(
            context,
            "--- {} ({}) ---\n{}\n",
            chunk.source, board_tag, chunk.content
        );
    }
    context.push('\n');
    context
}

/// Truncate a string to at most `max_chars` characters, appending "…" if truncated.
/// This is UTF-8 safe — it counts Unicode scalar values, not bytes.
fn truncate_chars(s: &str, max_chars: usize) -> String {
    let mut chars = s.chars();
    let truncated: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{truncated}…")
    } else {
        truncated
    }
}

/// Build cross-session recent conversation context from stored turns.
///
/// Uses a **memo substitution** strategy inspired by how humans handle conversation:
/// - Short turns (< MEMO_THRESHOLD chars): included verbatim — these are natural dialogue
/// - Long turns (>= MEMO_THRESHOLD chars): likely contain attached documents, search results,
///   code blocks, or RAG output. These are replaced with a compact memo:
///   `[opening] ... [MEMO: summary — full content searchable via "keyword"] ... [closing]`
///
/// This preserves conversational flow and nuance while avoiding token waste on
/// professional/technical content that can be retrieved on demand via vector search.
///
/// # Parameters
/// - `turns`: recent conversation turns (oldest-first, chronological order)
/// - `skip_current`: number of trailing turns to skip (e.g. 1 to skip the
///   current user message that was just appended)
/// - `max_bytes`: maximum total bytes for the context block
/// - `turn_max_chars`: maximum characters per individual turn content (used as MEMO_THRESHOLD)
pub(super) fn build_cross_session_context(
    turns: &[ChatMessage],
    skip_current: usize,
    max_bytes: usize,
    turn_max_chars: usize,
) -> String {
    if turns.is_empty() {
        return String::new();
    }

    let take_count = turns.len().saturating_sub(skip_current);
    if take_count == 0 {
        return String::new();
    }

    const HEADER: &str =
        "[Recent conversation history — verbatim, continue this conversation naturally]\n";

    // ── 3-tier progressive compression thresholds ──
    // Tier 1: 0~999 chars     → verbatim (normal conversation)
    // Tier 2: 1000~1499 chars → keep first 70% verbatim, summarize last 30%
    // Tier 3: 1500~1999 chars → keep first 50% verbatim, summarize last 50%
    // Tier 4: 2000+ chars     → 10% proportional memo (existing logic)
    //
    // This is pure string processing — no LLM call, zero cost.
    // Prevents gradual token bloat from medium-length turns that individually
    // seem acceptable but collectively consume excessive context.
    let memo_threshold_full = turn_max_chars.min(2000); // Tier 4 (full memo)
    let memo_threshold_heavy = 1500; // Tier 3 (50% tail compression)
    let memo_threshold_light = 1000; // Tier 2 (30% tail compression)

    let estimated = HEADER.len() + take_count.min(600) * 120;
    let mut ctx = String::with_capacity(estimated.min(max_bytes + 512));
    ctx.push_str(HEADER);
    let mut total = HEADER.len();

    for turn in turns.iter().take(take_count) {
        let label = if turn.role == "user" {
            "User"
        } else {
            "Assistant"
        };
        let content = &turn.content;
        let char_count = content.chars().count();

        let formatted = if char_count < memo_threshold_light {
            // ── Tier 1: Short turn (< 1000 chars) — verbatim ──
            format!("{label}: {content}\n")
        } else if char_count < memo_threshold_heavy {
            // ── Tier 2: Medium turn (1000~1499 chars) — keep 70%, compress tail 30% ──
            let keep_chars = (char_count * 70) / 100;
            let kept: String = content.chars().take(keep_chars).collect();
            let tail: String = content.chars().skip(keep_chars).collect();
            let tail_summary = summarize_section(&tail, (char_count * 5) / 100); // 5% summary
            format!(
                "{label}: {kept}\n  [📋 이하 {tail_len}자 축약: {tail_summary}]\n",
                tail_len = char_count - keep_chars,
            )
        } else if char_count < memo_threshold_full {
            // ── Tier 3: Long turn (1500~1999 chars) — keep 50%, compress tail 50% ──
            let keep_chars = char_count / 2;
            let kept: String = content.chars().take(keep_chars).collect();
            let tail: String = content.chars().skip(keep_chars).collect();
            let tail_summary = summarize_section(&tail, (char_count * 8) / 100); // 8% summary
            format!(
                "{label}: {kept}\n  [📋 이하 {tail_len}자 축약: {tail_summary}]\n",
                tail_len = char_count - keep_chars,
            )
        } else {
            // ── Tier 4: Very long turn (2000+ chars) — 10% proportional memo ──
            let opening_chars = 300;
            let closing_chars = 300;

            let opening: String = content.chars().take(opening_chars).collect();
            let closing: String = content
                .chars()
                .rev()
                .take(closing_chars)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();

            let memo = generate_turn_memo(content, char_count);

            format!(
                "{label}: {opening}\n  [📋 MEMO ({char_count}자 중 요약): {memo}]\n  ...{closing}\n"
            )
        };

        if total + formatted.len() > max_bytes {
            break;
        }
        total += formatted.len();
        ctx.push_str(&formatted);
    }

    if ctx.len() == HEADER.len() {
        String::new()
    } else {
        ctx.push('\n');
        ctx
    }
}

/// Generate a proportional memo from a long conversation turn.
///
/// Memo size = 10% of the original content length (not a fixed 200 chars).
/// For multi-topic content, each topic/item is summarized separately
/// following the 6W principle (who/what/when/where/why/how).
///
/// Examples:
/// - 2,000 char turn → ~200 char memo
/// - 10,000 char turn → ~1,000 char memo
/// - 100,000 char turn → ~10,000 char memo
fn generate_turn_memo(content: &str, char_count: usize) -> String {
    // Memo budget: 10% of original, minimum 100 chars, maximum 10,000 chars
    let memo_budget = (char_count / 10).clamp(100, 10_000);

    let mut memo_parts: Vec<String> = Vec::new();

    // 1. Content type detection
    let mut content_types = Vec::new();
    if content.contains("```") || content.contains("fn ") || content.contains("function ") {
        content_types.push("코드");
    }
    if content.contains("http://") || content.contains("https://") {
        content_types.push("URL");
    }
    if content.contains("검색 결과") || content.contains("Search results") {
        content_types.push("검색결과");
    }
    if !content_types.is_empty() {
        memo_parts.push(format!("[{}]", content_types.join(", ")));
    }

    // 2. Extract section-level summaries for multi-topic content
    //    Split by blank lines, headers (##, ###), or numbered items (1., 2.)
    let sections = split_into_sections(content);
    let chars_per_section = if sections.is_empty() {
        memo_budget
    } else {
        (memo_budget / sections.len()).max(50)
    };

    for section in &sections {
        let summary = summarize_section(section, chars_per_section);
        if !summary.is_empty() {
            memo_parts.push(summary);
        }
    }

    // 3. Extract trailing question or action item
    let last_lines: Vec<&str> = content
        .lines()
        .rev()
        .filter(|l| l.trim().len() > 5)
        .take(3)
        .collect();
    for line in last_lines.iter().rev() {
        let trimmed = line.trim();
        if trimmed.contains('?')
            || trimmed.contains("할까")
            || trimmed.contains("드릴까")
            || trimmed.contains("하시겠")
            || trimmed.contains("해줘")
            || trimmed.contains("알려")
            || trimmed.contains("확인")
            || trimmed.contains("제안")
        {
            let question: String = trimmed.chars().take(chars_per_section).collect();
            memo_parts.push(format!("▸ 질문/요청: {question}"));
            break;
        }
    }

    let memo = memo_parts.join("\n");
    if memo.is_empty() {
        format!("{char_count}자 장문 — 필요 시 memory_recall로 검색 가능")
    } else {
        // Final cap at memo_budget
        let result: String = memo.chars().take(memo_budget).collect();
        result
    }
}

/// Split content into logical sections by blank lines, headers, or numbered lists.
fn split_into_sections(content: &str) -> Vec<String> {
    let mut sections: Vec<String> = Vec::new();
    let mut current = String::new();

    for line in content.lines() {
        let trimmed = line.trim();

        // Section break: blank line, markdown header, or numbered item start
        let is_break = trimmed.is_empty()
            || trimmed.starts_with("## ")
            || trimmed.starts_with("### ")
            || trimmed.starts_with("---")
            || (trimmed.len() > 2
                && trimmed.chars().next().map_or(false, |c| c.is_ascii_digit())
                && (trimmed.contains(". ") || trimmed.contains(") ")));

        if is_break && !current.trim().is_empty() {
            sections.push(std::mem::take(&mut current));
        }
        current.push_str(line);
        current.push('\n');
    }
    if !current.trim().is_empty() {
        sections.push(current);
    }

    // Merge very small sections (< 50 chars) into previous
    let mut merged: Vec<String> = Vec::new();
    for section in sections {
        if section.trim().chars().count() < 50 {
            if let Some(last) = merged.last_mut() {
                last.push_str(&section);
            } else {
                merged.push(section);
            }
        } else {
            merged.push(section);
        }
    }

    merged
}

/// Summarize a single section to fit within `budget` characters.
/// Extracts the 6W essence: who did what, when, where, why, how.
fn summarize_section(section: &str, budget: usize) -> String {
    let trimmed = section.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    // If section fits within budget, return as-is
    let char_count = trimmed.chars().count();
    if char_count <= budget {
        return trimmed.to_string();
    }

    // Extract first meaningful line as topic
    let first_line = trimmed
        .lines()
        .find(|l| l.trim().len() > 5)
        .unwrap_or("")
        .trim();

    let topic: String = first_line.chars().take(budget.min(120)).collect();

    // If budget allows, add more context from the section
    if budget > 150 {
        let remaining_budget = budget - topic.chars().count() - 10;
        // Extract key sentences (lines containing important markers)
        let key_sentences: Vec<&str> = trimmed
            .lines()
            .skip(1)
            .filter(|l| {
                let t = l.trim();
                t.len() > 10
                    && (t.contains("결과")
                        || t.contains("결론")
                        || t.contains("요약")
                        || t.contains("중요")
                        || t.contains("핵심")
                        || t.contains("따라서")
                        || t.contains("때문")
                        || t.contains("위해")
                        || t.contains("Result")
                        || t.contains("Summary")
                        || t.contains("because")
                        || t.contains("therefore")
                        || t.starts_with("- ")
                        || t.starts_with("* "))
            })
            .collect();

        if !key_sentences.is_empty() {
            let mut extra = String::new();
            for sentence in key_sentences {
                let s = sentence.trim();
                if extra.chars().count() + s.chars().count() > remaining_budget {
                    break;
                }
                extra.push_str(" | ");
                extra.push_str(s);
            }
            if !extra.is_empty() {
                return format!("{topic}{extra}");
            }
        }
    }

    topic
}

// ═══════════════════════════════════════════════════════════════════
// ACE (Adaptive Context Engine) — 4-Layer Context Builder
// ═══════════════════════════════════════════════════════════════════

/// Result from the ACE context builder, including any trimmed memories
/// that the user should be notified about.
pub(super) struct AceContextResult {
    /// The constructed context string to prepend to the user message.
    pub context: String,
    /// If Layer 3 trimmed RAG results, this contains a notification
    /// for the user about available but hidden memories.
    pub trimmed_memories_notice: Option<String>,
}

// SectionPriority is used by the unified build_context() for Layer 3 budget guard.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SectionPriority {
    Essential,   // user profile — never trimmed
    Ontology,    // relationship graph — trimmed last
    RagMemory,   // past conversation RAG — trimmed second
    CrossSearch, // cross-referenced enrichment — trimmed first
}

impl SectionPriority {
    fn is_trimmable(&self) -> bool {
        !matches!(self, Self::Essential)
    }

    /// Lower rank = trimmed first
    fn rank(&self) -> u8 {
        match self {
            Self::CrossSearch => 0,
            Self::RagMemory => 1,
            Self::Ontology => 2,
            Self::Essential => 3,
        }
    }
}

/// Build profile + standing instructions context block (essential, always loaded).
async fn build_profile_context(mem: &dyn Memory) -> String {
    const ESSENTIAL_PROFILE_KEYS: &[&str] = &[
        "user_profile_identity",
        "user_profile_family",
        "user_profile_work",
        "user_profile_lifestyle",
        "user_profile_communication",
        "user_profile_routine",
        "user_moa_preferences",
    ];

    /// Key prefixes for user instructions that must always be in context.
    /// These are standing orders, cron directives, recurring reminders —
    /// the user's "지시사항" that MoA must never forget.
    const INSTRUCTION_PREFIXES: &[&str] = &[
        "user_instruction_",
        "user_standing_order_",
        "user_cron_",
        "user_reminder_",
        "user_schedule_",
    ];

    let mut context = String::new();
    let mut loaded = false;

    // 1. User profile (항상 로드)
    for key in ESSENTIAL_PROFILE_KEYS {
        if let Ok(Some(entry)) = mem.get(key).await {
            if !loaded {
                context.push_str("[이용자 프로필 — 항상 로드]\n");
                loaded = true;
            }
            let ts = format_short_timestamp(&entry.timestamp);
            let _ = writeln!(context, "- {}:{} {}", entry.key, ts, entry.content);
        }
    }
    if loaded {
        context.push('\n');
    }

    // 2. User instructions & standing orders (항상 로드)
    // "매일 9시에 날씨 알려줘", "항상 존칭 사용해줘" 등
    if let Ok(all_entries) = mem.list(None, None).await {
        let instructions: Vec<_> = all_entries
            .iter()
            .filter(|e| INSTRUCTION_PREFIXES.iter().any(|p| e.key.starts_with(p)))
            .collect();

        if !instructions.is_empty() {
            context.push_str("[이용자 지시사항 — 항상 이행]\n");
            for entry in &instructions {
                let ts = format_short_timestamp(&entry.timestamp);
                let _ = writeln!(context, "- {}:{} {}", entry.key, ts, entry.content);
            }
            context.push('\n');
        }
    }

    context
}

fn format_short_timestamp(timestamp: &str) -> String {
    if timestamp.is_empty() {
        String::new()
    } else {
        let short = if timestamp.len() > 19 {
            &timestamp[..19]
        } else {
            timestamp
        };
        format!(" [{}]", short)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_chars_ascii() {
        assert_eq!(truncate_chars("hello world", 5), "hello…");
        assert_eq!(truncate_chars("hello", 5), "hello");
        assert_eq!(truncate_chars("hi", 5), "hi");
    }

    #[test]
    fn truncate_chars_multibyte() {
        // Korean text: each character is 3 bytes in UTF-8
        let korean = "안녕하세요 반갑습니다";
        let result = truncate_chars(korean, 5);
        assert_eq!(result, "안녕하세요…");
        // Ensure no panic on multi-byte boundaries
        assert_eq!(truncate_chars(korean, 1), "안…");
    }

    #[test]
    fn build_cross_session_empty() {
        assert_eq!(build_cross_session_context(&[], 0, 16000, 600), "");
    }

    #[test]
    fn build_cross_session_skips_current() {
        let turns = vec![
            ChatMessage {
                role: "user".into(),
                content: "hello".into(),
            },
            ChatMessage {
                role: "assistant".into(),
                content: "hi there".into(),
            },
            ChatMessage {
                role: "user".into(),
                content: "current msg".into(),
            },
        ];
        let ctx = build_cross_session_context(&turns, 1, 16000, 600);
        assert!(ctx.contains("User: hello"));
        assert!(ctx.contains("Assistant: hi there"));
        assert!(!ctx.contains("current msg"));
    }

    #[test]
    fn build_cross_session_respects_byte_limit() {
        let turns: Vec<ChatMessage> = (0..100)
            .map(|i| ChatMessage {
                role: "user".into(),
                content: format!("message number {i} with some content padding"),
            })
            .collect();
        let ctx = build_cross_session_context(&turns, 0, 500, 600);
        assert!(ctx.len() <= 500 + 100); // small overshoot from last line is ok
    }

    #[test]
    fn build_cross_session_truncates_long_turns() {
        // Long turns (>= memo_threshold) should be memo-substituted, not just truncated
        let turns = vec![ChatMessage {
            role: "user".into(),
            content: "a".repeat(3000),
        }];
        let ctx = build_cross_session_context(&turns, 0, 16000, 2000);
        // Should contain memo marker, not raw 3000 'a's
        assert!(ctx.contains("MEMO"));
        // Should NOT contain the full 3000-char content
        assert!(ctx.len() < 2000);
    }

    #[test]
    fn build_cross_session_short_turns_verbatim() {
        let turns = vec![
            ChatMessage {
                role: "user".into(),
                content: "안녕하세요 변호사님".into(),
            },
            ChatMessage {
                role: "assistant".into(),
                content: "네, 변호사님! 무엇을 도와드릴까요?".into(),
            },
        ];
        let ctx = build_cross_session_context(&turns, 0, 16000, 2000);
        assert!(ctx.contains("User: 안녕하세요 변호사님"));
        assert!(ctx.contains("Assistant: 네, 변호사님! 무엇을 도와드릴까요?"));
    }

    #[test]
    fn build_cross_session_tier2_medium_turn_partial_compress() {
        // 1000~1499 chars: keep 70%, compress tail 30%
        let content = "가".repeat(1200);
        let turns = vec![ChatMessage {
            role: "assistant".into(),
            content,
        }];
        let ctx = build_cross_session_context(&turns, 0, 32000, 2000);
        // Should contain the 축약 marker
        assert!(ctx.contains("축약"));
        // Should NOT be fully verbatim (would be 1200 chars + label)
        assert!(ctx.len() < 1200 * 3 + 100); // Korean chars are 3 bytes each
    }

    #[test]
    fn build_cross_session_tier3_long_turn_half_compress() {
        // 1500~1999 chars: keep 50%, compress tail 50%
        let content = "나".repeat(1700);
        let turns = vec![ChatMessage {
            role: "assistant".into(),
            content,
        }];
        let ctx = build_cross_session_context(&turns, 0, 32000, 2000);
        assert!(ctx.contains("축약"));
        // Should be significantly shorter than verbatim
        assert!(ctx.len() < 1700 * 3);
    }

    #[test]
    fn build_cross_session_under_1000_verbatim() {
        // Under 1000 chars: fully verbatim
        let content = "다".repeat(999);
        let turns = vec![ChatMessage {
            role: "user".into(),
            content: content.clone(),
        }];
        let ctx = build_cross_session_context(&turns, 0, 32000, 2000);
        assert!(!ctx.contains("축약"));
        assert!(!ctx.contains("MEMO"));
        assert!(ctx.contains(&content));
    }
}
