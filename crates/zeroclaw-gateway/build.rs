use std::process::Command;

fn main() {
    // For `cargo install` users: attempt a best-effort npm build so the
    // dashboard is available out of the box. If node/npm is missing or
    // the build fails, we skip silently — the binary works fine without it.
    build_web_dashboard();
    ensure_embedded_web_dist_when_enabled();
    emit_git_info();
}

/// Captures git commit SHA and dirty-tree status at build time and exposes
/// them as `GIT_COMMIT_SHA` / `GIT_DIRTY` env vars for `env!()` in the crate.
///
/// Fails silently in environments where git is unavailable or the source was
/// extracted from a tarball — `"unknown"` / `"false"` are used as fallbacks
/// so the binary still builds cleanly in those cases (e.g. Docker multi-stage
/// builds, `cargo install` from crates.io, CI without `.git`).
fn emit_git_info() {
    // Resolve the workspace root so we can point at .git/ from this crate's
    // build script regardless of where cargo runs from.
    let git_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .map(|root| root.join(".git"));

    // Rerun the build script when HEAD or any ref changes so the embedded
    // SHA stays current on every commit or branch switch.
    if let Some(ref git_dir) = git_dir
        && git_dir.exists()
    {
        println!("cargo:rerun-if-changed={}", git_dir.join("HEAD").display());
        println!("cargo:rerun-if-changed={}", git_dir.join("refs").display());
    }

    // Short commit SHA — 9 hex chars gives 1-in-34-billion collision odds,
    // comfortably unambiguous for build identification.
    let sha = Command::new("git")
        .args(["rev-parse", "--short=9", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".to_string());

    println!("cargo:rustc-env=GIT_COMMIT_SHA={sha}");

    // Dirty flag: non-empty `git status --porcelain` means uncommitted changes.
    let dirty = Command::new("git")
        .args(["status", "--porcelain"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| !o.stdout.is_empty())
        .unwrap_or(false);

    println!("cargo:rustc-env=GIT_DIRTY={dirty}");
}

fn build_web_dashboard() {
    let web_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .map(|root| root.join("web"));

    let Some(web_dir) = web_dir else { return };
    if !web_dir.join("package.json").exists() {
        return;
    }

    // Emit rerun-if-changed before any early return so cargo registers
    // the dependency. Without it, source edits don't re-invoke the
    // script and stale dist/ stays served against changed web/src.
    println!(
        "cargo:rerun-if-changed={}",
        web_dir.join("package.json").display()
    );
    println!("cargo:rerun-if-changed={}", web_dir.join("src").display());

    let npm = if cfg!(target_os = "windows") {
        "npm.cmd"
    } else {
        "npm"
    };

    let ok = Command::new(npm)
        .args(["ci", "--ignore-scripts"])
        .current_dir(&web_dir)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    if !ok {
        // npm not available or install failed — skip silently
        return;
    }

    let _ = Command::new(npm)
        .args(["run", "build"])
        .current_dir(&web_dir)
        .status();
}

fn ensure_embedded_web_dist_when_enabled() {
    if std::env::var_os("CARGO_FEATURE_EMBEDDED_WEB").is_none() {
        return;
    }

    let web_dist = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .map(|root| root.join("web/dist"))
        .unwrap_or_default();

    println!("cargo:rerun-if-changed={}", web_dist.display());

    assert!(
        web_dist.join("index.html").exists(),
        "feature `embedded-web` requires `web/dist/index.html`; run: cargo web build"
    );
}
