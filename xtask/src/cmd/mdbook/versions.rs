use serde_json::json;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

// Simple struct to represent parsed semver for sorting
#[derive(Debug, PartialEq, Eq)]
struct Version {
    major: u32,
    minor: u32,
    patch: u32,
    pre: Option<String>,
    tag: String,
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.major
            .cmp(&other.major)
            .then(self.minor.cmp(&other.minor))
            .then(self.patch.cmp(&other.patch))
            .then_with(|| match (&self.pre, &other.pre) {
                // No pre-release is greater than pre-release
                (None, None) => std::cmp::Ordering::Equal,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (Some(_), None) => std::cmp::Ordering::Less,
                (Some(a), Some(b)) => a.cmp(b),
            })
    }
}

fn parse_version(tag: &str) -> Option<Version> {
    if !tag.starts_with('v') {
        return None;
    }
    let rest = &tag[1..];
    let (base, pre) = match rest.find('-') {
        Some(idx) => (&rest[..idx], Some(rest[idx + 1..].to_string())),
        None => (rest, None),
    };

    let parts: Vec<&str> = base.split('.').collect();
    if parts.len() != 3 {
        return None;
    }

    let major = parts[0].parse().ok()?;
    let minor = parts[1].parse().ok()?;
    let patch = parts[2].parse().ok()?;

    Some(Version {
        major,
        minor,
        patch,
        pre,
        tag: tag.to_string(),
    })
}

/// True when a gh-pages root directory name denotes a deployable docs version:
/// `master` or a `vX.Y.Z[-pre]` tag. `stable` is intentionally NOT a version
/// dir. Stable resolves to a real version via the committed pointer, never a
/// duplicate `stable/` tree. A leftover `stable/` from the old layout is treated
/// as a non-version dir and pruned by `prune_root`. Single source of truth
/// shared by versions.json generation, root pruning, and selector retrofit.
pub fn is_version_dir(name: &str) -> bool {
    name == "master" || parse_version(name).is_some()
}

/// Root-level directories that are not version dirs but must survive pruning.
const ROOT_KEEP_DIRS: &[&str] = &["_shared", ".git"];

/// Remove orphaned root *directories* left over from the pre-versioned docs
/// layout (e.g. top-level `en/`, `fr/`, `api/`). Keeps the shared chrome dir
/// and every recognized version dir. Root *files* are never touched — the
/// orphans are all directories, and a closed file allowlist would silently
/// delete legitimate root files a future deploy might add (`404.html`,
/// `robots.txt`, `sitemap.xml`, ...). Operates on the current working
/// directory (the gh-pages clone root).
pub fn prune_root() -> anyhow::Result<()> {
    let entries = fs::read_dir(".")?;
    for entry in entries.flatten() {
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if ROOT_KEEP_DIRS.contains(&name.as_str()) || is_version_dir(&name) {
            continue;
        }
        println!("prune-root: removing orphaned dir {name}/");
        fs::remove_dir_all(entry.path())?;
    }
    Ok(())
}

/// Default number of stable releases to retain on gh-pages, in addition to
/// `master` and `stable`. Overridable via `DOCS_KEEP_VERSIONS`.
const DEFAULT_KEEP_VERSIONS: usize = 3;

fn keep_versions_limit() -> usize {
    env::var("DOCS_KEEP_VERSIONS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(DEFAULT_KEEP_VERSIONS)
}

/// Decide which version dirs survive a retention pass. `master` and `stable`
/// are always kept. Among `vX.Y.Z` tag dirs, the newest `keep` final releases
/// (no pre-release suffix) survive; every pre-release and every older final
/// release is dropped. Pure decision over names so it is unit-testable without
/// touching the filesystem.
fn retained_versions(present: &[String], keep: usize) -> Vec<String> {
    let mut kept: Vec<String> = present
        .iter()
        .filter(|n| n.as_str() == "master")
        .cloned()
        .collect();

    let mut finals: Vec<Version> = present
        .iter()
        .filter_map(|n| parse_version(n))
        .filter(|v| v.pre.is_none())
        .collect();
    finals.sort_by(|a, b| b.cmp(a));
    kept.extend(finals.into_iter().take(keep).map(|v| v.tag));
    kept
}

/// Retention pass for the ephemeral gh-pages tree: keep `master`, `stable`, and
/// the newest `DOCS_KEEP_VERSIONS` final releases; remove every other version
/// dir (all pre-releases and older finals). Non-version root entries are left to
/// `prune_root`. Operates on the gh-pages clone root. Every clone pays a roughly
/// linear packed cost per retained version, so capping the set caps clone size.
pub fn prune_versions() -> anyhow::Result<()> {
    let keep = keep_versions_limit();
    let mut present = Vec::new();
    for entry in fs::read_dir(".")?.flatten() {
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if is_version_dir(&name) {
            present.push(name);
        }
    }

    let retained = retained_versions(&present, keep);
    for name in &present {
        if retained.contains(name) {
            continue;
        }
        println!("prune-versions: removing {name}/");
        fs::remove_dir_all(Path::new(name))?;
    }
    Ok(())
}

/// Inject the shared version-selector script into every deployed version page
/// that lacks it. Old tags built before the selector existed render without a
/// version dropdown; this retrofits the `<script>` reference so any deployed
/// version — past, present, or future — surfaces the dropdown that reads the
/// root `versions.json`. Idempotent: pages that already reference the selector
/// are left untouched. Operates on the gh-pages clone root.
pub fn retrofit_selector() -> anyhow::Result<()> {
    let mut patched = 0usize;
    for entry in fs::read_dir(".")?.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if !entry.file_type()?.is_dir() || !is_version_dir(&name) {
            continue;
        }
        patched += retrofit_dir(&entry.path())?;
    }
    println!("retrofit-selector: patched {patched} page(s)");
    Ok(())
}

fn retrofit_dir(version_root: &Path) -> anyhow::Result<usize> {
    let mut stack: Vec<PathBuf> = vec![version_root.to_path_buf()];
    let mut patched = 0usize;
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir)?.flatten() {
            let path = entry.path();
            let ty = entry.file_type()?;
            if ty.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "html") && retrofit_file(&path)? {
                patched += 1;
            }
        }
    }
    Ok(patched)
}

/// Patch one HTML file. Returns true if it was modified. Skips pages that
/// already reference the selector or have no menu bar to host the dropdown.
fn retrofit_file(path: &Path) -> anyhow::Result<bool> {
    let content = fs::read_to_string(path)?;
    if content.contains("theme/version-selector.js") {
        return Ok(false);
    }
    if !content.contains("right-buttons") {
        return Ok(false); // No menu bar — nothing for the dropdown to attach to.
    }
    let Some(prefix) = shared_prefix(path) else {
        return Ok(false);
    };
    let script =
        format!("        <script src=\"{prefix}_shared/theme/version-selector.js\"></script>\n");
    // Insert just before </body> so it loads with the other chrome scripts.
    let Some(pos) = content.rfind("</body>") else {
        return Ok(false);
    };
    let line_start = content[..pos].rfind('\n').map(|i| i + 1).unwrap_or(pos);
    let mut updated = String::with_capacity(content.len() + script.len());
    updated.push_str(&content[..line_start]);
    updated.push_str(&script);
    updated.push_str(&content[line_start..]);
    fs::write(path, updated)?;
    Ok(true)
}

/// Relative prefix from `file` (under the gh-pages root) up to the root, where
/// `_shared/` lives. One `../` per directory level above the file. E.g.
/// `master/en/index.html` -> `../../`, `v1.2.3/en/a/b.html` -> `../../../`.
/// Only real path segments count — a leading `./` from `read_dir(".")` must not
/// inflate the depth, or the injected `_shared` ref would 404.
fn shared_prefix(file: &Path) -> Option<String> {
    let depth = file
        .components()
        .filter(|c| matches!(c, std::path::Component::Normal(_)))
        .count();
    if depth < 1 {
        return None;
    }
    Some("../".repeat(depth - 1))
}

/// Resolve the stable tag for the docs site. Source of truth is the committed
/// pointer `docs/book/stable-version.txt`, copied to the gh-pages root as
/// `stable-version.txt` at deploy. Running `bump-version` writes it, so it
/// records the maintainer's explicit "this version is stable" decision rather
/// than inferring it by numeric comparison. Returns None when absent or when
/// the pointed-at version dir is not present in the deployed set.
fn resolve_stable(present: &[String]) -> Option<String> {
    let raw = fs::read_to_string("stable-version.txt").ok()?;
    let tag = raw.trim().to_string();
    if tag.is_empty() {
        return None;
    }
    present.contains(&tag).then_some(tag)
}

/// Build the versions.json payload from the version dirs present in the gh-pages
/// root. `master` is labeled Development; the stable tag (from the committed
/// pointer) is labeled "Stable (latest release)"; all other tags use their bare
/// tag as label. There is no synthetic `stable` entry or `stable/` dir. Stable
/// resolves to the real version dir, never a duplicate copy.
fn build_versions_json(present: &[String], stable: Option<&str>) -> serde_json::Value {
    let min_parsed = env::var("DOCS_MIN_VERSION")
        .ok()
        .filter(|s| !s.is_empty())
        .and_then(|s| parse_version(&s));

    let mut dirs: Vec<String> = present
        .iter()
        .filter(|name| {
            if name.as_str() == "master" {
                return true;
            }
            match (parse_version(name), &min_parsed) {
                (Some(v), Some(min)) => v >= *min,
                (Some(_), None) => true,
                _ => false,
            }
        })
        .cloned()
        .collect();

    dirs.sort_by(|a, b| {
        if a == "master" {
            return std::cmp::Ordering::Less;
        }
        if b == "master" {
            return std::cmp::Ordering::Greater;
        }
        match (parse_version(a), parse_version(b)) {
            (Some(va), Some(vb)) => vb.cmp(&va),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => a.cmp(b),
        }
    });

    let versions: Vec<_> = dirs
        .iter()
        .map(|tag| {
            let label = if tag == "master" {
                "Development (master)".to_string()
            } else if Some(tag.as_str()) == stable {
                "Stable (latest release)".to_string()
            } else {
                tag.clone()
            };
            json!({ "tag": tag, "label": label })
        })
        .collect();

    json!({ "stable": stable, "versions": versions })
}

/// Emit the gh-pages root `index.html`: a redirect to the stable version's
/// English landing page, or to master when no stable pointer resolves. Reads
/// the same `stable-version.txt` pointer as `gen_versions`, so root and selector
/// always agree on what Stable is. Prints HTML to stdout.
pub fn gen_root_index() -> anyhow::Result<()> {
    let mut present = Vec::new();
    for entry in fs::read_dir(".")?.flatten() {
        if entry.file_type()?.is_dir() {
            let name = entry.file_name().to_string_lossy().to_string();
            if is_version_dir(&name) {
                present.push(name);
            }
        }
    }
    let target = resolve_stable(&present).unwrap_or_else(|| "master".to_string());
    let dest = format!("./{target}/en/");
    print!(
        "<!doctype html>\n\
         <meta charset=\"utf-8\">\n\
         <meta http-equiv=\"refresh\" content=\"0; url={dest}\">\n\
         <link rel=\"canonical\" href=\"{dest}\">\n\
         <title>ZeroClaw Docs</title>\n"
    );
    Ok(())
}

pub fn run() -> anyhow::Result<()> {
    let mut present = Vec::new();
    if let Ok(entries) = fs::read_dir(".") {
        for entry in entries.flatten() {
            if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                let name = entry.file_name().to_string_lossy().to_string();
                if is_version_dir(&name) {
                    present.push(name);
                }
            }
        }
    }
    let stable = resolve_stable(&present);
    let output = build_versions_json(&present, stable.as_deref());
    println!("{}", serde_json::to_string_pretty(&output)?);

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_prefix_matches_page_depth() {
        // Depth counts only real segments; a leading `./` must not inflate it,
        // or the injected _shared ref 404s.
        let two = "..".to_string() + "/" + ".." + "/";
        let three = "..".to_string() + "/" + ".." + "/" + ".." + "/";
        assert_eq!(
            shared_prefix(Path::new("./master/en/index.html")).unwrap(),
            two
        );
        assert_eq!(
            shared_prefix(Path::new("master/en/index.html")).unwrap(),
            two
        );
        assert_eq!(
            shared_prefix(Path::new("v0.8.0-beta-2/en/architecture/crates.html")).unwrap(),
            three
        );
    }

    #[test]
    fn is_version_dir_accepts_only_real_versions() {
        for ok in ["master", "v0.8.0", "v0.8.0-beta-2", "v1.2.3"] {
            assert!(is_version_dir(ok), "{ok} should be a version dir");
        }
        for orphan in [
            "en", "fr", "zh-CN", "api", "_shared", "main", "v1.2", "stable",
        ] {
            assert!(
                !is_version_dir(orphan),
                "{orphan} must not be a version dir"
            );
        }
    }

    #[test]
    fn retained_keeps_master_and_newest_finals() {
        let present = vec![
            "master".to_string(),
            "v0.8.0".to_string(),
            "v0.7.5".to_string(),
            "v0.7.4".to_string(),
            "v0.8.0-beta-1".to_string(),
            "v0.8.0-beta-2".to_string(),
        ];
        let kept = retained_versions(&present, 2);
        assert!(kept.contains(&"master".to_string()));
        assert!(kept.contains(&"v0.8.0".to_string()));
        assert!(kept.contains(&"v0.7.5".to_string()));
        assert!(!kept.contains(&"v0.7.4".to_string()), "older final dropped");
        assert!(
            !kept.contains(&"v0.8.0-beta-1".to_string()),
            "pre-release dropped"
        );
        assert!(
            !kept.contains(&"v0.8.0-beta-2".to_string()),
            "pre-release dropped"
        );
    }

    #[test]
    fn retained_drops_all_prereleases_even_when_no_final_matches() {
        let present = vec![
            "master".to_string(),
            "v0.9.0-beta-1".to_string(),
            "v0.9.0-beta-2".to_string(),
        ];
        let kept = retained_versions(&present, 3);
        assert_eq!(kept, vec!["master".to_string()]);
    }

    #[test]
    fn retained_keep_zero_keeps_only_master() {
        let present = vec!["master".to_string(), "v1.0.0".to_string()];
        let kept = retained_versions(&present, 0);
        assert_eq!(kept, vec!["master".to_string()]);
    }

    #[test]
    fn stable_label_applies_to_pointer_target_only() {
        let present = vec![
            "master".to_string(),
            "v0.8.0".to_string(),
            "v0.7.5".to_string(),
        ];
        let out = build_versions_json(&present, Some("v0.8.0"));
        assert_eq!(out["stable"], "v0.8.0");
        let versions = out["versions"].as_array().unwrap();
        let labels: std::collections::HashMap<&str, &str> = versions
            .iter()
            .map(|v| (v["tag"].as_str().unwrap(), v["label"].as_str().unwrap()))
            .collect();
        assert_eq!(labels["master"], "Development (master)");
        assert_eq!(labels["v0.8.0"], "Stable (latest release)");
        assert_eq!(labels["v0.7.5"], "v0.7.5");
    }

    #[test]
    fn no_synthetic_stable_entry_in_versions_list() {
        let present = vec!["master".to_string(), "v0.8.0".to_string()];
        let out = build_versions_json(&present, Some("v0.8.0"));
        let tags: Vec<&str> = out["versions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["tag"].as_str().unwrap())
            .collect();
        assert!(!tags.contains(&"stable"), "no synthetic stable entry");
    }
}
