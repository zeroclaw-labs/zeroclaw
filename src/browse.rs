//! `zeroclaw browse [path]` — CLI adapter over
//! `zeroclaw_runtime::browse::list_directory`. Thin print formatter; the
//! walking + containment rule lives in the runtime crate so the gateway
//! and the CLI share one implementation.

use anyhow::Result;
use zeroclaw_runtime::browse::list_directory;
use zeroclaw_runtime::i18n::{get_required_cli_string, get_required_cli_string_with_args};

pub fn handle_browse(path: String, config: &crate::config::Config) -> Result<()> {
    let result = list_directory(config, &path)?;
    let display_path = if result.path.is_empty() {
        "/"
    } else {
        &result.path
    };
    println!(
        "{}",
        get_required_cli_string_with_args(
            "cli-browse-header",
            &[
                (
                    "path",
                    &console::style(format!("shared/{display_path}"))
                        .white()
                        .bold()
                        .to_string(),
                ),
                ("count", &result.entries.len().to_string()),
            ],
        )
    );
    if result.entries.is_empty() {
        println!("  {}", get_required_cli_string("cli-browse-empty"));
        return Ok(());
    }
    for entry in result.entries {
        match entry.kind {
            "dir" => println!("  {}/", console::style(&entry.name).cyan().bold()),
            _ => match entry.size {
                Some(s) => println!(
                    "  {}",
                    get_required_cli_string_with_args(
                        "cli-browse-file-bytes",
                        &[
                            ("name", &console::style(&entry.name).dim().to_string()),
                            ("bytes", &s.to_string()),
                        ],
                    )
                ),
                None => println!("  {}", console::style(&entry.name).dim()),
            },
        }
    }
    Ok(())
}
