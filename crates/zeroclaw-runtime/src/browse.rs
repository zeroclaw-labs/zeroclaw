//! Scoped one-level directory browser. Gateway (`api_browse.rs`), CLI
//! (`src/browse.rs`), and the future TUI directory picker all reach the
//! same canonical implementation here.

use std::path::PathBuf;

use serde::Serialize;

use zeroclaw_config::paths::{RootEscapeError, resolve_under};
use zeroclaw_config::schema::Config;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BrowseEntry {
    pub name: String,
    /// `"dir"` or `"file"`. Symlinks resolve through their target.
    pub kind: &'static str,
    /// File size in bytes. `None` for directories.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    /// Set when this entry is on the runtime's protected list and the
    /// dashboard must hide delete/rename affordances. Server-side checks
    /// (delete/move/mkdir) reject mutations on these regardless of UI.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub protected: bool,
}

#[derive(Debug, Clone)]
pub struct BrowseResult {
    /// Path relative to `<install>/shared/` that the result describes.
    /// Useful for breadcrumb rendering.
    pub path: String,
    pub entries: Vec<BrowseEntry>,
}

#[derive(Debug, thiserror::Error)]
pub enum BrowseError {
    #[error(transparent)]
    Escape(#[from] RootEscapeError),
    #[error("'{0}' is not an agent alias")]
    InvalidAgent(String),
    #[error("path '{0}' does not exist")]
    NotFound(String),
    #[error("path '{0}' is not a directory")]
    NotADirectory(String),
    #[error("'{0}' is a system directory and cannot be removed via the dashboard")]
    Protected(String),
    #[error("'{0}' is a system file and cannot be modified or removed via the dashboard")]
    ProtectedFile(String),
    #[error("file '{0}' exceeds the {1}-byte read cap; download via CLI or zeroclaw shell")]
    TooLarge(String, u64),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Browse one level of `<install>/shared/<raw>`. Returns entries sorted by
/// (kind, name) — directories first, then files, alphabetical within each.
pub fn list_directory(config: &Config, raw: &str) -> Result<BrowseResult, BrowseError> {
    let mut result = list_under_root(&config.shared_workspace_dir(), raw)?;
    if raw.trim_matches('/').is_empty() {
        for entry in &mut result.entries {
            if entry.kind == "dir" && PROTECTED_SHARED_TOP_LEVEL.contains(&entry.name.as_str()) {
                entry.protected = true;
            }
        }
    }
    Ok(result)
}

fn list_under_root(root: &std::path::Path, raw: &str) -> Result<BrowseResult, BrowseError> {
    let resolved: PathBuf = resolve_under(root, raw)?;

    let metadata = match std::fs::metadata(&resolved) {
        Ok(m) => m,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Err(BrowseError::NotFound(raw.to_string()));
        }
        Err(err) => return Err(err.into()),
    };
    if !metadata.is_dir() {
        return Err(BrowseError::NotADirectory(raw.to_string()));
    }

    let mut entries: Vec<BrowseEntry> = Vec::new();
    for child in std::fs::read_dir(&resolved)?.flatten() {
        let Ok(file_type) = child.file_type() else {
            continue;
        };
        let name = child.file_name().to_string_lossy().into_owned();
        if file_type.is_dir() {
            entries.push(BrowseEntry {
                name,
                kind: "dir",
                size: None,
                protected: false,
            });
        } else if file_type.is_file() {
            let size = child.metadata().ok().map(|m| m.len());
            entries.push(BrowseEntry {
                name,
                kind: "file",
                size,
                protected: false,
            });
        }
    }
    entries.sort_by(|a, b| (a.kind, &a.name).cmp(&(b.kind, &b.name)));

    Ok(BrowseResult {
        path: raw.trim_matches('/').to_string(),
        entries,
    })
}

const PROTECTED_SHARED_TOP_LEVEL: &[&str] = &["skills", "skill-bundles", "knowledge"];

/// Create a new directory at `<install>/shared/<raw>`. Idempotent — if the
/// path already exists as a directory, returns Ok without re-creating.
/// Rejects path traversal and refuses to create over an existing file.
pub fn make_directory(config: &Config, raw: &str) -> Result<(), BrowseError> {
    let shared = config.shared_workspace_dir();
    let resolved: PathBuf = resolve_under(&shared, raw)?;
    if let Ok(meta) = std::fs::metadata(&resolved) {
        if meta.is_dir() {
            return Ok(());
        }
        return Err(BrowseError::NotADirectory(raw.to_string()));
    }
    std::fs::create_dir_all(&resolved)?;
    Ok(())
}

/// Delete the directory at `<install>/shared/<raw>` recursively. Refuses
/// to remove protected top-level entries (skills/, skill-bundles/,
/// knowledge/) or the shared root itself. Rejects path traversal.
pub fn remove_directory(config: &Config, raw: &str) -> Result<(), BrowseError> {
    let shared = config.shared_workspace_dir();
    let (resolved, relative) = resolve_relative(&shared, raw)?;
    if relative.is_empty() {
        return Err(BrowseError::Protected("shared".to_string()));
    }
    if PROTECTED_SHARED_TOP_LEVEL.contains(&relative.as_str()) {
        return Err(BrowseError::Protected(format!("shared/{relative}")));
    }
    let metadata = match std::fs::metadata(&resolved) {
        Ok(m) => m,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Err(BrowseError::NotFound(raw.to_string()));
        }
        Err(err) => return Err(err.into()),
    };
    if !metadata.is_dir() {
        return Err(BrowseError::NotADirectory(raw.to_string()));
    }
    std::fs::remove_dir_all(&resolved)?;
    Ok(())
}

/// Hard byte cap on file-read responses. Anything larger surfaces as
/// `BrowseError::TooLarge`; the dashboard can offer a CLI hint.
pub const AGENT_WORKSPACE_READ_CAP: u64 = 4 * 1024 * 1024; // 4 MiB

const AGENT_WORKSPACE_PROTECTED_FILES: &[&str] = &[
    "IDENTITY.md",
    "SOUL.md",
    "USER.md",
    "AGENTS.md",
    "MEMORY.md",
    "DAILY.md",
];

/// Top-level agent-workspace directories the runtime owns. `sessions/`
/// holds the per-agent session DB (`sessions/sessions.db`) created on first
/// session write by `zeroclaw_infra::session_sqlite`. Deleting it wipes
/// session history.
const AGENT_WORKSPACE_PROTECTED_DIRS: &[&str] = &["sessions"];

/// Resolve `raw` under `root` and return the resolved path together with its
/// normalized path relative to `root`: `/`-separated, and empty for the root
/// itself.
///
/// Every check on what an operation may touch runs on that relative form,
/// never on the raw input. `resolve_under` folds `..` lexically, so a raw
/// path such as `x/..` or `x/../SOUL.md` passes a check on its own text
/// while resolving to the root or a protected entry.
fn resolve_relative(root: &std::path::Path, raw: &str) -> Result<(PathBuf, String), BrowseError> {
    let resolved = resolve_under(root, raw)?;
    let normalized_root = resolve_under(root, "")?;
    let relative = resolved
        .strip_prefix(&normalized_root)
        .unwrap_or(resolved.as_path())
        .components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/");
    Ok((resolved, relative))
}

/// The workspace root for `agent_alias`. The alias becomes a path component
/// under `<install>/agents/`, so it must be exactly one plain component:
/// `..`, a separator, or an absolute path would otherwise select a directory
/// outside the agents tree, which `PathBuf::join` would happily produce.
fn agent_root(config: &Config, agent_alias: &str) -> Result<PathBuf, BrowseError> {
    use std::path::Component;
    let mut components = std::path::Path::new(agent_alias).components();
    match (components.next(), components.next()) {
        (Some(Component::Normal(_)), None) => Ok(config.agent_workspace_dir(agent_alias)),
        _ => Err(BrowseError::InvalidAgent(agent_alias.to_string())),
    }
}

fn protected_file(rel: &str) -> bool {
    AGENT_WORKSPACE_PROTECTED_FILES.contains(&rel)
}

fn protected_dir(rel: &str) -> bool {
    AGENT_WORKSPACE_PROTECTED_DIRS.contains(&rel)
}

/// One-level listing inside the agent's workspace. Top-level entries that
/// match the protected file/dir lists are tagged so the dashboard hides
/// destructive affordances; server-side mutations still re-check.
pub fn list_agent_workspace(
    config: &Config,
    agent_alias: &str,
    raw: &str,
) -> Result<BrowseResult, BrowseError> {
    let mut result = list_under_root(&agent_root(config, agent_alias)?, raw)?;
    if raw.trim_matches('/').is_empty() {
        for entry in &mut result.entries {
            entry.protected = match entry.kind {
                "file" => protected_file(&entry.name),
                "dir" => protected_dir(&entry.name),
                _ => false,
            };
        }
    }
    Ok(result)
}

/// Create a directory under the agent's workspace. Idempotent — if the
/// path already exists as a directory, returns Ok. Rejects path traversal
/// and refuses to create over an existing file or to overwrite a protected
/// top-level file path.
pub fn make_agent_workspace_directory(
    config: &Config,
    agent_alias: &str,
    raw: &str,
) -> Result<(), BrowseError> {
    let root = agent_root(config, agent_alias)?;
    let (resolved, relative) = resolve_relative(&root, raw)?;
    if relative.is_empty() {
        return Err(BrowseError::NotFound(raw.to_string()));
    }
    if protected_file(&relative) {
        return Err(BrowseError::ProtectedFile(relative));
    }
    if let Ok(meta) = std::fs::metadata(&resolved) {
        if meta.is_dir() {
            return Ok(());
        }
        return Err(BrowseError::NotADirectory(raw.to_string()));
    }
    std::fs::create_dir_all(&resolved)?;
    Ok(())
}

/// Result of reading a file from the agent workspace.
#[derive(Debug, Clone)]
pub struct FileReadResult {
    pub path: String,
    pub bytes: Vec<u8>,
    pub size: u64,
    /// True when the bytes look like UTF-8 text. Drives whether the
    /// dashboard renders inline or offers a download.
    pub is_text: bool,
}

/// A directory listing as it goes over the wire. The dashboard's HTTP
/// adapter and the `workspace/list` RPC method both serialize this, so the
/// two bodies cannot drift apart.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BrowseListing {
    pub path: String,
    pub entries: Vec<BrowseEntry>,
}

impl From<BrowseResult> for BrowseListing {
    fn from(result: BrowseResult) -> Self {
        Self {
            path: result.path,
            entries: result.entries,
        }
    }
}

/// A single-file read as it goes over the wire, shared by the HTTP adapter
/// and the `fs/read` RPC method.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FileReadBody {
    pub path: String,
    pub size: u64,
    pub is_text: bool,
    /// UTF-8 text when `is_text` is true, base64 when false, so a client can
    /// render inline without a second round-trip for a binary preview.
    pub content: String,
    pub encoding: &'static str,
}

impl From<FileReadResult> for FileReadBody {
    fn from(result: FileReadResult) -> Self {
        use base64::Engine;
        let (content, encoding) = if result.is_text {
            (String::from_utf8(result.bytes).unwrap_or_default(), "utf8")
        } else {
            (
                base64::engine::general_purpose::STANDARD.encode(&result.bytes),
                "base64",
            )
        };
        Self {
            path: result.path,
            size: result.size,
            is_text: result.is_text,
            content,
            encoding,
        }
    }
}

/// Read a file from the agent's workspace. Refuses paths that don't
/// resolve to a regular file; enforces the size cap.
pub fn read_agent_workspace_file(
    config: &Config,
    agent_alias: &str,
    raw: &str,
) -> Result<FileReadResult, BrowseError> {
    let root = agent_root(config, agent_alias)?;
    let resolved: PathBuf = resolve_under(&root, raw)?;
    let metadata = match std::fs::metadata(&resolved) {
        Ok(m) => m,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Err(BrowseError::NotFound(raw.to_string()));
        }
        Err(err) => return Err(err.into()),
    };
    if !metadata.is_file() {
        return Err(BrowseError::NotADirectory(raw.to_string()));
    }
    if metadata.len() > AGENT_WORKSPACE_READ_CAP {
        return Err(BrowseError::TooLarge(
            raw.to_string(),
            AGENT_WORKSPACE_READ_CAP,
        ));
    }
    let bytes = std::fs::read(&resolved)?;
    let is_text = std::str::from_utf8(&bytes).is_ok();
    Ok(FileReadResult {
        path: raw.trim_matches('/').to_string(),
        size: metadata.len(),
        bytes,
        is_text,
    })
}

/// Delete a file or directory inside the agent's workspace. Recursive
/// for directories. Refuses to delete the workspace root itself or any
/// of the protected bootstrap files.
pub fn delete_agent_workspace_path(
    config: &Config,
    agent_alias: &str,
    raw: &str,
) -> Result<(), BrowseError> {
    let root = agent_root(config, agent_alias)?;
    let (resolved, relative) = resolve_relative(&root, raw)?;
    if relative.is_empty() {
        return Err(BrowseError::Protected(format!(
            "agents/{agent_alias}/workspace"
        )));
    }
    if protected_file(&relative) {
        return Err(BrowseError::ProtectedFile(relative));
    }
    if protected_dir(&relative) {
        return Err(BrowseError::Protected(format!(
            "agents/{agent_alias}/workspace/{relative}"
        )));
    }
    let metadata = match std::fs::metadata(&resolved) {
        Ok(m) => m,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Err(BrowseError::NotFound(raw.to_string()));
        }
        Err(err) => return Err(err.into()),
    };
    if metadata.is_dir() {
        std::fs::remove_dir_all(&resolved)?;
    } else {
        std::fs::remove_file(&resolved)?;
    }
    Ok(())
}

/// Move (rename) a path inside the agent's workspace. Both `from` and
/// `to` are relative to the workspace root; both must stay inside it.
/// Refuses to touch protected files on either side.
pub fn move_agent_workspace_path(
    config: &Config,
    agent_alias: &str,
    from: &str,
    to: &str,
) -> Result<(), BrowseError> {
    let root = agent_root(config, agent_alias)?;
    let (src, from_trimmed) = resolve_relative(&root, from)?;
    let (dst, to_trimmed) = resolve_relative(&root, to)?;
    let (from_trimmed, to_trimmed) = (from_trimmed.as_str(), to_trimmed.as_str());
    if from_trimmed.is_empty() || to_trimmed.is_empty() {
        return Err(BrowseError::NotFound(from.to_string()));
    }
    if protected_file(from_trimmed) || protected_file(to_trimmed) {
        return Err(BrowseError::ProtectedFile(
            if protected_file(from_trimmed) {
                from_trimmed
            } else {
                to_trimmed
            }
            .to_string(),
        ));
    }
    if protected_dir(from_trimmed) || protected_dir(to_trimmed) {
        return Err(BrowseError::Protected(format!(
            "agents/{agent_alias}/workspace/{}",
            if protected_dir(from_trimmed) {
                from_trimmed
            } else {
                to_trimmed
            }
        )));
    }
    if !src.exists() {
        return Err(BrowseError::NotFound(from.to_string()));
    }
    if dst.exists() {
        return Err(BrowseError::NotADirectory(format!(
            "target '{to_trimmed}' already exists"
        )));
    }
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::rename(&src, &dst)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn fixture() -> (TempDir, Config) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("shared/skills/alpha")).unwrap();
        std::fs::create_dir_all(dir.path().join("shared/skills/beta")).unwrap();
        std::fs::write(dir.path().join("shared/readme.txt"), b"hi").unwrap();

        let cfg = Config {
            config_path: dir.path().join("config.toml"),
            ..Config::default()
        };
        (dir, cfg)
    }

    /// An alias is spliced into `<install>/agents/<alias>/workspace`, so any
    /// alias that is not one plain component would select a directory outside
    /// the agents tree. A `workspace` directory is planted where each escape
    /// would land; every operation must refuse the alias before touching it.
    #[test]
    fn agent_workspace_operations_refuse_an_alias_that_escapes_the_agents_tree() {
        let (dir, cfg) = fixture();
        let outside = dir.path().join("elsewhere");
        std::fs::create_dir_all(outside.join("workspace")).unwrap();
        std::fs::write(outside.join("workspace/secret.txt"), b"s3cret").unwrap();
        std::fs::create_dir_all(dir.path().join("workspace")).unwrap();
        std::fs::write(dir.path().join("workspace/secret.txt"), b"s3cret").unwrap();

        let absolute = outside.to_string_lossy().to_string();
        for alias in ["..", "../elsewhere", "a/b", ".", "", absolute.as_str()] {
            assert!(
                matches!(
                    list_agent_workspace(&cfg, alias, ""),
                    Err(BrowseError::InvalidAgent(_))
                ),
                "list must refuse alias {alias:?}"
            );
            assert!(
                matches!(
                    read_agent_workspace_file(&cfg, alias, "secret.txt"),
                    Err(BrowseError::InvalidAgent(_))
                ),
                "read must refuse alias {alias:?}"
            );
            assert!(
                matches!(
                    delete_agent_workspace_path(&cfg, alias, "secret.txt"),
                    Err(BrowseError::InvalidAgent(_))
                ),
                "delete must refuse alias {alias:?}"
            );
            assert!(
                matches!(
                    move_agent_workspace_path(&cfg, alias, "secret.txt", "moved.txt"),
                    Err(BrowseError::InvalidAgent(_))
                ),
                "move must refuse alias {alias:?}"
            );
            assert!(
                matches!(
                    make_agent_workspace_directory(&cfg, alias, "made"),
                    Err(BrowseError::InvalidAgent(_))
                ),
                "mkdir must refuse alias {alias:?}"
            );
        }
        assert!(outside.join("workspace/secret.txt").exists());
        assert!(dir.path().join("workspace/secret.txt").exists());
        assert!(!outside.join("workspace/made").exists());

        // A plain alias still resolves under the agents tree.
        std::fs::create_dir_all(dir.path().join("agents/alpha/workspace")).unwrap();
        assert!(list_agent_workspace(&cfg, "alpha", "").is_ok());
    }

    /// Guards run on the path as resolved, not as written: `..` folded into a
    /// path must not reach the root or a protected entry that the same path
    /// written plainly is refused.
    #[test]
    fn mutations_check_the_resolved_path_not_the_raw_one() {
        let (dir, cfg) = fixture();
        std::fs::create_dir_all(dir.path().join("shared/knowledge")).unwrap();
        let workspace = dir.path().join("agents/alpha/workspace");
        std::fs::create_dir_all(workspace.join("notes")).unwrap();
        std::fs::create_dir_all(workspace.join("sessions")).unwrap();
        std::fs::write(workspace.join("SOUL.md"), b"soul").unwrap();

        // Shared area: the root and protected top-level directories.
        for raw in [
            "skills/..",
            "x/..",
            "x/../skills",
            "skills/alpha/../..",
            "./knowledge",
        ] {
            assert!(
                matches!(remove_directory(&cfg, raw), Err(BrowseError::Protected(_))),
                "rmdir {raw:?}"
            );
        }
        assert!(dir.path().join("shared/skills/alpha").exists());
        assert!(dir.path().join("shared/knowledge").exists());

        // Agent workspace: the root, a protected file and a protected directory.
        for raw in ["notes/..", "x/..", "./"] {
            assert!(
                matches!(
                    delete_agent_workspace_path(&cfg, "alpha", raw),
                    Err(BrowseError::Protected(_))
                ),
                "delete {raw:?}"
            );
        }
        assert!(matches!(
            delete_agent_workspace_path(&cfg, "alpha", "notes/../SOUL.md"),
            Err(BrowseError::ProtectedFile(_))
        ));
        assert!(matches!(
            delete_agent_workspace_path(&cfg, "alpha", "notes/../sessions"),
            Err(BrowseError::Protected(_))
        ));
        assert!(workspace.join("SOUL.md").exists());
        assert!(workspace.join("sessions").exists());

        // Move: from or to the root, or onto or off a protected name.
        assert!(move_agent_workspace_path(&cfg, "alpha", "notes/..", "moved").is_err());
        assert!(matches!(
            move_agent_workspace_path(&cfg, "alpha", "notes/../SOUL.md", "soul-copy.md"),
            Err(BrowseError::ProtectedFile(_))
        ));
        assert!(matches!(
            move_agent_workspace_path(&cfg, "alpha", "notes", "x/../SOUL.md"),
            Err(BrowseError::ProtectedFile(_))
        ));
        assert!(matches!(
            move_agent_workspace_path(&cfg, "alpha", "notes", "x/../sessions"),
            Err(BrowseError::Protected(_))
        ));
        assert!(workspace.join("notes").exists());
        assert!(workspace.join("SOUL.md").exists());

        // mkdir cannot squat a protected file name through `..` either.
        assert!(matches!(
            make_agent_workspace_directory(&cfg, "alpha", "notes/../SOUL.md"),
            Err(BrowseError::ProtectedFile(_))
        ));

        // Ordinary nested paths still work.
        std::fs::create_dir_all(dir.path().join("shared/scratch/inner")).unwrap();
        remove_directory(&cfg, "scratch/inner/..").unwrap();
        assert!(!dir.path().join("shared/scratch").exists());
    }

    #[test]
    fn lists_shared_root_when_path_empty() {
        let (_dir, cfg) = fixture();
        let result = list_directory(&cfg, "").unwrap();
        assert_eq!(result.entries.len(), 2);
        assert_eq!(result.entries[0].name, "skills");
        assert_eq!(result.entries[0].kind, "dir");
        assert_eq!(result.entries[1].name, "readme.txt");
        assert_eq!(result.entries[1].kind, "file");
    }

    #[test]
    fn descends_one_level() {
        let (_dir, cfg) = fixture();
        let result = list_directory(&cfg, "skills").unwrap();
        let names: Vec<_> = result.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["alpha", "beta"]);
    }

    #[test]
    fn rejects_escape() {
        let (_dir, cfg) = fixture();
        let err = list_directory(&cfg, "../etc").unwrap_err();
        assert!(matches!(err, BrowseError::Escape(_)));
    }

    #[test]
    fn errors_on_missing_path() {
        let (_dir, cfg) = fixture();
        let err = list_directory(&cfg, "ghost").unwrap_err();
        assert!(matches!(err, BrowseError::NotFound(_)));
    }

    #[test]
    fn errors_when_path_is_a_file() {
        let (_dir, cfg) = fixture();
        let err = list_directory(&cfg, "readme.txt").unwrap_err();
        assert!(matches!(err, BrowseError::NotADirectory(_)));
    }

    #[test]
    fn make_directory_creates_nested_path() {
        let (dir, cfg) = fixture();
        make_directory(&cfg, "skills/gamma/sub").unwrap();
        assert!(dir.path().join("shared/skills/gamma/sub").is_dir());
    }

    #[test]
    fn make_directory_is_idempotent() {
        let (_dir, cfg) = fixture();
        make_directory(&cfg, "skills/alpha").unwrap();
        make_directory(&cfg, "skills/alpha").unwrap();
    }

    #[test]
    fn make_directory_rejects_escape() {
        let (_dir, cfg) = fixture();
        let err = make_directory(&cfg, "../etc").unwrap_err();
        assert!(matches!(err, BrowseError::Escape(_)));
    }

    #[test]
    fn make_directory_refuses_over_existing_file() {
        let (_dir, cfg) = fixture();
        let err = make_directory(&cfg, "readme.txt").unwrap_err();
        assert!(matches!(err, BrowseError::NotADirectory(_)));
    }

    #[test]
    fn remove_directory_recursively_drops_subtree() {
        let (dir, cfg) = fixture();
        make_directory(&cfg, "skills/alpha/nested/deep").unwrap();
        remove_directory(&cfg, "skills/alpha").unwrap();
        assert!(!dir.path().join("shared/skills/alpha").exists());
        assert!(dir.path().join("shared/skills/beta").is_dir());
    }

    #[test]
    fn remove_directory_refuses_protected_top_level() {
        let (_dir, cfg) = fixture();
        for name in ["skills", "skill-bundles", "knowledge"] {
            let err = remove_directory(&cfg, name).unwrap_err();
            assert!(
                matches!(err, BrowseError::Protected(_)),
                "must refuse to remove protected top-level '{name}', got {err:?}"
            );
        }
    }

    #[test]
    fn remove_directory_refuses_empty_path() {
        let (_dir, cfg) = fixture();
        let err = remove_directory(&cfg, "").unwrap_err();
        assert!(matches!(err, BrowseError::Protected(_)));
    }

    #[test]
    fn remove_directory_rejects_escape() {
        let (_dir, cfg) = fixture();
        let err = remove_directory(&cfg, "../etc").unwrap_err();
        assert!(matches!(err, BrowseError::Escape(_)));
    }

    #[test]
    fn remove_directory_errors_on_missing() {
        let (_dir, cfg) = fixture();
        let err = remove_directory(&cfg, "skills/ghost").unwrap_err();
        assert!(matches!(err, BrowseError::NotFound(_)));
    }

    #[test]
    fn remove_directory_allows_nested_under_protected_top_level() {
        // skills/ is protected, but skills/alpha is operator-owned.
        let (dir, cfg) = fixture();
        remove_directory(&cfg, "skills/alpha").unwrap();
        assert!(!dir.path().join("shared/skills/alpha").exists());
        assert!(dir.path().join("shared/skills").is_dir());
    }

    // ── agent workspace ──────────────────────────────────────────────

    fn workspace_fixture() -> (TempDir, Config) {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path().join("agents/alpha/workspace");
        std::fs::create_dir_all(ws.join("notes/sub")).unwrap();
        std::fs::write(ws.join("notes/draft.md"), b"draft content").unwrap();
        std::fs::write(ws.join("IDENTITY.md"), b"identity").unwrap();
        std::fs::write(ws.join("SOUL.md"), b"soul").unwrap();
        let cfg = Config {
            config_path: dir.path().join("config.toml"),
            ..Config::default()
        };
        (dir, cfg)
    }

    #[test]
    fn list_agent_workspace_returns_one_level() {
        let (_dir, cfg) = workspace_fixture();
        let result = list_agent_workspace(&cfg, "alpha", "").unwrap();
        let names: Vec<_> = result.entries.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains(&"notes"));
        assert!(names.contains(&"IDENTITY.md"));
    }

    #[test]
    fn list_agent_workspace_rejects_escape() {
        let (_dir, cfg) = workspace_fixture();
        let err = list_agent_workspace(&cfg, "alpha", "../../etc").unwrap_err();
        assert!(matches!(err, BrowseError::Escape(_)));
    }

    #[test]
    fn read_agent_workspace_file_returns_bytes_and_text_flag() {
        let (_dir, cfg) = workspace_fixture();
        let r = read_agent_workspace_file(&cfg, "alpha", "notes/draft.md").unwrap();
        assert_eq!(r.bytes, b"draft content");
        assert!(r.is_text);
        assert_eq!(r.size, 13);
    }

    #[test]
    fn read_agent_workspace_file_errors_on_directory() {
        let (_dir, cfg) = workspace_fixture();
        let err = read_agent_workspace_file(&cfg, "alpha", "notes").unwrap_err();
        assert!(matches!(err, BrowseError::NotADirectory(_)));
    }

    #[test]
    fn read_agent_workspace_file_enforces_size_cap() {
        let (dir, cfg) = workspace_fixture();
        let ws = dir.path().join("agents/alpha/workspace");
        let big = vec![b'x'; (AGENT_WORKSPACE_READ_CAP + 1) as usize];
        std::fs::write(ws.join("big.bin"), &big).unwrap();
        let err = read_agent_workspace_file(&cfg, "alpha", "big.bin").unwrap_err();
        assert!(matches!(err, BrowseError::TooLarge(_, _)));
    }

    #[test]
    fn delete_agent_workspace_path_removes_file() {
        let (dir, cfg) = workspace_fixture();
        delete_agent_workspace_path(&cfg, "alpha", "notes/draft.md").unwrap();
        assert!(
            !dir.path()
                .join("agents/alpha/workspace/notes/draft.md")
                .exists()
        );
    }

    #[test]
    fn delete_agent_workspace_path_removes_directory_recursively() {
        let (dir, cfg) = workspace_fixture();
        delete_agent_workspace_path(&cfg, "alpha", "notes").unwrap();
        assert!(!dir.path().join("agents/alpha/workspace/notes").exists());
    }

    #[test]
    fn delete_agent_workspace_path_refuses_protected_files() {
        let (_dir, cfg) = workspace_fixture();
        for name in ["IDENTITY.md", "SOUL.md"] {
            let err = delete_agent_workspace_path(&cfg, "alpha", name).unwrap_err();
            assert!(
                matches!(err, BrowseError::ProtectedFile(_)),
                "must refuse {name}, got {err:?}"
            );
        }
    }

    #[test]
    fn delete_agent_workspace_path_refuses_root() {
        let (_dir, cfg) = workspace_fixture();
        let err = delete_agent_workspace_path(&cfg, "alpha", "").unwrap_err();
        assert!(matches!(err, BrowseError::Protected(_)));
    }

    #[test]
    fn delete_agent_workspace_path_rejects_escape() {
        let (_dir, cfg) = workspace_fixture();
        let err = delete_agent_workspace_path(&cfg, "alpha", "../../etc").unwrap_err();
        assert!(matches!(err, BrowseError::Escape(_)));
    }

    #[test]
    fn move_agent_workspace_path_renames_within_jail() {
        let (dir, cfg) = workspace_fixture();
        move_agent_workspace_path(&cfg, "alpha", "notes/draft.md", "notes/final.md").unwrap();
        assert!(
            !dir.path()
                .join("agents/alpha/workspace/notes/draft.md")
                .exists()
        );
        assert!(
            dir.path()
                .join("agents/alpha/workspace/notes/final.md")
                .is_file()
        );
    }

    #[test]
    fn move_agent_workspace_path_creates_intermediate_dirs() {
        let (dir, cfg) = workspace_fixture();
        move_agent_workspace_path(&cfg, "alpha", "notes/draft.md", "archive/2026/draft.md")
            .unwrap();
        assert!(
            dir.path()
                .join("agents/alpha/workspace/archive/2026/draft.md")
                .is_file()
        );
    }

    #[test]
    fn move_agent_workspace_path_refuses_protected_src() {
        let (_dir, cfg) = workspace_fixture();
        let err = move_agent_workspace_path(&cfg, "alpha", "IDENTITY.md", "id.md").unwrap_err();
        assert!(matches!(err, BrowseError::ProtectedFile(_)));
    }

    #[test]
    fn move_agent_workspace_path_refuses_protected_dst() {
        let (_dir, cfg) = workspace_fixture();
        let err =
            move_agent_workspace_path(&cfg, "alpha", "notes/draft.md", "IDENTITY.md").unwrap_err();
        assert!(matches!(err, BrowseError::ProtectedFile(_)));
    }

    #[test]
    fn move_agent_workspace_path_rejects_escape() {
        let (_dir, cfg) = workspace_fixture();
        let err = move_agent_workspace_path(&cfg, "alpha", "notes/draft.md", "../../etc/draft.md")
            .unwrap_err();
        assert!(matches!(err, BrowseError::Escape(_)));
    }

    #[test]
    fn move_agent_workspace_path_refuses_overwrite() {
        let (_dir, cfg) = workspace_fixture();
        let err =
            move_agent_workspace_path(&cfg, "alpha", "notes/draft.md", "notes/sub").unwrap_err();
        assert!(matches!(err, BrowseError::NotADirectory(_)));
    }

    #[test]
    fn list_agent_workspace_tags_protected_top_level_entries() {
        let (dir, cfg) = workspace_fixture();
        std::fs::create_dir_all(dir.path().join("agents/alpha/workspace/sessions")).unwrap();
        let result = list_agent_workspace(&cfg, "alpha", "").unwrap();
        let sessions = result
            .entries
            .iter()
            .find(|e| e.name == "sessions")
            .unwrap();
        assert!(sessions.protected, "sessions/ must be tagged protected");
        let identity = result
            .entries
            .iter()
            .find(|e| e.name == "IDENTITY.md")
            .unwrap();
        assert!(identity.protected, "IDENTITY.md must be tagged protected");
        let notes = result.entries.iter().find(|e| e.name == "notes").unwrap();
        assert!(!notes.protected, "operator dirs must not be tagged");
    }

    #[test]
    fn list_agent_workspace_does_not_tag_protected_names_below_root() {
        let (dir, cfg) = workspace_fixture();
        std::fs::create_dir_all(dir.path().join("agents/alpha/workspace/notes/sessions")).unwrap();
        let result = list_agent_workspace(&cfg, "alpha", "notes").unwrap();
        let sessions = result
            .entries
            .iter()
            .find(|e| e.name == "sessions")
            .unwrap();
        assert!(
            !sessions.protected,
            "protection only applies at workspace root"
        );
    }

    #[test]
    fn delete_agent_workspace_path_refuses_protected_dir() {
        let (dir, cfg) = workspace_fixture();
        std::fs::create_dir_all(dir.path().join("agents/alpha/workspace/sessions")).unwrap();
        let err = delete_agent_workspace_path(&cfg, "alpha", "sessions").unwrap_err();
        assert!(matches!(err, BrowseError::Protected(_)));
        assert!(dir.path().join("agents/alpha/workspace/sessions").is_dir());
    }

    #[test]
    fn move_agent_workspace_path_refuses_protected_src_dir() {
        let (dir, cfg) = workspace_fixture();
        std::fs::create_dir_all(dir.path().join("agents/alpha/workspace/sessions")).unwrap();
        let err = move_agent_workspace_path(&cfg, "alpha", "sessions", "old_sessions").unwrap_err();
        assert!(matches!(err, BrowseError::Protected(_)));
    }

    #[test]
    fn move_agent_workspace_path_refuses_protected_dst_dir() {
        let (_dir, cfg) = workspace_fixture();
        let err = move_agent_workspace_path(&cfg, "alpha", "notes", "sessions").unwrap_err();
        assert!(matches!(err, BrowseError::Protected(_)));
    }

    #[test]
    fn make_agent_workspace_directory_creates_nested_path() {
        let (dir, cfg) = workspace_fixture();
        make_agent_workspace_directory(&cfg, "alpha", "archive/2026").unwrap();
        assert!(
            dir.path()
                .join("agents/alpha/workspace/archive/2026")
                .is_dir()
        );
    }

    #[test]
    fn make_agent_workspace_directory_is_idempotent() {
        let (_dir, cfg) = workspace_fixture();
        make_agent_workspace_directory(&cfg, "alpha", "notes").unwrap();
        make_agent_workspace_directory(&cfg, "alpha", "notes").unwrap();
    }

    #[test]
    fn make_agent_workspace_directory_rejects_escape() {
        let (_dir, cfg) = workspace_fixture();
        let err = make_agent_workspace_directory(&cfg, "alpha", "../../etc").unwrap_err();
        assert!(matches!(err, BrowseError::Escape(_)));
    }

    #[test]
    fn make_agent_workspace_directory_refuses_over_existing_file() {
        let (_dir, cfg) = workspace_fixture();
        let err = make_agent_workspace_directory(&cfg, "alpha", "notes/draft.md").unwrap_err();
        assert!(matches!(err, BrowseError::NotADirectory(_)));
    }

    #[test]
    fn make_agent_workspace_directory_refuses_protected_file_path() {
        let (_dir, cfg) = workspace_fixture();
        let err = make_agent_workspace_directory(&cfg, "alpha", "IDENTITY.md").unwrap_err();
        assert!(matches!(err, BrowseError::ProtectedFile(_)));
    }

    #[test]
    fn make_agent_workspace_directory_refuses_empty_path() {
        let (_dir, cfg) = workspace_fixture();
        let err = make_agent_workspace_directory(&cfg, "alpha", "").unwrap_err();
        assert!(matches!(err, BrowseError::NotFound(_)));
    }
}
