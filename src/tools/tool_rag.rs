//! Tools RAG — Semantic tool selection via Memory-backed retrieval.
//!
//! Instead of injecting all ~45 tools into every LLM turn, this module:
//! 1. **Registers** LLM-enriched tool descriptions into Memory at startup
//! 2. **Selects** relevant tools per-query via `Memory::recall()` (Tools RAG)
//! 3. **Discovers** additional tools via SubAgent-style LLM reasoning when
//!    the initial selection is insufficient
//!
//! All storage and queries use **English only** for cross-language embedding
//! consistency.

use crate::memory::traits::{Memory, MemoryCategory};
use crate::providers::traits::Provider;
use crate::tools::traits::Tool;
use anyhow::Result;
use sha2::{Digest, Sha256};
use std::collections::{HashSet, VecDeque};
use std::sync::{Arc, Mutex};

/// Memory key prefix for tool registry entries.
const TOOL_KEY_PREFIX: &str = "tool:";

/// Memory category for tool registry.
fn tool_registry_category() -> MemoryCategory {
    MemoryCategory::Custom("tool_registry".into())
}

/// Manages tool registration in Memory and semantic retrieval.
pub struct ToolRagIndex {
    memory: Arc<dyn Memory>,
    /// Tool names that are always included regardless of RAG results.
    core_set: HashSet<String>,
    /// Maximum number of tools to retrieve via RAG.
    top_k: usize,
    /// Minimum similarity score threshold for RAG results.
    threshold: f64,
    /// Whether SubAgent-based tool discovery fallback is enabled.
    discovery_enabled: bool,
    /// Sliding window of tool sets from recent turns.
    recent_tools: Mutex<VecDeque<HashSet<String>>>,
    /// Maximum number of recent turns to cache.
    cache_window: usize,
}

impl ToolRagIndex {
    /// Create a new `ToolRagIndex`.
    pub fn new(
        memory: Arc<dyn Memory>,
        core_set: Vec<String>,
        top_k: usize,
        threshold: f64,
        discovery_enabled: bool,
        cache_window: usize,
    ) -> Self {
        Self {
            memory,
            core_set: core_set.into_iter().collect(),
            top_k,
            threshold,
            discovery_enabled,
            recent_tools: Mutex::new(VecDeque::with_capacity(cache_window.max(1))),
            cache_window,
        }
    }

    // ── Phase 1: Tool Registration ─────────────────────────────────────

    /// Register all tools into Memory with LLM-enriched descriptions.
    ///
    /// For each tool, checks if an up-to-date entry already exists (via
    /// content hash). Only calls the LLM for new or changed tools.
    pub async fn register_tools(
        &self,
        tools: &[Box<dyn Tool>],
        provider: &dyn Provider,
        model: &str,
    ) -> Result<RegisterReport> {
        let mut report = RegisterReport::default();

        for tool in tools {
            let tool_key = format!("{TOOL_KEY_PREFIX}{}", tool.name());
            let current_hash = content_hash(tool.name(), tool.description());

            // Check if already registered with same content hash
            if let Ok(Some(existing)) = self.memory.get(&tool_key).await {
                if existing.content.contains(&current_hash) {
                    report.skipped += 1;
                    continue;
                }
            }

            // Generate enriched description via LLM
            let enriched = match generate_enriched_description(
                provider,
                model,
                tool.name(),
                tool.description(),
                &tool.parameters_schema().to_string(),
            )
            .await
            {
                Ok(desc) => desc,
                Err(e) => {
                    tracing::warn!(
                        tool = tool.name(),
                        error = %e,
                        "Failed to generate enriched description; using fallback"
                    );
                    fallback_description(tool.name(), tool.description())
                }
            };

            // Append content hash for change detection
            let content_with_hash = format!("{enriched}\n__hash__:{current_hash}");

            self.memory
                .store(
                    &tool_key,
                    &content_with_hash,
                    tool_registry_category(),
                    None,
                )
                .await?;
            report.registered += 1;
        }

        report.total = tools.len();
        tracing::info!(
            total = report.total,
            registered = report.registered,
            skipped = report.skipped,
            "Tool RAG index registration complete"
        );
        Ok(report)
    }

    // ── Phase 2: Per-Query Tool Selection ──────────────────────────────

    /// Select relevant tools for a user message via Memory RAG.
    ///
    /// Returns a set of tool names: core_set ∪ RAG-matched tools.
    pub async fn select_tools(&self, user_message: &str) -> Result<HashSet<String>> {
        let mut selected = self.core_set.clone();

        // Merge cached tools from recent turns
        let cached = self.cached_tools();
        let cached_count = cached.len();
        selected.extend(cached);

        // Build enriched query for tool retrieval
        let query = build_tool_rag_query(user_message);

        // Recall from memory
        let entries = self.memory.recall(&query, self.top_k, None).await?;

        for entry in &entries {
            if let Some(tool_name) = entry.key.strip_prefix(TOOL_KEY_PREFIX) {
                if entry.score.unwrap_or(0.0) >= self.threshold {
                    selected.insert(tool_name.to_string());
                }
            }
        }

        tracing::debug!(
            query = %query,
            core_count = self.core_set.len(),
            cached_count = cached_count,
            rag_matched = selected.len().saturating_sub(self.core_set.len() + cached_count),
            total = selected.len(),
            "Tool RAG selection complete (with cache)"
        );

        Ok(selected)
    }

    // ── Phase 3: SubAgent Tool Discovery ───────────────────────────────

    /// Discover additional tools when the primary selection is insufficient.
    ///
    /// Uses an LLM to generate refined search queries, then re-queries
    /// Memory to find tools the initial RAG pass missed.
    pub async fn discover_tools(
        &self,
        provider: &dyn Provider,
        model: &str,
        user_message: &str,
        current_tools: &[String],
        feedback: &str,
    ) -> Result<Vec<String>> {
        if !self.discovery_enabled {
            return Ok(Vec::new());
        }

        // 1. LLM generates refined search queries
        let queries =
            generate_tool_search_queries(provider, model, user_message, current_tools, feedback)
                .await?;

        // 2. Each query recalls from Memory
        let current_set: HashSet<String> = current_tools.iter().cloned().collect();
        let mut discovered: HashSet<String> = HashSet::new();

        for query in &queries {
            let entries = self.memory.recall(query, self.top_k, None).await?;
            for entry in entries {
                if let Some(tool_name) = entry.key.strip_prefix(TOOL_KEY_PREFIX) {
                    if entry.score.unwrap_or(0.0) >= self.threshold
                        && !current_set.contains(tool_name)
                    {
                        discovered.insert(tool_name.to_string());
                    }
                }
            }
        }

        let result: Vec<String> = discovered.into_iter().collect();
        tracing::info!(
            queries_count = queries.len(),
            discovered_count = result.len(),
            discovered_tools = ?result,
            "Tool discovery complete"
        );
        Ok(result)
    }

    /// Whether this index has discovery enabled.
    pub fn discovery_enabled(&self) -> bool {
        self.discovery_enabled
    }

    /// Returns the core tool set.
    pub fn core_set(&self) -> &HashSet<String> {
        &self.core_set
    }

    // ── Phase 4: Turn-level Tool Cache ─────────────────────────────────

    /// Record the tools used in a completed turn into the sliding window cache.
    ///
    /// Only tools that are NOT in the core set are cached (the core set is
    /// always included anyway). The cache evicts the oldest entry when the
    /// window size is exceeded.
    pub fn record_turn_tools(&self, tools: &HashSet<String>) {
        if self.cache_window == 0 {
            return;
        }
        // Only cache non-core tools (core set is always present)
        let non_core: HashSet<String> = tools
            .iter()
            .filter(|t| !self.core_set.contains(t.as_str()))
            .cloned()
            .collect();
        if non_core.is_empty() {
            return;
        }
        if let Ok(mut cache) = self.recent_tools.lock() {
            if cache.len() >= self.cache_window {
                cache.pop_front();
            }
            cache.push_back(non_core);
        }
    }

    /// Returns the union of all tool names in the recent-turns cache.
    pub fn cached_tools(&self) -> HashSet<String> {
        self.recent_tools
            .lock()
            .map(|cache| cache.iter().flatten().cloned().collect())
            .unwrap_or_default()
    }
}

// ── Registration Report ───────────────────────────────────────────────

/// Summary of a tool registration run.
#[derive(Debug, Default)]
pub struct RegisterReport {
    pub total: usize,
    pub registered: usize,
    pub skipped: usize,
}

// ── Internal Helpers ──────────────────────────────────────────────────

/// Build an enriched RAG query from a user message.
///
/// Prefixes with "tool:" to anchor the vector search in tool-description
/// space, matching the stored format.
fn build_tool_rag_query(user_message: &str) -> String {
    format!("tool: what tools are needed to: {user_message}")
}

/// Generate a content hash for change detection.
fn content_hash(name: &str, description: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(name.as_bytes());
    hasher.update(b"|");
    hasher.update(description.as_bytes());
    let result = hasher.finalize();
    // Use first 16 hex chars for brevity
    hex::encode(&result[..8])
}

/// Generate an LLM-enriched tool description (English only).
async fn generate_enriched_description(
    provider: &dyn Provider,
    model: &str,
    name: &str,
    description: &str,
    parameters: &str,
) -> Result<String> {
    let prompt = format!(
        "Generate a capability description for the following tool (max 200 words, English only).\n\
         Tool: {name}\n\
         Description: {description}\n\
         Parameters: {parameters}\n\n\
         Output format (plain text, no markdown):\n\
         tool: {name}\n\
         capabilities: <what this tool can do, including indirect uses>\n\
         scenarios: <comma-separated list of real-world scenarios where this tool is useful>"
    );

    let response = provider.simple_chat(&prompt, model, 0.3).await?;

    // Validate response has expected format
    let trimmed = response.trim().to_string();
    if trimmed.contains("tool:") && trimmed.contains("capabilities:") {
        Ok(trimmed)
    } else {
        // If LLM didn't follow format, wrap it
        Ok(format!(
            "tool: {name}\ncapabilities: {description}\nscenarios: {trimmed}"
        ))
    }
}

/// Fallback description when LLM enrichment fails.
fn fallback_description(name: &str, description: &str) -> String {
    format!("tool: {name}\ncapabilities: {description}\nscenarios: general use")
}

/// Heuristic: does the LLM response indicate the available tools are insufficient?
///
/// Checks for common phrases (English and Chinese) that models use when they
/// cannot find a suitable tool for the user's request.
pub fn looks_like_tool_insufficient(text: &str) -> bool {
    let lower = text.to_lowercase();
    const SIGNALS: &[&str] = &[
        // English signals
        "don't have",
        "do not have",
        "no tool",
        "no suitable tool",
        "cannot find a tool",
        "not available",
        "lack of tool",
        "no appropriate tool",
        "i don't have access to",
        "unable to",
        "i cannot",
        "no way to",
        "need a tool",
        "require a tool",
        "capability needed",
        "tool needed",
        "need the capability",
        "need access to",
        "don't have real-time",
        "no real-time data",
        "cannot fetch",
        "unable to fetch",
        "unable to access internet",
        "cannot access the internet",
        // Chinese signals
        "没有合适",
        "没有工具",
        "无法找到",
        "缺少工具",
        "没有可用的",
        "无法使用",
        "需要工具",
        "需要一个工具",
        "需要使用工具",
        "需要相关工具",
        "需要访问",
        "没有实时数据",
        "无法直接获取",
        "无法获取",
        "无法查询",
        "我没有",
        "我无法",
    ];
    SIGNALS.iter().any(|s| lower.contains(s))
}

/// Ask LLM to generate refined search queries for tool discovery.
async fn generate_tool_search_queries(
    provider: &dyn Provider,
    model: &str,
    user_message: &str,
    current_tools: &[String],
    feedback: &str,
) -> Result<Vec<String>> {
    let current_list = current_tools.join(", ");
    let prompt = format!(
        "You are a tool selection expert. The current tools are insufficient for the user's task.\n\
         Analyze the problem and generate 2-3 English search queries to find suitable tools.\n\
         Each query MUST start with \"tool:\" prefix.\n\n\
         User task: {user_message}\n\
         Current tools: {current_list}\n\
         Feedback: {feedback}\n\n\
         Output ONLY a JSON object with this format:\n\
         {{\"queries\": [\"tool: search query 1\", \"tool: search query 2\"]}}"
    );

    let response = provider.simple_chat(&prompt, model, 0.3).await?;

    // Parse JSON from response
    let trimmed = response.trim();
    // Try to extract JSON from potential markdown code blocks
    let json_str = if let Some(start) = trimmed.find('{') {
        if let Some(end) = trimmed.rfind('}') {
            &trimmed[start..=end]
        } else {
            trimmed
        }
    } else {
        trimmed
    };

    match serde_json::from_str::<serde_json::Value>(json_str) {
        Ok(parsed) => {
            let queries = parsed
                .get("queries")
                .and_then(|q| q.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            Ok(queries)
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                response = %trimmed,
                "Failed to parse tool discovery response; using fallback queries"
            );
            // Fallback: generate basic queries from the user message
            Ok(vec![
                format!("tool: {user_message}"),
                format!("tool: what tools are needed to: {user_message}"),
            ])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_tool_rag_query_has_prefix() {
        let query = build_tool_rag_query("check the weather today");
        assert!(query.starts_with("tool:"));
        assert!(query.contains("check the weather today"));
    }

    #[test]
    fn content_hash_is_deterministic() {
        let h1 = content_hash("web_search", "Search the web");
        let h2 = content_hash("web_search", "Search the web");
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 16); // 8 bytes = 16 hex chars
    }

    #[test]
    fn content_hash_differs_on_change() {
        let h1 = content_hash("web_search", "Search the web");
        let h2 = content_hash("web_search", "Search the web v2");
        assert_ne!(h1, h2);
    }

    #[test]
    fn fallback_description_has_expected_format() {
        let desc = fallback_description("shell", "Execute commands");
        assert!(desc.starts_with("tool: shell"));
        assert!(desc.contains("capabilities: Execute commands"));
        assert!(desc.contains("scenarios:"));
    }

    #[test]
    fn tool_key_prefix_format() {
        let key = format!("{TOOL_KEY_PREFIX}web_search");
        assert_eq!(key, "tool:web_search");
    }

    #[test]
    fn tool_registry_category_is_custom() {
        let cat = tool_registry_category();
        assert!(matches!(cat, MemoryCategory::Custom(ref s) if s == "tool_registry"));
    }

    // ── ToolRagIndex unit tests with mock ──

    use crate::memory::traits::MemoryEntry;
    use async_trait::async_trait;

    struct MockMemory {
        entries: std::sync::Mutex<Vec<(String, String)>>,
    }

    impl MockMemory {
        fn new() -> Self {
            Self {
                entries: std::sync::Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl Memory for MockMemory {
        fn name(&self) -> &str {
            "mock"
        }

        async fn store(
            &self,
            key: &str,
            content: &str,
            _category: MemoryCategory,
            _session_id: Option<&str>,
        ) -> Result<()> {
            let mut entries = self.entries.lock().unwrap();
            entries.retain(|(k, _)| k != key);
            entries.push((key.to_string(), content.to_string()));
            Ok(())
        }

        async fn recall(
            &self,
            query: &str,
            limit: usize,
            _session_id: Option<&str>,
        ) -> Result<Vec<MemoryEntry>> {
            let entries = self.entries.lock().unwrap();
            // Simple keyword matching for tests
            let mut results: Vec<MemoryEntry> = entries
                .iter()
                .filter(|(_, content)| {
                    let q_lower = query.to_lowercase();
                    let c_lower = content.to_lowercase();
                    // Check for any word overlap
                    q_lower.split_whitespace().any(|w| c_lower.contains(w))
                })
                .take(limit)
                .enumerate()
                .map(|(i, (key, content))| MemoryEntry {
                    id: format!("id-{i}"),
                    key: key.clone(),
                    content: content.clone(),
                    category: tool_registry_category(),
                    timestamp: "2026-01-01T00:00:00Z".into(),
                    session_id: None,
                    score: Some(0.8),
                })
                .collect();
            results.truncate(limit);
            Ok(results)
        }

        async fn get(&self, key: &str) -> Result<Option<MemoryEntry>> {
            let entries = self.entries.lock().unwrap();
            Ok(entries
                .iter()
                .find(|(k, _)| k == key)
                .map(|(key, content)| MemoryEntry {
                    id: "id-get".into(),
                    key: key.clone(),
                    content: content.clone(),
                    category: tool_registry_category(),
                    timestamp: "2026-01-01T00:00:00Z".into(),
                    session_id: None,
                    score: None,
                }))
        }

        async fn list(
            &self,
            _category: Option<&MemoryCategory>,
            _session_id: Option<&str>,
        ) -> Result<Vec<MemoryEntry>> {
            Ok(Vec::new())
        }

        async fn forget(&self, key: &str) -> Result<bool> {
            let mut entries = self.entries.lock().unwrap();
            let len_before = entries.len();
            entries.retain(|(k, _)| k != key);
            Ok(entries.len() != len_before)
        }

        async fn count(&self) -> Result<usize> {
            Ok(self.entries.lock().unwrap().len())
        }

        async fn health_check(&self) -> bool {
            true
        }
    }

    #[tokio::test]
    async fn select_tools_always_includes_core_set() {
        let memory = Arc::new(MockMemory::new());
        let index = ToolRagIndex::new(
            memory,
            vec!["shell".into(), "file_read".into()],
            5,
            0.3,
            false,
            0,
        );

        let selected = index.select_tools("hello world").await.unwrap();
        assert!(selected.contains("shell"));
        assert!(selected.contains("file_read"));
    }

    #[tokio::test]
    async fn select_tools_includes_rag_matches() {
        let memory = Arc::new(MockMemory::new());

        // Pre-populate with a tool entry
        memory
            .store(
                "tool:web_search",
                "tool: web_search\ncapabilities: search the internet for weather, news",
                tool_registry_category(),
                None,
            )
            .await
            .unwrap();

        let index = ToolRagIndex::new(memory, vec!["shell".into()], 5, 0.3, false, 0);

        let selected = index.select_tools("weather forecast").await.unwrap();
        assert!(selected.contains("shell")); // core
        assert!(selected.contains("web_search")); // RAG match on "weather"
    }

    #[tokio::test]
    async fn tool_rag_index_core_set_accessor() {
        let memory = Arc::new(MockMemory::new());
        let index = ToolRagIndex::new(
            memory,
            vec!["shell".into(), "file_read".into()],
            5,
            0.3,
            true,
            0,
        );

        assert!(index.core_set().contains("shell"));
        assert!(index.core_set().contains("file_read"));
        assert!(index.discovery_enabled());
    }
}
