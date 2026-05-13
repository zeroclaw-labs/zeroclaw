//! `cargo xtask gen-openapi` — render the gateway's OpenAPI 3.1 spec in
//! process and write it to the committed snapshot at
//! `crates/zeroclaw-gateway/openapi.json`.
//!
//! - No flags: overwrite the snapshot with the current in-process spec.
//! - `--check`: render to a string and diff against the committed snapshot.
//!   Exits non-zero on drift and prints the first 80 lines of unified diff.
//!
//! Wired into CI via `./dev/ci.sh` so any handler change that forgets to
//! re-run this step fails review rather than silently diverging.

use anyhow::{Context, Result};
use clap::Parser;
use std::path::{Path, PathBuf};

use xtask::util::repo_root;

const SNAPSHOT_REL_PATH: &str = "crates/zeroclaw-gateway/openapi.json";

#[derive(Parser, Debug)]
#[command(
    name = "gen-openapi",
    about = "Render the gateway OpenAPI spec to the committed snapshot"
)]
struct Cli {
    /// Instead of writing, compare the rendered spec against the committed
    /// snapshot and exit non-zero on drift.
    #[arg(long)]
    check: bool,

    /// Override the snapshot path (default: `crates/zeroclaw-gateway/openapi.json`).
    #[arg(long)]
    output: Option<PathBuf>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let root = repo_root();
    let snapshot_path = cli.output.unwrap_or_else(|| root.join(SNAPSHOT_REL_PATH));

    let rendered = render_spec()?;

    if cli.check {
        check_against_snapshot(&snapshot_path, &rendered)
    } else {
        write_snapshot(&snapshot_path, &rendered)
    }
}

fn render_spec() -> Result<String> {
    let value = zeroclaw_gateway::openapi::build_spec();
    // Pretty-print so the committed file diffs cleanly in reviews.
    let rendered = serde_json::to_string_pretty(&value).context("serialize openapi spec")?;
    // Guarantee a trailing newline so the file follows POSIX line convention.
    let mut rendered = rendered;
    if !rendered.ends_with('\n') {
        rendered.push('\n');
    }
    Ok(rendered)
}

fn write_snapshot(path: &Path, rendered: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create parent directory {}", parent.display()))?;
    }
    std::fs::write(path, rendered)
        .with_context(|| format!("write openapi snapshot to {}", path.display()))?;
    println!("wrote {}", path.display());
    Ok(())
}

fn check_against_snapshot(path: &Path, rendered: &str) -> Result<()> {
    let current = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            eprintln!(
                "openapi snapshot not found at {}\n\
                 run `cargo run -p xtask --bin gen-openapi` to create it",
                path.display()
            );
            std::process::exit(2);
        }
        Err(e) => {
            return Err(anyhow::anyhow!(
                "failed to read openapi snapshot at {}: {e}",
                path.display()
            ));
        }
    };

    if current == rendered {
        println!("openapi snapshot up to date ({})", path.display());
        return Ok(());
    }

    eprintln!(
        "OpenAPI snapshot drift detected at {}\n\
         rerun `cargo run -p xtask --bin gen-openapi` to refresh it.\n",
        path.display()
    );
    eprintln!("--- committed snapshot");
    eprintln!("+++ current build\n");
    print_unified_diff(&current, rendered, 80);
    std::process::exit(1);
}

/// Minimal line-by-line diff. Good enough for CI failure output — the
/// full diff tool is available locally if the developer wants more.
fn print_unified_diff(left: &str, right: &str, max_lines: usize) {
    let left_lines: Vec<&str> = left.lines().collect();
    let right_lines: Vec<&str> = right.lines().collect();
    let max_len = left_lines.len().max(right_lines.len());
    let mut printed = 0usize;
    for i in 0..max_len {
        if printed >= max_lines {
            eprintln!("… (diff truncated at {max_lines} lines)");
            break;
        }
        let l = left_lines.get(i).copied();
        let r = right_lines.get(i).copied();
        match (l, r) {
            (Some(a), Some(b)) if a == b => {}
            (Some(a), None) => {
                eprintln!("-{a}");
                printed += 1;
            }
            (None, Some(b)) => {
                eprintln!("+{b}");
                printed += 1;
            }
            (Some(a), Some(b)) => {
                eprintln!("-{a}");
                eprintln!("+{b}");
                printed += 2;
            }
            (None, None) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::render_spec;

    #[test]
    fn render_is_deterministic() {
        // The same in-process build must render byte-identical output on
        // every call so the `--check` gate is reliable.
        let a = render_spec().unwrap();
        let b = render_spec().unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn render_ends_with_newline() {
        let s = render_spec().unwrap();
        assert!(s.ends_with('\n'), "snapshot must end with newline");
    }

    #[test]
    fn render_contains_slot_paths() {
        let s = render_spec().unwrap();
        assert!(s.contains("/api/slots"));
        assert!(s.contains("SlotResponse"));
    }
}
