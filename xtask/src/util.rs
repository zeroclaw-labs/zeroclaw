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
        .ok_or_else(|| anyhow::Error::msg("PATH environment variable is unset"))?;
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

/// Catalogue roots that `cargo fluent` walks. Each root holds `<locale>/`
/// subdirectories of `.ftl` files. The runtime catalogue is the primary
/// source; zerocode ships an independent catalogue under the same layout.
/// Named Fluent catalogue roots. Each root holds `<locale>/` subdirectories of
/// `.ftl` files. The runtime catalogue is the primary source; zerocode ships an
/// independent catalogue under the same layout. The name is the `--catalog`
/// selector value.
pub fn fluent_catalog_roots_named(root: &Path) -> Vec<(&'static str, PathBuf)> {
    vec![
        ("runtime", root.join("crates/zeroclaw-runtime/locales")),
        ("zerocode", root.join("apps/zerocode/locales")),
    ]
}

/// Catalogue roots filtered by an optional `--catalog` name. `None` returns all
/// roots; an unknown name is an error listing the valid choices.
pub fn fluent_catalog_roots_for(
    root: &Path,
    catalog: Option<&str>,
) -> anyhow::Result<Vec<PathBuf>> {
    let all = fluent_catalog_roots_named(root);
    match catalog {
        None => Ok(all.into_iter().map(|(_, p)| p).collect()),
        Some(name) => {
            if let Some((_, path)) = all.iter().find(|(n, _)| *n == name) {
                Ok(vec![path.clone()])
            } else {
                let choices = all.iter().map(|(n, _)| *n).collect::<Vec<_>>().join(", ");
                anyhow::bail!("unknown --catalog '{name}'; valid choices: {choices}")
            }
        }
    }
}

pub fn fluent_catalog_roots(root: &Path) -> Vec<PathBuf> {
    fluent_catalog_roots_named(root)
        .into_iter()
        .map(|(_, p)| p)
        .collect()
}

pub fn fluent_locales_dir(root: &Path) -> PathBuf {
    root.join("crates/zeroclaw-runtime/locales")
}

/// Locale codes present in a single catalogue root (its `<locale>/` subdirs).
pub fn fluent_locales_in(dir: &Path) -> anyhow::Result<Vec<String>> {
    let mut out = vec![];
    if !dir.exists() {
        return Ok(out);
    }
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            out.push(entry.file_name().to_string_lossy().into_owned());
        }
    }
    out.sort();
    Ok(out)
}

pub fn fluent_locales(root: &Path) -> anyhow::Result<Vec<String>> {
    fluent_locales_in(&fluent_locales_dir(root))
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

/// Build a ready-to-use `ModelProvider` for a configured alias, loading the
/// typed `Config` from `config_dir` (mirrors `zeroclaw --config-dir`; defaults
/// to ~/.zeroclaw then ~/.config/zeroclaw). The provider stack resolves the
/// family endpoint, auth header, wire protocol, and decrypts secrets — this
/// tool hand-rolls none of it. Returns the provider plus the resolved model id.
pub fn build_model_provider(
    provider_name: &str,
    config_dir: Option<&str>,
) -> anyhow::Result<(Box<dyn zeroclaw_api::model_provider::ModelProvider>, String)> {
    let home =
        std::env::var("HOME").unwrap_or_else(|_| std::env::var("USERPROFILE").unwrap_or_default());
    let dir_candidates: Vec<std::path::PathBuf> = match config_dir {
        Some(d) => vec![std::path::PathBuf::from(d)],
        None => vec![
            std::path::PathBuf::from(format!("{home}/.zeroclaw")),
            std::path::PathBuf::from(format!("{home}/.config/zeroclaw")),
        ],
    };
    let dir = dir_candidates
        .into_iter()
        .find(|d| d.join("config.toml").is_file())
        .ok_or_else(|| {
            anyhow::Error::msg(
                "config.toml not found (looked under --config-dir / ~/.zeroclaw / ~/.config/zeroclaw)",
            )
        })?;

    let raw = std::fs::read_to_string(dir.join("config.toml"))?;
    let mut config: zeroclaw_config::schema::Config = toml::from_str(&raw)?;

    // Decrypt secrets through the canonical store (same path the daemon uses).
    let store = zeroclaw_config::secrets::SecretStore::new(&dir, config.secrets.encrypt);
    config.decrypt_secrets(&store)?;

    // Resolve bare-or-dotted name to a concrete `kind.alias` + its model + key.
    let (kind, alias, model, api_key) = {
        let (k, a, cfg) = config
            .providers
            .models
            .find_by_name(provider_name)
            .ok_or_else(|| {
                anyhow::Error::msg(format!(
                    "model-provider '{provider_name}' not found (or ambiguous) under \
                     [providers.models.<kind>.<alias>] in config.toml"
                ))
            })?;
        let model = cfg.model.clone().ok_or_else(|| {
            anyhow::Error::msg(format!(
                "model-provider '{provider_name}' has no `model` set under its \
                 [providers.models.<kind>.<alias>] entry"
            ))
        })?;
        (k, a, model, cfg.api_key.clone())
    };
    let dotted = format!("{kind}.{alias}");

    let options = zeroclaw_providers::provider_runtime_options_for_alias(&config, kind, &alias);
    let provider = zeroclaw_providers::create_resilient_model_provider_from_ref(
        &config,
        &dotted,
        api_key.as_deref(),
        None,
        &config.reliability,
        &options,
    )?;

    Ok((provider, model))
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
