//! `cargo xtask gen-openapi` — render the gateway's OpenAPI 3.1 spec to a
//! committed snapshot file and (optionally) verify the snapshot is in
//! sync with the runtime spec.
//!
//! `cargo run -p xtask --bin gen-openapi` writes the rendered spec to
//! `crates/zeroclaw-gateway/openapi.json` (overwrite). `--check` does
//! not touch the file but exits non-zero when the rendered spec differs
//! from the committed snapshot — wire into CI so a handler change
//! without a corresponding spec update fails the build.
//!
//! See #6175.

use anyhow::{Context, Result};
use clap::Parser;
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    name = "gen-openapi",
    about = "Render the gateway's OpenAPI 3.1 spec to crates/zeroclaw-gateway/openapi.json"
)]
struct Args {
    /// Verify the committed snapshot matches the runtime spec without
    /// modifying it. Exits non-zero on drift; suitable for CI.
    #[arg(long)]
    check: bool,

    /// Path to the snapshot file. Defaults to
    /// `crates/zeroclaw-gateway/openapi.json` resolved against the
    /// workspace root.
    #[arg(long)]
    output: Option<PathBuf>,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let snapshot_path = args
        .output
        .unwrap_or_else(|| workspace_root().join("crates/zeroclaw-gateway/openapi.json"));

    let rendered = serde_json::to_string_pretty(&zeroclaw_gateway::openapi::build_spec())
        .context("serialize openapi spec to JSON")?;
    // Ensure trailing newline matches Git's expectation for text files.
    let rendered = format!("{rendered}\n");

    if args.check {
        let committed = std::fs::read_to_string(&snapshot_path).with_context(|| {
            format!(
                "failed to read committed snapshot at {} — \
                 run `cargo xtask gen-openapi` to create it",
                snapshot_path.display(),
            )
        })?;
        if committed != rendered {
            eprintln!(
                "openapi snapshot is stale at {}.\n\
                 The runtime spec built from the gateway crate doesn't match the \
                 committed snapshot. Run `cargo run -p xtask --bin gen-openapi` and \
                 commit the result.\n",
                snapshot_path.display(),
            );
            // Brief diff hint — show the first ~10 lines of difference so
            // the CI log is useful without dumping the whole spec.
            print_brief_diff(&committed, &rendered);
            std::process::exit(1);
        }
        println!(
            "openapi snapshot is up to date ({})",
            snapshot_path.display()
        );
        return Ok(());
    }

    if let Some(parent) = snapshot_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create parent directory {}", parent.display()))?;
    }
    std::fs::write(&snapshot_path, &rendered)
        .with_context(|| format!("write snapshot to {}", snapshot_path.display()))?;
    println!("wrote openapi snapshot to {}", snapshot_path.display());
    Ok(())
}

fn workspace_root() -> PathBuf {
    // `CARGO_MANIFEST_DIR` for this binary is `<workspace>/xtask`. Go
    // one up to land at the workspace root regardless of where the
    // binary is invoked from.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn print_brief_diff(committed: &str, rendered: &str) {
    let committed_lines: Vec<&str> = committed.lines().collect();
    let rendered_lines: Vec<&str> = rendered.lines().collect();
    let mut shown = 0;
    let max = committed_lines.len().max(rendered_lines.len());
    for i in 0..max {
        let a = committed_lines.get(i).copied().unwrap_or("");
        let b = rendered_lines.get(i).copied().unwrap_or("");
        if a != b {
            eprintln!("--- L{}: {}", i + 1, a);
            eprintln!("+++ L{}: {}", i + 1, b);
            shown += 1;
            if shown >= 10 {
                eprintln!("...");
                break;
            }
        }
    }
}
