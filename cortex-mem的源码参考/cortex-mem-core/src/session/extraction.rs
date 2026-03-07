//! Session memory extraction module
//!
//! Implements memory extraction from sessions:
//! - Extract user preferences
//! - Extract entities (people, projects)
//! - Extract events/decisions
//! - Extract agent cases (problem + solution)

use crate::{CortexFilesystem, Error, Result, llm::LLMClient};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Extracted memory from session
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractedMemories {
    /// User preferences extracted
    #[serde(default)]
    pub preferences: Vec<PreferenceMemory>,
    /// Entities mentioned (people, projects)
    #[serde(default)]
    pub entities: Vec<EntityMemory>,
    /// Events/decisions
    #[serde(default)]
    pub events: Vec<EventMemory>,
    /// Agent cases (problem + solution)
    #[serde(default)]
    pub cases: Vec<CaseMemory>,
    /// Personal information (age, occupation, education, etc.)
    #[serde(default)]
    pub personal_info: Vec<PersonalInfoMemory>,
    /// Work history (companies, roles, durations)
    #[serde(default)]
    pub work_history: Vec<WorkHistoryMemory>,
    /// Relationships (family, friends, colleagues)
    #[serde(default)]
    pub relationships: Vec<RelationshipMemory>,
    /// Goals (career goals, personal goals)
    #[serde(default)]
    pub goals: Vec<GoalMemory>,
}

impl Default for ExtractedMemories {
    fn default() -> Self {
        Self {
            preferences: Vec::new(),
            entities: Vec::new(),
            events: Vec::new(),
            cases: Vec::new(),
            personal_info: Vec::new(),
            work_history: Vec::new(),
            relationships: Vec::new(),
            goals: Vec::new(),
        }
    }
}

impl ExtractedMemories {
    /// Check if all memory lists are empty
    pub fn is_empty(&self) -> bool {
        self.preferences.is_empty()
            && self.entities.is_empty()
            && self.events.is_empty()
            && self.cases.is_empty()
            && self.personal_info.is_empty()
            && self.work_history.is_empty()
            && self.relationships.is_empty()
            && self.goals.is_empty()
    }
}

/// User preference memory
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreferenceMemory {
    pub topic: String,
    pub preference: String,
    pub confidence: f32,
}

/// Entity memory (person, project, etc.)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntityMemory {
    pub name: String,
    pub entity_type: String,
    pub description: String,
    pub context: String,
}

/// Event memory (decision, milestone)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventMemory {
    pub title: String,
    pub event_type: String,
    pub summary: String,
    pub timestamp: Option<String>,
}

/// Case memory (problem + solution)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaseMemory {
    pub title: String,
    pub problem: String,
    pub solution: String,
    pub lessons_learned: Vec<String>,
}

/// Personal information memory
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersonalInfoMemory {
    pub category: String, // e.g., "age", "occupation", "education", "location"
    pub content: String,
    pub confidence: f32,
}

/// Work history memory
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkHistoryMemory {
    pub company: String,
    pub role: String,
    pub duration: Option<String>,
    pub description: String,
    pub confidence: f32,
}

/// Relationship memory
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelationshipMemory {
    pub person: String,
    pub relation_type: String, // e.g., "family", "colleague", "friend"
    pub context: String,
    pub confidence: f32,
}

/// Goal memory
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoalMemory {
    pub goal: String,
    pub category: String, // e.g., "career", "personal", "health", "learning"
    pub timeline: Option<String>,
    pub confidence: f32,
}

/// Memory extractor for session commit
pub struct MemoryExtractor {
    llm_client: Arc<dyn LLMClient>,
    #[allow(dead_code)]
    filesystem: Arc<CortexFilesystem>,
    #[allow(dead_code)]
    user_id: String,
    #[allow(dead_code)]
    agent_id: String,
}

impl MemoryExtractor {
    /// Create a new memory extractor
    pub fn new(
        llm_client: Arc<dyn LLMClient>,
        filesystem: Arc<CortexFilesystem>,
        user_id: String,
        agent_id: String,
    ) -> Self {
        Self {
            llm_client,
            filesystem,
            user_id,
            agent_id,
        }
    }

    /// Extract memories from session messages using LLM
    pub async fn extract(&self, messages: &[String]) -> Result<ExtractedMemories> {
        if messages.is_empty() {
            return Ok(ExtractedMemories::default());
        }

        tracing::info!("🧠 开始从 {} 条消息中提取记忆", messages.len());

        let prompt = self.build_extraction_prompt(messages);
        tracing::debug!("📝 记忆提取 prompt 长度: {} 字符", prompt.len());

        let response = self.llm_client.complete(&prompt).await?;

        let memories = self.parse_extraction_response(&response)?;

        tracing::info!(
            "✅ 记忆提取完成: 偏好={}, 实体={}, 事件={}, 案例={}, 个人信息={}, 工作经历={}, 关系={}, 目标={}",
            memories.preferences.len(),
            memories.entities.len(),
            memories.events.len(),
            memories.cases.len(),
            memories.personal_info.len(),
            memories.work_history.len(),
            memories.relationships.len(),
            memories.goals.len()
        );

        Ok(memories)
    }

    /// Build the extraction prompt
    fn build_extraction_prompt(&self, messages: &[String]) -> String {
        let messages_text = messages.join("\n\n---\n\n");

        format!(
            r#"Analyze the following conversation and extract memories in JSON format.

## CRITICAL LANGUAGE RULES

1. **Language Consistency** (MANDATORY):
   - Extract memories in the SAME language as the conversation
   - If conversation is in Chinese (中文) → memories in Chinese
   - If conversation is in English → memories in English
   - If mixed language → use the dominant language (>60% of content)

2. **Preserve Technical Terms** (MANDATORY):
   - Keep technical terminology unchanged in their original language
   - Programming languages: Rust, Python, TypeScript, JavaScript, Go
   - Frameworks: Cortex Memory, Rig, React, Vue
   - Personality types: INTJ, ENTJ, MBTI, DISC
   - Proper nouns: names, companies, projects
   - Acronyms: LLM, AI, ML, API, HTTP, REST

3. **Examples**:
   ✅ CORRECT (Chinese conversation):
   - "Cortex Memory 是基于 Rust 的长期记忆系统"
   - "用户是 INTJ 人格类型，擅长 Python 和 Rust"

   ❌ WRONG (Chinese conversation):
   - "Cortex Memory is based on 铁锈 long-term memory system"
   - "User is an INTJ personality type skilled in 蟒蛇 and 铁锈"

   ✅ CORRECT (English conversation):
   - "User works at 快手 (Kuaishou) as a Rust engineer"
   - "Cortex Memory is a long-term memory system for Agent"

   ❌ WRONG (English conversation):
   - "用户 works at Kuaishou as a Rust 工程师"
   - "Cortex Memory is a 长期记忆 system for Agent"

## Instructions

Extract the following types of memories:

1. **Personal Info** (user's personal information):
   - category: "age", "occupation", "education", "location", "nationality", etc.
   - content: The specific information
   - confidence: 0.0-1.0 confidence level

2. **Work History** (user's work experience):
   - company: Company name
   - role: Job title/role
   - duration: Time period (optional)
   - description: Brief description of role/responsibilities
   - confidence: 0.0-1.0 confidence level

3. **Preferences** (user preferences by topic):
   - topic: The topic/subject area
   - preference: The user's stated preference
   - confidence: 0.0-1.0 confidence level

4. **Relationships** (people user mentions):
   - person: Person's name
   - relation_type: "family", "colleague", "friend", "mentor", etc.
   - context: How they're related/context
   - confidence: 0.0-1.0 confidence level

5. **Goals** (user's goals and aspirations):
   - goal: The specific goal
   - category: "career", "personal", "health", "learning", "financial", etc.
   - timeline: When they want to achieve it (optional)
   - confidence: 0.0-1.0 confidence level

6. **Entities** (people, projects, organizations mentioned):
   - name: Entity name
   - entity_type: "person", "project", "organization", "technology", etc.
   - description: Brief description
   - context: How it was mentioned

7. **Events** (decisions, milestones, important occurrences):
   - title: Event title
   - event_type: "decision", "milestone", "occurrence"
   - summary: Brief summary
   - timestamp: If mentioned

8. **Cases** (problems encountered and solutions found):
   - title: Case title
   - problem: The problem encountered
   - solution: How it was solved
   - lessons_learned: Array of lessons learned

## Response Format

Return ONLY a JSON object with this structure:

{{
  "personal_info": [{{"category": "age", "content": "30岁", "confidence": 0.9}}],
  "work_history": [{{"company": "...", "role": "...", "duration": "...", "description": "...", "confidence": 0.9}}],
  "preferences": [{{"topic": "...", "preference": "...", "confidence": 0.9}}],
  "relationships": [{{"person": "...", "relation_type": "...", "context": "...", "confidence": 0.9}}],
  "goals": [{{"goal": "...", "category": "...", "timeline": "...", "confidence": 0.9}}],
  "entities": [{{"name": "...", "entity_type": "...", "description": "...", "context": "..."}}],
  "events": [{{"title": "...", "event_type": "...", "summary": "...", "timestamp": "..."}}],
  "cases": [{{"title": "...", "problem": "...", "solution": "...", "lessons_learned": ["..."]}}]
}}

Only include memories that are clearly stated in the conversation. Set empty arrays for categories with no data.

## Conversation

{}

## Response

Return ONLY the JSON object. No additional text before or after."#,
            messages_text
        )
    }

    /// Parse the LLM response into ExtractedMemories
    fn parse_extraction_response(&self, response: &str) -> Result<ExtractedMemories> {
        // Try to extract JSON from the response
        let json_str = if response.starts_with('{') {
            response.to_string()
        } else {
            // Try to find JSON block
            response
                .find('{')
                .and_then(|start| response.rfind('}').map(|end| &response[start..=end]))
                .map(|s| s.to_string())
                .unwrap_or_default()
        };

        if json_str.is_empty() {
            return Ok(ExtractedMemories::default());
        }

        serde_json::from_str(&json_str)
            .map_err(|e| Error::Other(format!("Failed to parse extraction response: {}", e)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_extraction_response() {
        let json = r#"{
            "preferences": [{"topic": "language", "preference": "Chinese", "confidence": 0.9}],
            "entities": [{"name": "Alice", "entity_type": "person", "description": "Developer", "context": "Colleague"}],
            "events": [],
            "cases": []
        }"#;

        // Note: This test would need a mock filesystem to work properly
        // For now, we just verify the parsing logic
        let parsed: ExtractedMemories = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.preferences.len(), 1);
        assert_eq!(parsed.preferences[0].topic, "language");
        assert_eq!(parsed.entities.len(), 1);
    }
}
