//! Build-time provenance for ZeroClaw binaries.
//!
//! One package version covers hundreds of master commits, so the package
//! version alone cannot identify a build. Call [`emit`] from the package's
//! `build.rs`, then read the rendered string at
//! compile time with `env!("ZEROCLAW_VERSION")` (the key is [`VERSION_ENV`]).
//! Builds that cannot see a checkout (container images, crates.io unpacks)
//! supply the id through the [`BUILD_ID_ENV`] environment variable instead.
//! This is a build dependency only: nothing here is linked into a running
//! binary. Invalidating the stamp when refs move relinks a stamped binary
//! after every commit, source edits or not; in exchange, a cached binary
//! always keeps the stamp of the source it was actually built from.

use std::path::{Path, PathBuf};
use std::process::Command;

/// `cargo:rustc-env` key holding the version to display, e.g.
/// `0.8.5 (v0.8.5-332-g24e7324dc6-dirty)`.
pub const VERSION_ENV: &str = "ZEROCLAW_VERSION";

/// Environment variable that supplies the build id from outside the checkout.
/// Container images drop `.git` (`.dockerignore`), so the release workflow
/// passes the commit through this key and it wins over `git describe`. Empty or
/// whitespace-only counts as unset. It is an input only: nothing is published
/// back through `cargo:rustc-env`, because [`VERSION_ENV`] already carries the
/// composed string and no consumer exists for the parts.
pub const BUILD_ID_ENV: &str = "ZEROCLAW_BUILD_ID";

/// Build id when git or its history is unavailable: a crates.io tarball, a
/// shallow export, or a container build with no `.git`.
pub const UNKNOWN: &str = "unknown";

/// Renders the display version from a package version and a build id.
pub fn version_string(package_version: &str, build_id: &str) -> String {
    format!("{package_version} ({build_id})")
}

fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8(out.stdout).ok()?;
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

/// `git describe --tags --always --dirty`: nearest tag plus commits-since plus
/// short hash, marked `-dirty` for uncommitted tracked changes. `--always`
/// degrades to a bare hash in an untagged history; untracked files never mark a
/// build, so scratch files cannot make every developer's build look dirty.
pub fn build_id(dir: &Path) -> String {
    git(dir, &["describe", "--tags", "--always", "--dirty"]).unwrap_or_else(|| UNKNOWN.to_string())
}

/// An externally supplied id wins over `git describe`: the builds that cannot
/// see a checkout are exactly the ones where git has nothing to say.
fn resolve_build_id(dir: &Path, external: Option<String>) -> String {
    external
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| build_id(dir))
}

/// git paths whose content moves when the commit being built moves. Watching
/// `.git/HEAD` alone freezes the stamp at the last branch switch, because
/// committing rewrites the branch ref and leaves `HEAD` untouched. The cost of
/// watching them is a relink of the stamped binaries on every commit, with or
/// without source edits. The git index is deliberately not watched: `git
/// describe --dirty` refreshes it during the build, so watching it would
/// invalidate every following build.
fn watched_refs(dir: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    for key in ["HEAD", "packed-refs"] {
        if let Some(path) = git_path(dir, key)
            && path.exists()
        {
            paths.push(path);
        }
    }
    if let Some(branch) = git(dir, &["symbolic-ref", "--quiet", "HEAD"])
        && let Some(path) = git_path(dir, &branch)
        && path.exists()
    {
        paths.push(path);
    }
    paths
}

/// Resolves a git path for `key` relative to `dir`, which may be a linked
/// worktree where `.git` is a file pointing elsewhere.
fn git_path(dir: &Path, key: &str) -> Option<PathBuf> {
    let raw = git(dir, &["rev-parse", "--git-path", key])?;
    let path = PathBuf::from(&raw);
    Some(if path.is_absolute() {
        path
    } else {
        dir.join(path)
    })
}

/// Emits provenance for the package whose `build.rs` calls this. Never fails:
/// without git and without [`BUILD_ID_ENV`] the build still completes and
/// reports [`UNKNOWN`].
pub fn emit() {
    let dir = manifest_dir();
    let build_id = resolve_build_id(&dir, std::env::var(BUILD_ID_ENV).ok());
    for line in instructions(&dir, &package_version(), &build_id) {
        println!("{line}");
    }
}

fn manifest_dir() -> PathBuf {
    std::env::var_os("CARGO_MANIFEST_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn package_version() -> String {
    std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| UNKNOWN.to_string())
}

fn instructions(dir: &Path, package_version: &str, build_id: &str) -> Vec<String> {
    let mut lines = vec![
        format!(
            "cargo:rustc-env={VERSION_ENV}={}",
            version_string(package_version, build_id)
        ),
        // Image builds share a cached `target` dir across commits, so without
        // this cargo can serve a stamp from the previous build's input.
        format!("cargo:rerun-if-env-changed={BUILD_ID_ENV}"),
    ];
    lines.extend(
        watched_refs(dir)
            .iter()
            .map(|path| format!("cargo:rerun-if-changed={}", path.display())),
    );
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_describe_id_with_the_package_version() {
        assert_eq!(
            version_string("0.8.5", "v0.8.5-332-g24e7324dc6-dirty"),
            "0.8.5 (v0.8.5-332-g24e7324dc6-dirty)"
        );
    }

    #[test]
    fn unknown_build_id_stays_visible() {
        assert_eq!(version_string("0.8.5", UNKNOWN), "0.8.5 (unknown)");
    }

    #[test]
    fn instructions_publish_one_key_and_invalidate_on_every_input() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let id = build_id(dir);
        let lines = instructions(dir, "0.8.5", &id);
        assert!(
            lines.iter().any(|l| l
                == &format!(
                    "cargo:rustc-env={VERSION_ENV}={}",
                    version_string("0.8.5", &id)
                )),
            "rendered version missing: {lines:?}"
        );
        assert!(
            !lines
                .iter()
                .any(|l| l.starts_with(&format!("cargo:rustc-env={BUILD_ID_ENV}="))),
            "BUILD_ID_ENV is an input only; a second emitted key has no consumer: {lines:?}"
        );
        assert!(
            lines
                .iter()
                .any(|l| l == &format!("cargo:rerun-if-env-changed={BUILD_ID_ENV}")),
            "without this a cached image build keeps the previous build's id: {lines:?}"
        );
        assert!(
            lines
                .iter()
                .any(|l| l.starts_with("cargo:rerun-if-changed=")),
            "nothing invalidates the stamp when the commit moves: {lines:?}"
        );
    }

    #[test]
    fn external_build_id_beats_git_and_blanks_do_not_count() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        assert_eq!(
            resolve_build_id(dir, Some("24e7324dc6".to_string())),
            "24e7324dc6"
        );
        assert_eq!(resolve_build_id(dir, Some("  ".to_string())), build_id(dir));
        assert_eq!(resolve_build_id(dir, None), build_id(dir));
    }

    #[test]
    fn watched_refs_include_the_file_a_commit_rewrites() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let Some(branch) = git(dir, &["symbolic-ref", "--quiet", "HEAD"]) else {
            return; // detached HEAD: no branch ref advances on commit
        };
        let expected = git_path(dir, &branch).expect("branch ref resolves to a path");
        assert!(
            watched_refs(dir).contains(&expected),
            "committing advances {expected:?}, so the stamp must be invalidated by it"
        );
    }

    #[test]
    fn build_id_resolves_inside_a_real_checkout() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let id = build_id(dir);
        assert_ne!(id, UNKNOWN, "tests run inside the repository");
        assert!(
            id.contains('g') || id.chars().all(|c| c.is_ascii_hexdigit()),
            "unexpected build id: {id}"
        );
        assert!(
            !watched_refs(dir).is_empty(),
            "commit changes must be able to invalidate the stamp"
        );
    }
}
