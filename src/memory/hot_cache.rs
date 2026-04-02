//! Hot Memory Cache for MoA ACE Layer 2.
//!
//! Caches the most frequently recalled memories in-memory for instant access.
//! The cache is populated at session start and refreshed periodically.
//!
//! **Always cached (essential):**
//! - User profile entries (user_profile_*)
//! - User preferences (user_moa_preferences)
//!
//! **Dynamically cached (by recall frequency):**
//! - Top N most frequently recalled entries (configurable, default 50)
//!
//! Cache invalidation:
//! - On memory store/update → entry evicted from cache
//! - On memory forget → entry removed from cache
//! - Periodic refresh every 5 minutes

use super::traits::{Memory, MemoryEntry};
use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

/// Default number of hot entries to cache (beyond essential profile).
const DEFAULT_HOT_CACHE_SIZE: usize = 50;

/// Cache refresh interval.
const REFRESH_INTERVAL_SECS: u64 = 300; // 5 minutes

/// In-memory cache for frequently accessed memories.
pub struct HotMemoryCache {
    entries: Arc<RwLock<HashMap<String, MemoryEntry>>>,
    last_refresh: Arc<RwLock<Instant>>,
    max_entries: usize,
}

impl HotMemoryCache {
    pub fn new() -> Self {
        Self {
            entries: Arc::new(RwLock::new(HashMap::new())),
            last_refresh: Arc::new(RwLock::new(Instant::now())),
            max_entries: DEFAULT_HOT_CACHE_SIZE,
        }
    }

    /// Try to get a memory from cache. Returns None if not cached.
    pub fn get(&self, key: &str) -> Option<MemoryEntry> {
        self.entries.read().get(key).cloned()
    }

    /// Get all cached entries (for context building).
    pub fn all_entries(&self) -> Vec<MemoryEntry> {
        self.entries.read().values().cloned().collect()
    }

    /// Check if cache needs refresh.
    pub fn needs_refresh(&self) -> bool {
        self.last_refresh.read().elapsed().as_secs() >= REFRESH_INTERVAL_SECS
    }

    /// Populate/refresh cache from the memory backend.
    pub async fn refresh(&self, mem: &dyn Memory) {
        let mut cache = HashMap::new();

        // 1. Always cache essential profile entries
        const ESSENTIAL_KEYS: &[&str] = &[
            "user_profile_identity",
            "user_profile_family",
            "user_profile_work",
            "user_profile_lifestyle",
            "user_profile_communication",
            "user_profile_routine",
            "user_moa_preferences",
        ];

        for key in ESSENTIAL_KEYS {
            if let Ok(Some(entry)) = mem.get(key).await {
                cache.insert(entry.key.clone(), entry);
            }
        }

        // 2. Always cache user instructions & standing orders
        // These are tasks the user has told MoA to do regularly — cron jobs,
        // recurring reminders, standing directives. They must be top-of-mind
        // for the agent at all times, especially for cron and heartbeat.
        const INSTRUCTION_KEY_PREFIXES: &[&str] = &[
            "user_instruction_",    // "매일 아침 날씨 알려줘" 등 지시사항
            "user_standing_order_", // 상시 지시 (예: "항상 존칭 사용")
            "user_cron_",           // cron 작업 관련 기억
            "user_reminder_",       // 리마인더/알림 관련
            "user_schedule_",       // 일정 관련 지시
        ];

        if let Ok(all_entries) = mem.list(None, None).await {
            for entry in all_entries {
                if INSTRUCTION_KEY_PREFIXES
                    .iter()
                    .any(|prefix| entry.key.starts_with(prefix))
                {
                    cache.insert(entry.key.clone(), entry);
                }
            }
        }

        // 2. Cache top N most frequently recalled entries
        if let Ok(hot) = mem.hot_memories(self.max_entries).await {
            for entry in hot {
                if !cache.contains_key(&entry.key) {
                    cache.insert(entry.key.clone(), entry);
                }
            }
        }

        *self.entries.write() = cache;
        *self.last_refresh.write() = Instant::now();
    }

    /// Invalidate a specific key (called on store/update/forget).
    pub fn invalidate(&self, key: &str) {
        self.entries.write().remove(key);
    }

    /// Invalidate all entries matching a pattern (called on forget_matching).
    pub fn invalidate_matching(&self, pattern: &str) {
        let pattern_lower = pattern.to_lowercase();
        self.entries.write().retain(|k, v| {
            !k.to_lowercase().contains(&pattern_lower)
                && !v.content.to_lowercase().contains(&pattern_lower)
        });
    }

    /// Number of cached entries.
    pub fn len(&self) -> usize {
        self.entries.read().len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.read().is_empty()
    }
}

impl Default for HotMemoryCache {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hot_cache_starts_empty() {
        let cache = HotMemoryCache::new();
        assert!(cache.is_empty());
        assert!(cache.get("test").is_none());
    }

    #[test]
    fn hot_cache_invalidate() {
        let cache = HotMemoryCache::new();
        {
            let mut entries = cache.entries.write();
            entries.insert(
                "test_key".to_string(),
                MemoryEntry {
                    id: "1".into(),
                    key: "test_key".into(),
                    content: "test content".into(),
                    category: super::super::traits::MemoryCategory::Core,
                    timestamp: "2026-03-30".into(),
                    session_id: None,
                    score: None,
                    recall_count: 5,
                    last_recalled: None,
                },
            );
        }
        assert!(cache.get("test_key").is_some());
        cache.invalidate("test_key");
        assert!(cache.get("test_key").is_none());
    }

    #[test]
    fn hot_cache_invalidate_matching() {
        let cache = HotMemoryCache::new();
        {
            let mut entries = cache.entries.write();
            entries.insert(
                "ex_boyfriend_memory".to_string(),
                MemoryEntry {
                    id: "1".into(),
                    key: "ex_boyfriend_memory".into(),
                    content: "전남친과의 추억".into(),
                    category: super::super::traits::MemoryCategory::Core,
                    timestamp: "2025-01-01".into(),
                    session_id: None,
                    score: None,
                    recall_count: 0,
                    last_recalled: None,
                },
            );
            entries.insert(
                "work_project".to_string(),
                MemoryEntry {
                    id: "2".into(),
                    key: "work_project".into(),
                    content: "업무 프로젝트".into(),
                    category: super::super::traits::MemoryCategory::Core,
                    timestamp: "2026-03-30".into(),
                    session_id: None,
                    score: None,
                    recall_count: 10,
                    last_recalled: None,
                },
            );
        }
        assert_eq!(cache.len(), 2);
        cache.invalidate_matching("전남친");
        assert_eq!(cache.len(), 1);
        assert!(cache.get("work_project").is_some());
    }
}
