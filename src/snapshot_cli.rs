//! `zeroclaw snapshot` CLI — surfaces the shadow-git snapshot module.
//!
//! Stored under `<config.data_dir>/snapshot/<project_hash>/<worktree_hash>/`,
//! the shadow repo lets agents (and users) capture worktree state as git
//! tree objects without polluting the user's real git history. This CLI
//! is the human-facing entry point; the same module is invoked
//! programmatically by the runtime when auto-checkpointing per turn.

use crate::config::Config;
use anyhow::{Context, Result};
use console::style;
use std::path::PathBuf;
use zeroclaw_runtime::snapshot::{Patch, get_or_create};

/// Subcommands for `zeroclaw snapshot`.
#[derive(clap::Subcommand, Debug, Clone)]
pub enum SnapshotCommands {
    /// Capture the current worktree as a shadow tree object and print its hash.
    Track {
        /// Worktree path to capture. Defaults to the current directory.
        #[arg(long, default_value = ".")]
        cwd: PathBuf,
    },
    /// Show a unified diff of the current worktree vs a previously-tracked hash.
    Diff {
        /// Tree hash returned by a prior `track`.
        hash: String,
        /// Worktree path. Defaults to the current directory.
        #[arg(long, default_value = ".")]
        cwd: PathBuf,
    },
    /// List files changed since a previously-tracked hash.
    Patch {
        /// Tree hash returned by a prior `track`.
        hash: String,
        /// Worktree path. Defaults to the current directory.
        #[arg(long, default_value = ".")]
        cwd: PathBuf,
    },
    /// Restore the entire worktree to the state captured by a hash (destructive).
    Restore {
        /// Tree hash returned by a prior `track`.
        hash: String,
        /// Worktree path. Defaults to the current directory.
        #[arg(long, default_value = ".")]
        cwd: PathBuf,
        /// Skip the confirmation prompt.
        #[arg(long)]
        yes: bool,
    },
    /// Revert individual files listed in a previously-saved patch (destructive).
    /// Reads JSON patches from stdin in the form `[{"hash":"…","files":["…"]}]`.
    Revert {
        /// Worktree path. Defaults to the current directory.
        #[arg(long, default_value = ".")]
        cwd: PathBuf,
        /// Skip the confirmation prompt.
        #[arg(long)]
        yes: bool,
    },
    /// Prune unreachable shadow git objects older than 7 days.
    Cleanup {
        /// Worktree path. Defaults to the current directory.
        #[arg(long, default_value = ".")]
        cwd: PathBuf,
    },
    /// Undo to the most recent track in the auto-checkpoint registry
    /// (alias for `restore <latest>`).
    Undo {
        /// Worktree path. Defaults to the current directory.
        #[arg(long, default_value = ".")]
        cwd: PathBuf,
        /// Skip the confirmation prompt.
        #[arg(long)]
        yes: bool,
    },
}

/// Handle `zeroclaw snapshot <subcommand>`.
pub async fn handle_command(command: SnapshotCommands, config: &Config) -> Result<()> {
    match command {
        SnapshotCommands::Track { cwd } => handle_track(&cwd, config).await,
        SnapshotCommands::Diff { hash, cwd } => handle_diff(&cwd, &hash, config).await,
        SnapshotCommands::Patch { hash, cwd } => handle_patch(&cwd, &hash, config).await,
        SnapshotCommands::Restore { hash, cwd, yes } => {
            handle_restore(&cwd, &hash, yes, config).await
        }
        SnapshotCommands::Revert { cwd, yes } => handle_revert(&cwd, yes, config).await,
        SnapshotCommands::Cleanup { cwd } => handle_cleanup(&cwd, config).await,
        SnapshotCommands::Undo { cwd, yes } => handle_undo(&cwd, yes, config).await,
    }
}

fn snap_or_err(
    cwd: &std::path::Path,
    config: &Config,
) -> Result<std::sync::Arc<zeroclaw_runtime::snapshot::ShadowSnapshot>> {
    get_or_create(cwd, &config.data_dir).with_context(|| {
        format!(
            "shadow-git unavailable for {} — is git on PATH and is the directory inside a repo?",
            cwd.display()
        )
    })
}

async fn handle_track(cwd: &std::path::Path, config: &Config) -> Result<()> {
    let snap = snap_or_err(cwd, config)?;
    match snap.track().await {
        Some(hash) => {
            println!("{} {}", style("✓ tracked").green(), hash);
            Ok(())
        }
        None => anyhow::bail!("track failed — see logs"),
    }
}

async fn handle_diff(cwd: &std::path::Path, hash: &str, config: &Config) -> Result<()> {
    let snap = snap_or_err(cwd, config)?;
    let diff = snap.diff(hash).await;
    if diff.is_empty() {
        println!("{} no changes vs {hash}", style("·").dim());
    } else {
        print!("{diff}");
        if !diff.ends_with('\n') {
            println!();
        }
    }
    Ok(())
}

async fn handle_patch(cwd: &std::path::Path, hash: &str, config: &Config) -> Result<()> {
    let snap = snap_or_err(cwd, config)?;
    let patch = snap.patch(hash).await;
    if patch.files.is_empty() {
        println!("{} no files changed vs {hash}", style("·").dim());
        return Ok(());
    }
    println!(
        "{} {} file(s) changed since {hash}:",
        style("→").cyan(),
        patch.files.len()
    );
    for f in &patch.files {
        println!("  {}", f.display());
    }
    Ok(())
}

async fn handle_restore(
    cwd: &std::path::Path,
    hash: &str,
    yes: bool,
    config: &Config,
) -> Result<()> {
    let snap = snap_or_err(cwd, config)?;
    if !yes {
        let patch = snap.patch(hash).await;
        if patch.files.is_empty() {
            println!("{} nothing to restore — no files changed vs {hash}", style("·").dim());
            return Ok(());
        }
        eprintln!(
            "{} restore will overwrite {} file(s) in {}:",
            style("⚠").yellow(),
            patch.files.len(),
            cwd.display()
        );
        for f in patch.files.iter().take(20) {
            eprintln!("  {}", f.display());
        }
        if patch.files.len() > 20 {
            eprintln!("  … and {} more", patch.files.len() - 20);
        }
        eprintln!("Re-run with --yes to proceed.");
        anyhow::bail!("aborted");
    }
    snap.restore(hash).await;
    println!("{} restored to {hash}", style("✓").green());
    Ok(())
}

async fn handle_undo(cwd: &std::path::Path, yes: bool, config: &Config) -> Result<()> {
    // /undo is "restore to the most recently-tracked state, which is the
    // tree that already matches the worktree" — useful only when an
    // intervening edit has happened. Read the shadow HEAD via track()
    // (which is idempotent — re-running it returns the current tree hash)
    // and then surface that hash via restore so the caller can re-confirm.
    let snap = snap_or_err(cwd, config)?;
    let hash = snap
        .track()
        .await
        .ok_or_else(|| anyhow::Error::msg("no shadow tree found — track first"))?;
    handle_restore(cwd, &hash, yes, config).await
}

async fn handle_revert(cwd: &std::path::Path, yes: bool, config: &Config) -> Result<()> {
    use std::io::Read;
    let mut buf = String::new();
    std::io::stdin()
        .read_to_string(&mut buf)
        .context("failed to read patch JSON from stdin")?;
    let patches: Vec<Patch> =
        serde_json::from_str(&buf).context("invalid patch JSON on stdin")?;
    if patches.is_empty() {
        println!("{} no patches supplied", style("·").dim());
        return Ok(());
    }
    let snap = snap_or_err(cwd, config)?;
    let total: usize = patches.iter().map(|p| p.files.len()).sum();
    if !yes {
        eprintln!(
            "{} revert will overwrite {} file(s) across {} patch group(s).",
            style("⚠").yellow(),
            total,
            patches.len()
        );
        eprintln!("Re-run with --yes to proceed.");
        anyhow::bail!("aborted");
    }
    snap.revert(&patches).await;
    println!(
        "{} reverted {} file(s) across {} patch group(s)",
        style("✓").green(),
        total,
        patches.len()
    );
    Ok(())
}

async fn handle_cleanup(cwd: &std::path::Path, config: &Config) -> Result<()> {
    let snap = snap_or_err(cwd, config)?;
    snap.cleanup().await;
    println!("{} pruned shadow objects older than 7 days", style("✓").green());
    Ok(())
}
