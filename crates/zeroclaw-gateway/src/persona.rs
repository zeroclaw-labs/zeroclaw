//! Named persona presets — bundles of (provider, model, personality, mode)
//! that the dashboard's slot settings drawer offers as one-click slot
//! identities.
//!
//! Storage shape: `<workspace_dir>/personas/<name>.toml`. One TOML doc
//! per preset; `name` is sandboxed against the same regex personality
//! filenames use — `[A-Za-z0-9._-]+`. Workspace-scoped (rather than
//! `~/.zeroclaw/`) so multi-workspace setups isolate persona collections
//! the way they isolate personality files; tests use a tempdir.
//!
//! On first read of an empty/missing personas dir, four defaults are
//! seeded — `claude-code-default`, `codex-researcher`,
//! `gemini-cli-coder`, `bedrock-claude` — wiring the existing
//! CLI-delegating + cloud providers to a one-click identity. Defaults
//! are inert until the user customises them; deletion is permanent.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[cfg(feature = "schema-export")]
use schemars::JsonSchema;

use crate::slot::SlotMode;

/// Static allowlist regex for persona names. Matches the personality
/// filename rules (no path separators, no whitespace, no `..`). Kept
/// inline rather than reaching for the `regex` crate so persona
/// validation has no extra dep.
fn name_is_safe(name: &str) -> bool {
    if name.is_empty() || name.len() > 64 {
        return false;
    }
    name.chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        && !name.starts_with('.')
}

/// One persona preset.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(JsonSchema))]
pub struct PersonaPreset {
    /// Stable identifier — used as the TOML filename and as the
    /// `persona_preset` value the slot stores.
    pub name: String,
    /// Provider id — must match an entry in `[providers.models]`.
    pub provider: String,
    /// Optional model override; defaults to the provider's configured
    /// model when omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Optional personality filename (must be in
    /// `EDITABLE_PERSONALITY_FILES` for the runtime to pick it up).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub personality: Option<String>,
    /// Operating mode the slot should run in. Defaults to Normal.
    #[serde(default)]
    pub mode: SlotMode,
    /// Human-readable description shown in the dropdown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// `GET /api/personas` response.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "schema-export", derive(JsonSchema))]
pub struct PersonaListResponse {
    pub personas: Vec<PersonaPreset>,
}

/// Error body for persona endpoints. `code` is a stable string the
/// frontend can switch on.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(JsonSchema))]
pub struct PersonaError {
    pub code: String,
    pub message: String,
}

impl PersonaError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}

/// Resolve the personas directory under a given workspace dir.
pub fn personas_dir(workspace_dir: &Path) -> PathBuf {
    workspace_dir.join("personas")
}

/// Path to a single persona's TOML file. Caller is responsible for
/// validating `name` first via [`validate_name`].
pub fn persona_path(workspace_dir: &Path, name: &str) -> PathBuf {
    personas_dir(workspace_dir).join(format!("{name}.toml"))
}

/// Validate a candidate persona name. Returns the borrowed name on
/// success so the caller can build a path with confidence.
pub fn validate_name(name: &str) -> Result<&str, PersonaError> {
    if name_is_safe(name) {
        Ok(name)
    } else {
        Err(PersonaError::new(
            "invalid_persona_name",
            format!(
                "Persona name {name:?} is invalid; must match [A-Za-z0-9._-]+ \
                 (1..=64 chars, no leading dot)",
            ),
        ))
    }
}

/// The four default presets seeded on first list of an empty personas
/// dir. Hand-curated to exercise the existing provider zoo: a
/// CLI-delegating Anthropic provider, an OpenAI Codex CLI provider, a
/// Gemini CLI provider, and a Bedrock cloud entry.
pub fn default_presets() -> Vec<PersonaPreset> {
    vec![
        PersonaPreset {
            name: "claude-code-default".into(),
            provider: "claude_code".into(),
            model: None,
            personality: None,
            mode: SlotMode::Normal,
            description: Some("Claude Code CLI delegate — workspace-aware coding".into()),
        },
        PersonaPreset {
            name: "codex-researcher".into(),
            provider: "openai_codex".into(),
            model: Some("gpt-5".into()),
            personality: None,
            mode: SlotMode::Normal,
            description: Some("OpenAI Codex CLI — research and exploration".into()),
        },
        PersonaPreset {
            name: "gemini-cli-coder".into(),
            provider: "gemini_cli".into(),
            model: None,
            personality: None,
            mode: SlotMode::Normal,
            description: Some("Gemini CLI — long-context code generation".into()),
        },
        PersonaPreset {
            name: "bedrock-claude".into(),
            provider: "bedrock".into(),
            model: Some("anthropic.claude-3-5-sonnet-20241022-v2:0".into()),
            personality: None,
            mode: SlotMode::Normal,
            description: Some("Anthropic Claude on AWS Bedrock — enterprise inference".into()),
        },
    ]
}

/// Read every `<personas_dir>/*.toml` into a sorted Vec. Files that
/// fail to parse are logged and skipped — a single broken preset
/// doesn't take the whole list down.
///
/// Sort order: alphabetical by `name` so dropdowns render
/// deterministically.
pub fn load_all(workspace_dir: &Path) -> std::io::Result<Vec<PersonaPreset>> {
    let dir = personas_dir(workspace_dir);
    if !dir.exists() {
        return Ok(Vec::new());
    }

    let mut presets = Vec::new();
    for entry in std::fs::read_dir(&dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("toml") {
            continue;
        }
        match std::fs::read_to_string(&path) {
            Ok(raw) => match toml::from_str::<PersonaPreset>(&raw) {
                Ok(preset) => presets.push(preset),
                Err(e) => {
                    tracing::warn!(
                        path = %path.display(),
                        error = %e,
                        "skipping malformed persona TOML",
                    );
                }
            },
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "failed to read persona TOML",
                );
            }
        }
    }
    presets.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(presets)
}

/// Read one persona by name. Returns `Ok(None)` when the file does
/// not exist (no error case there — the API maps this to 404).
pub fn load_one(workspace_dir: &Path, name: &str) -> std::io::Result<Option<PersonaPreset>> {
    let path = persona_path(workspace_dir, name);
    match std::fs::read_to_string(&path) {
        Ok(raw) => match toml::from_str::<PersonaPreset>(&raw) {
            Ok(preset) => Ok(Some(preset)),
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "malformed persona TOML");
                Ok(None)
            }
        },
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err),
    }
}

/// Write one persona. Creates the personas dir on first call.
pub fn save_one(workspace_dir: &Path, preset: &PersonaPreset) -> std::io::Result<()> {
    let dir = personas_dir(workspace_dir);
    std::fs::create_dir_all(&dir)?;
    let path = persona_path(workspace_dir, &preset.name);
    let raw = toml::to_string_pretty(preset).map_err(|e| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("failed to serialize persona: {e}"),
        )
    })?;
    std::fs::write(&path, raw)
}

/// Delete one persona. Returns `true` when something was removed,
/// `false` when the file did not exist.
pub fn delete_one(workspace_dir: &Path, name: &str) -> std::io::Result<bool> {
    let path = persona_path(workspace_dir, name);
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(true),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(err),
    }
}

/// Seed the four default presets when the personas dir is empty or
/// missing. No-op when any persona already exists. Callers should
/// invoke this from `GET /api/personas` so the dashboard's first
/// request gets a populated dropdown without explicit user setup.
pub fn seed_defaults_if_empty(workspace_dir: &Path) -> std::io::Result<bool> {
    let dir = personas_dir(workspace_dir);
    if dir.exists() {
        let mut has_any = false;
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            if entry.path().extension().and_then(|e| e.to_str()) == Some("toml") {
                has_any = true;
                break;
            }
        }
        if has_any {
            return Ok(false);
        }
    }
    for preset in default_presets() {
        save_one(workspace_dir, &preset)?;
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_is_safe_accepts_valid_examples() {
        for name in [
            "claude-code-default",
            "codex_researcher",
            "preset.1",
            "A",
            "abc-123",
        ] {
            assert!(name_is_safe(name), "should accept {name:?}");
        }
    }

    #[test]
    fn name_is_safe_rejects_invalid_examples() {
        for name in [
            "",
            ".hidden",
            "../escape",
            "with space",
            "with/slash",
            "🦀-emoji",
            &"x".repeat(65),
        ] {
            assert!(!name_is_safe(name), "should reject {name:?}");
        }
    }

    #[test]
    fn personas_dir_joins_workspace() {
        let p = personas_dir(Path::new("/tmp/ws"));
        assert_eq!(p, Path::new("/tmp/ws/personas"));
    }

    #[test]
    fn persona_path_appends_toml() {
        let p = persona_path(Path::new("/tmp/ws"), "x");
        assert_eq!(p, Path::new("/tmp/ws/personas/x.toml"));
    }

    #[test]
    fn validate_name_returns_invalid_for_bad_input() {
        let err = validate_name("../etc").expect_err("traversal must reject");
        assert_eq!(err.code, "invalid_persona_name");
    }

    #[test]
    fn save_then_load_round_trips() {
        let tmp = tempfile::TempDir::new().unwrap();
        let preset = PersonaPreset {
            name: "round-trip".into(),
            provider: "anthropic".into(),
            model: Some("claude-sonnet-4".into()),
            personality: Some("SOUL.md".into()),
            mode: SlotMode::Trust,
            description: Some("test".into()),
        };
        save_one(tmp.path(), &preset).unwrap();
        let loaded = load_one(tmp.path(), "round-trip").unwrap().unwrap();
        assert_eq!(loaded.name, "round-trip");
        assert_eq!(loaded.provider, "anthropic");
        assert_eq!(loaded.model.as_deref(), Some("claude-sonnet-4"));
        assert_eq!(loaded.personality.as_deref(), Some("SOUL.md"));
        assert!(matches!(loaded.mode, SlotMode::Trust));
    }

    #[test]
    fn load_all_returns_empty_when_dir_missing() {
        let tmp = tempfile::TempDir::new().unwrap();
        let presets = load_all(tmp.path()).unwrap();
        assert!(presets.is_empty());
    }

    #[test]
    fn load_all_skips_malformed_and_returns_sorted() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = personas_dir(tmp.path());
        std::fs::create_dir_all(&dir).unwrap();
        // Valid B then valid A; load_all must alphabetise to A,B.
        save_one(
            tmp.path(),
            &PersonaPreset {
                name: "b-second".into(),
                provider: "openai".into(),
                model: None,
                personality: None,
                mode: SlotMode::Normal,
                description: None,
            },
        )
        .unwrap();
        save_one(
            tmp.path(),
            &PersonaPreset {
                name: "a-first".into(),
                provider: "anthropic".into(),
                model: None,
                personality: None,
                mode: SlotMode::Normal,
                description: None,
            },
        )
        .unwrap();
        // Garbage TOML should be silently skipped.
        std::fs::write(dir.join("broken.toml"), "this is = not toml\n[[[").unwrap();
        // Non-TOML extension is skipped before parse is attempted.
        std::fs::write(dir.join("README.md"), "# nope").unwrap();

        let presets = load_all(tmp.path()).unwrap();
        let names: Vec<&str> = presets.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, vec!["a-first", "b-second"]);
    }

    #[test]
    fn delete_one_returns_false_for_missing() {
        let tmp = tempfile::TempDir::new().unwrap();
        let removed = delete_one(tmp.path(), "ghost").unwrap();
        assert!(!removed);
    }

    #[test]
    fn delete_one_returns_true_after_save() {
        let tmp = tempfile::TempDir::new().unwrap();
        save_one(
            tmp.path(),
            &PersonaPreset {
                name: "to-delete".into(),
                provider: "ollama".into(),
                model: None,
                personality: None,
                mode: SlotMode::Normal,
                description: None,
            },
        )
        .unwrap();
        assert!(delete_one(tmp.path(), "to-delete").unwrap());
        // Second delete returns false (now missing).
        assert!(!delete_one(tmp.path(), "to-delete").unwrap());
    }

    #[test]
    fn seed_defaults_if_empty_seeds_when_missing() {
        let tmp = tempfile::TempDir::new().unwrap();
        let seeded = seed_defaults_if_empty(tmp.path()).unwrap();
        assert!(seeded);
        let presets = load_all(tmp.path()).unwrap();
        assert_eq!(presets.len(), 4);
        assert!(presets.iter().any(|p| p.name == "claude-code-default"));
        assert!(presets.iter().any(|p| p.name == "codex-researcher"));
        assert!(presets.iter().any(|p| p.name == "gemini-cli-coder"));
        assert!(presets.iter().any(|p| p.name == "bedrock-claude"));
    }

    #[test]
    fn seed_defaults_if_empty_is_noop_when_populated() {
        let tmp = tempfile::TempDir::new().unwrap();
        save_one(
            tmp.path(),
            &PersonaPreset {
                name: "user-custom".into(),
                provider: "openai".into(),
                model: None,
                personality: None,
                mode: SlotMode::Normal,
                description: None,
            },
        )
        .unwrap();
        let seeded = seed_defaults_if_empty(tmp.path()).unwrap();
        assert!(
            !seeded,
            "seed must be a no-op when a persona already exists"
        );
        let presets = load_all(tmp.path()).unwrap();
        assert_eq!(presets.len(), 1);
        assert_eq!(presets[0].name, "user-custom");
    }
}
