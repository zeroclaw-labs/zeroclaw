//! Head-to-head comparison: SQLite vs Markdown memory backends
//!
//! Run with: cargo test --test memory_comparison -- --nocapture

use std::time::Instant;
use tempfile::TempDir;

// We test both backends through the public memory module
use zeroclaw::memory::{markdown::MarkdownMemory, sqlite::SqliteMemory, Memory, MemoryCategory};

// ── Helpers ────────────────────────────────────────────────────

fn sqlite_backend(dir: &std::path::Path) -> SqliteMemory {
    SqliteMemory::new(dir).expect("SQLite init failed")
}

fn markdown_backend(dir: &std::path::Path) -> MarkdownMemory {
    MarkdownMemory::new(dir)
}

// ── Test 1: Store performance ──────────────────────────────────

#[tokio::test]
async fn compare_store_speed() {
    let tmp_sq = TempDir::new().unwrap();
    let tmp_md = TempDir::new().unwrap();
    let sq = sqlite_backend(tmp_sq.path());
    let md = markdown_backend(tmp_md.path());

    let n = 100;

    // SQLite: 100 stores
    let start = Instant::now();
    for i in 0..n {
        sq.store(
            &format!("key_{i}"),
            &format!("Memory entry number {i} about Rust programming"),
            MemoryCategory::Core,
            None,
        )
        .await
        .unwrap();
    }
    let sq_dur = start.elapsed();

    // Markdown: 100 stores
    let start = Instant::now();
    for i in 0..n {
        md.store(
            &format!("key_{i}"),
            &format!("Memory entry number {i} about Rust programming"),
            MemoryCategory::Core,
            None,
        )
        .await
        .unwrap();
    }
    let md_dur = start.elapsed();

    println!("\n============================================================");
    println!("STORE {n} entries:");
    println!("  SQLite:   {:?}", sq_dur);
    println!("  Markdown: {:?}", md_dur);

    // Both should succeed
    assert_eq!(sq.count().await.unwrap(), n);
    // Markdown count parses lines, may differ slightly from n
    let md_count = md.count().await.unwrap();
    assert!(md_count >= n, "Markdown stored {md_count}, expected >= {n}");
}

// ── Test 2: Recall / search quality ────────────────────────────

#[tokio::test]
async fn compare_recall_quality() {
    let tmp_sq = TempDir::new().unwrap();
    let tmp_md = TempDir::new().unwrap();
    let sq = sqlite_backend(tmp_sq.path());
    let md = markdown_backend(tmp_md.path());

    // Seed both with identical data
    let entries = vec![
        (
            "lang_pref",
            "User prefers Rust over Python",
            MemoryCategory::Core,
        ),
        (
            "editor",
            "Uses VS Code with rust-analyzer",
            MemoryCategory::Core,
        ),
        ("tz", "Timezone is EST, works 9-5", MemoryCategory::Core),
        (
            "proj1",
            "Working on ZeroClaw AI assistant",
            MemoryCategory::Daily,
        ),
        (
            "proj2",
            "Previous project was a web scraper in Python",
            MemoryCategory::Daily,
        ),
        (
            "deploy",
            "Deploys to Hetzner VPS via Docker",
            MemoryCategory::Core,
        ),
        (
            "model",
            "Prefers Claude Sonnet for coding tasks",
            MemoryCategory::Core,
        ),
        (
            "style",
            "Likes concise responses, no fluff",
            MemoryCategory::Core,
        ),
        (
            "rust_note",
            "Rust's ownership model prevents memory bugs",
            MemoryCategory::Daily,
        ),
        (
            "perf",
            "Cares about binary size and startup time",
            MemoryCategory::Core,
        ),
    ];

    for (key, content, cat) in &entries {
        sq.store(key, content, cat.clone(), None).await.unwrap();
        md.store(key, content, cat.clone(), None).await.unwrap();
    }

    // Test queries and compare results
    let queries = vec![
        ("Rust", "Should find Rust-related entries"),
        ("Python", "Should find Python references"),
        ("deploy Docker", "Multi-keyword search"),
        ("Claude", "Specific tool reference"),
        ("javascript", "No matches expected"),
        ("binary size startup", "Multi-keyword partial match"),
    ];

    println!("\n============================================================");
    println!("RECALL QUALITY (10 entries seeded):\n");

    for (query, desc) in &queries {
        let sq_results = sq.recall(query, 10, None).await.unwrap();
        let md_results = md.recall(query, 10, None).await.unwrap();

        println!("  Query: \"{query}\" — {desc}");
        println!("    SQLite:   {} results", sq_results.len());
        for r in &sq_results {
            println!(
                "      [{:.2}] {}: {}",
                r.score.unwrap_or(0.0),
                r.key,
                &r.content[..r.content.len().min(50)]
            );
        }
        println!("    Markdown: {} results", md_results.len());
        for r in &md_results {
            println!(
                "      [{:.2}] {}: {}",
                r.score.unwrap_or(0.0),
                r.key,
                &r.content[..r.content.len().min(50)]
            );
        }
        println!();
    }
}

// ── Test 3: Recall speed at scale ──────────────────────────────

#[tokio::test]
async fn compare_recall_speed() {
    let tmp_sq = TempDir::new().unwrap();
    let tmp_md = TempDir::new().unwrap();
    let sq = sqlite_backend(tmp_sq.path());
    let md = markdown_backend(tmp_md.path());

    // Seed 200 entries
    let n = 200;
    for i in 0..n {
        let content = if i % 3 == 0 {
            format!("Rust is great for systems programming, entry {i}")
        } else if i % 3 == 1 {
            format!("Python is popular for data science, entry {i}")
        } else {
            format!("TypeScript powers modern web apps, entry {i}")
        };
        sq.store(&format!("e{i}"), &content, MemoryCategory::Core, None)
            .await
            .unwrap();
        md.store(&format!("e{i}"), &content, MemoryCategory::Daily, None)
            .await
            .unwrap();
    }

    // Benchmark recall
    let start = Instant::now();
    let sq_results = sq.recall("Rust systems", 10, None).await.unwrap();
    let sq_dur = start.elapsed();

    let start = Instant::now();
    let md_results = md.recall("Rust systems", 10, None).await.unwrap();
    let md_dur = start.elapsed();

    println!("\n============================================================");
    println!("RECALL from {n} entries (query: \"Rust systems\", limit 10):");
    println!("  SQLite:   {:?} → {} results", sq_dur, sq_results.len());
    println!("  Markdown: {:?} → {} results", md_dur, md_results.len());

    // Both should find results
    assert!(!sq_results.is_empty());
    assert!(!md_results.is_empty());
}

// ── Test 4: Persistence (SQLite wins by design) ────────────────

#[tokio::test]
async fn compare_persistence() {
    let tmp_sq = TempDir::new().unwrap();
    let tmp_md = TempDir::new().unwrap();

    // Store in both, then drop and re-open
    {
        let sq = sqlite_backend(tmp_sq.path());
        sq.store(
            "persist_test",
            "I should survive",
            MemoryCategory::Core,
            None,
        )
        .await
        .unwrap();
    }
    {
        let md = markdown_backend(tmp_md.path());
        md.store(
            "persist_test",
            "I should survive",
            MemoryCategory::Core,
            None,
        )
        .await
        .unwrap();
    }

    // Re-open
    let sq2 = sqlite_backend(tmp_sq.path());
    let md2 = markdown_backend(tmp_md.path());

    let sq_entry = sq2.get("persist_test").await.unwrap();
    let md_entry = md2.get("persist_test").await.unwrap();

    println!("\n============================================================");
    println!("PERSISTENCE (store → drop → re-open → get):");
    println!(
        "  SQLite:   {}",
        if sq_entry.is_some() {
            "✅ Survived"
        } else {
            "❌ Lost"
        }
    );
    println!(
        "  Markdown: {}",
        if md_entry.is_some() {
            "✅ Survived"
        } else {
            "❌ Lost"
        }
    );

    // SQLite should always persist by key
    assert!(sq_entry.is_some());
    assert_eq!(sq_entry.unwrap().content, "I should survive");

    // Markdown persists content to files (get uses content search)
    assert!(md_entry.is_some());
}

// ── Test 5: Upsert / update behavior ──────────────────────────

#[tokio::test]
async fn compare_upsert() {
    let tmp_sq = TempDir::new().unwrap();
    let tmp_md = TempDir::new().unwrap();
    let sq = sqlite_backend(tmp_sq.path());
    let md = markdown_backend(tmp_md.path());

    // Store twice with same key, different content
    sq.store("pref", "likes Rust", MemoryCategory::Core, None)
        .await
        .unwrap();
    sq.store("pref", "loves Rust", MemoryCategory::Core, None)
        .await
        .unwrap();

    md.store("pref", "likes Rust", MemoryCategory::Core, None)
        .await
        .unwrap();
    md.store("pref", "loves Rust", MemoryCategory::Core, None)
        .await
        .unwrap();

    let sq_count = sq.count().await.unwrap();
    let md_count = md.count().await.unwrap();

    let sq_entry = sq.get("pref").await.unwrap();
    let md_results = md.recall("loves Rust", 5, None).await.unwrap();

    println!("\n============================================================");
    println!("UPSERT (store same key twice):");
    println!(
        "  SQLite:   count={sq_count}, latest=\"{}\"",
        sq_entry.as_ref().map_or("none", |e| &e.content)
    );
    println!("  Markdown: count={md_count} (append-only, both entries kept)");
    println!("    Can still find latest: {}", !md_results.is_empty());

    // SQLite: upsert replaces, count stays at 1
    assert_eq!(sq_count, 1);
    assert_eq!(sq_entry.unwrap().content, "loves Rust");

    // Markdown: append-only, count increases
    assert!(md_count >= 2, "Markdown should keep both entries");
}

// ── Test 6: Forget / delete capability ─────────────────────────

#[tokio::test]
async fn compare_forget() {
    let tmp_sq = TempDir::new().unwrap();
    let tmp_md = TempDir::new().unwrap();
    let sq = sqlite_backend(tmp_sq.path());
    let md = markdown_backend(tmp_md.path());

    sq.store("secret", "API key: sk-1234", MemoryCategory::Core, None)
        .await
        .unwrap();
    md.store("secret", "API key: sk-1234", MemoryCategory::Core, None)
        .await
        .unwrap();

    let sq_forgot = sq.forget("secret").await.unwrap();
    let md_forgot = md.forget("secret").await.unwrap();

    println!("\n============================================================");
    println!("FORGET (delete sensitive data):");
    println!(
        "  SQLite:   {} (count={})",
        if sq_forgot { "✅ Deleted" } else { "❌ Kept" },
        sq.count().await.unwrap()
    );
    println!(
        "  Markdown: {} (append-only by design)",
        if md_forgot {
            "✅ Deleted"
        } else {
            "⚠️  Cannot delete (audit trail)"
        },
    );

    // SQLite can delete
    assert!(sq_forgot);
    assert_eq!(sq.count().await.unwrap(), 0);

    // Markdown cannot delete (by design)
    assert!(!md_forgot);
}

// ── Test 7: Category filtering ─────────────────────────────────

#[tokio::test]
async fn compare_category_filter() {
    let tmp_sq = TempDir::new().unwrap();
    let tmp_md = TempDir::new().unwrap();
    let sq = sqlite_backend(tmp_sq.path());
    let md = markdown_backend(tmp_md.path());

    // Mix of categories
    sq.store("a", "core fact 1", MemoryCategory::Core, None)
        .await
        .unwrap();
    sq.store("b", "core fact 2", MemoryCategory::Core, None)
        .await
        .unwrap();
    sq.store("c", "daily note", MemoryCategory::Daily, None)
        .await
        .unwrap();
    sq.store("d", "convo msg", MemoryCategory::Conversation, None)
        .await
        .unwrap();

    md.store("a", "core fact 1", MemoryCategory::Core, None)
        .await
        .unwrap();
    md.store("b", "core fact 2", MemoryCategory::Core, None)
        .await
        .unwrap();
    md.store("c", "daily note", MemoryCategory::Daily, None)
        .await
        .unwrap();

    let sq_core = sq.list(Some(&MemoryCategory::Core), None).await.unwrap();
    let sq_daily = sq.list(Some(&MemoryCategory::Daily), None).await.unwrap();
    let sq_conv = sq
        .list(Some(&MemoryCategory::Conversation), None)
        .await
        .unwrap();
    let sq_all = sq.list(None, None).await.unwrap();

    let md_core = md.list(Some(&MemoryCategory::Core), None).await.unwrap();
    let md_daily = md.list(Some(&MemoryCategory::Daily), None).await.unwrap();
    let md_all = md.list(None, None).await.unwrap();

    println!("\n============================================================");
    println!("CATEGORY FILTERING:");
    println!(
        "  SQLite:   core={}, daily={}, conv={}, all={}",
        sq_core.len(),
        sq_daily.len(),
        sq_conv.len(),
        sq_all.len()
    );
    println!(
        "  Markdown: core={}, daily={}, all={}",
        md_core.len(),
        md_daily.len(),
        md_all.len()
    );

    // SQLite: precise category filtering via SQL WHERE
    assert_eq!(sq_core.len(), 2);
    assert_eq!(sq_daily.len(), 1);
    assert_eq!(sq_conv.len(), 1);
    assert_eq!(sq_all.len(), 4);

    // Markdown: categories determined by file location
    assert!(!md_core.is_empty());
    assert!(!md_all.is_empty());
}
