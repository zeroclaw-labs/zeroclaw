//! `gen-seo`: search-engine metadata for the published docs tree.
//!
//! Runs in the gh-pages clone root after `gen-root-index`, over every version
//! directory that survived pruning. The published tree carries every locale of
//! every retained version, so the same page exists many times over. This
//! finalizer tells crawlers which copy is the one that counts, and gives them
//! a map of it:
//!
//! - every page gets a `<link rel="canonical">`: the stable copy of the same
//!   page when one exists, itself otherwise (master-only pages). The stable
//!   copy is named by its version-less URL under `stable/`, which
//!   `gen-stable-alias` fills from the release dir after this step, so a new
//!   release updates the canonical pages in place instead of moving all of
//!   them to a new `/vX.Y.Z/` path;
//! - pages of old versions that no longer exist in stable get
//!   `<meta name="robots" content="noindex">`, because they describe removed
//!   behaviour;
//! - stable pages get `hreflang` alternates for every locale that has the page,
//!   with English as the default;
//! - the book-wide `<meta name="description">` is replaced by the page's first
//!   paragraph, so each page describes itself;
//! - Open Graph and Twitter tags mirror the title, description and canonical;
//! - `robots.txt` and `sitemap.xml` are written at the root, the sitemap
//!   listing every stable page in every locale under `stable/`;
//! - the shared rustdoc tree under `api/` and mdBook's per-locale `404.html`
//!   get `noindex` and nothing else. The API reference is for readers who
//!   follow a link from the book, not a search landing page: the umbrella
//!   `zeroclaw` crate re-exports the member crates, so most item pages exist
//!   twice with the same text. The error page answers 200 when it is requested
//!   by name, so without the tag it is indexable.
//!
//! The injected tags sit between `<!-- seo:start -->` and `<!-- seo:end -->`
//! markers so a later deploy replaces them instead of stacking duplicates.
//! The site host is read from the `CNAME` file the deploy writes first.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use super::versions::{STABLE_ALIAS, is_version_dir, resolve_stable_in};

const SEO_START: &str = "<!-- seo:start -->";
const SEO_END: &str = "<!-- seo:end -->";
const DEFAULT_LOCALE: &str = "en";
const DESCRIPTION_MAX_CHARS: usize = 160;
/// Pages that are navigation or print artefacts, never search results.
const SKIP_FILES: &[&str] = &["print.html", "404.html", "toc.html"];
/// mdBook's per-locale error page. It is skipped like the other artefacts but
/// still marked `noindex`, because GitHub Pages serves it with status 200 when
/// it is requested by name.
const NOT_FOUND_PAGE: &str = "404.html";
/// The shared rustdoc tree at the clone root, written by master's deploy.
const API_DIR: &str = "api";

pub fn run() -> anyhow::Result<()> {
    run_in(Path::new("."))
}

/// [`run`] against an explicit gh-pages root, so tests never touch the cwd.
fn run_in(root: &Path) -> anyhow::Result<()> {
    let site = site_origin(root)?;
    let present = version_dirs(root)?;
    let stable = resolve_stable_in(root, &present);
    let mut patched = 0usize;
    let mut hidden = 0usize;
    let mut sitemap: Vec<String> = Vec::new();

    for version in &present {
        let version_root = root.join(version);
        let is_stable = Some(version.as_str()) == stable.as_deref();
        let version_locales = locales(&version_root)?;
        for locale in &version_locales {
            let locale_root = version_root.join(locale);
            for page in html_files(&locale_root, SKIP_FILES)? {
                let rel = page
                    .strip_prefix(&locale_root)
                    .map_err(|e| anyhow::Error::msg(format!("{}: {e}", page.display())))?
                    .to_string_lossy()
                    .replace('\\', "/");
                let placement = classify(root, version, is_stable, stable.as_deref(), locale, &rel);
                let alternates: Vec<String> = if is_stable {
                    version_locales
                        .iter()
                        .filter(|l| version_root.join(l).join(&rel).is_file())
                        .cloned()
                        .collect()
                } else {
                    Vec::new()
                };
                if patch_page(&page, &site, &placement, &alternates)? {
                    patched += 1;
                }
                if is_stable {
                    sitemap.push(page_url(&site, STABLE_ALIAS, locale, &rel));
                }
            }
            let not_found = locale_root.join(NOT_FOUND_PAGE);
            if not_found.is_file() && patch_noindex(&not_found)? {
                hidden += 1;
            }
        }
    }

    let api = root.join(API_DIR);
    if api.is_dir() {
        for page in html_files(&api, &[])? {
            if patch_noindex(&page)? {
                hidden += 1;
            }
        }
    }

    fs::write(root.join("robots.txt"), robots_txt(&site))?;
    fs::write(root.join("sitemap.xml"), sitemap_xml(&site, &sitemap))?;
    println!(
        "gen-seo: patched {patched} page(s), marked {hidden} page(s) noindex; sitemap lists {} stable page(s)",
        sitemap.len()
    );
    Ok(())
}

/// Where a page's canonical points and whether it should be indexed.
#[derive(Debug, PartialEq, Eq)]
struct Placement {
    /// `(version path segment, locale, relative path)` of the canonical copy.
    /// The segment is [`STABLE_ALIAS`] for the stable copy.
    canonical: (String, String, String),
    noindex: bool,
}

/// Place one page of `version`, looking for its stable copy under `root`, the
/// gh-pages clone root that holds the version dirs.
fn classify(
    root: &Path,
    version: &str,
    is_stable: bool,
    stable: Option<&str>,
    locale: &str,
    rel: &str,
) -> Placement {
    let this = (version.to_string(), locale.to_string(), rel.to_string());
    let alias = || {
        (
            STABLE_ALIAS.to_string(),
            locale.to_string(),
            rel.to_string(),
        )
    };
    if is_stable {
        return Placement {
            canonical: alias(),
            noindex: false,
        };
    }
    let stable_copy = stable
        .filter(|s| root.join(s).join(locale).join(rel).is_file())
        .map(|_| alias());
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

/// Mark a page `noindex` and add nothing else: a page that should not be found
/// through search needs no canonical, description or share tags. The block goes
/// directly before `</head>`, because rustdoc writes its whole head on one line.
fn patch_noindex(path: &Path) -> anyhow::Result<bool> {
    let content = fs::read_to_string(path)?;
    let stripped = strip_previous(&content);
    let Some(head_end) = stripped.find("</head>") else {
        return Ok(false);
    };
    let updated = format!(
        "{}{SEO_START}<meta name=\"robots\" content=\"noindex\">{SEO_END}{}",
        &stripped[..head_end],
        &stripped[head_end..]
    );
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
    // A block on its own line also takes its indentation and trailing newline.
    // A block that shares its line (rustdoc's one-line head, or `noindex` put
    // straight before an indented `</head>`) takes nothing else, or the rest
    // of that line would go with it.
    let line_start = content[..start].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let own_line = content[end..].starts_with('\n') && content[line_start..start].trim().is_empty();
    if own_line {
        format!("{}{}", &content[..line_start], &content[end + 1..])
    } else {
        format!("{}{}", &content[..start], &content[end..])
    }
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
fn site_origin(root: &Path) -> anyhow::Result<String> {
    let host = fs::read_to_string(root.join("CNAME")).map_err(|e| {
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

/// Every `.html` file under `root`, minus the file names in `skip`.
fn html_files(root: &Path, skip: &[&str]) -> anyhow::Result<Vec<PathBuf>> {
    let mut stack = vec![root.to_path_buf()];
    let mut pages = Vec::new();
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir)?.flatten() {
            let path = entry.path();
            if entry.file_type()?.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "html")
                && !skip.contains(
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
        // An explicit root, not `set_current_dir`: the cwd is shared by every
        // test thread, and sibling tests spawn `cargo metadata` from it.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("v0.8.5/en")).unwrap();
        fs::write(root.join("v0.8.5/en/kept.html"), "x").unwrap();

        let dup = classify(root, "master", false, Some("v0.8.5"), "en", "kept.html");
        assert_eq!(dup.canonical.0, STABLE_ALIAS);
        assert!(!dup.noindex);

        let fresh = classify(root, "master", false, Some("v0.8.5"), "en", "new.html");
        assert_eq!(fresh.canonical.0, "master");
        assert!(!fresh.noindex, "master-only pages stay indexable");

        let dropped = classify(root, "v0.8.2", false, Some("v0.8.5"), "en", "gone.html");
        assert_eq!(dropped.canonical.0, "v0.8.2");
        assert!(dropped.noindex, "pages the release removed are hidden");

        let stable = classify(root, "v0.8.5", true, Some("v0.8.5"), "en", "kept.html");
        assert_eq!(stable.canonical.0, STABLE_ALIAS);

        let older = classify(root, "v0.8.4", false, Some("v0.8.5"), "fr", "kept.html");
        assert_eq!(older.canonical.0, "v0.8.4", "no fr copy in stable");
        assert!(older.noindex);
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

    /// rustdoc's head, abridged: one line, `</head>` mid-line.
    const RUSTDOC_PAGE: &str = "<!DOCTYPE html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"description\" content=\"Config schema.\"><title>zeroclaw_config - Rust</title></head><body class=\"rustdoc mod crate\"><main><p>Config schema.</p></main></body></html>\n";

    #[test]
    fn noindex_lands_inside_a_one_line_head_and_reruns_cleanly() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("index.html");
        fs::write(&path, RUSTDOC_PAGE).unwrap();

        assert!(patch_noindex(&path).unwrap());
        let out = fs::read_to_string(&path).unwrap();
        let head_end = RUSTDOC_PAGE.find("</head>").unwrap();
        assert_eq!(
            out,
            format!(
                "{}{SEO_START}<meta name=\"robots\" content=\"noindex\">{SEO_END}{}",
                &RUSTDOC_PAGE[..head_end],
                &RUSTDOC_PAGE[head_end..]
            )
        );
        // No canonical, description rewrite or share tags.
        assert!(!out.contains("canonical"));
        assert!(!out.contains("og:"));

        assert!(!patch_noindex(&path).unwrap(), "a rerun is a no-op");
        assert_eq!(fs::read_to_string(&path).unwrap(), out);
    }

    #[test]
    fn stripping_a_mid_line_block_keeps_the_rest_of_the_line() {
        let head_end = RUSTDOC_PAGE.find("</head>").unwrap();
        let marked = format!(
            "{}{SEO_START}<meta name=\"robots\" content=\"noindex\">{SEO_END}{}",
            &RUSTDOC_PAGE[..head_end],
            &RUSTDOC_PAGE[head_end..]
        );
        assert_eq!(strip_previous(&marked), RUSTDOC_PAGE);
        // `noindex` put before an indented `</head>` keeps that indentation.
        let indented = format!("<head>\n    {SEO_START}<meta>{SEO_END}</head>\n");
        assert_eq!(strip_previous(&indented), "<head>\n    </head>\n");
        // A block on its own line still leaves no blank line behind.
        let own_line = format!("<head>\n        {SEO_START}\n        {SEO_END}\n    </head>\n");
        assert_eq!(strip_previous(&own_line), "<head>\n    </head>\n");
    }

    #[test]
    fn run_hides_the_api_tree_and_error_pages_and_keeps_them_out_of_the_sitemap() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("CNAME"), "docs.example\n").unwrap();
        fs::write(root.join("stable-version.txt"), "v0.8.5\n").unwrap();
        fs::create_dir_all(root.join("v0.8.5/en")).unwrap();
        fs::write(root.join("v0.8.5/en/index.html"), PAGE).unwrap();
        fs::write(root.join("v0.8.5/en/404.html"), PAGE).unwrap();
        fs::create_dir_all(root.join("api/zeroclaw/config")).unwrap();
        fs::write(root.join("api/zeroclaw/index.html"), RUSTDOC_PAGE).unwrap();
        fs::write(
            root.join("api/zeroclaw/config/struct.Config.html"),
            RUSTDOC_PAGE,
        )
        .unwrap();

        run_in(root).unwrap();

        let noindex = "<meta name=\"robots\" content=\"noindex\">";
        for page in [
            "api/zeroclaw/index.html",
            "api/zeroclaw/config/struct.Config.html",
            "v0.8.5/en/404.html",
        ] {
            let out = fs::read_to_string(root.join(page)).unwrap();
            assert_eq!(out.matches(noindex).count(), 1, "{page}");
            assert!(!out.contains("rel=\"canonical\""), "{page}");
        }
        let index = fs::read_to_string(root.join("v0.8.5/en/index.html")).unwrap();
        assert!(index.contains(
            "<link rel=\"canonical\" href=\"https://docs.example/stable/en/index.html\">"
        ));
        assert!(!index.contains(noindex));

        let sitemap = fs::read_to_string(root.join("sitemap.xml")).unwrap();
        assert!(sitemap.contains("<loc>https://docs.example/stable/en/index.html</loc>"));
        assert!(!sitemap.contains("404.html"));
        assert!(!sitemap.contains("/api/"));

        // A second deploy over the same tree changes nothing.
        let snapshot = |p: &str| fs::read_to_string(root.join(p)).unwrap();
        let before = [
            snapshot("api/zeroclaw/index.html"),
            snapshot("v0.8.5/en/404.html"),
            snapshot("v0.8.5/en/index.html"),
        ];
        run_in(root).unwrap();
        let after = [
            snapshot("api/zeroclaw/index.html"),
            snapshot("v0.8.5/en/404.html"),
            snapshot("v0.8.5/en/index.html"),
        ];
        assert_eq!(before, after);
    }

    #[test]
    fn every_copy_of_a_stable_page_names_the_version_less_url() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("CNAME"), "docs.example\n").unwrap();
        fs::write(root.join("stable-version.txt"), "v0.8.6\n").unwrap();
        for page in [
            "master/en/intro.html",
            "master/en/index.html",
            "master/en/new.html",
            "v0.8.6/en/intro.html",
            "v0.8.6/en/index.html",
            "v0.8.6/fr/intro.html",
            "v0.8.6/fr/index.html",
            "v0.8.5/en/intro.html",
            "v0.8.5/en/index.html",
            "v0.8.5/en/gone.html",
        ] {
            fs::create_dir_all(root.join(page).parent().unwrap()).unwrap();
            fs::write(root.join(page), PAGE).unwrap();
        }

        run_in(root).unwrap();

        let read = |p: &str| fs::read_to_string(root.join(p)).unwrap();
        let canonical =
            "<link rel=\"canonical\" href=\"https://docs.example/stable/en/intro.html\">";
        for copy in [
            "master/en/intro.html",
            "v0.8.6/en/intro.html",
            "v0.8.5/en/intro.html",
        ] {
            assert!(read(copy).contains(canonical), "{copy}");
        }
        let stable = read("v0.8.6/en/intro.html");
        assert!(
            stable.contains("hreflang=\"fr\" href=\"https://docs.example/stable/fr/intro.html\"")
        );
        assert!(
            stable.contains(
                "hreflang=\"x-default\" href=\"https://docs.example/stable/en/intro.html\""
            )
        );
        assert!(
            !stable.contains("/v0.8.6/"),
            "no release-pinned URL on the stable copy"
        );
        // Pages stable does not have keep their own URL.
        assert!(
            read("master/en/new.html").contains("href=\"https://docs.example/master/en/new.html\"")
        );
        assert!(read("v0.8.5/en/gone.html").contains("<meta name=\"robots\" content=\"noindex\">"));

        let sitemap = read("sitemap.xml");
        assert_eq!(sitemap.matches("<loc>").count(), 5, "{sitemap}");
        assert!(sitemap.contains("<loc>https://docs.example/stable/fr/intro.html</loc>"));
        assert!(!sitemap.contains("/v0.8.6/"));
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
