//! Internal link integrity check for the built book.
//!
//! mdBook does not validate that a page's `href` resolves to a real file, so a
//! relative link that points nowhere (commonly from an `{{#include}}` that
//! drags a page's own relative links into a different directory) renders as a
//! dead link and ships silently. This walks the generated HTML for one locale,
//! resolves every internal `href` against the file it appears in, and fails the
//! build if any target is missing.
//!
//! Only same-site relative links are checked. External (`http(s)://`,
//! `mailto:`), in-page anchors (`#...`), and the generated rustdoc `api/` tree
//! are skipped — the api tree is copied in `assemble()` after this runs, and is
//! rustdoc's own output, not authored.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Check internal links in the built HTML for `tag`'s first locale.
///
/// Run after `build_locales` (HTML present) and before `assemble` (which adds
/// the `api/` tree). Checking one locale is sufficient: every locale is built
/// from the same Markdown source, so a broken authored link breaks identically
/// in all of them.
pub fn check_internal_links(root: &Path, tag: &str) -> anyhow::Result<()> {
    let locale = crate::util::locale_entries()
        .into_iter()
        .next()
        .map(|e| e.code)
        .unwrap_or_else(|| "en".to_string());
    let base = crate::util::book_dir(root)
        .join("book")
        .join(tag)
        .join(&locale);
    if !base.is_dir() {
        return Ok(());
    }

    let mut broken: Vec<(PathBuf, String)> = Vec::new();
    for html in walk_html(&base) {
        let content = match std::fs::read_to_string(&html) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let dir = html.parent().unwrap_or(&base);
        for href in extract_hrefs(&content) {
            if !is_checkable(&href) {
                continue;
            }
            // Strip the in-page anchor; we only verify the target file exists.
            let path_part = href.split('#').next().unwrap_or(&href);
            if path_part.is_empty() {
                continue;
            }
            let target = normalize(&dir.join(path_part));
            // A link ending in `/` (or to a dir) resolves to its index.html.
            let resolved = if path_part.ends_with('/') {
                target.join("index.html")
            } else {
                target
            };
            if !resolved.exists() {
                let rel = html.strip_prefix(&base).unwrap_or(&html).to_path_buf();
                broken.push((rel, href));
            }
        }
    }

    if broken.is_empty() {
        println!("==> Internal link check passed");
        return Ok(());
    }
    let mut msg = String::from("internal link check failed; dead links found:\n");
    let mut seen = BTreeSet::new();
    for (page, href) in &broken {
        let line = format!("  {} -> {}", page.display(), href);
        if seen.insert(line.clone()) {
            msg.push_str(&line);
            msg.push('\n');
        }
    }
    anyhow::bail!(msg)
}

fn walk_html(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(p) = stack.pop() {
        if let Ok(entries) = std::fs::read_dir(&p) {
            for e in entries.flatten() {
                let path = e.path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.extension().is_some_and(|x| x == "html") {
                    out.push(path);
                }
            }
        }
    }
    out
}

/// Pull `href="..."` values out of rendered HTML. Naive but sufficient for
/// mdBook output, which quotes every href with double quotes.
fn extract_hrefs(html: &str) -> Vec<String> {
    let mut out = Vec::new();
    let needle = "href=\"";
    let mut rest = html;
    while let Some(i) = rest.find(needle) {
        rest = &rest[i + needle.len()..];
        if let Some(end) = rest.find('"') {
            out.push(rest[..end].to_string());
            rest = &rest[end + 1..];
        } else {
            break;
        }
    }
    out
}

/// Whether an href is a same-site relative link we should verify.
fn is_checkable(href: &str) -> bool {
    if href.is_empty() || href.starts_with('#') {
        return false;
    }
    // Absolute URLs, protocol-relative, scheme links: not ours to check.
    if href.contains("://") || href.starts_with("//") || href.starts_with("mailto:") {
        return false;
    }
    // Site-absolute paths (e.g. `/_shared/...`) and the rustdoc api tree are
    // added/owned outside the authored Markdown.
    if href.starts_with('/') || href.starts_with("api/") || href.contains("/api/") {
        return false;
    }
    true
}

/// Resolve `.`/`..` segments without touching the filesystem (the target may
/// not exist, which is exactly what we are testing for).
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_hrefs_pulls_quoted_links() {
        let html = r#"<a href="./config.html">x</a> <a href="../zerocode/overview.html#sec">y</a>"#;
        assert_eq!(
            extract_hrefs(html),
            vec!["./config.html", "../zerocode/overview.html#sec"]
        );
    }

    #[test]
    fn is_checkable_skips_external_anchor_and_site_absolute() {
        assert!(is_checkable("./config.html"));
        assert!(is_checkable("../zerocode/running.html"));
        assert!(!is_checkable("https://example.com"));
        assert!(!is_checkable("#section"));
        assert!(!is_checkable("/_shared/theme/custom.css"));
        assert!(!is_checkable("mailto:x@y.z"));
        assert!(!is_checkable("api/zeroclaw/index.html"));
    }

    #[test]
    fn normalize_resolves_parent_segments() {
        let p = normalize(Path::new(
            "/book/en/getting-started/../zerocode/overview.html",
        ));
        assert_eq!(p, PathBuf::from("/book/en/zerocode/overview.html"));
    }
}
