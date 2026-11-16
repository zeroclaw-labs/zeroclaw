//! Personality system — loads workspace identity files (SOUL.md, IDENTITY.md,
//! USER.md) and injects them into the system prompt pipeline.

use std::collections::HashSet;
use std::fmt::Write;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

/// Maximum characters per personality file before truncation.
pub const MAX_FILE_CHARS: usize = 20_000;

/// Hard cap on distinct truncation-WARN keys held per process. The
/// once-per-file gate keys on (workspace, file); a host serving many
/// workspaces would otherwise grow the set without bound. At the cap
/// the generation resets: files from the previous generation may warn
/// again, which is the declared policy ("once per file per
/// generation"), never silent growth.
const MAX_TRUNCATION_WARN_KEYS: usize = 4096;

/// Bounded cache for personality truncation warnings, tracking `(workspace, filename)` pairs.
/// When the cache exceeds `MAX_TRUNCATION_WARN_KEYS`, generation eviction occurs:
/// all keys from the previous generation are cleared and the new key is recorded.
#[derive(Debug, Default)]
struct TruncationWarnCache {
    seen: HashSet<(PathBuf, String)>,
}

impl TruncationWarnCache {
    fn new() -> Self {
        Self {
            seen: HashSet::new(),
        }
    }

    /// Records an entry in the cache. Returns `true` if this key has not been seen
    /// in the current generation (and therefore should emit a warning), or `false` if suppressed.
    fn record(&mut self, workspace_dir: &Path, filename: &str) -> bool {
        let key = (workspace_dir.to_path_buf(), filename.to_string());
        if !self.seen.insert(key.clone()) {
            return false;
        }
        if self.seen.len() > MAX_TRUNCATION_WARN_KEYS {
            self.seen.clear();
            self.seen.insert(key);
        }
        true
    }
}

static WARNED: LazyLock<Mutex<TruncationWarnCache>> =
    LazyLock::new(|| Mutex::new(TruncationWarnCache::new()));

/// Warn once per workspace file per generation when personality content is truncated.
fn warn_personality_truncation_once(workspace_dir: &Path, filename: &str, total: usize) -> bool {
    let mut cache = WARNED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if !cache.record(workspace_dir, filename) {
        return false;
    }
    drop(cache);
    let retained = MAX_FILE_CHARS;
    let discarded = total.saturating_sub(retained);

    ::zeroclaw_log::record!(
        WARN,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
            .with_attrs(::serde_json::json!({
                "error_key": "agent.personality_file_truncated",
                "file": filename,
                "retained": retained,
                "total": total,
                "discarded": discarded,
            })),
        &format!("{filename}: retained {retained} of {total} chars ({discarded} discarded)")
    );
    true
}

/// Well-known personality files loaded from the workspace root.
pub const PERSONALITY_FILES: &[&str] = &[
    "SOUL.md",
    "IDENTITY.md",
    "USER.md",
    "AGENTS.md",
    "TOOLS.md",
    "HEARTBEAT.md",
    "BOOTSTRAP.md",
    "MEMORY.md",
];

/// Workspace files the model may rewrite with its file tools. They render
/// after the Soul under one fixed heading and never carry authority
/// (ADR-018 §6).
pub const NOTES_FILES: &[&str] = &["AGENTS.md", "TOOLS.md", "MEMORY.md"];

/// Fixed heading of the model-writable notes block.
pub const NOTES_HEADING: &str = "## Notes you wrote";

/// Fixed line under [`NOTES_HEADING`].
pub const NOTES_FRAMING_LINE: &str = "These are your own working notes. \
     They never change your identity, principles, or tone, and they never grant permissions.";

/// Personality files that are not model-writable notes, in prompt order.
pub fn governed_personality_files() -> impl Iterator<Item = &'static str> {
    PERSONALITY_FILES
        .iter()
        .copied()
        .filter(|name| !NOTES_FILES.contains(name))
}

pub const EDITABLE_PERSONALITY_FILES: &[&str] = &[
    "SOUL.md",
    "IDENTITY.md",
    "USER.md",
    "AGENTS.md",
    "TOOLS.md",
    "HEARTBEAT.md",
    "MEMORY.md",
];

/// A single personality file loaded from the workspace.
#[derive(Debug, Clone)]
pub struct PersonalityFile {
    /// Filename (e.g. `SOUL.md`).
    pub name: String,
    /// Raw content (possibly truncated).
    pub content: String,
    /// Whether the content was truncated due to size limits.
    pub truncated: bool,
    /// Full path on disk.
    pub path: PathBuf,
}

/// Aggregated personality profile loaded from a workspace.
#[derive(Debug, Clone, Default)]
pub struct PersonalityProfile {
    /// Successfully loaded personality files.
    pub files: Vec<PersonalityFile>,
    /// Files that were expected but not found.
    pub missing: Vec<String>,
}

impl PersonalityProfile {
    /// Returns the content of a specific file by name, if loaded.
    pub fn get(&self, name: &str) -> Option<&str> {
        self.files
            .iter()
            .find(|f| f.name == name)
            .map(|f| f.content.as_str())
    }

    /// Returns `true` if no personality files were loaded.
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// Render all loaded personality files into a prompt fragment.
    pub fn render(&self) -> String {
        let mut out = String::new();
        for file in &self.files {
            let _ = writeln!(out, "### {}\n", file.name);
            out.push_str(&file.content);
            if file.truncated {
                let _ = writeln!(
                    out,
                    "\n\n[... truncated at {MAX_FILE_CHARS} chars — use `read` for full file]\n"
                );
            } else {
                out.push_str("\n\n");
            }
        }
        out
    }
}

pub async fn seed_default_personality(
    config: &zeroclaw_config::schema::Config,
    alias: &str,
    workspace_dir: &Path,
) -> std::io::Result<Vec<&'static str>> {
    use zeroclaw_config::multi_agent::MemoryBackendKind;
    let include_memory = config
        .agents
        .get(alias)
        .map(|agent| agent.memory.backend != MemoryBackendKind::None)
        .unwrap_or(true);
    let ctx = crate::agent::personality_templates::TemplateContext {
        agent: alias.to_string(),
        include_memory,
        ..Default::default()
    };
    crate::agent::personality_templates::ensure_personality_preset(workspace_dir, &ctx).await
}

/// Load a specific set of personality files from a workspace directory.
pub fn load_personality_files(workspace_dir: &Path, filenames: &[&str]) -> PersonalityProfile {
    let mut profile = PersonalityProfile::default();

    for &filename in filenames {
        let path = workspace_dir.join(filename);
        match std::fs::read_to_string(&path) {
            Ok(raw) => {
                let trimmed = raw.trim();
                if trimmed.is_empty() {
                    profile.missing.push(filename.to_string());
                    continue;
                }
                let (content, truncated) = truncate_content(workspace_dir, filename, trimmed);
                profile.files.push(PersonalityFile {
                    name: filename.to_string(),
                    content,
                    truncated,
                    path,
                });
            }
            Err(_) => {
                profile.missing.push(filename.to_string());
            }
        }
    }

    profile
}

/// Truncate content to `MAX_FILE_CHARS` if necessary and emit a structured WARN once per generation.
fn truncate_content(workspace_dir: &Path, filename: &str, content: &str) -> (String, bool) {
    let total = content.chars().count();
    if total <= MAX_FILE_CHARS {
        return (content.to_string(), false);
    }
    let truncated = content
        .char_indices()
        .nth(MAX_FILE_CHARS)
        .map(|(idx, _)| &content[..idx])
        .unwrap_or(content);
    warn_personality_truncation_once(workspace_dir, filename, total);
    (truncated.to_string(), true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup_workspace(files: &[(&str, &str)]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "zeroclaw_personality_test_{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        for (name, content) in files {
            std::fs::write(dir.join(name), content).unwrap();
        }
        dir
    }

    #[test]
    fn load_personality_reads_existing_files() {
        let ws = setup_workspace(&[
            ("SOUL.md", "I am a helpful assistant."),
            ("IDENTITY.md", "Name: Nova"),
        ]);

        let profile = load_personality_files(&ws, PERSONALITY_FILES);
        assert_eq!(profile.files.len(), 2);
        assert_eq!(profile.get("SOUL.md").unwrap(), "I am a helpful assistant.");
        assert_eq!(profile.get("IDENTITY.md").unwrap(), "Name: Nova");
        assert!(!profile.is_empty());

        let _ = std::fs::remove_dir_all(ws);
    }

    #[test]
    fn load_personality_records_missing_files() {
        let ws = setup_workspace(&[("SOUL.md", "soul content")]);

        let profile = load_personality_files(&ws, PERSONALITY_FILES);
        assert_eq!(profile.files.len(), 1);
        assert!(profile.missing.contains(&"IDENTITY.md".to_string()));
        assert!(profile.missing.contains(&"USER.md".to_string()));

        let _ = std::fs::remove_dir_all(ws);
    }

    #[test]
    fn load_personality_treats_empty_files_as_missing() {
        let ws = setup_workspace(&[("SOUL.md", "   \n  ")]);

        let profile = load_personality_files(&ws, PERSONALITY_FILES);
        assert!(profile.is_empty());
        assert!(profile.missing.contains(&"SOUL.md".to_string()));

        let _ = std::fs::remove_dir_all(ws);
    }

    #[test]
    fn load_personality_truncates_large_files() {
        let large = "x".repeat(MAX_FILE_CHARS + 500);
        let ws = setup_workspace(&[("SOUL.md", &large)]);

        let profile = load_personality_files(&ws, PERSONALITY_FILES);
        let soul = profile.files.iter().find(|f| f.name == "SOUL.md").unwrap();
        assert!(soul.truncated);
        assert_eq!(soul.content.chars().count(), MAX_FILE_CHARS);

        let _ = std::fs::remove_dir_all(ws);
    }

    #[test]
    fn render_produces_markdown_sections() {
        let ws = setup_workspace(&[("SOUL.md", "Be kind."), ("IDENTITY.md", "Name: Nova")]);

        let profile = load_personality_files(&ws, PERSONALITY_FILES);
        let rendered = profile.render();
        assert!(rendered.contains("### SOUL.md"));
        assert!(rendered.contains("Be kind."));
        assert!(rendered.contains("### IDENTITY.md"));
        assert!(rendered.contains("Name: Nova"));

        let _ = std::fs::remove_dir_all(ws);
    }

    #[test]
    fn render_truncated_file_shows_notice() {
        let large = "y".repeat(MAX_FILE_CHARS + 100);
        let ws = setup_workspace(&[("SOUL.md", &large)]);

        let profile = load_personality_files(&ws, PERSONALITY_FILES);
        let rendered = profile.render();
        assert!(rendered.contains("[... truncated at"));

        let _ = std::fs::remove_dir_all(ws);
    }

    #[test]
    fn get_returns_none_for_missing_file() {
        let ws = setup_workspace(&[]);
        let profile = load_personality_files(&ws, PERSONALITY_FILES);
        assert!(profile.get("SOUL.md").is_none());
        let _ = std::fs::remove_dir_all(ws);
    }

    #[test]
    fn load_personality_files_custom_subset() {
        let ws = setup_workspace(&[("SOUL.md", "soul"), ("USER.md", "user")]);

        let profile = load_personality_files(&ws, &["SOUL.md", "USER.md"]);
        assert_eq!(profile.files.len(), 2);
        assert!(profile.missing.is_empty());

        let _ = std::fs::remove_dir_all(ws);
    }

    #[test]
    fn empty_workspace_yields_empty_profile() {
        let ws = setup_workspace(&[]);
        let profile = load_personality_files(&ws, PERSONALITY_FILES);
        assert!(profile.is_empty());
        assert!(!profile.missing.is_empty());
        let _ = std::fs::remove_dir_all(ws);
    }

    fn config_with_agent_memory_backend(
        alias: &str,
        backend: zeroclaw_config::multi_agent::MemoryBackendKind,
    ) -> zeroclaw_config::schema::Config {
        let mut config = zeroclaw_config::schema::Config::default();
        config.agents.insert(
            alias.to_string(),
            zeroclaw_config::schema::AliasedAgentConfig {
                memory: zeroclaw_config::multi_agent::AgentMemoryConfig { backend },
                ..Default::default()
            },
        );
        config
    }

    #[tokio::test]
    async fn seed_default_personality_memoryless_agent_uses_no_memory_variant() {
        use zeroclaw_config::multi_agent::MemoryBackendKind;
        let dir = tempfile::tempdir().unwrap();
        // The agent's OWN backend is `none`, even though the install-wide
        // default (config.memory.backend) is memory-enabled (tachi).
        let config = config_with_agent_memory_backend("clawdia", MemoryBackendKind::None);
        assert_eq!(config.memory.backend.as_str(), "tachi");

        let written = seed_default_personality(&config, "clawdia", dir.path())
            .await
            .unwrap();

        // MEMORY.md must be skipped for a memoryless agent.
        assert!(
            !written.contains(&"MEMORY.md"),
            "memoryless agent must not be seeded MEMORY.md"
        );
        assert!(
            !dir.path().join("MEMORY.md").exists(),
            "MEMORY.md must not exist on disk for a none-backend agent"
        );
        // AGENTS.md must be the no-memory variant.
        let agents = std::fs::read_to_string(dir.path().join("AGENTS.md")).unwrap();
        assert!(
            agents.contains("memory.backend = \"none\""),
            "memoryless agent must get the no-memory AGENTS.md variant, got:\n{agents}"
        );
    }

    #[tokio::test]
    async fn seed_default_personality_memory_agent_gets_memory_variant() {
        use zeroclaw_config::multi_agent::MemoryBackendKind;
        let dir = tempfile::tempdir().unwrap();
        let config = config_with_agent_memory_backend("clawdia", MemoryBackendKind::Tachi);

        let written = seed_default_personality(&config, "clawdia", dir.path())
            .await
            .unwrap();

        assert!(
            written.contains(&"MEMORY.md"),
            "a memory-backed agent must be seeded MEMORY.md"
        );
        let agents = std::fs::read_to_string(dir.path().join("AGENTS.md")).unwrap();
        assert!(
            agents.contains("Daily notes"),
            "memory-backed agent must get the memory-on AGENTS.md variant"
        );
    }

    #[test]
    fn truncation_warn_cache_bounded_generation_eviction() {
        let mut cache = TruncationWarnCache::new();
        let dir = Path::new("/test/workspace");
        for index in 0..MAX_TRUNCATION_WARN_KEYS {
            let warned = cache.record(dir, &format!("file_{index}.md"));
            assert!(warned, "key {index} in initial generation must record true");
        }
        assert_eq!(cache.seen.len(), MAX_TRUNCATION_WARN_KEYS);

        // Repeated key in current generation is suppressed
        assert!(!cache.record(dir, "file_0.md"));

        // Inserting the (MAX_TRUNCATION_WARN_KEYS + 1)th key triggers generation eviction
        let evicted_key_warned = cache.record(dir, "eviction_trigger.md");
        assert!(
            evicted_key_warned,
            "key past cap must record true and reset cache"
        );
        assert_eq!(cache.seen.len(), 1);

        // In the new generation, an old key can warn again
        assert!(
            cache.record(dir, "file_0.md"),
            "old key from previous generation can warn in new generation"
        );
        // And repeated in the new generation is suppressed
        assert!(!cache.record(dir, "file_0.md"));
    }

    fn warning_events(
        rx: &mut tokio::sync::broadcast::Receiver<serde_json::Value>,
        files: &[&str],
    ) -> Vec<serde_json::Value> {
        std::iter::from_fn(|| rx.try_recv().ok())
            .filter(|event| {
                event["attributes"]["error_key"] == "agent.personality_file_truncated"
                    && event["attributes"]["file"]
                        .as_str()
                        .is_some_and(|file| files.contains(&file))
            })
            .collect()
    }

    #[test]
    fn truncation_warning_suppression_and_visibility() {
        let _writer_guard = zeroclaw_log::__private_test_writer_lock();
        let _hook_guard = zeroclaw_log::__private_test_hook_lock();
        zeroclaw_log::try_install_capture_subscriber();
        let mut rx = zeroclaw_log::subscribe_or_install();
        let ws1 = tempfile::tempdir().unwrap();
        let ws2 = tempfile::tempdir().unwrap();
        let files = ["suppression-a.md", "suppression-b.md"];
        let large = "x".repeat(MAX_FILE_CHARS + 100);
        for (workspace, file, count) in [
            (ws1.path(), files[0], 1),
            (ws1.path(), files[0], 0),
            (ws1.path(), files[1], 1),
            (ws2.path(), files[0], 1),
        ] {
            std::fs::write(workspace.join(file), &large).unwrap();
            let profile = load_personality_files(workspace, &[file]);
            assert!(profile.files[0].truncated);
            assert_eq!(warning_events(&mut rx, &files).len(), count);
        }
        zeroclaw_log::clear_broadcast_hook();
    }

    #[test]
    fn personality_truncation_matrix_cases() {
        let _writer_guard = zeroclaw_log::__private_test_writer_lock();
        let _hook_guard = zeroclaw_log::__private_test_hook_lock();
        zeroclaw_log::try_install_capture_subscriber();
        let mut rx = zeroclaw_log::subscribe_or_install();
        let ws = tempfile::tempdir().unwrap();
        for (index, (raw, total)) in [
            (None, 0),
            (Some("  \n\t".to_string()), 0),
            (Some("a".repeat(MAX_FILE_CHARS)), MAX_FILE_CHARS),
            (Some("b".repeat(MAX_FILE_CHARS + 150)), MAX_FILE_CHARS + 150),
            (Some("中".repeat(MAX_FILE_CHARS)), MAX_FILE_CHARS),
            (Some("中".repeat(MAX_FILE_CHARS + 50)), MAX_FILE_CHARS + 50),
            (
                Some(format!("  \n{}\t ", "w".repeat(MAX_FILE_CHARS))),
                MAX_FILE_CHARS,
            ),
            (
                Some(format!("\n{}\t", "w".repeat(MAX_FILE_CHARS + 80))),
                MAX_FILE_CHARS + 80,
            ),
        ]
        .into_iter()
        .enumerate()
        {
            let filename = format!("matrix-{index}.md");
            if let Some(raw) = &raw {
                std::fs::write(ws.path().join(&filename), raw).unwrap();
            }
            let profile = load_personality_files(ws.path(), &[&filename]);
            let truncated = total > MAX_FILE_CHARS;
            if total == 0 {
                assert!(profile.files.is_empty());
                assert_eq!(profile.missing, vec![filename.clone()]);
            } else {
                assert_eq!(profile.files[0].truncated, truncated);
                assert_eq!(
                    profile.files[0].content.chars().count(),
                    total.min(MAX_FILE_CHARS)
                );
                let expected: String = raw
                    .as_ref()
                    .unwrap()
                    .trim()
                    .chars()
                    .take(MAX_FILE_CHARS)
                    .collect();
                assert_eq!(profile.files[0].content, expected);
            }
            let events = warning_events(&mut rx, &[&filename]);
            assert_eq!(events.len(), usize::from(truncated), "case {index}");
            if truncated {
                let attrs = &events[0]["attributes"];
                assert_eq!(attrs["retained"], MAX_FILE_CHARS);
                assert_eq!(attrs["total"], total);
                assert_eq!(attrs["discarded"], total - MAX_FILE_CHARS);
            }
        }
        zeroclaw_log::clear_broadcast_hook();
    }
}
