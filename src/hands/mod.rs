pub mod types;

pub use types::{Hand, HandContext, HandRun, HandRunStatus};

use anyhow::{Context, Result};
use std::path::Path;

/// Load all hand definitions from TOML files in the given directory.
///
/// Each `.toml` file in `hands_dir` is expected to deserialize into a [`Hand`].
/// Files that fail to parse are logged and skipped.
pub fn load_hands(hands_dir: &Path) -> Result<Vec<Hand>> {
    if !hands_dir.is_dir() {
        return Ok(Vec::new());
    }

    let mut hands = Vec::new();
    let entries = std::fs::read_dir(hands_dir)
        .with_context(|| format!("failed to read hands directory: {}", hands_dir.display()))?;

    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("toml") {
            continue;
        }
        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("failed to read hand file: {}", path.display()))?;
        match toml::from_str::<Hand>(&content) {
            Ok(hand) => hands.push(hand),
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "skipping malformed hand file");
            }
        }
    }

    Ok(hands)
}

/// Load the rolling context for a hand.
///
/// Reads from `{hands_dir}/{name}/context.json`. Returns a fresh
/// [`HandContext`] if the file does not exist yet.
pub fn load_hand_context(hands_dir: &Path, name: &str) -> Result<HandContext> {
    let path = hands_dir.join(name).join("context.json");
    if !path.exists() {
        return Ok(HandContext::new(name));
    }
    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read hand context: {}", path.display()))?;
    let ctx: HandContext = serde_json::from_str(&content)
        .with_context(|| format!("failed to parse hand context: {}", path.display()))?;
    Ok(ctx)
}

/// Persist the rolling context for a hand.
///
/// Writes to `{hands_dir}/{name}/context.json`, creating the
/// directory if it does not exist.
pub fn save_hand_context(hands_dir: &Path, context: &HandContext) -> Result<()> {
    let dir = hands_dir.join(&context.hand_name);
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("failed to create hand context dir: {}", dir.display()))?;
    let path = dir.join("context.json");
    let json = serde_json::to_string_pretty(context)?;
    std::fs::write(&path, json)
        .with_context(|| format!("failed to write hand context: {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write_hand_toml(dir: &Path, filename: &str, content: &str) {
        std::fs::write(dir.join(filename), content).unwrap();
    }

    #[test]
    fn load_hands_empty_dir() {
        let tmp = TempDir::new().unwrap();
        let hands = load_hands(tmp.path()).unwrap();
        assert!(hands.is_empty());
    }

    #[test]
    fn load_hands_nonexistent_dir() {
        let hands = load_hands(Path::new("/nonexistent/path/hands")).unwrap();
        assert!(hands.is_empty());
    }

    #[test]
    fn load_hands_parses_valid_files() {
        let tmp = TempDir::new().unwrap();
        write_hand_toml(
            tmp.path(),
            "scanner.toml",
            r#"
name = "scanner"
description = "Market scanner"
prompt = "Scan markets."

[schedule]
kind = "cron"
expr = "0 9 * * *"
"#,
        );
        write_hand_toml(
            tmp.path(),
            "digest.toml",
            r#"
name = "digest"
description = "News digest"
prompt = "Digest news."

[schedule]
kind = "every"
every_ms = 3600000
"#,
        );

        let hands = load_hands(tmp.path()).unwrap();
        assert_eq!(hands.len(), 2);
    }

    #[test]
    fn load_hands_skips_malformed_files() {
        let tmp = TempDir::new().unwrap();
        write_hand_toml(tmp.path(), "bad.toml", "this is not valid toml struct");
        write_hand_toml(
            tmp.path(),
            "good.toml",
            r#"
name = "good"
description = "A good hand"
prompt = "Do good things."

[schedule]
kind = "every"
every_ms = 60000
"#,
        );

        let hands = load_hands(tmp.path()).unwrap();
        assert_eq!(hands.len(), 1);
        assert_eq!(hands[0].name, "good");
    }

    #[test]
    fn load_hands_ignores_non_toml_files() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("readme.md"), "# Hands").unwrap();
        std::fs::write(tmp.path().join("notes.txt"), "some notes").unwrap();

        let hands = load_hands(tmp.path()).unwrap();
        assert!(hands.is_empty());
    }

    #[test]
    fn context_roundtrip_through_filesystem() {
        let tmp = TempDir::new().unwrap();
        let mut ctx = HandContext::new("test-hand");
        let run = HandRun {
            hand_name: "test-hand".into(),
            run_id: "run-001".into(),
            started_at: chrono::Utc::now(),
            finished_at: Some(chrono::Utc::now()),
            status: HandRunStatus::Completed,
            findings: vec!["found something".into()],
            knowledge_added: vec!["learned something".into()],
            duration_ms: Some(500),
        };
        ctx.record_run(run, 100);

        save_hand_context(tmp.path(), &ctx).unwrap();
        let loaded = load_hand_context(tmp.path(), "test-hand").unwrap();

        assert_eq!(loaded.hand_name, "test-hand");
        assert_eq!(loaded.total_runs, 1);
        assert_eq!(loaded.history.len(), 1);
        assert_eq!(loaded.learned_facts.len(), 1);
        assert!(loaded.learned_facts.contains("learned something"));
    }

    #[test]
    fn load_context_returns_fresh_when_missing() {
        let tmp = TempDir::new().unwrap();
        let ctx = load_hand_context(tmp.path(), "nonexistent").unwrap();
        assert_eq!(ctx.hand_name, "nonexistent");
        assert_eq!(ctx.total_runs, 0);
        assert!(ctx.history.is_empty());
    }

    #[test]
    fn save_context_creates_directory() {
        let tmp = TempDir::new().unwrap();
        let ctx = HandContext::new("new-hand");
        save_hand_context(tmp.path(), &ctx).unwrap();

        assert!(tmp.path().join("new-hand").join("context.json").exists());
    }

    #[test]
    fn save_then_load_preserves_multiple_runs() {
        let tmp = TempDir::new().unwrap();
        let mut ctx = HandContext::new("multi");

        for i in 0..5 {
            let run = HandRun {
                hand_name: "multi".into(),
                run_id: format!("run-{i:03}"),
                started_at: chrono::Utc::now(),
                finished_at: Some(chrono::Utc::now()),
                status: HandRunStatus::Completed,
                findings: vec![format!("finding-{i}")],
                knowledge_added: vec![format!("fact-{i}")],
                duration_ms: Some(100),
            };
            ctx.record_run(run, 3);
        }

        save_hand_context(tmp.path(), &ctx).unwrap();
        let loaded = load_hand_context(tmp.path(), "multi").unwrap();

        assert_eq!(loaded.total_runs, 5);
        assert_eq!(loaded.history.len(), 3, "history capped at max_history=3");
        assert_eq!(loaded.learned_facts.len(), 5);
    }
}
