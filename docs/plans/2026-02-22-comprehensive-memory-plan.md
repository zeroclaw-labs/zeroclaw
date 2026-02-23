# Comprehensive Memory Capture Enhancement — Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add structured fact extraction with dedicated Gemini 3 Flash memory agents per tier, agent response auto-save, and fact-first recall to the existing three-tier memory system.

**Architecture:** A `FactExtractor` trait backed by an OpenRouter HTTP client sends conversation turns to Gemini 3 Flash for structured fact extraction. Facts are stored as `MemoryCategory::Core` entries with JSON-serialized `FactEntry` content. The STM extraction runs non-blocking via a bounded `tokio::mpsc` queue. The MTM deep pass extracts facts during daily compression. Enhanced recall boosts facts in `merge_and_rank`.

**Tech Stack:** Rust, tokio, reqwest (HTTP client), serde_json, OpenRouter API (OpenAI-compatible), google/gemini-3-flash-preview

---

### Task 0: Add MemoryAgentConfig to TierConfig

**Files:**
- Modify: `src/memory/tiered/types.rs`

**Step 1: Write the failing test**

Add to the `#[cfg(test)] mod tests` block at the bottom of `src/memory/tiered/types.rs`:

```rust
#[test]
fn default_memory_agent_config_has_sane_values() {
    let cfg = MemoryAgentConfig::default();
    assert_eq!(cfg.model, "google/gemini-3-flash-preview");
    assert_eq!(cfg.temperature, 0.1);
    assert!(cfg.timeout_secs > 0);
    assert!(cfg.max_tokens > 0);
}

#[test]
fn default_tier_config_has_memory_agents() {
    let cfg = TierConfig::default();
    assert_eq!(cfg.memory_agents.openrouter_base_url, "https://openrouter.ai/api/v1");
    assert_eq!(cfg.memory_agents.stm.model, "google/gemini-3-flash-preview");
    assert_eq!(cfg.memory_agents.stm.timeout_secs, 10);
    assert_eq!(cfg.memory_agents.mtm.timeout_secs, 60);
    assert_eq!(cfg.memory_agents.ltm.timeout_secs, 60);
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p zeroclaw --lib memory::tiered::types::tests::default_memory_agent_config_has_sane_values -- --nocapture`
Expected: FAIL — `MemoryAgentConfig` not found

**Step 3: Write minimal implementation**

Add before the `TierConfig` struct in `src/memory/tiered/types.rs`:

```rust
/// Configuration for a single memory agent (one per tier).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryAgentConfig {
    pub model: String,
    pub max_tokens: usize,
    pub temperature: f32,
    pub timeout_secs: u64,
}

impl Default for MemoryAgentConfig {
    fn default() -> Self {
        Self {
            model: "google/gemini-3-flash-preview".to_string(),
            max_tokens: 4096,
            temperature: 0.1,
            timeout_secs: 30,
        }
    }
}

/// Configuration for all memory agents across the three tiers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryAgentsConfig {
    pub openrouter_api_key: Option<String>,
    pub openrouter_base_url: String,
    pub stm: MemoryAgentConfig,
    pub mtm: MemoryAgentConfig,
    pub ltm: MemoryAgentConfig,
}

impl Default for MemoryAgentsConfig {
    fn default() -> Self {
        Self {
            openrouter_api_key: None,
            openrouter_base_url: "https://openrouter.ai/api/v1".to_string(),
            stm: MemoryAgentConfig {
                max_tokens: 400,
                timeout_secs: 10,
                ..MemoryAgentConfig::default()
            },
            mtm: MemoryAgentConfig {
                max_tokens: 8192,
                timeout_secs: 60,
                ..MemoryAgentConfig::default()
            },
            ltm: MemoryAgentConfig {
                max_tokens: 8192,
                timeout_secs: 60,
                ..MemoryAgentConfig::default()
            },
        }
    }
}
```

Add `memory_agents` field to `TierConfig`:

```rust
pub memory_agents: MemoryAgentsConfig,  // add after min_relevance_threshold
```

And in `TierConfig::default()`:

```rust
memory_agents: MemoryAgentsConfig::default(),
```

**Step 4: Run test to verify it passes**

Run: `cargo test -p zeroclaw --lib memory::tiered::types::tests -- --nocapture`
Expected: ALL PASS

**Step 5: Commit**

```bash
git add src/memory/tiered/types.rs
git commit -m "feat(memory/tiered): add MemoryAgentConfig and MemoryAgentsConfig to TierConfig"
```

---

### Task 1: FactEntry struct and key taxonomy

**Files:**
- Create: `src/memory/tiered/facts.rs`
- Modify: `src/memory/tiered/mod.rs` (add `pub mod facts;`)

**Step 1: Write the failing test**

Create `src/memory/tiered/facts.rs` with tests at the bottom:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fact_key_builds_correctly() {
        let key = build_fact_key("personal", "user", "birthday");
        assert_eq!(key, "fact:personal:user:birthday");
    }

    #[test]
    fn fact_key_slugifies_inputs() {
        let key = build_fact_key("Personal Info", "John Doe", "Birth Day");
        assert_eq!(key, "fact:personal-info:john-doe:birth-day");
    }

    #[test]
    fn fact_key_with_vhash_for_multivalued() {
        let key = build_fact_key_multivalued("infra", "servers", "ip_address", "192.168.1.10");
        assert!(key.starts_with("fact:infra:servers:ip-address:"));
        assert_eq!(key.len(), "fact:infra:servers:ip-address:".len() + 8);
    }

    #[test]
    fn fact_entry_to_searchable_text() {
        let entry = FactEntry {
            fact_id: "test-id".into(),
            fact_key: "fact:personal:user:birthday".into(),
            category: "personal".into(),
            subject: "user".into(),
            attribute: "birthday".into(),
            value: "March 15".into(),
            context_narrative: "User mentioned birthday while discussing deployment freeze".into(),
            source_turn: SourceTurnRef {
                conversation_id: "conv-1".into(),
                turn_index: 5,
                message_id: None,
                role: SourceRole::User,
                timestamp_unix_ms: 1708600000000,
            },
            confidence: FactConfidence::High,
            related_facts: vec![],
            extracted_by_tier: "stm".into(),
            extracted_at_unix_ms: 1708600000000,
            source_role: SourceRole::User,
            status: FactStatus::Active,
            revision: 1,
            supersedes_fact_id: None,
            tags: vec!["birthday".into(), "personal".into()],
            volatility_class: VolatilityClass::Stable,
            ttl_days: None,
            expires_at_unix_ms: None,
            last_verified_unix_ms: Some(1708600000000),
        };
        let text = entry.to_searchable_text();
        assert!(text.contains("birthday"));
        assert!(text.contains("March 15"));
        assert!(text.contains("deployment freeze"));
    }

    #[test]
    fn supersede_marks_old_fact() {
        let mut old = FactEntry {
            fact_id: "old-id".into(),
            fact_key: "fact:personal:user:birthday".into(),
            category: "personal".into(),
            subject: "user".into(),
            attribute: "birthday".into(),
            value: "March 14".into(),
            context_narrative: "Initially said March 14".into(),
            source_turn: SourceTurnRef {
                conversation_id: "conv-1".into(),
                turn_index: 3,
                message_id: None,
                role: SourceRole::User,
                timestamp_unix_ms: 1708500000000,
            },
            confidence: FactConfidence::High,
            related_facts: vec![],
            extracted_by_tier: "stm".into(),
            extracted_at_unix_ms: 1708500000000,
            source_role: SourceRole::User,
            status: FactStatus::Active,
            revision: 1,
            supersedes_fact_id: None,
            tags: vec!["birthday".into()],
            volatility_class: VolatilityClass::Stable,
            ttl_days: None,
            expires_at_unix_ms: None,
            last_verified_unix_ms: None,
        };
        old.mark_superseded();
        assert_eq!(old.status, FactStatus::Superseded);
    }

    #[test]
    fn agent_fact_capped_at_medium_confidence() {
        let mut entry = FactEntry {
            fact_id: "a".into(),
            fact_key: "fact:test:a:b".into(),
            category: "test".into(),
            subject: "a".into(),
            attribute: "b".into(),
            value: "v".into(),
            context_narrative: "ctx".into(),
            source_turn: SourceTurnRef {
                conversation_id: "c".into(),
                turn_index: 1,
                message_id: None,
                role: SourceRole::Agent,
                timestamp_unix_ms: 0,
            },
            confidence: FactConfidence::High,
            related_facts: vec![],
            extracted_by_tier: "stm".into(),
            extracted_at_unix_ms: 0,
            source_role: SourceRole::Agent,
            status: FactStatus::Active,
            revision: 1,
            supersedes_fact_id: None,
            tags: vec![],
            volatility_class: VolatilityClass::Stable,
            ttl_days: None,
            expires_at_unix_ms: None,
            last_verified_unix_ms: None,
        };
        entry.apply_poisoning_guard();
        assert_eq!(entry.confidence, FactConfidence::Medium);
    }
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p zeroclaw --lib memory::tiered::facts::tests -- --nocapture`
Expected: FAIL — module not found

**Step 3: Write minimal implementation**

Write the full `src/memory/tiered/facts.rs`:

```rust
//! Structured fact extraction types and key taxonomy.
//!
//! Facts are stored as [`MemoryCategory::Core`] entries with the content field
//! containing a human-readable searchable summary and the key field using the
//! canonical `fact:{category}:{subject}:{attribute}` format.

use serde::{Deserialize, Serialize};

// ── Enums ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum FactConfidence {
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum FactStatus {
    Active,
    Superseded,
    Retracted,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum SourceRole {
    User,
    Agent,
    System,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum VolatilityClass {
    Stable,
    SemiStable,
    Volatile,
}

// ── Source reference ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SourceTurnRef {
    pub conversation_id: String,
    pub turn_index: u64,
    pub message_id: Option<String>,
    pub role: SourceRole,
    pub timestamp_unix_ms: i64,
}

// ── FactEntry ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FactEntry {
    pub fact_id: String,
    pub fact_key: String,
    pub category: String,
    pub subject: String,
    pub attribute: String,
    pub value: String,
    pub context_narrative: String,
    pub source_turn: SourceTurnRef,
    pub confidence: FactConfidence,
    pub related_facts: Vec<String>,
    pub extracted_by_tier: String,
    pub extracted_at_unix_ms: i64,
    pub source_role: SourceRole,
    pub status: FactStatus,
    pub revision: u32,
    pub supersedes_fact_id: Option<String>,
    pub tags: Vec<String>,
    pub volatility_class: VolatilityClass,
    pub ttl_days: Option<u16>,
    pub expires_at_unix_ms: Option<i64>,
    pub last_verified_unix_ms: Option<i64>,
}

impl FactEntry {
    /// Format the fact as searchable natural-language text for embedding/FTS.
    pub fn to_searchable_text(&self) -> String {
        format!(
            "{} {} {}: {}. {}",
            self.category, self.subject, self.attribute, self.value, self.context_narrative
        )
    }

    /// Mark this fact as superseded by a newer revision.
    pub fn mark_superseded(&mut self) {
        self.status = FactStatus::Superseded;
    }

    /// Cap agent-derived facts at Medium confidence (poisoning guard).
    pub fn apply_poisoning_guard(&mut self) {
        if self.source_role == SourceRole::Agent && self.confidence == FactConfidence::High {
            self.confidence = FactConfidence::Medium;
        }
    }
}

// ── Key taxonomy ─────────────────────────────────────────────────────────────

fn slugify(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

/// Build a canonical fact key: `fact:{category}:{subject}:{attribute}`.
pub fn build_fact_key(category: &str, subject: &str, attribute: &str) -> String {
    format!("fact:{}:{}:{}", slugify(category), slugify(subject), slugify(attribute))
}

/// Build a multi-valued fact key with an 8-char hash suffix.
pub fn build_fact_key_multivalued(
    category: &str,
    subject: &str,
    attribute: &str,
    value: &str,
) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    let hash = format!("{:016x}", hasher.finish());
    format!(
        "fact:{}:{}:{}:{}",
        slugify(category),
        slugify(subject),
        slugify(attribute),
        &hash[..8]
    )
}
```

Add `pub mod facts;` to `src/memory/tiered/mod.rs` after the existing module declarations.

**Step 4: Run test to verify it passes**

Run: `cargo test -p zeroclaw --lib memory::tiered::facts::tests -- --nocapture`
Expected: ALL PASS (6 tests)

**Step 5: Commit**

```bash
git add src/memory/tiered/facts.rs src/memory/tiered/mod.rs
git commit -m "feat(memory/tiered): FactEntry struct, key taxonomy, poisoning guard"
```

---

### Task 2: FactExtractor trait and OpenRouter client

**Files:**
- Create: `src/memory/tiered/extractor.rs`
- Modify: `src/memory/tiered/mod.rs` (add `pub mod extractor;`)

**Step 1: Write the failing test**

Create `src/memory/tiered/extractor.rs` with tests:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn mock_extractor_returns_facts() {
        let extractor = MockFactExtractor::new(vec![
            FactEntryDraft {
                category: "personal".into(),
                subject: "user".into(),
                attribute: "name".into(),
                value: "John".into(),
                context_narrative: "User introduced themselves".into(),
                confidence: "high".into(),
                related_facts: vec![],
                volatility_class: "stable".into(),
            },
        ]);
        let result = extractor.extract("Hello, I'm John").await.unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].value, "John");
    }

    #[tokio::test]
    async fn mock_extractor_empty_input() {
        let extractor = MockFactExtractor::new(vec![]);
        let result = extractor.extract("").await.unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn parse_extraction_response_valid_json() {
        let json = r#"[{"category":"personal","subject":"user","attribute":"name","value":"John","context_narrative":"Introduced self","confidence":"high","related_facts":[],"volatility_class":"stable"}]"#;
        let drafts = parse_extraction_response(json).unwrap();
        assert_eq!(drafts.len(), 1);
        assert_eq!(drafts[0].attribute, "name");
    }

    #[test]
    fn parse_extraction_response_invalid_json() {
        let result = parse_extraction_response("not json at all");
        assert!(result.is_err());
    }

    #[test]
    fn parse_extraction_response_extracts_from_markdown_fence() {
        let json = "Some text\n```json\n[{\"category\":\"test\",\"subject\":\"s\",\"attribute\":\"a\",\"value\":\"v\",\"context_narrative\":\"c\",\"confidence\":\"medium\",\"related_facts\":[],\"volatility_class\":\"volatile\"}]\n```\nmore text";
        let drafts = parse_extraction_response(json).unwrap();
        assert_eq!(drafts.len(), 1);
    }
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p zeroclaw --lib memory::tiered::extractor::tests -- --nocapture`
Expected: FAIL — module not found

**Step 3: Write minimal implementation**

Write `src/memory/tiered/extractor.rs`:

```rust
//! Fact extraction client — FactExtractor trait + OpenRouter and Mock impls.
//!
//! The [`FactExtractor`] trait is the interface for sending conversation text
//! to an LLM and receiving structured [`FactEntryDraft`] results.

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

// ── Draft type (LLM output before enrichment) ────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FactEntryDraft {
    pub category: String,
    pub subject: String,
    pub attribute: String,
    pub value: String,
    pub context_narrative: String,
    pub confidence: String,       // "low", "medium", "high"
    pub related_facts: Vec<String>,
    pub volatility_class: String, // "stable", "semi_stable", "volatile"
}

// ── Trait ─────────────────────────────────────────────────────────────────────

#[async_trait]
pub trait FactExtractor: Send + Sync {
    /// Extract structured facts from conversation text.
    async fn extract(&self, prompt: &str) -> Result<Vec<FactEntryDraft>>;
}

// ── JSON parsing helper ──────────────────────────────────────────────────────

/// Parse LLM response into FactEntryDraft vec.
/// Handles raw JSON arrays and markdown-fenced JSON blocks.
pub fn parse_extraction_response(response: &str) -> Result<Vec<FactEntryDraft>> {
    let trimmed = response.trim();

    // Try direct parse first
    if let Ok(drafts) = serde_json::from_str::<Vec<FactEntryDraft>>(trimmed) {
        return Ok(drafts);
    }

    // Try extracting from markdown code fence
    if let Some(start) = trimmed.find("```json") {
        if let Some(end) = trimmed[start + 7..].find("```") {
            let json_str = trimmed[start + 7..start + 7 + end].trim();
            if let Ok(drafts) = serde_json::from_str::<Vec<FactEntryDraft>>(json_str) {
                return Ok(drafts);
            }
        }
    }

    // Try finding any JSON array
    if let Some(start) = trimmed.find('[') {
        if let Some(end) = trimmed.rfind(']') {
            let json_str = &trimmed[start..=end];
            if let Ok(drafts) = serde_json::from_str::<Vec<FactEntryDraft>>(json_str) {
                return Ok(drafts);
            }
        }
    }

    anyhow::bail!("Failed to parse fact extraction response as JSON array")
}

// ── Mock implementation ──────────────────────────────────────────────────────

pub struct MockFactExtractor {
    fixed_response: Vec<FactEntryDraft>,
}

impl MockFactExtractor {
    pub fn new(response: Vec<FactEntryDraft>) -> Self {
        Self { fixed_response: response }
    }
}

#[async_trait]
impl FactExtractor for MockFactExtractor {
    async fn extract(&self, _prompt: &str) -> Result<Vec<FactEntryDraft>> {
        Ok(self.fixed_response.clone())
    }
}

// ── OpenRouter implementation ────────────────────────────────────────────────

pub struct OpenRouterFactExtractor {
    client: reqwest::Client,
    base_url: String,
    api_key: String,
    model: String,
    max_tokens: usize,
    temperature: f32,
    timeout: std::time::Duration,
}

impl OpenRouterFactExtractor {
    pub fn new(
        base_url: String,
        api_key: String,
        model: String,
        max_tokens: usize,
        temperature: f32,
        timeout_secs: u64,
    ) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url,
            api_key,
            model,
            max_tokens,
            temperature,
            timeout: std::time::Duration::from_secs(timeout_secs),
        }
    }
}

#[derive(Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<ChatMessage>,
    max_tokens: usize,
    temperature: f32,
}

#[derive(Serialize)]
struct ChatMessage {
    role: String,
    content: String,
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<ChatChoice>,
}

#[derive(Deserialize)]
struct ChatChoice {
    message: ChatResponseMessage,
}

#[derive(Deserialize)]
struct ChatResponseMessage {
    content: String,
}

#[async_trait]
impl FactExtractor for OpenRouterFactExtractor {
    async fn extract(&self, prompt: &str) -> Result<Vec<FactEntryDraft>> {
        let request = ChatRequest {
            model: self.model.clone(),
            messages: vec![ChatMessage {
                role: "user".to_string(),
                content: prompt.to_string(),
            }],
            max_tokens: self.max_tokens,
            temperature: self.temperature,
        };

        let response = self
            .client
            .post(format!("{}/chat/completions", self.base_url))
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .timeout(self.timeout)
            .json(&request)
            .send()
            .await
            .context("OpenRouter request failed")?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("OpenRouter returned {}: {}", status, body);
        }

        let chat_response: ChatResponse = response
            .json()
            .await
            .context("Failed to parse OpenRouter response")?;

        let content = chat_response
            .choices
            .first()
            .map(|c| c.message.content.as_str())
            .unwrap_or("[]");

        parse_extraction_response(content)
    }
}
```

Add `pub mod extractor;` to `src/memory/tiered/mod.rs`.

**Step 4: Run test to verify it passes**

Run: `cargo test -p zeroclaw --lib memory::tiered::extractor::tests -- --nocapture`
Expected: ALL PASS (5 tests)

**Step 5: Commit**

```bash
git add src/memory/tiered/extractor.rs src/memory/tiered/mod.rs
git commit -m "feat(memory/tiered): FactExtractor trait, OpenRouter client, MockFactExtractor"
```

---

### Task 3: Extraction prompt templates

**Files:**
- Create: `src/memory/tiered/prompts.rs`
- Modify: `src/memory/tiered/mod.rs` (add `pub mod prompts;`)

**Step 1: Write the failing test**

Create `src/memory/tiered/prompts.rs` with tests:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stm_prompt_includes_messages() {
        let prompt = build_stm_extraction_prompt(
            "Hello, I'm John from Cebu",
            "Nice to meet you John!",
            &["Previous turn about weather"],
        );
        assert!(prompt.contains("John"));
        assert!(prompt.contains("Cebu"));
        assert!(prompt.contains("Previous turn"));
        assert!(prompt.contains("JSON"));
    }

    #[test]
    fn stm_prompt_truncates_context() {
        let long_context: Vec<String> = (0..20).map(|i| format!("Turn {} content here", i)).collect();
        let refs: Vec<&str> = long_context.iter().map(|s| s.as_str()).collect();
        let prompt = build_stm_extraction_prompt("msg", "resp", &refs);
        // Should only include last 5 turns max
        assert!(!prompt.contains("Turn 0"));
    }

    #[test]
    fn mtm_prompt_includes_transcript_and_facts() {
        let prompt = build_mtm_extraction_prompt(
            "Full day transcript here",
            &["fact:personal:user:name → John"],
        );
        assert!(prompt.contains("Full day transcript"));
        assert!(prompt.contains("John"));
    }

    #[test]
    fn ltm_prompt_includes_summaries() {
        let prompt = build_ltm_compression_prompt(
            &["Summary day 1", "Summary day 2"],
            &["fact:personal:user:name → John"],
        );
        assert!(prompt.contains("Summary day 1"));
        assert!(prompt.contains("Summary day 2"));
    }
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p zeroclaw --lib memory::tiered::prompts::tests -- --nocapture`
Expected: FAIL — module not found

**Step 3: Write minimal implementation**

Write `src/memory/tiered/prompts.rs`:

```rust
//! Prompt templates for fact extraction at each memory tier.

const MAX_PRIOR_TURNS: usize = 5;

/// Build the STM real-time extraction prompt.
///
/// Includes the current user+agent turn and up to 5 prior turns for context.
/// Output: strict JSON array of fact objects.
pub fn build_stm_extraction_prompt(
    user_message: &str,
    agent_response: &str,
    prior_turns: &[&str],
) -> String {
    let context = prior_turns
        .iter()
        .rev()
        .take(MAX_PRIOR_TURNS)
        .rev()
        .enumerate()
        .map(|(i, t)| format!("[Prior turn {}]: {}", i + 1, t))
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        r#"Extract ALL facts, numbers, names, dates, preferences, technical details, relationships, and any other concrete information from this conversation turn.

For each fact, output a JSON object with these exact fields:
- "category": one of "personal", "technical", "preference", "project", "relationship", "location", "temporal", "financial", "organizational"
- "subject": who or what the fact is about (e.g., "user", "main_server", "zeroclaw")
- "attribute": what aspect (e.g., "birthday", "ip_address", "language")
- "value": the concrete value
- "context_narrative": 1-2 sentences explaining when/why/how this was mentioned
- "confidence": "high" if explicitly stated, "medium" if reasonably inferred, "low" if uncertain
- "related_facts": array of related fact keys (can be empty)
- "volatility_class": "stable" (permanent facts like birthday), "semi_stable" (configs, tech stack), "volatile" (current tasks, today's plans)

Return ONLY a JSON array. No markdown, no explanation. If no facts found, return [].

{context}

[User]: {user_message}
[Agent]: {agent_response}"#,
        context = if context.is_empty() { String::new() } else { format!("\n{}\n", context) },
        user_message = user_message,
        agent_response = agent_response,
    )
}

/// Build the MTM deep extraction prompt for daily compression.
///
/// Uses the full day's transcript + existing facts for cross-referencing.
pub fn build_mtm_extraction_prompt(
    day_transcript: &str,
    existing_facts: &[&str],
) -> String {
    let facts_section = if existing_facts.is_empty() {
        "No existing facts.".to_string()
    } else {
        existing_facts.join("\n")
    };

    format!(
        r#"You are a memory extraction agent. Analyze this full day's conversation transcript and extract ALL facts comprehensively.

Your tasks:
1. Extract every fact, number, name, date, preference, technical detail, relationship, and concrete information
2. Cross-reference with existing facts below — flag contradictions and corrections
3. Identify implicit preferences and relationships not explicitly stated
4. For each fact, assess whether it's new, confirms existing, or corrects/supersedes existing

Existing facts from recent days:
{facts_section}

Today's full transcript:
{day_transcript}

Return a JSON array of fact objects. Each object must have:
- "category", "subject", "attribute", "value", "context_narrative"
- "confidence": "high"/"medium"/"low"
- "related_facts": array of related fact keys
- "volatility_class": "stable"/"semi_stable"/"volatile"
- "is_correction": true if this corrects an existing fact, false otherwise
- "corrects_key": the fact key being corrected (if is_correction=true)

Return ONLY a JSON array. No markdown, no explanation."#,
        facts_section = facts_section,
        day_transcript = day_transcript,
    )
}

/// Build the LTM compression prompt for archival consolidation.
pub fn build_ltm_compression_prompt(
    mtm_summaries: &[&str],
    existing_facts: &[&str],
) -> String {
    let summaries = mtm_summaries
        .iter()
        .enumerate()
        .map(|(i, s)| format!("[Day {}]: {}", i + 1, s))
        .collect::<Vec<_>>()
        .join("\n\n");

    let facts_section = if existing_facts.is_empty() {
        "No existing facts.".to_string()
    } else {
        existing_facts.join("\n")
    };

    format!(
        r#"You are a long-term memory consolidation agent. Merge these daily summaries into a concise archival entry while preserving ALL important facts.

Tasks:
1. Merge overlapping information across days
2. Preserve all durable facts (stable and semi-stable)
3. Discard volatile facts that are no longer relevant
4. Produce a compressed narrative summary
5. Return updated fact list

Daily summaries:
{summaries}

Current fact inventory:
{facts_section}

Return a JSON object with:
- "summary": compressed narrative (string)
- "facts": array of fact objects to keep (same schema as input)
- "expired_keys": array of fact keys that should be marked stale

Return ONLY JSON. No markdown, no explanation."#,
        summaries = summaries,
        facts_section = facts_section,
    )
}
```

Add `pub mod prompts;` to `src/memory/tiered/mod.rs`.

**Step 4: Run test to verify it passes**

Run: `cargo test -p zeroclaw --lib memory::tiered::prompts::tests -- --nocapture`
Expected: ALL PASS (4 tests)

**Step 5: Commit**

```bash
git add src/memory/tiered/prompts.rs src/memory/tiered/mod.rs
git commit -m "feat(memory/tiered): extraction prompt templates for STM/MTM/LTM agents"
```

---

### Task 4: STM extraction queue (non-blocking pipeline)

**Files:**
- Modify: `src/memory/tiered/mod.rs`

**Step 1: Write the failing test**

Add to the test module in `src/memory/tiered/mod.rs`:

```rust
#[tokio::test]
async fn store_enqueues_extraction_when_extractor_set() {
    let stm = make_shared(InMemoryBackend::new());
    let mtm = make_shared(InMemoryBackend::new());
    let ltm = make_shared(InMemoryBackend::new());
    let (cmd_tx, _cmd_rx) = mpsc::channel(16);
    let (extraction_tx, mut extraction_rx) = mpsc::channel::<ExtractionRequest>(16);

    let tm = TieredMemory {
        stm: stm.clone(),
        mtm,
        ltm,
        cfg: Arc::new(TierConfig::default()),
        cmd_tx,
        extraction_tx: Some(extraction_tx),
    };

    tm.store("key1", "Hello I am John from Cebu", MemoryCategory::Conversation, None)
        .await
        .unwrap();

    // Should have enqueued an extraction request
    let req = extraction_rx.try_recv().unwrap();
    assert!(req.content.contains("John"));
    assert_eq!(req.role, "user");
}

#[tokio::test]
async fn store_works_without_extractor() {
    let stm = make_shared(InMemoryBackend::new());
    let mtm = make_shared(InMemoryBackend::new());
    let ltm = make_shared(InMemoryBackend::new());
    let (cmd_tx, _cmd_rx) = mpsc::channel(16);

    let tm = TieredMemory {
        stm: stm.clone(),
        mtm,
        ltm,
        cfg: Arc::new(TierConfig::default()),
        cmd_tx,
        extraction_tx: None,
    };

    tm.store("key1", "Hello", MemoryCategory::Conversation, None)
        .await
        .unwrap();

    let entries = stm.lock().await.list(None, None).await.unwrap();
    assert_eq!(entries.len(), 1);
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p zeroclaw --lib memory::tiered::tests::store_enqueues_extraction -- --nocapture`
Expected: FAIL — `ExtractionRequest` and `extraction_tx` not found

**Step 3: Write minimal implementation**

Add to `src/memory/tiered/mod.rs`:

```rust
/// A request to extract facts from a conversation turn.
#[derive(Debug, Clone)]
pub struct ExtractionRequest {
    pub content: String,
    pub role: String,       // "user" or "agent"
    pub key: String,
    pub session_id: Option<String>,
    pub timestamp_unix_ms: i64,
}
```

Add `extraction_tx: Option<mpsc::Sender<ExtractionRequest>>` to the `TieredMemory` struct.

In the `store()` method, after storing to STM, add:

```rust
// Enqueue for fact extraction (non-blocking, best-effort)
if let Some(ref tx) = self.extraction_tx {
    let role = if key.starts_with("msg:agent:") { "agent" } else { "user" };
    let req = ExtractionRequest {
        content: content.to_string(),
        role: role.to_string(),
        key: key.to_string(),
        session_id: session_id.map(|s| s.to_string()),
        timestamp_unix_ms: Utc::now().timestamp_millis(),
    };
    let _ = tx.try_send(req); // Non-blocking, drop if queue full
}
```

Update `create_tiered_memory_from_backends` in `src/memory/mod.rs` to accept and pass through `extraction_tx`.

**Step 4: Run test to verify it passes**

Run: `cargo test -p zeroclaw --lib memory::tiered::tests -- --nocapture`
Expected: ALL PASS

**Step 5: Commit**

```bash
git add src/memory/tiered/mod.rs src/memory/mod.rs
git commit -m "feat(memory/tiered): non-blocking STM extraction queue via bounded mpsc"
```

---

### Task 5: STM extraction worker

**Files:**
- Create: `src/memory/tiered/extraction_worker.rs`
- Modify: `src/memory/tiered/mod.rs` (add `pub mod extraction_worker;`)

**Step 1: Write the failing test**

Create `src/memory/tiered/extraction_worker.rs` with tests:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::tiered::extractor::{MockFactExtractor, FactEntryDraft};
    use crate::memory::tiered::ExtractionRequest;

    fn make_shared_mem() -> SharedMemory {
        Arc::new(tokio::sync::Mutex::new(
            Box::new(crate::memory::tiered::tests::InMemoryBackend::new()) as Box<dyn Memory + Send>
        ))
    }

    #[tokio::test]
    async fn worker_processes_extraction_request() {
        let stm = make_shared_mem();
        let (tx, rx) = mpsc::channel::<ExtractionRequest>(16);
        let extractor = Arc::new(MockFactExtractor::new(vec![
            FactEntryDraft {
                category: "personal".into(),
                subject: "user".into(),
                attribute: "name".into(),
                value: "John".into(),
                context_narrative: "User said their name".into(),
                confidence: "high".into(),
                related_facts: vec![],
                volatility_class: "stable".into(),
            },
        ]));

        // Send a request
        tx.send(ExtractionRequest {
            content: "Hello, I'm John".into(),
            role: "user".into(),
            key: "user_msg_123".into(),
            session_id: None,
            timestamp_unix_ms: 1708600000000,
        }).await.unwrap();
        drop(tx); // Close channel so worker exits

        // Run worker
        run_extraction_worker(rx, stm.clone(), extractor, "conv-1".into()).await;

        // Check that a fact was stored in STM
        let entries = stm.lock().await.list(
            Some(&MemoryCategory::Core),
            None,
        ).await.unwrap();
        assert!(!entries.is_empty());
        assert!(entries[0].key.starts_with("fact:"));
    }

    #[tokio::test]
    async fn worker_applies_poisoning_guard_for_agent_messages() {
        let stm = make_shared_mem();
        let (tx, rx) = mpsc::channel::<ExtractionRequest>(16);
        let extractor = Arc::new(MockFactExtractor::new(vec![
            FactEntryDraft {
                category: "technical".into(),
                subject: "server".into(),
                attribute: "status".into(),
                value: "running".into(),
                context_narrative: "Agent observed server is running".into(),
                confidence: "high".into(),
                related_facts: vec![],
                volatility_class: "volatile".into(),
            },
        ]));

        tx.send(ExtractionRequest {
            content: "The server appears to be running".into(),
            role: "agent".into(),
            key: "msg:agent:conv-1:5".into(),
            session_id: None,
            timestamp_unix_ms: 1708600000000,
        }).await.unwrap();
        drop(tx);

        run_extraction_worker(rx, stm.clone(), extractor, "conv-1".into()).await;

        let entries = stm.lock().await.list(Some(&MemoryCategory::Core), None).await.unwrap();
        assert!(!entries.is_empty());
        // The stored fact JSON should have confidence=Medium (capped from High)
        let fact: crate::memory::tiered::facts::FactEntry =
            serde_json::from_str(&entries[0].content).unwrap();
        assert_eq!(fact.confidence, crate::memory::tiered::facts::FactConfidence::Medium);
    }
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p zeroclaw --lib memory::tiered::extraction_worker::tests -- --nocapture`
Expected: FAIL — module not found

**Step 3: Write minimal implementation**

Write `src/memory/tiered/extraction_worker.rs`:

```rust
//! Background worker that processes the STM extraction queue.
//!
//! Reads [`ExtractionRequest`]s from the bounded channel, calls the
//! [`FactExtractor`], converts drafts to [`FactEntry`]s, applies the
//! poisoning guard, and stores results as [`MemoryCategory::Core`] entries.

use std::sync::Arc;

use chrono::Utc;
use tokio::sync::mpsc;
use tracing::{debug, warn};

use crate::memory::tiered::extractor::FactExtractor;
use crate::memory::tiered::facts::{
    build_fact_key, FactConfidence, FactEntry, FactStatus, SourceRole, SourceTurnRef,
    VolatilityClass,
};
use crate::memory::tiered::prompts::build_stm_extraction_prompt;
use crate::memory::tiered::ExtractionRequest;
use crate::memory::tiered::SharedMemory;
use crate::memory::traits::{Memory, MemoryCategory};

/// Run the STM extraction worker loop until the channel is closed.
pub async fn run_extraction_worker(
    mut rx: mpsc::Receiver<ExtractionRequest>,
    stm: SharedMemory,
    extractor: Arc<dyn FactExtractor>,
    conversation_id: String,
) {
    while let Some(req) = rx.recv().await {
        if let Err(e) = process_extraction(&req, &stm, &extractor, &conversation_id).await {
            warn!("STM fact extraction failed (non-fatal): {:#}", e);
        }
    }
    debug!("STM extraction worker shutting down (channel closed)");
}

async fn process_extraction(
    req: &ExtractionRequest,
    stm: &SharedMemory,
    extractor: &Arc<dyn FactExtractor>,
    conversation_id: &str,
) -> anyhow::Result<()> {
    // Build prompt — for now, single turn without prior context
    // (prior context requires accumulating recent turns, added in a later task)
    let prompt = build_stm_extraction_prompt(&req.content, "", &[]);

    let drafts = extractor.extract(&prompt).await?;

    let now_ms = Utc::now().timestamp_millis();
    let source_role = if req.role == "agent" {
        SourceRole::Agent
    } else {
        SourceRole::User
    };

    let stm_guard = stm.lock().await;

    for draft in drafts {
        let fact_key = build_fact_key(&draft.category, &draft.subject, &draft.attribute);
        let confidence = match draft.confidence.to_lowercase().as_str() {
            "high" => FactConfidence::High,
            "low" => FactConfidence::Low,
            _ => FactConfidence::Medium,
        };
        let volatility = match draft.volatility_class.to_lowercase().as_str() {
            "stable" => VolatilityClass::Stable,
            "volatile" => VolatilityClass::Volatile,
            _ => VolatilityClass::SemiStable,
        };

        let mut entry = FactEntry {
            fact_id: uuid::Uuid::new_v4().to_string(),
            fact_key: fact_key.clone(),
            category: draft.category,
            subject: draft.subject,
            attribute: draft.attribute,
            value: draft.value,
            context_narrative: draft.context_narrative,
            source_turn: SourceTurnRef {
                conversation_id: conversation_id.to_string(),
                turn_index: 0, // enriched later if available
                message_id: Some(req.key.clone()),
                role: source_role.clone(),
                timestamp_unix_ms: req.timestamp_unix_ms,
            },
            confidence,
            related_facts: draft.related_facts,
            extracted_by_tier: "stm".to_string(),
            extracted_at_unix_ms: now_ms,
            source_role: source_role.clone(),
            status: FactStatus::Active,
            revision: 1,
            supersedes_fact_id: None,
            tags: vec![],
            volatility_class: volatility,
            ttl_days: None,
            expires_at_unix_ms: None,
            last_verified_unix_ms: Some(now_ms),
        };

        // Poisoning guard: agent facts capped at Medium
        entry.apply_poisoning_guard();

        // Store as Core entry — key-based upsert handles collisions
        let searchable_text = entry.to_searchable_text();
        let content = serde_json::to_string(&entry)?;

        // Use searchable text as content for embedding/FTS, store full JSON
        // The key is the fact_key, enabling natural upsert on correction
        stm_guard
            .store(&fact_key, &content, MemoryCategory::Core, None)
            .await?;
    }

    Ok(())
}
```

Add `pub mod extraction_worker;` to `src/memory/tiered/mod.rs`.

**Step 4: Run test to verify it passes**

Run: `cargo test -p zeroclaw --lib memory::tiered::extraction_worker::tests -- --nocapture`
Expected: ALL PASS (2 tests)

**Step 5: Commit**

```bash
git add src/memory/tiered/extraction_worker.rs src/memory/tiered/mod.rs
git commit -m "feat(memory/tiered): STM extraction worker — processes queue, stores facts, applies poisoning guard"
```

---

### Task 6: Agent response auto-save in loop_.rs

**Files:**
- Modify: `src/agent/loop_.rs`

**Step 1: Identify insertion points**

The auto-save currently happens at lines 2893-2898 (daemon mode) and 3017-3022 (interactive mode) for user messages only. Agent responses need to be saved after the LLM returns a response.

**Step 2: Add agent response auto-save**

After each location where the agent response is received and before it's returned to the user, add:

```rust
// Auto-save agent response to memory
if config.memory.auto_save {
    let agent_key = autosave_memory_key("msg:agent");
    let _ = mem
        .store(&agent_key, &agent_response, MemoryCategory::Conversation, None)
        .await;
}
```

The exact insertion points depend on where `agent_response` (or equivalent variable holding the LLM output) is available in the loop. Search for where the response text is assembled after the LLM call.

**Step 3: Run full test suite**

Run: `cargo test -p zeroclaw 2>&1 | tail -5`
Expected: No new failures

**Step 4: Commit**

```bash
git add src/agent/loop_.rs
git commit -m "feat(agent): auto-save agent responses with msg:agent key prefix"
```

---

### Task 7: Fact-first recall in merge.rs

**Files:**
- Modify: `src/memory/tiered/merge.rs`

**Step 1: Write the failing test**

Add to the test module in `src/memory/tiered/merge.rs`:

```rust
#[test]
fn fact_entries_get_confidence_boost() {
    let items = vec![
        TieredRecallItem {
            entry_id: "raw-1".into(),
            origin_id: "raw-1".into(),
            tier: MemoryTier::Stm,
            base_score: 0.8,
            recency_score: 0.5,
            has_cross_tier_link: false,
            final_score: 0.0,
            is_fact: false,
            fact_confidence: None,
        },
        TieredRecallItem {
            entry_id: "fact-1".into(),
            origin_id: "fact-1".into(),
            tier: MemoryTier::Stm,
            base_score: 0.7,
            recency_score: 0.5,
            has_cross_tier_link: false,
            final_score: 0.0,
            is_fact: true,
            fact_confidence: Some(FactConfidenceLevel::High),
        },
    ];
    let weights = TierWeights::default();
    let results = merge_and_rank(items, &weights, 0.0, 10);
    // The fact entry should score higher due to confidence boost
    assert_eq!(results[0].entry_id, "fact-1");
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p zeroclaw --lib memory::tiered::merge::tests::fact_entries_get_confidence_boost -- --nocapture`
Expected: FAIL — `is_fact` field not found

**Step 3: Write minimal implementation**

Add to `TieredRecallItem`:

```rust
pub is_fact: bool,
pub fact_confidence: Option<FactConfidenceLevel>,
```

Add enum:

```rust
#[derive(Debug, Clone, PartialEq)]
pub enum FactConfidenceLevel {
    Low,
    Medium,
    High,
}
```

Update the scoring formula in `merge_and_rank()`:

```rust
let fact_boost = if item.is_fact {
    match &item.fact_confidence {
        Some(FactConfidenceLevel::High) => 0.20,
        Some(FactConfidenceLevel::Medium) => 0.10,
        Some(FactConfidenceLevel::Low) => 0.05,
        None => 0.0,
    }
} else {
    0.0
};

item.final_score = tier_weight * item.base_score
    + 0.25 * item.recency_score
    + link_boost
    + fact_boost;
```

Update all existing test item constructions to include `is_fact: false, fact_confidence: None`.

**Step 4: Run test to verify it passes**

Run: `cargo test -p zeroclaw --lib memory::tiered::merge::tests -- --nocapture`
Expected: ALL PASS

**Step 5: Commit**

```bash
git add src/memory/tiered/merge.rs
git commit -m "feat(memory/tiered): fact-first recall with confidence-weighted boosting in merge_and_rank"
```

---

### Task 8: Facts section in TieredMemoryLoader

**Files:**
- Modify: `src/memory/tiered/loader.rs`

**Step 1: Write the failing test**

Add to the test module in `src/memory/tiered/loader.rs`:

```rust
#[tokio::test]
async fn loads_facts_section_from_core_entries() {
    // Store a fact as a Core entry in STM
    let stm = make_shared_mem();
    let fact_json = r#"{"fact_id":"f1","fact_key":"fact:personal:user:name","category":"personal","subject":"user","attribute":"name","value":"John","context_narrative":"User said their name","confidence":"High","related_facts":[],"extracted_by_tier":"stm","extracted_at_unix_ms":0,"source_role":"User","status":"Active","revision":1,"tags":[],"volatility_class":"Stable","source_turn":{"conversation_id":"c","turn_index":1,"message_id":null,"role":"User","timestamp_unix_ms":0}}"#;
    stm.lock().await
        .store("fact:personal:user:name", fact_json, MemoryCategory::Core, None)
        .await.unwrap();

    let mtm = make_shared_mem();
    let loader = TieredMemoryLoader::new(stm, mtm, Arc::new(TierConfig::default()));
    let ctx = loader.load_context(&NoneMemory, "").await.unwrap();

    assert!(ctx.contains("Known Facts"));
    assert!(ctx.contains("name"));
    assert!(ctx.contains("John"));
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p zeroclaw --lib memory::tiered::loader::tests::loads_facts_section -- --nocapture`
Expected: FAIL — no "Known Facts" section in output

**Step 3: Write minimal implementation**

In `load_context()`, after splitting STM entries into raw vs index, add a third split for facts:

```rust
// Split into raw, index, and fact entries
let mut raw_entries = Vec::new();
let mut index_entries = Vec::new();
let mut fact_entries = Vec::new();

for entry in &stm_entries {
    if entry.category == MemoryCategory::Custom("stm_index".into()) {
        // index entry handling (existing)
    } else if entry.category == MemoryCategory::Core && entry.key.starts_with("fact:") {
        // Parse fact entry
        if let Ok(fact) = serde_json::from_str::<crate::memory::tiered::facts::FactEntry>(&entry.content) {
            if fact.status == crate::memory::tiered::facts::FactStatus::Active {
                fact_entries.push(fact);
            }
        }
    } else {
        raw_entries.push(entry);
    }
}
```

Add a new section to the output format between Active Memory and Memory Index:

```rust
// Section: Known Facts
if !fact_entries.is_empty() {
    output.push_str("\n## Known Facts\n");
    for fact in &fact_entries {
        let confidence_tag = match fact.confidence {
            FactConfidence::High => "high",
            FactConfidence::Medium => "medium",
            FactConfidence::Low => "low",
        };
        output.push_str(&format!(
            "- [{}] {} {}: {} ({})\n",
            confidence_tag, fact.subject, fact.attribute, fact.value, fact.context_narrative
        ));
    }
}
```

**Step 4: Run test to verify it passes**

Run: `cargo test -p zeroclaw --lib memory::tiered::loader::tests -- --nocapture`
Expected: ALL PASS

**Step 5: Commit**

```bash
git add src/memory/tiered/loader.rs
git commit -m "feat(memory/tiered): inject Known Facts section into prompt via TieredMemoryLoader"
```

---

### Task 9: Wire OpenRouter agents into TierManager

**Files:**
- Modify: `src/memory/tiered/manager.rs`
- Modify: `src/memory/tiered/summarization.rs`

**Step 1: Add FactExtractor to TierManager**

Add `fact_extractor: Option<Arc<dyn FactExtractor>>` field to `TierManager`.

**Step 2: Update compress_day() to run deep fact extraction**

After the existing summarization step in `compress_day()`, add the deep extraction pass:

```rust
// Deep fact extraction via MTM agent (if available)
if let Some(ref extractor) = self.fact_extractor {
    let transcript = stm_entries.iter()
        .map(|e| format!("[{}] {}: {}", e.timestamp, e.key, e.content))
        .collect::<Vec<_>>()
        .join("\n");

    // Gather existing facts for cross-referencing
    let existing_facts = self.stm.lock().await
        .list(Some(&MemoryCategory::Core), None).await
        .unwrap_or_default();
    let fact_refs: Vec<&str> = existing_facts.iter()
        .filter(|e| e.key.starts_with("fact:"))
        .map(|e| e.content.as_str())
        .collect();

    let prompt = build_mtm_extraction_prompt(&transcript, &fact_refs);
    match extractor.extract(&prompt).await {
        Ok(drafts) => {
            // Store extracted facts (similar to extraction_worker logic)
            // ... convert drafts to FactEntry, apply poisoning guard, store
        }
        Err(e) => {
            warn!("MTM deep fact extraction failed (non-fatal): {:#}", e);
        }
    }
}
```

**Step 3: Run full test suite**

Run: `cargo test -p zeroclaw 2>&1 | tail -5`
Expected: No new failures

**Step 4: Commit**

```bash
git add src/memory/tiered/manager.rs src/memory/tiered/summarization.rs
git commit -m "feat(memory/tiered): wire FactExtractor into TierManager for deep MTM extraction"
```

---

### Task 10: Update factory function and exports

**Files:**
- Modify: `src/memory/mod.rs`
- Modify: `src/memory/tiered/mod.rs`

**Step 1: Update exports**

In `src/memory/tiered/mod.rs`, add to the public exports:

```rust
pub use facts::{FactEntry, FactConfidence, FactStatus, SourceRole, VolatilityClass};
pub use extractor::{FactExtractor, MockFactExtractor, OpenRouterFactExtractor, FactEntryDraft};
pub use types::{MemoryAgentConfig, MemoryAgentsConfig};
```

In `src/memory/mod.rs`, update the re-exports and factory function to accept `extraction_tx` parameter.

**Step 2: Run full test suite**

Run: `cargo test -p zeroclaw 2>&1 | tail -5`
Expected: No new failures

**Step 3: Commit**

```bash
git add src/memory/mod.rs src/memory/tiered/mod.rs
git commit -m "feat(memory): export fact types and update factory for extraction queue"
```

---

### Task 11: Integration test — full fact extraction pipeline

**Files:**
- Create or modify: `tests/tiered_memory_integration.rs`

**Step 1: Write integration test**

```rust
#[tokio::test]
async fn fact_extraction_stores_structured_facts() {
    // Setup: TieredMemory with MockFactExtractor returning known facts
    // Action: store a user message + agent response
    // Assert: fact entries appear in STM as Core entries
    // Assert: facts have correct keys, values, confidence
    // Assert: agent-derived facts have Medium confidence (poisoning guard)
}

#[tokio::test]
async fn fact_first_recall_boosts_facts_over_raw_entries() {
    // Setup: store raw messages AND fact entries
    // Action: recall with a query matching the fact
    // Assert: fact entry appears before raw entry in results
}

#[tokio::test]
async fn loader_includes_facts_section() {
    // Setup: store fact entries as Core in STM
    // Action: load_context()
    // Assert: output contains "Known Facts" section with fact values
}
```

**Step 2: Run integration tests**

Run: `cargo test --test tiered_memory_integration -- --nocapture`
Expected: ALL PASS

**Step 3: Commit**

```bash
git add tests/tiered_memory_integration.rs
git commit -m "test(memory/tiered): integration tests for fact extraction pipeline"
```

---

### Task 12: Update config/schema.rs with memory agent settings

**Files:**
- Modify: `src/config/schema.rs`

**Step 1: Add memory agent config fields**

Add to `MemoryConfig` struct:

```rust
pub memory_agent_openrouter_api_key: Option<String>,
pub memory_agent_openrouter_base_url: String,     // default "https://openrouter.ai/api/v1"
pub memory_agent_model: String,                     // default "google/gemini-3-flash-preview"
pub memory_agent_stm_timeout_secs: u64,            // default 10
pub memory_agent_mtm_timeout_secs: u64,            // default 60
pub memory_agent_ltm_timeout_secs: u64,            // default 60
```

**Step 2: Run full test suite**

Run: `cargo test -p zeroclaw 2>&1 | tail -5`
Expected: No new failures

**Step 3: Commit**

```bash
git add src/config/schema.rs
git commit -m "feat(config): add memory agent OpenRouter settings to MemoryConfig"
```
