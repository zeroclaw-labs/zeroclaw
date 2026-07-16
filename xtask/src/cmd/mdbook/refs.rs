use crate::util::*;
use std::path::Path;
use std::process::Command;

pub fn run(_tag: Option<&str>) -> anyhow::Result<()> {
    let root = repo_root();
    require_tool("cargo", "https://rustup.rs")?;
    build_refs(&root)?;
    build_api(&root)?;
    let api_dest = book_dir(&root).join("book").join("api");
    std::fs::create_dir_all(book_dir(&root).join("book"))?;
    let _ = std::fs::remove_dir_all(&api_dest);
    copy_dir_all(doc_dir(&root), &api_dest)?;
    crate::cmd::mdbook::build::prune_rustdoc_source_view(&api_dest)?;
    println!(
        "==> API reference: {}",
        api_dest.join("index.html").display()
    );
    Ok(())
}

pub fn build_refs(root: &Path) -> anyhow::Result<()> {
    let ref_dir = ref_dir(root);
    println!("==> Generating reference/cli.md and reference/config.md from code");
    std::fs::create_dir_all(&ref_dir)?;

    let help = Command::new("cargo")
        .args([
            "run",
            "--no-default-features",
            "--features",
            "schema-export",
            "--",
            "markdown-help",
        ])
        .current_dir(root)
        .output()?;
    if !help.status.success() {
        anyhow::bail!("cargo run markdown-help failed");
    }
    let cli_content: String = String::from_utf8_lossy(&help.stdout)
        .lines()
        .map(|l| {
            if let Some(rest) = l.strip_prefix("###### ") {
                rest
            } else {
                l
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(ref_dir.join("cli.md"), cli_content + "\n")?;

    let schema = Command::new("cargo")
        .args([
            "run",
            "--no-default-features",
            "--features",
            "schema-export",
            "--",
            "markdown-schema",
        ])
        .current_dir(root)
        .output()?;
    if !schema.status.success() {
        anyhow::bail!("cargo run markdown-schema failed");
    }
    std::fs::write(ref_dir.join("config.md"), &schema.stdout)?;
    Ok(())
}

pub fn build_api(root: &Path) -> anyhow::Result<()> {
    println!("==> Generating rustdoc API reference");
    let target = target_dir(root);
    // The docs-site rustdoc theme is owned here, not by `[build] rustdocflags`
    // in `.cargo/config.toml` — the repository-wide flag would also reach
    // `cargo test --doc`, which under Rust 1.96's stricter parser rejects
    // a duplicate `--default-theme` with `Option 'default-theme' given more
    // than once`. `compose_rustdocflags` preserves any caller-supplied flags
    // (e.g. `-D warnings`) and only appends the site default when the caller
    // did not already pin a theme.
    let inherited = std::env::var_os("RUSTDOCFLAGS").map(|s| s.to_string_lossy().into_owned());
    let rustdocflags = compose_rustdocflags(inherited.as_deref());
    run_cmd(
        Command::new("cargo")
            .args([
                "doc",
                "--no-deps",
                "--workspace",
                "--exclude",
                "zeroclaw-desktop",
                "--target-dir",
            ])
            .arg(&target)
            .env("RUSTDOCFLAGS", rustdocflags)
            .current_dir(root),
    )
}

/// Compose the `RUSTDOCFLAGS` value for the docs-site `cargo doc` invocation.
///
/// Behaviour:
/// - When the caller did not supply `--default-theme`, append the site default
///   (`--default-theme=ayu`) to the inherited flags verbatim. This keeps any
///   caller-supplied options (`-D warnings`, `--cfg=...`, etc.) intact.
/// - When the caller already pinned `--default-theme=<value>`, return the
///   inherited flags unchanged. Rust 1.96+ rejects a duplicate `--default-theme`
///   with `Option 'default-theme' given more than once`, so the caller wins.
///
/// Pure / sync; unit-tested in isolation from `Command` and `cargo doc`.
pub(crate) fn compose_rustdocflags(inherited: Option<&str>) -> String {
    const SITE_DEFAULT_THEME: &str = "--default-theme=ayu";
    const THEME_PREFIX: &str = "--default-theme=";
    match inherited {
        None | Some("") => SITE_DEFAULT_THEME.to_string(),
        Some(flags) => {
            let caller_pinned_theme = flags
                .split_whitespace()
                .any(|tok| tok.starts_with(THEME_PREFIX));
            if caller_pinned_theme {
                flags.to_string()
            } else {
                format!("{flags} {SITE_DEFAULT_THEME}")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::compose_rustdocflags;

    #[test]
    fn compose_with_no_inherited_returns_site_default() {
        // No caller flags → output is exactly the site default.
        assert_eq!(compose_rustdocflags(None), "--default-theme=ayu");
        // Empty string is the same case (caller had no real flags).
        assert_eq!(compose_rustdocflags(Some("")), "--default-theme=ayu");
    }

    #[test]
    fn compose_appends_site_default_to_unrelated_inherited_flags() {
        // Unrelated inherited flags survive verbatim; site default is appended.
        assert_eq!(
            compose_rustdocflags(Some("-D warnings")),
            "-D warnings --default-theme=ayu"
        );
        assert_eq!(
            compose_rustdocflags(Some("-D warnings --cfg=docsrs")),
            "-D warnings --cfg=docsrs --default-theme=ayu"
        );
    }

    #[test]
    fn compose_preserves_caller_pinned_theme_without_duplication() {
        // Rust 1.96+ rejects a duplicate `--default-theme`, so when the caller
        // has already pinned one we keep theirs verbatim — including a non-site
        // value (e.g. `--default-theme=light`) that the site default would
        // otherwise overwrite.
        assert_eq!(
            compose_rustdocflags(Some("--default-theme=light")),
            "--default-theme=light"
        );
        assert_eq!(
            compose_rustdocflags(Some("-D warnings --default-theme=light")),
            "-D warnings --default-theme=light"
        );
        // Caller pinned the same site default; we still keep theirs rather
        // than risk a duplicate if a future change re-emits the default.
        assert_eq!(
            compose_rustdocflags(Some("--default-theme=ayu")),
            "--default-theme=ayu"
        );
    }
}
