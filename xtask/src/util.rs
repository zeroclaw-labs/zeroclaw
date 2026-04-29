use std::path::{Path, PathBuf};
use std::process::Command;

pub fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives one level below repo root")
        .to_owned()
}

pub fn book_dir(root: &Path) -> PathBuf {
    root.join("docs/book")
}

pub fn ref_dir(root: &Path) -> PathBuf {
    root.join("docs/book/src/reference")
}

pub fn po_dir(root: &Path) -> PathBuf {
    root.join("docs/book/po")
}

pub fn pot_file(root: &Path) -> PathBuf {
    root.join("docs/book/po/messages.pot")
}

pub struct LocaleEntry {
    pub code: String,
    pub label: String,
}

pub fn locale_entries() -> Vec<LocaleEntry> {
    let path = repo_root().join("locales.toml");
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|_| panic!("locales.toml not found at {}", path.display()));
    let table: toml::Table = raw.parse().expect("locales.toml is invalid TOML");
    table
        .get("locale")
        .and_then(|v| v.as_array())
        .unwrap_or_else(|| panic!("locales.toml missing [[locale]] entries"))
        .iter()
        .filter_map(|entry| {
            let code = entry.get("code")?.as_str()?.to_string();
            let label = entry.get("label")?.as_str()?.to_string();
            Some(LocaleEntry { code, label })
        })
        .collect()
}

pub fn locales() -> Vec<String> {
    locale_entries().into_iter().map(|e| e.code).collect()
}

pub fn require_tool(cmd: &str, install_hint: &str) -> anyhow::Result<()> {
    if tool_on_path(cmd) {
        return Ok(());
    }
    anyhow::bail!("'{}' not found on PATH\n  install: {}", cmd, install_hint);
}

/// Like `require_tool`, but if the binary is a cargo-installable crate that's missing,
/// auto-install it via `cargo install --locked <crate>`. Idempotent — a no-op when present.
pub fn ensure_cargo_tool(cmd: &str, crate_name: &str) -> anyhow::Result<()> {
    if tool_on_path(cmd) {
        return Ok(());
    }
    println!("==> installing '{crate_name}' (missing '{cmd}')");
    run_cmd(Command::new("cargo").args(["install", "--locked", crate_name]))
}

fn tool_on_path(cmd: &str) -> bool {
    std::env::var_os("PATH")
        .map(|paths| {
            std::env::split_paths(&paths)
                .any(|dir| dir.join(cmd).is_file() || dir.join(format!("{cmd}.exe")).is_file())
        })
        .unwrap_or(false)
}

/// Resolve the real `mdbook` binary on PATH, skipping the xtask's own build dir.
/// The xtask itself is named `mdbook`; Cargo prepends `target/debug` and
/// `target/debug/deps` to PATH for `cargo run`, and on Windows `Command::new`
/// also searches the parent process's directory first — so without this guard
/// the xtask would recursively spawn itself.
pub fn mdbook_program() -> anyhow::Result<PathBuf> {
    let exclude = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_owned))
        .and_then(|p| std::fs::canonicalize(&p).ok());
    let paths = std::env::var_os("PATH")
        .ok_or_else(|| anyhow::anyhow!("PATH environment variable is unset"))?;
    for dir in std::env::split_paths(&paths) {
        if let (Some(ex), Ok(canon)) = (exclude.as_deref(), std::fs::canonicalize(&dir))
            && canon.starts_with(ex)
        {
            continue;
        }
        for name in ["mdbook", "mdbook.exe"] {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }
    anyhow::bail!(
        "'mdbook' not found on PATH\n  install: cargo install mdbook --version 0.5.0 --locked"
    )
}

pub fn run_cmd(cmd: &mut Command) -> anyhow::Result<()> {
    let status = cmd.status()?;
    if !status.success() {
        anyhow::bail!("command failed: {:?}", cmd.get_program());
    }
    Ok(())
}

pub fn fluent_locales_dir(root: &Path) -> PathBuf {
    root.join("crates/zeroclaw-runtime/locales")
}

pub fn fluent_locales(root: &Path) -> anyhow::Result<Vec<String>> {
    let dir = fluent_locales_dir(root);
    let mut out = vec![];
    for entry in std::fs::read_dir(&dir)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            out.push(entry.file_name().to_string_lossy().into_owned());
        }
    }
    out.sort();
    Ok(out)
}

pub fn ftl_files_in(locale_dir: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut out = vec![];
    for entry in std::fs::read_dir(locale_dir)? {
        let entry = entry?;
        if entry.path().extension().is_some_and(|e| e == "ftl") {
            out.push(entry.path());
        }
    }
    out.sort();
    Ok(out)
}

pub struct ProviderConfig {
    pub base_url: String,
    pub model: Option<String>,
    pub api_key: Option<String>,
}

/// Read a `[providers.models.<name>]` entry from ~/.zeroclaw/config.toml.
pub fn read_provider_config(provider_name: &str) -> anyhow::Result<ProviderConfig> {
    let home =
        std::env::var("HOME").unwrap_or_else(|_| std::env::var("USERPROFILE").unwrap_or_default());
    let candidates = [
        format!("{home}/.zeroclaw/config.toml"),
        format!("{home}/.config/zeroclaw/config.toml"),
    ];
    let raw = candidates
        .iter()
        .find_map(|p| std::fs::read_to_string(p).ok())
        .ok_or_else(|| anyhow::anyhow!("config.toml not found (tried ~/.zeroclaw/config.toml)"))?;

    let table: toml::Table = raw.parse()?;
    let provider = table
        .get("providers")
        .and_then(|v| v.get("models"))
        .and_then(|v| v.get(provider_name))
        .ok_or_else(|| {
            anyhow::anyhow!("[providers.models.{provider_name}] not found in config.toml")
        })?;

    Ok(ProviderConfig {
        base_url: provider
            .get("base_url")
            .and_then(|v| v.as_str())
            .unwrap_or("http://localhost:11434")
            .to_string(),
        model: provider
            .get("model")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        api_key: provider
            .get("api_key")
            .and_then(|v| v.as_str())
            .map(str::to_string),
    })
}

pub fn copy_dir_all(src: impl AsRef<Path>, dst: impl AsRef<Path>) -> anyhow::Result<()> {
    std::fs::create_dir_all(&dst)?;
    for entry in std::fs::read_dir(&src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        if ty.is_dir() {
            copy_dir_all(entry.path(), dst.as_ref().join(entry.file_name()))?;
        } else {
            std::fs::copy(entry.path(), dst.as_ref().join(entry.file_name()))?;
        }
    }
    Ok(())
}
