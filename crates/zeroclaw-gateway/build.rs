use std::process::Command;

fn main() {
    // For `cargo install` users: attempt a best-effort npm build so the
    // dashboard is available out of the box. If node/npm is missing or
    // the build fails, we skip silently — the binary works fine without it.
    build_web_dashboard();
    ensure_embedded_web_dist_when_enabled();
}

fn build_web_dashboard() {
    // Use the runtime env var, not `env!`, so the path stays correct when
    // cargo reuses a cached build-script binary built under a different
    // CARGO_MANIFEST_DIR (e.g. GitLab CI's per-slot /builds/.../<slot> dir
    // changing between pipelines). `env!` bakes the value at build-script
    // compile time and goes stale across that cache reuse.
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR")
        .expect("CARGO_MANIFEST_DIR is always set by cargo for build scripts");
    let web_dir = std::path::Path::new(&manifest_dir)
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

    // See build_web_dashboard for why this reads the var at runtime.
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR")
        .expect("CARGO_MANIFEST_DIR is always set by cargo for build scripts");
    let web_dist = std::path::Path::new(&manifest_dir)
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
