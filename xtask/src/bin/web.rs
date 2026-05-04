//! `cargo xtask web` — drive the web dashboard build from cargo.
//!
//! Subcommands:
//!   gen-api  — render the gateway's OpenAPI 3.1 spec in-process, write
//!              it to `target/openapi.json` (gitignored), and feed it to
//!              `npx openapi-typescript` to produce
//!              `web/src/lib/api-generated.ts`. Neither file is
//!              committed; both are derived artifacts.
//!   install  — `npm install` in `web/`.
//!   build    — gen-api + `npm run build` (vite production bundle).
//!   dev      — gen-api + `npm run dev` (vite dev server).
//!   check    — gen-api + `npx tsc -b` (typecheck without bundling).
//!
//! See #6175.

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::path::{Path, PathBuf};
use std::process::Command;
use xtask::util::{repo_root, require_tool, run_cmd};

#[derive(Parser, Debug)]
#[command(name = "web", about = "Build the ZeroClaw web dashboard")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Render the gateway's OpenAPI spec and regenerate the TS client.
    GenApi,
    /// Run `npm install` in web/.
    Install,
    /// Regenerate the TS client and run `npm run build`.
    Build,
    /// Regenerate the TS client and start `npm run dev`.
    Dev,
    /// Regenerate the TS client and typecheck (`tsc -b`) without
    /// producing a bundle.
    Check,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let root = repo_root();
    let web_dir = root.join("web");
    let spec_path = root.join("target/openapi.json");
    match cli.cmd {
        Cmd::GenApi => gen_api(&web_dir, &spec_path),
        Cmd::Install => npm_install(&web_dir),
        Cmd::Build => {
            gen_api(&web_dir, &spec_path)?;
            npm_run(&web_dir, "build")
        }
        Cmd::Dev => {
            gen_api(&web_dir, &spec_path)?;
            npm_run(&web_dir, "dev")
        }
        Cmd::Check => {
            gen_api(&web_dir, &spec_path)?;
            npx(&web_dir, &["tsc", "-b"])
        }
    }
}

fn npm_install(web_dir: &Path) -> Result<()> {
    require_tool("npm", "https://nodejs.org/ or `nvm install --lts`")?;
    println!("==> npm install ({})", web_dir.display());
    run_cmd(Command::new(bin("npm")).current_dir(web_dir).arg("install"))
}

/// Whether `web/node_modules` is missing or stale relative to `package-lock.json`.
///
/// npm writes a `node_modules/.package-lock.json` sentinel after every successful
/// install. If it's absent, or if the checked-in `package-lock.json` is newer
/// (a pull or merge updated dependencies), the install needs to re-run.
fn node_modules_needs_install(web_dir: &Path) -> bool {
    let node_modules = web_dir.join("node_modules");
    if !node_modules.exists() {
        return true;
    }
    let sentinel = node_modules.join(".package-lock.json");
    let lock = web_dir.join("package-lock.json");
    let (Ok(sentinel_meta), Ok(lock_meta)) = (sentinel.metadata(), lock.metadata()) else {
        // Sentinel missing → install never completed cleanly. Lock missing → treat
        // node_modules as authoritative (no signal to re-install).
        return !sentinel.exists() && lock.exists();
    };
    match (sentinel_meta.modified(), lock_meta.modified()) {
        (Ok(sentinel_t), Ok(lock_t)) => lock_t > sentinel_t,
        _ => false,
    }
}

fn npm_run(web_dir: &Path, script: &str) -> Result<()> {
    println!("==> npm run {script}");
    run_cmd(
        Command::new(bin("npm"))
            .current_dir(web_dir)
            .args(["run", script]),
    )
}

fn npx(web_dir: &Path, args: &[&str]) -> Result<()> {
    println!("==> npx {}", args.join(" "));
    let mut cmd = Command::new(bin("npx"));
    cmd.current_dir(web_dir).arg("--no-install").args(args);
    run_cmd(&mut cmd)
}

fn gen_api(web_dir: &Path, spec_path: &Path) -> Result<()> {
    require_tool("npm", "https://nodejs.org/ or `nvm install --lts`")?;
    if node_modules_needs_install(web_dir) {
        npm_install(web_dir)?;
    }
    let out_rel = PathBuf::from("src/lib/api-generated.ts");
    let out_abs = web_dir.join(&out_rel);
    if let Some(parent) = out_abs.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create parent directory {}", parent.display()))?;
    }
    if let Some(parent) = spec_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create parent directory {}", parent.display()))?;
    }

    let spec = serde_json::to_string(&zeroclaw_gateway::openapi::build_spec())
        .context("serialize openapi spec to JSON")?;
    std::fs::write(spec_path, &spec)
        .with_context(|| format!("write openapi spec to {}", spec_path.display()))?;
    println!("==> gen-api → {}", out_abs.display());

    let spec_arg = spec_path
        .to_str()
        .context("openapi spec path is not valid utf-8")?;
    let out_arg = out_rel
        .to_str()
        .context("api-generated.ts path is not valid utf-8")?;
    run_cmd(Command::new(bin("npx")).current_dir(web_dir).args([
        "--no-install",
        "openapi-typescript",
        spec_arg,
        "-o",
        out_arg,
    ]))
    .context("`npx openapi-typescript` failed (run `cargo web install` first?)")
}

fn bin(tool: &str) -> String {
    if cfg!(windows) {
        format!("{tool}.cmd")
    } else {
        tool.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::node_modules_needs_install;
    use std::fs;
    use std::thread::sleep;
    use std::time::Duration;
    use tempfile::TempDir;

    fn fresh_web_dir() -> TempDir {
        let dir = TempDir::new().expect("tempdir");
        fs::write(dir.path().join("package-lock.json"), "{}").unwrap();
        dir
    }

    #[test]
    fn needs_install_when_node_modules_missing() {
        let dir = fresh_web_dir();
        // No node_modules at all → must trigger install.
        assert!(node_modules_needs_install(dir.path()));
    }

    #[test]
    fn needs_install_when_sentinel_missing() {
        // node_modules exists but `.package-lock.json` sentinel was never written
        // (e.g. partial / failed previous install). Must trigger install.
        let dir = fresh_web_dir();
        fs::create_dir(dir.path().join("node_modules")).unwrap();
        assert!(node_modules_needs_install(dir.path()));
    }

    #[test]
    fn needs_install_when_lock_is_newer_than_sentinel() {
        // The actual staleness fix: a pull or merge updated package-lock.json
        // after the last install. Sentinel is OLDER than lock → reinstall.
        let dir = fresh_web_dir();
        let nm = dir.path().join("node_modules");
        fs::create_dir(&nm).unwrap();
        fs::write(nm.join(".package-lock.json"), "{}").unwrap();
        // Wait a measurable tick, then bump lock mtime.
        sleep(Duration::from_millis(20));
        fs::write(dir.path().join("package-lock.json"), "{ \"updated\": true }").unwrap();
        assert!(
            node_modules_needs_install(dir.path()),
            "stale node_modules (sentinel older than lock) must reinstall"
        );
    }

    #[test]
    fn skips_when_sentinel_is_at_least_as_new_as_lock() {
        // Clean install just finished. Sentinel mtime ≥ lock mtime → no
        // spurious reinstall on every build invocation.
        let dir = fresh_web_dir();
        let nm = dir.path().join("node_modules");
        fs::create_dir(&nm).unwrap();
        // Write sentinel AFTER lock so its mtime is newer.
        sleep(Duration::from_millis(20));
        fs::write(nm.join(".package-lock.json"), "{}").unwrap();
        assert!(
            !node_modules_needs_install(dir.path()),
            "fresh node_modules must not trigger spurious reinstall"
        );
    }

    #[test]
    fn skips_when_lock_missing() {
        // Defensive case: no package-lock.json (unusual — repo always has one).
        // Without a signal we cannot tell if node_modules is stale; treat as
        // valid to avoid a reinstall loop.
        let dir = TempDir::new().expect("tempdir");
        let nm = dir.path().join("node_modules");
        fs::create_dir(&nm).unwrap();
        fs::write(nm.join(".package-lock.json"), "{}").unwrap();
        assert!(!node_modules_needs_install(dir.path()));
    }
}
