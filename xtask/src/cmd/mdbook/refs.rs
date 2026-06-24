use crate::util::*;
use std::path::Path;
use std::process::Command;

pub fn run() -> anyhow::Result<()> {
    let root = repo_root();
    require_tool("cargo", "https://rustup.rs")?;
    build_refs(&root)?;
    build_api(&root)?;
    let api_dest = book_dir(&root).join("book/api");
    std::fs::create_dir_all(book_dir(&root).join("book"))?;
    let _ = std::fs::remove_dir_all(&api_dest);
    copy_dir_all(root.join("target/doc"), &api_dest)?;
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
    run_cmd(
        Command::new("cargo")
            .args([
                "doc",
                "--no-deps",
                "--workspace",
                "--exclude",
                "zeroclaw-desktop",
            ])
            .current_dir(root),
    )
}
