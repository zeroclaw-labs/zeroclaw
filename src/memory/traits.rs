use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// A single memory entry
#[derive(Clone, Serialize, Deserialize)]
pub struct MemoryEntry {
    pub id: String,
    pub key: String,
    pub content: String,
    pub category: MemoryCategory,
    pub timestamp: String,
    pub session_id: Option<String>,
    pub score: Option<f64>,
    /// Number of times this entry has been recalled/searched.
    /// Higher count = more frequently referenced = higher priority in RAG results.
    #[serde(default)]
    pub recall_count: u32,
    /// Last time this entry was recalled (ISO 8601).
    /// Used for hot cache eviction and decay scoring.
    #[serde(default)]
    pub last_recalled: Option<String>,
}

impl std::fmt::Debug for MemoryEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemoryEntry")
            .field("id", &self.id)
            .field("key", &self.key)
            .field("content", &self.content)
            .field("category", &self.category)
            .field("timestamp", &self.timestamp)
            .field("score", &self.score)
            .finish_non_exhaustive()
    }
}

/// Memory categories for organization
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MemoryCategory {
    /// Long-term facts, preferences, decisions
    Core,
    /// Daily session logs
    Daily,
    /// Conversation context
    Conversation,
    /// User-defined custom category
    Custom(String),
}

impl std::fmt::Display for MemoryCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Core => write!(f, "core"),
            Self::Daily => write!(f, "daily"),
            Self::Conversation => write!(f, "conversation"),
            Self::Custom(name) => write!(f, "{name}"),
        }
    }
}

/// Interaction categories for systematic memory classification.
/// Each memory entry is tagged with its work type for structured storage and retrieval.
/// Used in both short-term (conversation_turns) and long-term (Core) memory storage.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum InteractionCategory {
    /// General conversation / chat
    Chat,
    /// Document creation, editing, reading (PDF, DOCX, etc.)
    Document,
    /// Music creation, playback, playlist management
    Music,
    /// Image creation, editing, analysis
    Image,
    /// Translation and interpretation
    Translation,
    /// Coding and software development
    Coding,
    /// Web search and information retrieval
    Search,
    /// General / uncategorized interaction
    General,
}

impl InteractionCategory {
    /// Classify an interaction based on message content and tool usage hints.
    pub fn classify(message: &str, tool_hints: &[&str]) -> Self {
        let msg_lower = message.to_lowercase();

        // Tool-based classification takes priority
        for hint in tool_hints {
            match *hint {
                "shell" | "file_write" | "file_read" | "file_edit" | "git_operations"
                | "apply_patch" | "content_search" | "glob_search" => return Self::Coding,
                "web_search" | "web_fetch" => return Self::Search,
                "document_process" | "pdf_read" | "docx_read" | "xlsx_read" | "pptx_read" => {
                    return Self::Document
                }
                "screenshot" | "image_info" => return Self::Image,
                _ => {}
            }
        }

        // Keyword-based classification
        let coding_keywords = [
            "code",
            "function",
            "compile",
            "debug",
            "git ",
            "cargo ",
            "npm ",
            "python",
            "rust",
            "javascript",
            "코드",
            "함수",
            "컴파일",
            "디버그",
            "프로그램",
        ];
        let doc_keywords = [
            "document", "file", "pdf", "docx", "xlsx", "pptx", "hwp", "write", "문서", "파일",
            "작성", "편집", "읽기",
        ];
        let music_keywords = [
            "music",
            "song",
            "playlist",
            "audio",
            "mp3",
            "melody",
            "compose",
            "음악",
            "노래",
            "재생",
            "작곡",
            "멜로디",
        ];
        let image_keywords = [
            "image",
            "photo",
            "picture",
            "draw",
            "screenshot",
            "png",
            "jpg",
            "이미지",
            "사진",
            "그림",
            "그리기",
            "스크린샷",
        ];
        let translation_keywords = [
            "translate",
            "translation",
            "interpret",
            "language",
            "번역",
            "통역",
            "언어",
            "翻訳",
            "翻译",
        ];
        let search_keywords = [
            "search", "find", "look up", "google", "검색", "찾아", "조회",
        ];

        if coding_keywords.iter().any(|k| msg_lower.contains(k)) {
            Self::Coding
        } else if doc_keywords.iter().any(|k| msg_lower.contains(k)) {
            Self::Document
        } else if music_keywords.iter().any(|k| msg_lower.contains(k)) {
            Self::Music
        } else if image_keywords.iter().any(|k| msg_lower.contains(k)) {
            Self::Image
        } else if translation_keywords.iter().any(|k| msg_lower.contains(k)) {
            Self::Translation
        } else if search_keywords.iter().any(|k| msg_lower.contains(k)) {
            Self::Search
        } else {
            Self::Chat
        }
    }
}

impl std::fmt::Display for InteractionCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Chat => write!(f, "chat"),
            Self::Document => write!(f, "document"),
            Self::Music => write!(f, "music"),
            Self::Image => write!(f, "image"),
            Self::Translation => write!(f, "translation"),
            Self::Coding => write!(f, "coding"),
            Self::Search => write!(f, "search"),
            Self::General => write!(f, "general"),
        }
    }
}

impl InteractionCategory {
    /// Parse from string, defaulting to General for unknown values.
    pub fn from_str_lossy(s: &str) -> Self {
        match s {
            "chat" => Self::Chat,
            "document" => Self::Document,
            "music" => Self::Music,
            "image" => Self::Image,
            "translation" => Self::Translation,
            "coding" => Self::Coding,
            "search" => Self::Search,
            "general" => Self::General,
            _ => Self::General,
        }
    }
}

/// Core memory trait — implement for any persistence backend
#[async_trait]
pub trait Memory: Send + Sync {
    /// Backend name
    fn name(&self) -> &str;

    /// Store a memory entry, optionally scoped to a session
    async fn store(
        &self,
        key: &str,
        content: &str,
        category: MemoryCategory,
        session_id: Option<&str>,
    ) -> anyhow::Result<()>;

    /// Recall memories matching a query (keyword search), optionally scoped to a session
    async fn recall(
        &self,
        query: &str,
        limit: usize,
        session_id: Option<&str>,
    ) -> anyhow::Result<Vec<MemoryEntry>>;

    /// Get a specific memory by key
    async fn get(&self, key: &str) -> anyhow::Result<Option<MemoryEntry>>;

    /// List all memory keys, optionally filtered by category and/or session
    async fn list(
        &self,
        category: Option<&MemoryCategory>,
        session_id: Option<&str>,
    ) -> anyhow::Result<Vec<MemoryEntry>>;

    /// Remove a memory by key
    async fn forget(&self, key: &str) -> anyhow::Result<bool>;

    /// Count total memories
    async fn count(&self) -> anyhow::Result<usize>;

    /// Health check
    async fn health_check(&self) -> bool;

    /// Rebuild embeddings for all memories using the current embedding provider.
    async fn reindex(
        &self,
        progress_callback: Option<Box<dyn Fn(usize, usize) + Send + Sync>>,
    ) -> anyhow::Result<usize> {
        let _ = progress_callback;
        anyhow::bail!("Reindex not supported by {} backend", self.name())
    }

    /// Increment the recall count for a memory entry.
    /// **Status: prepared for future integration** — will be called automatically
    /// by the agent loop when memory entries are retrieved via recall().
    /// Entries with higher recall_count get priority in RAG search results.
    /// SQLite backend should override this with actual UPDATE query.
    async fn track_recall(&self, _key: &str) -> anyhow::Result<()> {
        Ok(()) // default no-op; concrete backends override
    }

    /// Get the most frequently recalled memories (hot memories).
    /// **Status: prepared for future integration** — will be called by
    /// HotMemoryCache::refresh() at session start to pre-load frequently
    /// accessed entries into in-memory cache.
    async fn hot_memories(&self, _limit: usize) -> anyhow::Result<Vec<MemoryEntry>> {
        Ok(vec![]) // default empty; concrete backends override
    }

    /// Detect conflicts between a new value and existing memory.
    /// **Status: prepared for future integration** — will be called before
    /// memory_store() to check if new info contradicts existing entries
    /// (e.g., address/job/phone change). Agent prompt already instructs
    /// MoA to ask for confirmation before updating.
    /// Returns the existing entry if a conflict is detected.
    async fn detect_conflict(
        &self,
        key: &str,
        new_content: &str,
    ) -> anyhow::Result<Option<MemoryConflict>> {
        let existing = self.get(key).await?;
        if let Some(entry) = existing {
            if entry.content != new_content && !new_content.is_empty() {
                return Ok(Some(MemoryConflict {
                    key: key.to_string(),
                    old_content: entry.content,
                    new_content: new_content.to_string(),
                    old_timestamp: entry.timestamp,
                }));
            }
        }
        Ok(None)
    }

    /// Bulk forget: remove all memories matching a keyword pattern.
    /// **Status: prepared for future integration** — will be called when
    /// user requests "전남편 관련 기억 다 지워줘". Agent prompt already
    /// instructs MoA to confirm before deletion.
    /// Returns the number of entries deleted.
    async fn forget_matching(&self, _pattern: &str) -> anyhow::Result<usize> {
        Ok(0) // default no-op; SQLite backend overrides this
    }
}

/// Detected conflict between existing and new memory content.
/// Used to prompt the user: "이 정보가 변경된 것 같습니다. 업데이트할까요?"
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryConflict {
    pub key: String,
    pub old_content: String,
    pub new_content: String,
    pub old_timestamp: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_category_display_outputs_expected_values() {
        assert_eq!(MemoryCategory::Core.to_string(), "core");
        assert_eq!(MemoryCategory::Daily.to_string(), "daily");
        assert_eq!(MemoryCategory::Conversation.to_string(), "conversation");
        assert_eq!(
            MemoryCategory::Custom("project_notes".into()).to_string(),
            "project_notes"
        );
    }

    #[test]
    fn memory_category_serde_uses_snake_case() {
        let core = serde_json::to_string(&MemoryCategory::Core).unwrap();
        let daily = serde_json::to_string(&MemoryCategory::Daily).unwrap();
        let conversation = serde_json::to_string(&MemoryCategory::Conversation).unwrap();

        assert_eq!(core, "\"core\"");
        assert_eq!(daily, "\"daily\"");
        assert_eq!(conversation, "\"conversation\"");
    }

    #[test]
    fn interaction_category_classify_detects_coding() {
        assert_eq!(
            InteractionCategory::classify("help me write a function", &[]),
            InteractionCategory::Coding
        );
        assert_eq!(
            InteractionCategory::classify("hello", &["shell"]),
            InteractionCategory::Coding
        );
    }

    #[test]
    fn interaction_category_classify_defaults_to_chat() {
        assert_eq!(
            InteractionCategory::classify("hello there", &[]),
            InteractionCategory::Chat
        );
    }

    #[test]
    fn interaction_category_display_roundtrip() {
        let cat = InteractionCategory::Document;
        assert_eq!(cat.to_string(), "document");
        assert_eq!(InteractionCategory::from_str_lossy("document"), cat);
    }

    #[test]
    fn memory_entry_roundtrip_preserves_optional_fields() {
        let entry = MemoryEntry {
            id: "id-1".into(),
            key: "favorite_language".into(),
            content: "Rust".into(),
            category: MemoryCategory::Core,
            timestamp: "2026-02-16T00:00:00Z".into(),
            session_id: Some("session-abc".into()),
            score: Some(0.98),
            recall_count: 0,
            last_recalled: None,
        };

        let json = serde_json::to_string(&entry).unwrap();
        let parsed: MemoryEntry = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.id, "id-1");
        assert_eq!(parsed.key, "favorite_language");
        assert_eq!(parsed.content, "Rust");
        assert_eq!(parsed.category, MemoryCategory::Core);
        assert_eq!(parsed.session_id.as_deref(), Some("session-abc"));
        assert_eq!(parsed.score, Some(0.98));
    }
}
