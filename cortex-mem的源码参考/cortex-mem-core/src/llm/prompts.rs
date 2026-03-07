/// Prompt templates for various LLM tasks
pub struct Prompts;

impl Prompts {
    /// Prompt for generating L0 abstract
    ///
    /// ~100 tokens for quick relevance checking and filtering
    pub fn abstract_generation(content: &str) -> String {
        format!(
            r#"Generate a concise abstract (~100 tokens maximum) for the following content.

Requirements:
- Stay within ~100 tokens limit
- Cover MULTIPLE key aspects when content is rich (who, what, key topics, important outcomes)
- Prioritize information breadth over depth - mention more topics rather than elaborating on one
- Use compact phrasing: "discussed X, Y, and Z" instead of long explanations
- For multi-topic content: list key themes briefly rather than focusing on just one
- Use clear, direct language
- Avoid filler words and unnecessary details
- **CRITICAL: Use the SAME LANGUAGE as the input content**
  - If content is in Chinese, write abstract in Chinese
  - If content is in English, write abstract in English
  - If content is in other languages, use that language
  - Preserve the original linguistic and cultural context

Content:
{}

Abstract (max 100 tokens, in the same language as the content):"#,
            content
        )
    }

    /// Prompt for generating L1 overview
    ///
    /// ~2K tokens, structured overview
    /// for decision-making and planning
    pub fn overview_generation(content: &str) -> String {
        format!(
            r#"Generate a structured overview (~500-2000 tokens) of the following content.

Structure your response as markdown with these sections:

## Summary
2-3 paragraph overview of the core content and its significance

## Core Topics
List 3-5 main themes or topics (bullet points)

## Key Points
List 5-10 important takeaways or insights (numbered or bullets)

## Entities
Important people, organizations, technologies, or concepts mentioned

## Context
Any relevant background, timeframe, or situational information

Requirements:
- Use clear markdown formatting
- Be comprehensive but concise
- Focus on information useful for understanding and decision-making
- Aim for ~500-2000 tokens total
- **CRITICAL: Use the SAME LANGUAGE as the input content**
  - If content is in Chinese, write overview in Chinese
  - If content is in English, write overview in English
  - If content is in other languages, use that language
  - Preserve cultural references and linguistic nuances

Content:
{}

Structured Overview (in the same language as the content):"#,
            content
        )
    }

    /// Prompt for memory extraction from conversation
    pub fn memory_extraction(conversation: &str) -> String {
        format!(
            r#"Analyze the following conversation and extract:

1. **Facts**: Factual information that was shared or discovered
2. **Decisions**: Decisions that were made during the conversation
3. **Action Items**: Tasks or next steps that were identified
4. **User Preferences**: Any preferences, habits, or patterns expressed by the user
5. **Agent Learnings**: Insights or lessons learned that could help in future interactions

Format your response as JSON with the following structure:
{{
  "facts": [{{ "content": "...", "confidence": 0.9 }}],
  "decisions": [{{ "description": "...", "rationale": "..." }}],
  "action_items": [{{ "description": "...", "priority": "high|medium|low" }}],
  "user_preferences": [{{ "category": "...", "content": "..." }}],
  "agent_learnings": [{{ "task_type": "...", "learned_approach": "...", "success_rate": 0.8 }}]
}}

Conversation:
{}

Extracted Memories (JSON):"#,
            conversation
        )
    }

    /// Prompt for intent analysis in retrieval
    pub fn intent_analysis(query: &str) -> String {
        format!(
            r#"Analyze the following query and extract:

1. **Keywords**: Important keywords for search (2-5 words)
2. **Entities**: Named entities mentioned (people, places, technologies)
3. **Time Range**: Any time-related constraints (if mentioned)
4. **Query Type**: The type of query (factual, procedural, conceptual, etc.)

Format as JSON:
{{
  "keywords": ["...", "..."],
  "entities": ["...", "..."],
  "time_range": {{ "start": "...", "end": "..." }},
  "query_type": "..."
}}

Query: {}

Intent Analysis (JSON):"#,
            query
        )
    }
}
