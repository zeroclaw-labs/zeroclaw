use super::types::{
    ExtractedDecision, ExtractedEntity, ExtractedFact, ExtractedMemories, MemoryImportance,
};
use crate::{CortexFilesystem, FilesystemOperations, LLMClient, Message, MessageRole, Result};
use std::sync::Arc;

/// Extraction configuration
#[derive(Debug, Clone)]
pub struct ExtractionConfig {
    pub min_confidence: f32,
    pub extract_facts: bool,
    pub extract_decisions: bool,
    pub extract_entities: bool,
    pub max_messages_per_batch: usize,
}

impl Default for ExtractionConfig {
    fn default() -> Self {
        Self {
            min_confidence: 0.6,
            extract_facts: true,
            extract_decisions: true,
            extract_entities: true,
            max_messages_per_batch: 50,
        }
    }
}

/// Memory extractor for analyzing conversations
pub struct MemoryExtractor {
    filesystem: Arc<CortexFilesystem>,
    llm_client: Arc<dyn LLMClient>,
    config: ExtractionConfig,
}

impl MemoryExtractor {
    /// Create a new memory extractor
    pub fn new(
        filesystem: Arc<CortexFilesystem>,
        llm_client: Arc<dyn LLMClient>,
        config: ExtractionConfig,
    ) -> Self {
        Self {
            filesystem,
            llm_client,
            config,
        }
    }

    /// Extract memories from a list of messages
    pub async fn extract_from_messages(
        &self,
        thread_id: &str,
        messages: &[Message],
    ) -> Result<ExtractedMemories> {
        let mut extracted = ExtractedMemories::new(thread_id);

        // Build conversation context
        let conversation = self.build_conversation_context(messages);

        // Extract facts
        if self.config.extract_facts {
            let facts = self.extract_facts(&conversation, messages).await?;
            for fact in facts {
                extracted.add_fact(fact);
            }
        }

        // Extract decisions
        if self.config.extract_decisions {
            let decisions = self.extract_decisions(&conversation, messages).await?;
            for decision in decisions {
                extracted.add_decision(decision);
            }
        }

        // Extract entities
        if self.config.extract_entities {
            let entities = self.extract_entities(&conversation, messages).await?;
            for entity in entities {
                extracted.add_entity(entity);
            }
        }

        Ok(extracted)
    }

    /// Extract memories from a thread
    pub async fn extract_from_thread(&self, thread_id: &str) -> Result<ExtractedMemories> {
        // List all messages in the thread
        let timeline_uri = format!("cortex://session/{}/timeline", thread_id);

        // Recursively collect all message files
        let mut message_contents = Vec::new();
        if self.filesystem.exists(&timeline_uri).await? {
            message_contents = self.collect_messages_recursive(&timeline_uri).await?;
        }

        if message_contents.is_empty() {
            // Return empty extraction if no messages found
            return Ok(ExtractedMemories::new(thread_id));
        }

        // Build messages from markdown content
        let mut messages = Vec::new();
        for (_uri, content) in &message_contents {
            // Parse markdown to extract message info
            if let Some(message) = self.parse_message_markdown(content) {
                messages.push(message);
            }
        }

        self.extract_from_messages(thread_id, &messages).await
    }

    /// Recursively collect all message files from timeline
    fn collect_messages_recursive<'a>(
        &'a self,
        uri: &'a str,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Vec<(String, String)>>> + Send + 'a>,
    > {
        Box::pin(async move {
            let mut result = Vec::new();

            let entries = self.filesystem.list(uri).await?;
            for entry in entries {
                if entry.is_directory && !entry.name.starts_with('.') {
                    // Recursively explore subdirectories
                    let sub_messages = self.collect_messages_recursive(&entry.uri).await?;
                    result.extend(sub_messages);
                } else if entry.name.ends_with(".md") && !entry.name.starts_with('.') {
                    // Read message file
                    if let Ok(content) = self.filesystem.read(&entry.uri).await {
                        result.push((entry.uri.clone(), content));
                    }
                }
            }

            Ok(result)
        })
    }

    /// Parse markdown message content to extract Message
    fn parse_message_markdown(&self, content: &str) -> Option<Message> {
        // Simple markdown parsing - look for role and content
        let mut role = MessageRole::User;
        let mut message_content = String::new();
        let _id = uuid::Uuid::new_v4();

        for line in content.lines() {
            if line.starts_with("# 👤 User") {
                role = MessageRole::User;
            } else if line.starts_with("# 🤖 Assistant") {
                role = MessageRole::Assistant;
            } else if line.starts_with("**ID**: `") {
                // Extract ID (currently not used, but parsing for future use)
                #[allow(unused_variables)]
                if let Some(id_str) = line
                    .strip_prefix("**ID**: `")
                    .and_then(|s| s.strip_suffix("`"))
                {
                    // ID parsing logic here if needed in future
                }
            } else if line.starts_with("## Content") {
                // Content starts after this line
                continue;
            } else if !line.is_empty() && !line.starts_with("**") && !line.starts_with("#") {
                // Actual content line
                if !message_content.is_empty() {
                    message_content.push('\n');
                }
                message_content.push_str(line);
            }
        }

        if message_content.is_empty() {
            return None;
        }

        Some(Message::new(role, message_content))
    }

    /// Build conversation context from messages
    fn build_conversation_context(&self, messages: &[Message]) -> String {
        let mut context = String::new();

        for (i, msg) in messages.iter().enumerate() {
            context.push_str(&format!("[{}] {:?}: {}\n", i + 1, msg.role, msg.content));
        }

        context
    }

    /// Extract facts using LLM
    async fn extract_facts(
        &self,
        conversation: &str,
        messages: &[Message],
    ) -> Result<Vec<ExtractedFact>> {
        let prompt = format!(
            r#"Analyze the following conversation and extract factual statements.

For each fact, provide:
- content: The factual statement
- subject: The main subject of the fact (optional)
- confidence: Confidence level (0.0-1.0)
- importance: low, medium, high, or critical

Return a JSON array of facts.

Conversation:
{}

Return JSON only, no additional text."#,
            conversation
        );

        // Call LLM (using placeholder implementation)
        let response = self.llm_client.extract_memories(&prompt).await?;

        // Parse response into facts - convert from LLM client's Fact type
        let mut facts = Vec::new();
        for llm_fact in &response.facts {
            let fact = ExtractedFact::new(&llm_fact.content)
                .with_confidence(llm_fact.confidence)
                .with_importance(MemoryImportance::Medium);

            // Add source URIs
            let mut fact_with_sources = fact;
            for msg in messages {
                fact_with_sources
                    .source_uris
                    .push(format!("cortex://session/temp/{}", msg.id));
            }

            if fact_with_sources.confidence >= self.config.min_confidence {
                facts.push(fact_with_sources);
            }
        }

        Ok(facts)
    }

    /// Extract decisions using LLM
    async fn extract_decisions(
        &self,
        conversation: &str,
        messages: &[Message],
    ) -> Result<Vec<ExtractedDecision>> {
        let prompt = format!(
            r#"Analyze the following conversation and extract decisions that were made.

For each decision, provide:
- decision: The decision that was made
- context: The context in which it was made
- rationale: The reasoning behind the decision (optional)
- confidence: Confidence level (0.0-1.0)
- importance: low, medium, high, or critical

Return a JSON array of decisions.

Conversation:
{}

Return JSON only, no additional text."#,
            conversation
        );

        let response = self.llm_client.extract_memories(&prompt).await?;

        // Convert from LLM client's Decision type
        let mut decisions = Vec::new();
        for llm_decision in &response.decisions {
            let decision = ExtractedDecision::new(
                &llm_decision.decision,
                llm_decision
                    .rationale
                    .as_ref()
                    .map(|s| s.as_str())
                    .unwrap_or("No rationale"),
            )
            .with_confidence(llm_decision.confidence)
            .with_importance(MemoryImportance::Medium);

            let mut decision_with_sources = decision;
            for msg in messages {
                decision_with_sources
                    .source_uris
                    .push(format!("cortex://session/temp/{}", msg.id));
            }

            if decision_with_sources.confidence >= self.config.min_confidence {
                decisions.push(decision_with_sources);
            }
        }

        Ok(decisions)
    }

    /// Extract entities using LLM
    async fn extract_entities(
        &self,
        conversation: &str,
        _messages: &[Message],
    ) -> Result<Vec<ExtractedEntity>> {
        let prompt = format!(
            r#"Analyze the following conversation and extract entities (people, organizations, products, etc.).

For each entity, provide:
- name: The entity name
- type: The entity type (person, organization, product, etc.)
- description: Brief description (optional)
- confidence: Confidence level (0.0-1.0)

Return a JSON array of entities.

Conversation:
{}

Return JSON only, no additional text."#,
            conversation
        );

        let response = self.llm_client.extract_memories(&prompt).await?;

        // Convert from LLM client's Entity type
        let mut entities = Vec::new();
        for llm_entity in &response.entities {
            let entity = ExtractedEntity::new(&llm_entity.name, &llm_entity.entity_type)
                .with_description(
                    llm_entity
                        .description
                        .as_ref()
                        .map(|s| s.as_str())
                        .unwrap_or(""),
                )
                .with_confidence(llm_entity.confidence as f64);

            entities.push(entity);
        }

        Ok(entities)
    }

    /// Save extracted memories to filesystem
    pub async fn save_extraction(
        &self,
        thread_id: &str,
        memories: &ExtractedMemories,
    ) -> Result<String> {
        let extraction_uri = format!(
            "cortex://session/{}/extractions/{}.md",
            thread_id,
            memories.extracted_at.format("%Y%m%d_%H%M%S")
        );

        let markdown = memories.to_markdown();
        self.filesystem.write(&extraction_uri, &markdown).await?;

        Ok(extraction_uri)
    }
}
