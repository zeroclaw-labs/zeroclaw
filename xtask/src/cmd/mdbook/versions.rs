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

/// Root directory that holds a copy of the stable release at a URL that does
/// not change from one release to the next. The stable pointer names the
/// release; this alias is what canonical, hreflang and sitemap URLs name, so a
/// release updates those pages in place instead of moving every one of them.
/// `version-selector.js` and `scripts/docs/promote_stable.py` use the same
/// directory name.
pub(super) const STABLE_ALIAS: &str = "stable";

pub fn is_version_dir(name: &str) -> bool {
    name == "master" || parse_version(name).is_some()
}

const ROOT_KEEP_DIRS: &[&str] = &["_shared", "api", ".git"];

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

/// Default number of final releases to retain on gh-pages, in addition to
/// `master` and the stable-pointer target. Overridable via `DOCS_KEEP_VERSIONS`.
const DEFAULT_KEEP_VERSIONS: usize = 3;

fn keep_versions_limit() -> usize {
    env::var("DOCS_KEEP_VERSIONS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(DEFAULT_KEEP_VERSIONS)
}

fn retained_versions(present: &[String], keep: usize, stable: Option<&str>) -> Vec<String> {
    let mut kept: Vec<String> = present
        .iter()
        .filter(|n| n.as_str() == "master")
        .cloned()
        .collect();

    if let Some(s) = stable
        && present.iter().any(|n| n == s)
        && !kept.iter().any(|n| n == s)
    {
        kept.push(s.to_string());
    }

    let mut finals: Vec<Version> = present
        .iter()
        .filter_map(|n| parse_version(n))
        .filter(|v| v.pre.is_none())
        .collect();
    finals.sort_by(|a, b| b.cmp(a));
    for v in finals.into_iter().take(keep) {
        if !kept.contains(&v.tag) {
            kept.push(v.tag);
        }
    }
    kept
}

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

    let stable = resolve_stable(&present);
    let retained = retained_versions(&present, keep, stable.as_deref());
    for name in &present {
        if retained.contains(name) {
            continue;
        }
        println!("prune-versions: removing {name}/");
        fs::remove_dir_all(Path::new(name))?;
    }
    Ok(())
}

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

pub(super) fn resolve_stable(present: &[String]) -> Option<String> {
    resolve_stable_in(Path::new("."), present)
}

/// [`resolve_stable`] against an explicit gh-pages root.
pub(super) fn resolve_stable_in(root: &Path, present: &[String]) -> Option<String> {
    let raw = fs::read_to_string(root.join("stable-version.txt")).ok()?;
    let tag = raw.trim().to_string();
    if tag.is_empty() {
        return None;
    }
    present.contains(&tag).then_some(tag)
}

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

/// Emit the gh-pages root `index.html`: a redirect to the English landing page
/// of the stable alias, or of master when no stable pointer resolves. Reads
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
    let target = if resolve_stable(&present).is_some() {
        STABLE_ALIAS
    } else {
        "master"
    };
    print!("{}", root_index_html(target));
    Ok(())
}

fn root_index_html(target: &str) -> String {
    let dest = format!("./{target}/en/");
    format!(
        "<!doctype html>\n\
         <meta charset=\"utf-8\">\n\
         <meta http-equiv=\"refresh\" content=\"0; url={dest}\">\n\
         <link rel=\"canonical\" href=\"{dest}\">\n\
         <title>ZeroClaw Docs</title>\n"
    )
}

/// Emit the gh-pages root `404.html`. GitHub Pages serves it, with status 404,
/// for every missing path. Retention pruning removes old release dirs, and the
/// legacy docs host forwards their URLs here unchanged, so a script sends a
/// `/<version>/<locale>/<page>` link, or one from the pre-versioned
/// `/<locale>/<page>` layout, to the same page under the stable alias (master
/// when no pointer resolves). Any other missing path shows the message. Prints
/// HTML to stdout.
pub fn gen_root_404() -> anyhow::Result<()> {
    let mut present = Vec::new();
    for entry in fs::read_dir(".")?.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if entry.file_type()?.is_dir() && is_version_dir(&name) {
            present.push(name);
        }
    }
    let (target, source) = match resolve_stable(&present) {
        Some(tag) => (STABLE_ALIAS, tag),
        None => ("master", "master".to_string()),
    };
    let mut locales = Vec::new();
    if let Ok(entries) = fs::read_dir(&source) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if entry.path().join("index.html").is_file() && is_locale_code(&name) {
                locales.push(name);
            }
        }
    }
    locales.sort();
    print!("{}", root_404_html(target, &locales)?);
    Ok(())
}

/// Locale directory names are interpolated into a script, so only plain codes
/// such as `en` or `zh-CN` qualify.
fn is_locale_code(name: &str) -> bool {
    !name.is_empty()
        && name
            .split('-')
            .all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_alphanumeric()))
}

/// The root 404 page's redirect logic. [`root_404_html`] sets its `target`
/// and `locales` on the two lines before it, and
/// `scripts/docs/promote_stable.py` rewrites those two lines on promotion, so
/// their shape (`var target = "...";`, `var locales = [...];`) is shared.
const ROOT_404_SCRIPT: &str = include_str!("root_404.js");

fn root_404_html(target: &str, locales: &[String]) -> anyhow::Result<String> {
    let locales = serde_json::to_string(locales)?;
    Ok(format!(
        r#"<!doctype html>
<html lang="en">
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<meta name="robots" content="noindex">
<title>Page not found · ZeroClaw Docs</title>
<style>body{{font:16px/1.5 system-ui,sans-serif;max-width:40rem;margin:4rem auto;padding:0 1rem}}</style>
<script>
var target = "{target}";
var locales = {locales};
{script}</script>
<h1>Page not found</h1>
<p>This page is not in the ZeroClaw docs. It may have moved in a newer release.</p>
<p><a href="/">Go to the documentation home</a></p>
</html>
"#,
        script = ROOT_404_SCRIPT,
    ))
}

/// Rebuild the `stable/` alias as a copy of the stable-pointer target, or
/// remove it when no pointer resolves. Run after `gen-seo`: the release dir then
/// already carries canonical tags that name the alias, so the copy is
/// byte-identical to it and git stores each file once.
pub fn gen_stable_alias() -> anyhow::Result<()> {
    gen_stable_alias_in(Path::new("."))
}

fn gen_stable_alias_in(root: &Path) -> anyhow::Result<()> {
    let alias = root.join(STABLE_ALIAS);
    match fs::symlink_metadata(&alias) {
        Ok(meta) if meta.is_dir() => fs::remove_dir_all(&alias)?,
        Ok(_) => fs::remove_file(&alias)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    let mut present = Vec::new();
    for entry in fs::read_dir(root)?.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if entry.file_type()?.is_dir() && is_version_dir(&name) {
            present.push(name);
        }
    }
    let Some(stable) = resolve_stable_in(root, &present) else {
        println!("gen-stable-alias: no stable pointer resolves; {STABLE_ALIAS}/ removed");
        return Ok(());
    };
    let copied = copy_tree(&root.join(&stable), &alias)?;
    println!("gen-stable-alias: copied {copied} file(s) from {stable}/ to {STABLE_ALIAS}/");
    Ok(())
}

/// Copy regular files and directories. A symlink is an error rather than
/// something to follow: nothing the docs build writes is a link, and following
/// one could publish a file from outside the tree.
fn copy_tree(from: &Path, to: &Path) -> anyhow::Result<usize> {
    fs::create_dir_all(to)?;
    let mut copied = 0usize;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let dest = to.join(entry.file_name());
        if ty.is_symlink() {
            anyhow::bail!("refusing to copy symlink {}", entry.path().display());
        } else if ty.is_dir() {
            copied += copy_tree(&entry.path(), &dest)?;
        } else {
            fs::copy(entry.path(), &dest)?;
            copied += 1;
        }
    }
    Ok(copied)
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
        let kept = retained_versions(&present, 2, None);
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
        let kept = retained_versions(&present, 3, None);
        assert_eq!(kept, vec!["master".to_string()]);
    }

    #[test]
    fn retained_keep_zero_keeps_only_master() {
        let present = vec!["master".to_string(), "v1.0.0".to_string()];
        let kept = retained_versions(&present, 0, None);
        assert_eq!(kept, vec!["master".to_string()]);
    }

    #[test]
    fn retained_always_keeps_stable_pointer_even_when_outside_window() {
        // Pointer names an older final that the recency window would drop.
        let present = vec![
            "master".to_string(),
            "v0.9.0".to_string(),
            "v0.8.5".to_string(),
            "v0.8.1".to_string(),
        ];
        let kept = retained_versions(&present, 2, Some("v0.8.1"));
        assert!(kept.contains(&"master".to_string()));
        assert!(kept.contains(&"v0.9.0".to_string()));
        assert!(kept.contains(&"v0.8.5".to_string()));
        assert!(
            kept.contains(&"v0.8.1".to_string()),
            "stable pointer target must survive retention"
        );
    }

    #[test]
    fn retained_no_duplicate_when_stable_within_window() {
        let present = vec!["master".to_string(), "v0.9.0".to_string()];
        let kept = retained_versions(&present, 3, Some("v0.9.0"));
        let count = kept.iter().filter(|n| n.as_str() == "v0.9.0").count();
        assert_eq!(count, 1, "stable in window must not be listed twice");
    }

    #[test]
    fn retained_ignores_stable_pointer_not_present() {
        let present = vec!["master".to_string(), "v0.9.0".to_string()];
        let kept = retained_versions(&present, 1, Some("v0.8.1"));
        assert!(!kept.contains(&"v0.8.1".to_string()));
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

    #[test]
    fn root_index_redirects_to_the_alias_landing_page() {
        let html = root_index_html(STABLE_ALIAS);
        assert!(html.contains("<meta http-equiv=\"refresh\" content=\"0; url=./stable/en/\">"));
        assert!(html.contains("<link rel=\"canonical\" href=\"./stable/en/\">"));
    }

    #[test]
    fn stable_alias_is_an_exact_copy_of_the_pointer_target() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for (path, body) in [
            ("v0.8.5/en/index.html", "old"),
            ("v0.8.6/en/index.html", "six"),
            ("v0.8.6/en/agents/anatomy.html", "anatomy"),
            ("v0.8.6/fr/index.html", "six fr"),
            ("v0.8.6/.docs-source-commit", "abc\n"),
            ("master/en/index.html", "dev"),
        ] {
            let file = root.join(path);
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            fs::write(file, body).unwrap();
        }
        fs::create_dir_all(root.join("stable/en")).unwrap();
        fs::write(root.join("stable/en/removed.html"), "stale").unwrap();
        fs::write(root.join("stable-version.txt"), "v0.8.6\n").unwrap();

        gen_stable_alias_in(root).unwrap();

        let tree = |base: &Path| -> Vec<(String, Vec<u8>)> {
            let mut out = Vec::new();
            let mut stack = vec![base.to_path_buf()];
            while let Some(d) = stack.pop() {
                for e in fs::read_dir(&d).unwrap().flatten() {
                    if e.file_type().unwrap().is_dir() {
                        stack.push(e.path());
                    } else {
                        let rel = e.path().strip_prefix(base).unwrap().display().to_string();
                        out.push((rel, fs::read(e.path()).unwrap()));
                    }
                }
            }
            out.sort();
            out
        };
        assert_eq!(tree(&root.join("stable")), tree(&root.join("v0.8.6")));
        assert!(!root.join("stable/en/removed.html").exists());

        // No pointer target: the alias goes away and nothing else changes.
        fs::write(root.join("stable-version.txt"), "v0.9.9\n").unwrap();
        gen_stable_alias_in(root).unwrap();
        assert!(!root.join("stable").exists());
        assert!(root.join("v0.8.6/en/index.html").is_file());
    }

    #[cfg(unix)]
    #[test]
    fn stable_alias_refuses_to_follow_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("v0.8.6/en")).unwrap();
        fs::write(root.join("outside.txt"), "secret").unwrap();
        std::os::unix::fs::symlink(root.join("outside.txt"), root.join("v0.8.6/en/link.html"))
            .unwrap();
        fs::write(root.join("stable-version.txt"), "v0.8.6\n").unwrap();
        let err = gen_stable_alias_in(root).unwrap_err();
        assert!(
            err.to_string().contains("refusing to copy symlink"),
            "{err}"
        );
    }

    #[test]
    fn root_404_names_the_redirect_target_and_only_plain_locale_codes() {
        let html = root_404_html(STABLE_ALIAS, &["en".into(), "zh-CN".into()]).unwrap();
        assert!(html.contains("<meta name=\"robots\" content=\"noindex\">"));
        // promote_stable.py rewrites exactly these two lines.
        assert!(html.contains("\nvar target = \"stable\";\n"));
        assert!(html.contains("\nvar locales = [\"en\",\"zh-CN\"];\n"));
        assert!(html.contains(ROOT_404_SCRIPT));
        assert!(html.contains("<a href=\"/\">"));

        for ok in ["en", "zh-CN", "pt-BR"] {
            assert!(is_locale_code(ok), "{ok}");
        }
        for bad in ["", "-", "en-", "a b", "en\"", "</script>", "api/x"] {
            assert!(!is_locale_code(bad), "{bad:?}");
        }
    }
}
