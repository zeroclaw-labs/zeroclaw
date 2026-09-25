//! `gen-seo`: search-engine metadata for the published docs tree.
//!
//! Runs in the gh-pages clone root after `gen-root-index`, over every version
//! directory that survived pruning. The published tree carries every locale of
//! every retained version, so the same page exists many times over. This
//! finalizer tells crawlers which copy is the one that counts, and gives them
//! a map of it:
//!
//! - every page gets a `<link rel="canonical">`: the stable copy of the same
//!   page when one exists, itself otherwise (stable pages and master-only
//!   pages);
//! - pages of old versions that no longer exist in stable get
//!   `<meta name="robots" content="noindex">`, because they describe removed
//!   behaviour;
//! - stable pages get `hreflang` alternates for every locale that has the page,
//!   with English as the default;
//! - the book-wide `<meta name="description">` is replaced by the page's first
//!   paragraph, so each page describes itself;
//! - Open Graph and Twitter tags mirror the title, description and canonical;
//! - `robots.txt` and `sitemap.xml` are written at the root, the sitemap
//!   listing every stable page in every locale.
//!
//! The injected tags sit between `<!-- seo:start -->` and `<!-- seo:end -->`
//! markers so a later deploy replaces them instead of stacking duplicates.
//! The site host is read from the `CNAME` file the deploy writes first.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use super::versions::{is_version_dir, resolve_stable};

const SEO_START: &str = "<!-- seo:start -->";
const SEO_END: &str = "<!-- seo:end -->";
const DEFAULT_LOCALE: &str = "en";
const DESCRIPTION_MAX_CHARS: usize = 160;
/// Pages that are navigation or print artefacts, never search results.
const SKIP_FILES: &[&str] = &["print.html", "404.html", "toc.html"];

pub fn run() -> anyhow::Result<()> {
    let site = site_origin()?;
    let present = version_dirs(Path::new("."))?;
    let stable = resolve_stable(&present);
    let mut patched = 0usize;
    let mut sitemap: Vec<String> = Vec::new();

    for version in &present {
        let is_stable = Some(version.as_str()) == stable.as_deref();
        for locale in locales(Path::new(version))? {
            let locale_root = Path::new(version).join(&locale);
            for page in html_pages(&locale_root)? {
                let rel = page
                    .strip_prefix(&locale_root)
                    .map_err(|e| anyhow::Error::msg(format!("{}: {e}", page.display())))?
                    .to_string_lossy()
                    .replace('\\', "/");
                let placement = classify(version, is_stable, stable.as_deref(), &locale, &rel);
                let alternates = if is_stable {
                    locales(Path::new(version))?
                        .into_iter()
                        .filter(|l| Path::new(version).join(l).join(&rel).is_file())
                        .collect()
                } else {
                    Vec::new()
                };
                if patch_page(&page, &site, &placement, &alternates)? {
                    patched += 1;
                }
                if is_stable {
                    sitemap.push(page_url(&site, version, &locale, &rel));
                }
            }
        }
    }

    fs::write("robots.txt", robots_txt(&site))?;
    fs::write("sitemap.xml", sitemap_xml(&site, &sitemap))?;
    println!(
        "gen-seo: patched {patched} page(s); sitemap lists {} stable page(s)",
        sitemap.len()
    );
    Ok(())
}

/// Where a page's canonical points and whether it should be indexed.
#[derive(Debug, PartialEq, Eq)]
struct Placement {
    /// `(version, locale, relative path)` of the canonical copy.
    canonical: (String, String, String),
    noindex: bool,
}

fn classify(
    version: &str,
    is_stable: bool,
    stable: Option<&str>,
    locale: &str,
    rel: &str,
) -> Placement {
    let this = (version.to_string(), locale.to_string(), rel.to_string());
    if is_stable {
        return Placement {
            canonical: this,
            noindex: false,
        };
    }
    let stable_copy = stable
        .filter(|s| Path::new(s).join(locale).join(rel).is_file())
        .map(|s| (s.to_string(), locale.to_string(), rel.to_string()));
    match stable_copy {
        Some(target) => Placement {
            canonical: target,
            noindex: false,
        },
        // master carries pages the next release will have; old versions carry
        // pages the current release dropped.
        None => Placement {
            canonical: this,
            noindex: version != "master",
        },
    }
}

fn patch_page(
    path: &Path,
    site: &str,
    placement: &Placement,
    alternates: &[String],
) -> anyhow::Result<bool> {
    let content = fs::read_to_string(path)?;
    let Some(head_end) = content.find("</head>") else {
        return Ok(false);
    };
    let stripped = strip_previous(&content);
    let title = extract_title(&stripped).unwrap_or_default();
    let description = first_paragraph(&stripped)
        .unwrap_or_else(|| extract_meta(&stripped, "description").unwrap_or_default());
    let (version, locale, rel) = &placement.canonical;
    let canonical = page_url(site, version, locale, rel);

    let mut block = String::new();
    block.push_str(SEO_START);
    block.push('\n');
    if placement.noindex {
        block.push_str("        <meta name=\"robots\" content=\"noindex\">\n");
    }
    block.push_str(&format!(
        "        <link rel=\"canonical\" href=\"{}\">\n",
        escape(&canonical)
    ));
    for alt in alternates {
        block.push_str(&format!(
            "        <link rel=\"alternate\" hreflang=\"{}\" href=\"{}\">\n",
            escape(alt),
            escape(&page_url(site, version, alt, rel))
        ));
    }
    if alternates.iter().any(|l| l == DEFAULT_LOCALE) {
        block.push_str(&format!(
            "        <link rel=\"alternate\" hreflang=\"x-default\" href=\"{}\">\n",
            escape(&page_url(site, version, DEFAULT_LOCALE, rel))
        ));
    }
    block.push_str(&format!(
        "        <meta property=\"og:type\" content=\"article\">\n        <meta property=\"og:site_name\" content=\"ZeroClaw Docs\">\n        <meta property=\"og:title\" content=\"{t}\">\n        <meta property=\"og:description\" content=\"{d}\">\n        <meta property=\"og:url\" content=\"{u}\">\n        <meta name=\"twitter:card\" content=\"summary\">\n        <meta name=\"twitter:title\" content=\"{t}\">\n        <meta name=\"twitter:description\" content=\"{d}\">\n",
        t = escape(&title),
        d = escape(&description),
        u = escape(&canonical)
    ));
    block.push_str("        ");
    block.push_str(SEO_END);
    block.push('\n');

    let with_description = replace_description(&stripped, &description);
    let Some(head_end) = with_description.find("</head>").or(Some(head_end)) else {
        return Ok(false);
    };
    let line_start = with_description[..head_end]
        .rfind('\n')
        .map(|i| i + 1)
        .unwrap_or(head_end);
    let mut updated = String::with_capacity(with_description.len() + block.len());
    updated.push_str(&with_description[..line_start]);
    updated.push_str(&block);
    updated.push_str(&with_description[line_start..]);
    if updated == content {
        return Ok(false);
    }
    fs::write(path, updated)?;
    Ok(true)
}

/// Drop a block injected by an earlier deploy, so the finalizer is idempotent.
fn strip_previous(content: &str) -> String {
    let Some(start) = content.find(SEO_START) else {
        return content.to_string();
    };
    let Some(end_rel) = content[start..].find(SEO_END) else {
        return content.to_string();
    };
    let end = start + end_rel + SEO_END.len();
    // Also swallow the trailing newline and the indentation before the marker.
    let line_start = content[..start].rfind('\n').map(|i| i + 1).unwrap_or(start);
    let after = if content[end..].starts_with('\n') {
        end + 1
    } else {
        end
    };
    format!("{}{}", &content[..line_start], &content[after..])
}

fn replace_description(content: &str, description: &str) -> String {
    let Some(start) = content.find("<meta name=\"description\" content=\"") else {
        return content.to_string();
    };
    let value_start = start + "<meta name=\"description\" content=\"".len();
    let Some(value_len) = content[value_start..].find('"') else {
        return content.to_string();
    };
    let mut out = String::with_capacity(content.len() + description.len());
    out.push_str(&content[..value_start]);
    out.push_str(&escape(description));
    out.push_str(&content[value_start + value_len..]);
    out
}

fn extract_title(content: &str) -> Option<String> {
    let start = content.find("<title>")? + "<title>".len();
    let end = content[start..].find("</title>")? + start;
    Some(unescape(content[start..end].trim()))
}

fn extract_meta(content: &str, name: &str) -> Option<String> {
    let needle = format!("<meta name=\"{name}\" content=\"");
    let start = content.find(&needle)? + needle.len();
    let end = content[start..].find('"')? + start;
    Some(unescape(&content[start..end]))
}

/// The first paragraph of the page body, tags stripped, cut to a sentence
/// boundary under the description limit. mdBook wraps the rendered Markdown in
/// `<main>`; headings are skipped so the description is prose, not the title.
fn first_paragraph(content: &str) -> Option<String> {
    let main_start = content.find("<main>")?;
    let main_end = content[main_start..]
        .find("</main>")
        .map(|i| i + main_start)
        .unwrap_or(content.len());
    let main = &content[main_start..main_end];
    let mut search = 0usize;
    while let Some(p) = main[search..].find("<p>") {
        let p_start = search + p + "<p>".len();
        let Some(p_len) = main[p_start..].find("</p>") else {
            break;
        };
        let text = collapse_whitespace(&unescape(&strip_tags(&main[p_start..p_start + p_len])));
        if text.chars().count() >= 20 {
            return Some(truncate_description(&text));
        }
        search = p_start + p_len;
    }
    None
}

fn truncate_description(text: &str) -> String {
    if text.chars().count() <= DESCRIPTION_MAX_CHARS {
        return text.to_string();
    }
    let cut: String = text.chars().take(DESCRIPTION_MAX_CHARS).collect();
    // Prefer ending on a sentence, then on a word.
    if let Some(i) = cut.rfind(". ") {
        return cut[..=i].to_string();
    }
    match cut.rfind(' ') {
        Some(i) => format!("{}…", &cut[..i]),
        None => format!("{cut}…"),
    }
}

fn strip_tags(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out
}

fn collapse_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn unescape(text: &str) -> String {
    text.replace("&quot;", "\"")
        .replace("&#x27;", "'")
        .replace("&#39;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
}

fn page_url(site: &str, version: &str, locale: &str, rel: &str) -> String {
    format!("{site}/{version}/{locale}/{rel}")
}

/// `https://<host>` from the `CNAME` file the deploy writes at the clone root.
fn site_origin() -> anyhow::Result<String> {
    let host = fs::read_to_string("CNAME").map_err(|e| {
        anyhow::Error::msg(format!(
            "gen-seo needs the CNAME file at the clone root: {e}"
        ))
    })?;
    let host = host.trim();
    anyhow::ensure!(!host.is_empty(), "CNAME is empty");
    Ok(format!("https://{host}"))
}

fn version_dirs(root: &Path) -> anyhow::Result<Vec<String>> {
    let mut present = Vec::new();
    for entry in fs::read_dir(root)?.flatten() {
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if is_version_dir(&name) {
            present.push(name);
        }
    }
    present.sort();
    Ok(present)
}

/// Locale directories of one version: every child directory with an
/// `index.html`, which excludes `api/`-style artefacts copied alongside.
fn locales(version_root: &Path) -> anyhow::Result<Vec<String>> {
    let mut found = BTreeSet::new();
    for entry in fs::read_dir(version_root)?.flatten() {
        if entry.file_type()?.is_dir() && entry.path().join("index.html").is_file() {
            found.insert(entry.file_name().to_string_lossy().to_string());
        }
    }
    Ok(found.into_iter().collect())
}

fn html_pages(root: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut stack = vec![root.to_path_buf()];
    let mut pages = Vec::new();
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir)?.flatten() {
            let path = entry.path();
            if entry.file_type()?.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "html")
                && !SKIP_FILES.contains(
                    &path
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .as_ref(),
                )
            {
                pages.push(path);
            }
        }
    }
    pages.sort();
    Ok(pages)
}

fn robots_txt(site: &str) -> String {
    format!("User-agent: *\nAllow: /\n\nSitemap: {site}/sitemap.xml\n")
}

fn sitemap_xml(site: &str, urls: &[String]) -> String {
    let mut out = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<urlset xmlns=\"http://www.sitemaps.org/schemas/sitemap/0.9\">\n",
    );
    out.push_str(&format!("  <url><loc>{}/</loc></url>\n", escape(site)));
    for url in urls {
        out.push_str(&format!("  <url><loc>{}</loc></url>\n", escape(url)));
    }
    out.push_str("</urlset>\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGE: &str = "<!DOCTYPE HTML>\n<html lang=\"en\">\n    <head>\n        <title>Introduction - ZeroClaw Docs</title>\n        <meta name=\"description\" content=\"Documentation for the ZeroClaw personal AI assistant.\">\n    </head>\n    <body>\n        <main>\n            <h1>Introduction</h1>\n            <p>ZeroClaw is an <strong>agent runtime</strong> &amp; a single Rust binary you run yourself.</p>\n        </main>\n    </body>\n</html>\n";

    fn page_with_block() -> String {
        let placement = Placement {
            canonical: ("v0.8.5".into(), "en".into(), "introduction.html".into()),
            noindex: false,
        };
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("introduction.html");
        fs::write(&path, PAGE).unwrap();
        assert!(
            patch_page(
                &path,
                "https://docs.example",
                &placement,
                &["en".into(), "fr".into()]
            )
            .unwrap()
        );
        fs::read_to_string(&path).unwrap()
    }

    #[test]
    fn injects_canonical_alternates_and_description_from_first_paragraph() {
        let out = page_with_block();
        assert!(out.contains(
            "<link rel=\"canonical\" href=\"https://docs.example/v0.8.5/en/introduction.html\">"
        ));
        assert!(
            out.contains(
                "hreflang=\"fr\" href=\"https://docs.example/v0.8.5/fr/introduction.html\""
            )
        );
        assert!(out.contains(
            "hreflang=\"x-default\" href=\"https://docs.example/v0.8.5/en/introduction.html\""
        ));
        assert!(out.contains(
            "<meta name=\"description\" content=\"ZeroClaw is an agent runtime &amp; a single Rust binary you run yourself.\">"
        ));
        assert!(
            out.contains("<meta property=\"og:title\" content=\"Introduction - ZeroClaw Docs\">")
        );
        assert!(!out.contains("noindex"));
        // The block sits inside <head>.
        let head_end = out.find("</head>").unwrap();
        assert!(out.find(SEO_START).unwrap() < head_end);
    }

    #[test]
    fn rerunning_replaces_the_block_instead_of_stacking() {
        let out = page_with_block();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("introduction.html");
        fs::write(&path, &out).unwrap();
        let placement = Placement {
            canonical: ("v0.8.6".into(), "en".into(), "introduction.html".into()),
            noindex: false,
        };
        assert!(patch_page(&path, "https://docs.example", &placement, &["en".into()]).unwrap());
        let again = fs::read_to_string(&path).unwrap();
        assert_eq!(again.matches(SEO_START).count(), 1);
        assert_eq!(again.matches("rel=\"canonical\"").count(), 1);
        assert!(again.contains("/v0.8.6/en/introduction.html\">"));
        assert!(!again.contains("/v0.8.5/"));
        // Same input twice is a no-op.
        assert!(!patch_page(&path, "https://docs.example", &placement, &["en".into()]).unwrap());
    }

    #[test]
    fn noindex_is_emitted_only_when_asked() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("old.html");
        fs::write(&path, PAGE).unwrap();
        let placement = Placement {
            canonical: ("v0.8.2".into(), "en".into(), "old.html".into()),
            noindex: true,
        };
        patch_page(&path, "https://docs.example", &placement, &[]).unwrap();
        let out = fs::read_to_string(&path).unwrap();
        assert!(out.contains("<meta name=\"robots\" content=\"noindex\">"));
        assert!(!out.contains("hreflang"));
    }

    #[test]
    fn classify_points_duplicates_at_stable_and_hides_dropped_pages() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = std::env::current_dir().unwrap();
        std::env::set_current_dir(dir.path()).unwrap();
        fs::create_dir_all("v0.8.5/en").unwrap();
        fs::write("v0.8.5/en/kept.html", "x").unwrap();

        let dup = classify("master", false, Some("v0.8.5"), "en", "kept.html");
        assert_eq!(dup.canonical.0, "v0.8.5");
        assert!(!dup.noindex);

        let fresh = classify("master", false, Some("v0.8.5"), "en", "new.html");
        assert_eq!(fresh.canonical.0, "master");
        assert!(!fresh.noindex, "master-only pages stay indexable");

        let dropped = classify("v0.8.2", false, Some("v0.8.5"), "en", "gone.html");
        assert_eq!(dropped.canonical.0, "v0.8.2");
        assert!(dropped.noindex, "pages the release removed are hidden");

        let stable = classify("v0.8.5", true, Some("v0.8.5"), "en", "kept.html");
        assert_eq!(stable.canonical.0, "v0.8.5");
        std::env::set_current_dir(cwd).unwrap();
    }

    #[test]
    fn description_is_cut_at_a_sentence_and_never_mid_word() {
        let long = "First sentence is here. ".repeat(3) + &"word ".repeat(80);
        let cut = truncate_description(&long);
        assert!(cut.chars().count() <= DESCRIPTION_MAX_CHARS + 1);
        assert!(cut.ends_with('.'));
        let no_sentence = "word ".repeat(80);
        let cut = truncate_description(no_sentence.trim());
        assert!(cut.ends_with("word…"));
    }

    #[test]
    fn sitemap_and_robots_point_at_the_site() {
        let s = sitemap_xml(
            "https://docs.example",
            &["https://docs.example/v1/en/a.html".into()],
        );
        assert!(s.contains("<loc>https://docs.example/</loc>"));
        assert!(s.contains("<loc>https://docs.example/v1/en/a.html</loc>"));
        assert!(
            robots_txt("https://docs.example")
                .contains("Sitemap: https://docs.example/sitemap.xml")
        );
    }
}
