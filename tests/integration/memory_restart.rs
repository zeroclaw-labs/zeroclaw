//! TG5: Memory Restart Resilience Tests
//!
//! Prevents: Pattern 5 — Memory & state persistence bugs (10% of user bugs).
//! Issues: #430, #693, #802
//!
//! Tests SqliteMemory deduplication on restart, session scoping, concurrent
//! message ordering, and recall behavior after re-initialization.

use std::sync::Arc;
use zeroclaw::memory::sqlite::SqliteMemory;
use zeroclaw::memory::traits::{Memory, MemoryCategory};

// ─────────────────────────────────────────────────────────────────────────────
// Deduplication: same key overwrites instead of duplicating (#430)
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn sqlite_memory_store_same_key_deduplicates() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mem = SqliteMemory::new(tmp.path()).unwrap();

    // Store same key twice with different content
    mem.store("greeting", "hello world", MemoryCategory::Core, None)
        .await
        .unwrap();
    mem.store("greeting", "hello updated", MemoryCategory::Core, None)
        .await
        .unwrap();

    // Should have exactly 1 entry, not 2
    let count = mem.count().await.unwrap();
    assert_eq!(
        count, 1,
        "storing same key twice should not create duplicates"
    );

    // Content should be the latest version
    let entry = mem
        .get("greeting")
        .await
        .unwrap()
        .expect("entry should exist");
    assert_eq!(entry.content, "hello updated");
}

#[tokio::test]
async fn sqlite_memory_store_different_keys_creates_separate_entries() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mem = SqliteMemory::new(tmp.path()).unwrap();

    mem.store("key_a", "content a", MemoryCategory::Core, None)
        .await
        .unwrap();
    mem.store("key_b", "content b", MemoryCategory::Core, None)
        .await
        .unwrap();

    let count = mem.count().await.unwrap();
    assert_eq!(count, 2, "different keys should create separate entries");
}

// ─────────────────────────────────────────────────────────────────────────────
// Restart resilience: data persists across memory re-initialization
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn sqlite_memory_persists_across_reinitialization() {
    let tmp = tempfile::TempDir::new().unwrap();

    // First "session": store data
    {
        let mem = SqliteMemory::new(tmp.path()).unwrap();
        mem.store(
            "persistent_fact",
            "Rust is great",
            MemoryCategory::Core,
            None,
        )
        .await
        .unwrap();
    }

    // Second "session": re-create memory from same path
    {
        let mem = SqliteMemory::new(tmp.path()).unwrap();
        let entry = mem
            .get("persistent_fact")
            .await
            .unwrap()
            .expect("entry should survive reinitialization");
        assert_eq!(entry.content, "Rust is great");
    }
}

#[tokio::test]
async fn sqlite_memory_restart_does_not_duplicate_on_rewrite() {
    let tmp = tempfile::TempDir::new().unwrap();

    // First session: store entries
    {
        let mem = SqliteMemory::new(tmp.path()).unwrap();
        mem.store("fact_1", "original content", MemoryCategory::Core, None)
            .await
            .unwrap();
        mem.store("fact_2", "another fact", MemoryCategory::Core, None)
            .await
            .unwrap();
    }

    // Second session: re-store same keys (simulates channel re-reading history)
    {
        let mem = SqliteMemory::new(tmp.path()).unwrap();
        mem.store("fact_1", "original content", MemoryCategory::Core, None)
            .await
            .unwrap();
        mem.store("fact_2", "another fact", MemoryCategory::Core, None)
            .await
            .unwrap();

        let count = mem.count().await.unwrap();
        assert_eq!(
            count, 2,
            "re-storing same keys after restart should not create duplicates"
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Session scoping: messages scoped to sessions don't leak
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn sqlite_memory_session_scoped_store_and_recall() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mem = SqliteMemory::new(tmp.path()).unwrap();

    // Store in different sessions
    mem.store(
        "session_a_fact",
        "fact from session A",
        MemoryCategory::Conversation,
        Some("session_a"),
    )
    .await
    .unwrap();
    mem.store(
        "session_b_fact",
        "fact from session B",
        MemoryCategory::Conversation,
        Some("session_b"),
    )
    .await
    .unwrap();

    // List scoped to session_a
    let session_a_entries = mem
        .list(Some(&MemoryCategory::Conversation), Some("session_a"))
        .await
        .unwrap();
    assert_eq!(
        session_a_entries.len(),
        1,
        "session_a should have exactly 1 entry"
    );
    assert_eq!(session_a_entries[0].content, "fact from session A");
}

#[tokio::test]
async fn sqlite_memory_global_recall_includes_all_sessions() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mem = SqliteMemory::new(tmp.path()).unwrap();

    mem.store(
        "global_a",
        "alpha content",
        MemoryCategory::Core,
        Some("s1"),
    )
    .await
    .unwrap();
    mem.store("global_b", "beta content", MemoryCategory::Core, Some("s2"))
        .await
        .unwrap();

    // Global count should include all
    let count = mem.count().await.unwrap();
    assert_eq!(
        count, 2,
        "global count should include entries from all sessions"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Recall and search behavior
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn sqlite_memory_recall_returns_relevant_results() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mem = SqliteMemory::new(tmp.path()).unwrap();

    mem.store(
        "lang_pref",
        "User prefers Rust programming",
        MemoryCategory::Core,
        None,
    )
    .await
    .unwrap();
    mem.store(
        "food_pref",
        "User likes sushi for lunch",
        MemoryCategory::Core,
        None,
    )
    .await
    .unwrap();

    let results = mem.recall("Rust programming", 10, None).await.unwrap();
    assert!(!results.is_empty(), "recall should find matching entries");
    // The Rust-related entry should be in results
    assert!(
        results.iter().any(|e| e.content.contains("Rust")),
        "recall for 'Rust' should include the Rust-related entry"
    );
}

#[tokio::test]
async fn sqlite_memory_recall_respects_limit() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mem = SqliteMemory::new(tmp.path()).unwrap();

    for i in 0..10 {
        mem.store(
            &format!("entry_{i}"),
            &format!("test content number {i}"),
            MemoryCategory::Core,
            None,
        )
        .await
        .unwrap();
    }

    let results = mem.recall("test content", 3, None).await.unwrap();
    assert!(
        results.len() <= 3,
        "recall should respect limit of 3, got {}",
        results.len()
    );
}

#[tokio::test]
async fn sqlite_memory_recall_empty_query_returns_empty() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mem = SqliteMemory::new(tmp.path()).unwrap();

    mem.store("fact", "some content", MemoryCategory::Core, None)
        .await
        .unwrap();

    let results = mem.recall("", 10, None).await.unwrap();
    assert!(results.is_empty(), "empty query should return no results");
}

// ─────────────────────────────────────────────────────────────────────────────
// Forget and health check
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn sqlite_memory_forget_removes_entry() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mem = SqliteMemory::new(tmp.path()).unwrap();

    mem.store("to_forget", "temporary info", MemoryCategory::Core, None)
        .await
        .unwrap();
    assert_eq!(mem.count().await.unwrap(), 1);

    let removed = mem.forget("to_forget").await.unwrap();
    assert!(removed, "forget should return true for existing key");
    assert_eq!(mem.count().await.unwrap(), 0);
}

#[tokio::test]
async fn sqlite_memory_forget_nonexistent_returns_false() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mem = SqliteMemory::new(tmp.path()).unwrap();

    let removed = mem.forget("nonexistent_key").await.unwrap();
    assert!(!removed, "forget should return false for nonexistent key");
}

#[tokio::test]
async fn sqlite_memory_health_check_returns_true() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mem = SqliteMemory::new(tmp.path()).unwrap();

    assert!(mem.health_check().await, "health_check should return true");
}

// ─────────────────────────────────────────────────────────────────────────────
// Concurrent access
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn sqlite_memory_concurrent_stores_no_data_loss() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mem = Arc::new(SqliteMemory::new(tmp.path()).unwrap());

    let mut handles = Vec::new();
    for i in 0..5 {
        let mem_clone = mem.clone();
        handles.push(tokio::spawn(async move {
            mem_clone
                .store(
                    &format!("concurrent_{i}"),
                    &format!("content from task {i}"),
                    MemoryCategory::Core,
                    None,
                )
                .await
                .unwrap();
        }));
    }

    for handle in handles {
        handle.await.unwrap();
    }

    let count = mem.count().await.unwrap();
    assert_eq!(
        count, 5,
        "all concurrent stores should succeed, got {count}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Memory categories
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn sqlite_memory_list_by_category() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mem = SqliteMemory::new(tmp.path()).unwrap();

    mem.store("core_fact", "core info", MemoryCategory::Core, None)
        .await
        .unwrap();
    mem.store("daily_note", "daily note", MemoryCategory::Daily, None)
        .await
        .unwrap();
    mem.store(
        "conv_msg",
        "conversation msg",
        MemoryCategory::Conversation,
        None,
    )
    .await
    .unwrap();

    let core_entries = mem.list(Some(&MemoryCategory::Core), None).await.unwrap();
    assert_eq!(core_entries.len(), 1, "should have 1 Core entry");
    assert_eq!(core_entries[0].key, "core_fact");

    let daily_entries = mem.list(Some(&MemoryCategory::Daily), None).await.unwrap();
    assert_eq!(daily_entries.len(), 1, "should have 1 Daily entry");
}
