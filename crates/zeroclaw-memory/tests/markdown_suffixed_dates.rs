use std::fs;
use zeroclaw_memory::{MarkdownMemory, Memory};

#[tokio::test]
async fn markdown_recall_applies_time_bounds_to_suffixed_daily_files() {
    let workspace = tempfile::tempdir().unwrap();
    let daily = workspace.path().join("memory");
    fs::create_dir(&daily).unwrap();
    for (name, content) in [
        ("2026-01-01_release.md", "release preparation"),
        ("2026-01-02-0800.md", "release checklist"),
        ("2026-01-03-1700.md", "release follow-up"),
    ] {
        fs::write(daily.join(name), content).unwrap();
    }
    fs::write(workspace.path().join("MEMORY.md"), "release policy").unwrap();
    let memory = MarkdownMemory::new("default", workspace.path());

    let all = memory
        .recall("release", 10, None, None, None)
        .await
        .unwrap();
    assert_eq!(all.len(), 4);
    let mut within_day: Vec<_> = memory
        .recall(
            "release",
            10,
            None,
            Some("2026-01-02T00:00:00Z"),
            Some("2026-01-02T23:59:59Z"),
        )
        .await
        .unwrap()
        .into_iter()
        .map(|entry| entry.content)
        .collect();
    within_day.sort();

    assert_eq!(within_day, ["release checklist", "release policy"]);
}
