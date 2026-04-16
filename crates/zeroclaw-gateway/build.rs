use std::process::Command;

fn main() {
    // Web dashboard is served from the filesystem at runtime via
    // `gateway.web_dist_dir` — no compile-time embedding needed.
    //
    // For `cargo install` users: attempt a best-effort npm build so the
    // dashboard is available out of the box. If node/npm is missing or
    // the build fails, we skip silently — the binary works fine without it.
    build_web_dashboard();
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

    // Already built — skip
    if web_dir.join("dist/index.html").exists() {
        return;
    }

    // Rerun if the web source changes
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
