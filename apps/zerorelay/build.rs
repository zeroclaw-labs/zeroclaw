//! Bake a version string into the binary at build time.
//!
//! Prefers `git describe --tags --always --dirty` (nearest tag + commits-since +
//! short hash, with a `-dirty` suffix on a modified tree). Falls back to the
//! crate version when git or the repository is unavailable (e.g. inside the
//! container build, which has no `.git` and no `git`).

use std::process::Command;

fn main() {
    let version = git_describe().unwrap_or_else(|| {
        std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "unknown".to_string())
    });
    println!("cargo:rustc-env=ZERORELAY_VERSION={version}");

    println!("cargo:rerun-if-changed=build.rs");
    // Rebuild when HEAD moves so --version tracks the current commit. Works in
    // git worktrees (--absolute-git-dir resolves the per-worktree git dir).
    if let Some(git_dir) = git_output(&["rev-parse", "--absolute-git-dir"]) {
        println!("cargo:rerun-if-changed={git_dir}/HEAD");
    }
}

fn git_describe() -> Option<String> {
    git_output(&["describe", "--tags", "--always", "--dirty"])
}

fn git_output(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (!s.is_empty()).then_some(s)
}
