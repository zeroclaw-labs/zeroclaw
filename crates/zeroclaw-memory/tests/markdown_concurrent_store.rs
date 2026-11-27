use std::sync::Arc;
use zeroclaw_memory::{MarkdownMemory, Memory, MemoryCategory};

/// 32 concurrent `store()` calls through one shared `Memory` handle (same
/// process, same instance) must all survive. This is the shape an in-process
/// mutex on the struct would mask; the append path must not depend on one.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn concurrent_stores_through_a_shared_handle_lose_no_row() {
    let dir = tempfile::tempdir().unwrap();
    let mem: Arc<dyn Memory> = Arc::new(MarkdownMemory::new("markdown", dir.path()));
    let n = 32;
    let mut handles = Vec::new();
    for i in 0..n {
        let m = Arc::clone(&mem);
        handles.push(zeroclaw_spawn::spawn!(async move {
            m.store(
                &format!("key{i}"),
                &format!("value{i}"),
                MemoryCategory::Core,
                None,
            )
            .await
            .unwrap();
        }));
    }
    for h in handles {
        h.await.unwrap();
    }
    let content = std::fs::read_to_string(dir.path().join("MEMORY.md")).unwrap();
    let kept = (0..n)
        .filter(|i| content.contains(&format!("**key{i}**")))
        .count();
    assert_eq!(kept, n);
}

/// A `MEMORY.md` pre-seeded with 20 rows, hit by 8 concurrent `store()`
/// calls, must keep every pre-existing row and gain every new one. The old
/// read-modify-write path could lose some or all of the 20 seeded rows, not
/// just the new ones, because a truncating write from one task could land
/// between another task's read and its own write.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn concurrent_stores_never_drop_preexisting_rows() {
    let rounds = 50;
    for _ in 0..rounds {
        let dir = tempfile::tempdir().unwrap();
        let mut seed = String::from("# Long-Term Memory\n\n");
        for p in 0..20 {
            seed.push_str(&format!("- **prev{p}**: old\n"));
        }
        std::fs::write(dir.path().join("MEMORY.md"), seed).unwrap();
        let mem: Arc<dyn Memory> = Arc::new(MarkdownMemory::new("markdown", dir.path()));
        let mut handles = Vec::new();
        for i in 0..8 {
            let m = Arc::clone(&mem);
            handles.push(zeroclaw_spawn::spawn!(async move {
                m.store(&format!("new{i}"), "v", MemoryCategory::Core, None)
                    .await
                    .unwrap();
            }));
        }
        for h in handles {
            h.await.unwrap();
        }
        let content = std::fs::read_to_string(dir.path().join("MEMORY.md")).unwrap();
        let prev = (0..20)
            .filter(|p| content.contains(&format!("**prev{p}**")))
            .count();
        let new = (0..8)
            .filter(|i| content.contains(&format!("**new{i}**")))
            .count();
        assert_eq!(prev, 20, "a pre-existing row was lost");
        assert_eq!(new, 8, "a concurrent store was lost");
    }
}
