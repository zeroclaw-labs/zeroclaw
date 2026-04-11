use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::SystemTime;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    // Watch web SOURCES — not web/dist (the output). This avoids a
    // self-invalidation loop where writing to web/dist triggers the build
    // script to rerun on the next `cargo build`.
    println!("cargo:rerun-if-changed=../../web/src");
    println!("cargo:rerun-if-changed=../../web/public");
    println!("cargo:rerun-if-changed=../../web/index.html");
    println!("cargo:rerun-if-changed=../../docs/assets/zeroclaw-trans.png");
    println!("cargo:rerun-if-changed=../../web/package.json");
    println!("cargo:rerun-if-changed=../../web/package-lock.json");
    println!("cargo:rerun-if-changed=../../web/tsconfig.json");
    println!("cargo:rerun-if-changed=../../web/tsconfig.app.json");
    println!("cargo:rerun-if-changed=../../web/tsconfig.node.json");
    println!("cargo:rerun-if-changed=../../web/vite.config.ts");

    let dist_dir = Path::new("../../web/dist");
    let web_dir = Path::new("../../web");

    // Attempt to build the web frontend if npm is available and web/dist is
    // missing or stale.  The build is best-effort: when Node.js is not
    // installed (e.g. CI containers, cross-compilation, minimal dev setups)
    // we fall back to the existing stub/empty dist directory so the Rust
    // build still succeeds (rust-embed just embeds whatever is there).
    let needs_build = web_build_required(web_dir, dist_dir);

    if needs_build
        && web_dir.join("package.json").exists()
        && let Ok(npm) = which_npm()
    {
        eprintln!("cargo:warning=Building web frontend (web/dist is missing or stale)...");

        // npm ci / npm install
        let install_status = Command::new(&npm)
            .args(["ci", "--ignore-scripts"])
            .current_dir(web_dir)
            .status();

        match install_status {
            Ok(s) if s.success() => {}
            Ok(s) => {
                eprintln!("cargo:warning=npm ci exited with {s}, trying npm install...");
                let fallback = Command::new(&npm)
                    .args(["install"])
                    .current_dir(web_dir)
                    .status();
                if !matches!(fallback, Ok(s) if s.success()) {
                    eprintln!("cargo:warning=npm install failed — skipping web build");
                    ensure_dist_dir(dist_dir);
                    return;
                }
            }
            Err(e) => {
                eprintln!("cargo:warning=Could not run npm: {e} — skipping web build");
                ensure_dist_dir(dist_dir);
                return;
            }
        }

        // npm run build
        let build_status = Command::new(&npm)
            .args(["run", "build"])
            .current_dir(web_dir)
            .status();

        match build_status {
            Ok(s) if s.success() => {
                eprintln!("cargo:warning=Web frontend built successfully.");
            }
            Ok(s) => {
                eprintln!(
                    "cargo:warning=npm run build exited with {s} — web dashboard may be unavailable"
                );
            }
            Err(e) => {
                eprintln!(
                    "cargo:warning=Could not run npm build: {e} — web dashboard may be unavailable"
                );
            }
        }
    }

    ensure_dist_dir(dist_dir);
    ensure_dashboard_assets(dist_dir);
}

fn web_build_required(web_dir: &Path, dist_dir: &Path) -> bool {
    let Some(dist_mtime) = latest_modified(dist_dir) else {
        return true;
    };

    [
        web_dir.join("src"),
        web_dir.join("public"),
        web_dir.join("index.html"),
        web_dir.join("package.json"),
        web_dir.join("package-lock.json"),
        web_dir.join("tsconfig.json"),
        web_dir.join("tsconfig.app.json"),
        web_dir.join("tsconfig.node.json"),
        web_dir.join("vite.config.ts"),
    ]
    .into_iter()
    .filter_map(|path| latest_modified(&path))
    .any(|mtime| mtime > dist_mtime)
}

fn latest_modified(path: &Path) -> Option<SystemTime> {
    let metadata = fs::metadata(path).ok()?;
    if metadata.is_file() {
        return metadata.modified().ok();
    }
    if !metadata.is_dir() {
        return None;
    }

    let mut latest = metadata.modified().ok();
    let entries = fs::read_dir(path).ok()?;
    for entry in entries.flatten() {
        if let Some(child_mtime) = latest_modified(&entry.path()) {
            latest = Some(match latest {
                Some(current) if current >= child_mtime => current,
                _ => child_mtime,
            });
        }
    }
    latest
}

/// Ensure the dist directory exists so `rust-embed` does not fail at compile
/// time even when the web frontend is not built.
fn ensure_dist_dir(dist_dir: &Path) {
    if !dist_dir.exists() {
        std::fs::create_dir_all(dist_dir).expect("failed to create web/dist/");
    }
}

fn ensure_dashboard_assets(dist_dir: &Path) {
    let src = Path::new("../../docs/assets/zeroclaw-trans.png");
    if !src.exists() {
        eprintln!(
            "cargo:warning=docs/assets/zeroclaw-trans.png not found; skipping dashboard asset copy"
        );
        return;
    }

    let dst = dist_dir.join("zeroclaw-trans.png");
    if let Err(e) = fs::copy(src, &dst) {
        eprintln!("cargo:warning=Failed to copy zeroclaw-trans.png into web/dist/: {e}");
    }
}

/// Locate the `npm` binary on the system PATH.
fn which_npm() -> Result<String, ()> {
    let cmd = if cfg!(target_os = "windows") {
        "where"
    } else {
        "which"
    };

    Command::new(cmd)
        .arg("npm")
        .output()
        .ok()
        .and_then(|output| {
            if output.status.success() {
                String::from_utf8(output.stdout)
                    .ok()
                    .map(|s| s.lines().next().unwrap_or("npm").trim().to_string())
            } else {
                None
            }
        })
        .ok_or(())
}
