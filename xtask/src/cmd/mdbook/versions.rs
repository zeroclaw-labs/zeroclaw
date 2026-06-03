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
/// `master`, `stable`, or a `vX.Y.Z[-pre]` tag. This is the single source of
/// truth shared by `versions.json` generation and root pruning so the two can
/// never disagree about what counts as a version.
pub fn is_version_dir(name: &str) -> bool {
    name == "master" || name == "stable" || parse_version(name).is_some()
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

pub fn run() -> anyhow::Result<()> {
    let min_version_env = env::var("DOCS_MIN_VERSION").unwrap_or_default();
    let min_parsed = if min_version_env.is_empty() {
        None
    } else {
        parse_version(&min_version_env)
    };

    let mut dirs = Vec::new();
    if let Ok(entries) = fs::read_dir(".") {
        for entry in entries.flatten() {
            if let Ok(file_type) = entry.file_type() {
                if !file_type.is_dir() {
                    continue;
                }
                let name = entry.file_name().to_string_lossy().to_string();
                if name == "master" || name == "stable" {
                    dirs.push(name);
                } else if parse_version(&name).is_some() {
                    // Check floor
                    if let Some(min) = &min_parsed
                        && let Some(parsed) = parse_version(&name)
                        && parsed < *min
                    {
                        continue;
                    }
                    dirs.push(name);
                }
            }
        }
    }

    // Sort: master, then stable, then tagged versions newest-first
    dirs.sort_by(|a, b| {
        if a == "master" && b != "master" {
            return std::cmp::Ordering::Less;
        }
        if b == "master" && a != "master" {
            return std::cmp::Ordering::Greater;
        }
        if a == "stable" && b != "stable" {
            return std::cmp::Ordering::Less;
        }
        if b == "stable" && a != "stable" {
            return std::cmp::Ordering::Greater;
        }

        let ver_a = parse_version(a);
        let ver_b = parse_version(b);
        match (ver_a, ver_b) {
            (Some(va), Some(vb)) => vb.cmp(&va), // Newest first
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => a.cmp(b),
        }
    });

    let mut versions = Vec::new();
    let mut stable_tag = None;

    for tag in &dirs {
        let label = match tag.as_str() {
            "master" => "Development (master)".to_string(),
            "stable" => {
                stable_tag = Some(tag.clone());
                "Stable".to_string()
            }
            _ => tag.clone(),
        };
        versions.push(json!({
            "tag": tag,
            "label": label,
        }));
    }

    if stable_tag.is_none() {
        // Find latest stable version (no pre-release)
        for tag in dirs.iter().rev() {
            if let Some(v) = parse_version(tag)
                && v.pre.is_none()
            {
                stable_tag = Some(tag.clone());
                break;
            }
        }
    }

    let output = json!({
        "stable": stable_tag,
        "versions": versions,
    });

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
        for ok in ["master", "stable", "v0.8.0", "v0.8.0-beta-2", "v1.2.3"] {
            assert!(is_version_dir(ok), "{ok} should be a version dir");
        }
        for orphan in ["en", "fr", "zh-CN", "api", "_shared", "main", "v1.2"] {
            assert!(
                !is_version_dir(orphan),
                "{orphan} must not be a version dir"
            );
        }
    }
}
