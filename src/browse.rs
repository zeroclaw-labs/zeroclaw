//! `zeroclaw browse [path]` — CLI adapter over
//! `zeroclaw_runtime::browse::list_directory`. Thin print formatter; the
//! walking + containment rule lives in the runtime crate so the gateway
//! and the CLI share one implementation.

use anyhow::Result;
use zeroclaw_runtime::browse::list_directory;

pub fn handle_browse(path: String, config: &crate::config::Config) -> Result<()> {
    let result = list_directory(config, &path)?;
    let display_path = if result.path.is_empty() {
        "/"
    } else {
        &result.path
    };
    println!(
        "{} ({} entries)",
        console::style(format!("shared/{display_path}"))
            .white()
            .bold(),
        result.entries.len(),
    );
    if result.entries.is_empty() {
        println!("  (empty)");
        return Ok(());
    }
    for entry in result.entries {
        match entry.kind {
            "dir" => println!("  {}/", console::style(&entry.name).cyan().bold()),
            _ => match entry.size {
                Some(s) => println!("  {} ({} bytes)", console::style(&entry.name).dim(), s),
                None => println!("  {}", console::style(&entry.name).dim()),
            },
        }
    }
    Ok(())
}
