use std::env;
use std::path::PathBuf;
use std::process::Command;

fn git_short_sha(manifest_dir: &str) -> Option<String> {
    let output = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .current_dir(manifest_dir)
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let short_sha = String::from_utf8(output.stdout).ok()?;
    let trimmed = short_sha.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn emit_git_rerun_hints(manifest_dir: &str) {
    let output = Command::new("git")
        .args(["rev-parse", "--git-dir"])
        .current_dir(manifest_dir)
        .output();

    let Ok(output) = output else {
        return;
    };
    if !output.status.success() {
        return;
    }

    let Ok(git_dir_raw) = String::from_utf8(output.stdout) else {
        return;
    };
    let git_dir_raw = git_dir_raw.trim();
    if git_dir_raw.is_empty() {
        return;
    }

    let git_dir = if PathBuf::from(git_dir_raw).is_absolute() {
        PathBuf::from(git_dir_raw)
    } else {
        PathBuf::from(manifest_dir).join(git_dir_raw)
    };

    println!("cargo:rerun-if-changed={}", git_dir.join("HEAD").display());
    println!("cargo:rerun-if-changed={}", git_dir.join("refs").display());
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=ZEROCLAW_GIT_SHORT_SHA");

    let manifest_dir = env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".to_string());
    emit_git_rerun_hints(&manifest_dir);

    let package_version = env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".to_string());
    let short_sha = env::var("ZEROCLAW_GIT_SHORT_SHA")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .or_else(|| git_short_sha(&manifest_dir));

    let build_version = if let Some(sha) = short_sha.as_deref() {
        format!("{package_version} ({sha})")
    } else {
        package_version
    };

    println!("cargo:rustc-env=ZEROCLAW_BUILD_VERSION={build_version}");
    println!(
        "cargo:rustc-env=ZEROCLAW_GIT_SHORT_SHA={}",
        short_sha.unwrap_or_default()
    );
}
