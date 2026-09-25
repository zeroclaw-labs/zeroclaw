//! mdBook renderer backend: emit `llms.txt` and `llms-full.txt`.
//!
//! `llms.txt` is the short index described at <https://llmstxt.org>: the book
//! title, a one-line summary, then one `## <top-level chapter>` section per
//! numbered chapter with a link and one-line description for every page it
//! contains. `llms-full.txt` is the entire book as one Markdown stream, each
//! page prefixed by its canonical URL, so an agent can ingest the whole site
//! in a single fetch.
//!
//! mdBook invokes this backend with the preprocessed book on stdin. Only the
//! preprocessors that declare support for the `llms` renderer run: gettext,
//! peer-groups, and placeholders. `mdbook-mermaid` supports the HTML renderer
//! alone, so fenced ```mermaid blocks stay as diagram source here, and page
//! bodies keep their authored `.md` links; nothing is rewritten to deployed
//! HTML URLs. `build.rs` runs the backend as a second `mdbook build` with the
//! `output` table replaced (`MDBOOK_OUTPUT`), which keeps the HTML backend
//! out of that pass and avoids mdBook's multi-backend `<dest>/html/` layout.
//!
//! [`sync_root`] is the deploy-time counterpart: it mirrors the stable
//! release's pair to the gh-pages root, or withdraws the root pair when that
//! release was built without this backend.

use anyhow::Context as _;
use serde::Deserialize;
use std::fmt::Write as _;
use std::io::Read as _;
use std::path::{Path, PathBuf};

/// Environment variable carrying the absolute URL prefix for every page link
/// (for example `https://docs.zeroclaw.com/v0.8.5/en/`). Set by `build.rs`.
pub const BASE_URL_ENV: &str = "ZEROCLAW_DOCS_LLMS_BASE_URL";

/// Public host for the docs site; the versioned prefix is appended per build.
pub const DOCS_ORIGIN: &str = "https://docs.zeroclaw.com";

pub const INDEX_FILE: &str = "llms.txt";
pub const FULL_FILE: &str = "llms-full.txt";

/// Locale whose pair the gh-pages root mirrors.
pub const ROOT_LOCALE: &str = "en";

/// Pointer file at the gh-pages root naming the stable release directory.
const STABLE_POINTER_FILE: &str = "stable-version.txt";

/// Longest description emitted per page in `llms.txt`.
const DESCRIPTION_MAX_CHARS: usize = 200;

#[derive(Deserialize)]
struct RenderContext {
    destination: PathBuf,
    config: Config,
    book: Book,
}

#[derive(Deserialize)]
struct Config {
    book: BookConfig,
}

#[derive(Deserialize)]
struct BookConfig {
    title: Option<String>,
    description: Option<String>,
}

#[derive(Deserialize)]
struct Book {
    /// Raw `BookItem`s: `{"Chapter": {...}}`, `"Separator"`, or
    /// `{"PartTitle": "..."}`. Only chapters carry pages.
    items: Vec<serde_json::Value>,
}

#[derive(Deserialize)]
struct Chapter {
    name: String,
    content: String,
    sub_items: Vec<serde_json::Value>,
    /// Output-relative path (`a/index.md` for `a/README.md`); `None` for
    /// draft chapters that have no file.
    path: Option<PathBuf>,
}

/// Entry point for `cargo mdbook llms`: read the render context from stdin
/// and write both files into the renderer destination.
pub fn run() -> anyhow::Result<()> {
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input)?;
    let ctx: RenderContext =
        serde_json::from_str(&input).context("llms backend: invalid render context on stdin")?;
    let base_url = std::env::var(BASE_URL_ENV).with_context(|| {
        format!("llms backend: {BASE_URL_ENV} must hold the absolute URL prefix for page links")
    })?;
    let rendered = render(&ctx, &base_url)?;
    std::fs::create_dir_all(&ctx.destination)?;
    std::fs::write(ctx.destination.join(INDEX_FILE), rendered.index)?;
    std::fs::write(ctx.destination.join(FULL_FILE), rendered.full)?;
    println!(
        "==> llms backend wrote {INDEX_FILE} and {FULL_FILE} to {}",
        ctx.destination.display()
    );
    Ok(())
}

/// Absolute URL prefix for a deployed version/locale pair, always with a
/// trailing slash so page paths can be appended directly.
pub fn base_url_for(tag: &str, locale: &str) -> String {
    format!("{DOCS_ORIGIN}/{tag}/{locale}/")
}

/// Entry point for `cargo mdbook sync-root-llms`, run in the gh-pages clone
/// root after `gen-root-index`: publish the stable release's `llms.txt` and
/// `llms-full.txt` at the site root, or withdraw the root pair when that
/// release has none.
///
/// The two files are one publication unit. The source is the stable pointer's
/// target when `stable-version.txt` names a version dir present on gh-pages,
/// otherwise `master`; that is the same resolution `gen-root-index` uses for
/// `/`. Only a source carrying both files is mirrored. In every other state
/// both root files are removed, so `/llms.txt` can never keep serving a
/// release other than the one `/` redirects to. A release built before this
/// backend existed therefore defers the root pair instead of inheriting a
/// stale copy or a master build presented as stable.
pub fn sync_root(root: &Path) -> anyhow::Result<()> {
    let source = root_source(root);
    let src_dir = root.join(&source).join(ROOT_LOCALE);
    let files = [INDEX_FILE, FULL_FILE];
    if files.iter().all(|file| src_dir.join(file).is_file()) {
        for file in files {
            let dest = root.join(file);
            let staged = root.join(format!("{file}.tmp"));
            std::fs::copy(src_dir.join(file), &staged)
                .with_context(|| format!("copy {source}/{ROOT_LOCALE}/{file} to the root"))?;
            std::fs::rename(&staged, &dest)
                .with_context(|| format!("publish {file} at the root"))?;
        }
        println!(
            "==> sync-root-llms: published {INDEX_FILE} and {FULL_FILE} from {source}/{ROOT_LOCALE}/"
        );
        return Ok(());
    }
    let mut removed = Vec::new();
    for file in files {
        let dest = root.join(file);
        if dest.is_file() {
            std::fs::remove_file(&dest).with_context(|| format!("remove stale root {file}"))?;
            removed.push(file);
        }
    }
    let removal = if removed.is_empty() {
        String::new()
    } else {
        format!(" Removed stale root {}.", removed.join(" and "))
    };
    println!(
        "::notice::sync-root-llms: {source}/{ROOT_LOCALE}/ has no {INDEX_FILE} + {FULL_FILE} pair \
         (built before the llms backend existed), so the root pair is deferred.{removal} It \
         publishes on the first deploy whose stable target carries both files."
    );
    Ok(())
}

/// Version dir whose English pair the root mirrors: the stable pointer's
/// target when it names a version dir present under `root`, else `master`.
/// Mirrors `gen-root-index`, so `/llms.txt` always describes the release `/`
/// redirects to.
fn root_source(root: &Path) -> String {
    let pointer = std::fs::read_to_string(root.join(STABLE_POINTER_FILE))
        .ok()
        .map(|raw| raw.trim().to_string())
        .filter(|tag| !tag.is_empty());
    match pointer {
        Some(tag) if super::versions::is_version_dir(&tag) && root.join(&tag).is_dir() => tag,
        _ => "master".to_string(),
    }
}

struct Rendered {
    index: String,
    full: String,
}

/// One page as it appears in both outputs.
struct Page {
    title: String,
    url: String,
    content: String,
    /// Top-level chapter title this page belongs to (itself for top level).
    section: String,
}

fn render(ctx: &RenderContext, base_url: &str) -> anyhow::Result<Rendered> {
    let base_url = if base_url.ends_with('/') {
        base_url.to_string()
    } else {
        format!("{base_url}/")
    };
    let title = ctx.config.book.title.as_deref().unwrap_or("Documentation");
    let description = ctx.config.book.description.as_deref().unwrap_or("").trim();

    let pages = collect_pages(&ctx.book.items, &base_url)?;

    let mut index = String::new();
    let _ = writeln!(index, "# {title}\n");
    if !description.is_empty() {
        let _ = writeln!(index, "> {description}\n");
    }
    let _ = writeln!(
        index,
        "This index lists every page of the {title} site. The full text of all pages is \
         available in one file at {base_url}{FULL_FILE}. The HTML site is at {base_url}.\n"
    );
    let mut current_section: Option<&str> = None;
    for page in &pages {
        if current_section != Some(page.section.as_str()) {
            if current_section.is_some() {
                index.push('\n');
            }
            let _ = writeln!(index, "## {}\n", page.section);
            current_section = Some(page.section.as_str());
        }
        let summary = first_paragraph(&page.content, DESCRIPTION_MAX_CHARS);
        if summary.is_empty() {
            let _ = writeln!(index, "- [{}]({})", page.title, page.url);
        } else {
            let _ = writeln!(index, "- [{}]({}): {}", page.title, page.url, summary);
        }
    }

    let mut full = String::new();
    let _ = writeln!(full, "# {title}\n");
    if !description.is_empty() {
        let _ = writeln!(full, "> {description}\n");
    }
    let _ = writeln!(
        full,
        "Every page of the {title} site, in reading order. Each page starts with its \
         canonical HTML URL. Page bodies are the authored Markdown: links inside a page \
         keep their `.md` targets and are not rewritten to deployed URLs, and every page \
         they point to is included in this file.\n"
    );
    for page in &pages {
        let _ = writeln!(full, "---\n\nSource: {}\n", page.url);
        full.push_str(page.content.trim_end());
        full.push_str("\n\n");
    }

    Ok(Rendered { index, full })
}

/// Flatten the book tree in reading order, skipping draft chapters (no file).
/// Separators and part titles carry no pages and are ignored.
fn collect_pages(items: &[serde_json::Value], base_url: &str) -> anyhow::Result<Vec<Page>> {
    let mut pages = Vec::new();
    for chapter in chapters(items)? {
        let section = chapter.name.clone();
        push_chapter(chapter, &section, base_url, &mut pages)?;
    }
    Ok(pages)
}

fn chapters(items: &[serde_json::Value]) -> anyhow::Result<Vec<Chapter>> {
    items
        .iter()
        .filter_map(|item| item.get("Chapter"))
        .map(|raw| serde_json::from_value(raw.clone()).context("llms backend: malformed chapter"))
        .collect()
}

fn push_chapter(
    chapter: Chapter,
    section: &str,
    base_url: &str,
    pages: &mut Vec<Page>,
) -> anyhow::Result<()> {
    let Chapter {
        name,
        content,
        sub_items,
        path,
    } = chapter;
    if let Some(path) = path {
        pages.push(Page {
            title: name,
            url: format!("{base_url}{}", html_path(&path)),
            content,
            section: section.to_string(),
        });
    }
    // Chapters nest arbitrarily; every descendant reports the top-level section.
    for child in chapters(&sub_items)? {
        push_chapter(child, section, base_url, pages)?;
    }
    Ok(())
}

/// Map a chapter's output path to the URL path mdBook's HTML renderer uses.
/// mdBook already rewrites `README.md` to `index.md` in `Chapter::path`.
fn html_path(path: &std::path::Path) -> String {
    let raw = path.to_string_lossy().replace('\\', "/");
    match raw.strip_suffix(".md") {
        Some(stem) => format!("{stem}.html"),
        None => raw,
    }
}

/// First prose paragraph of a page as plain text, skipping the leading `#`
/// heading, HTML blocks, fenced code, and mdBook directives. A page that
/// opens with a list falls back to its first list item. Truncated on a word
/// boundary with an ellipsis when longer than `max_chars`.
pub fn first_paragraph(markdown: &str, max_chars: usize) -> String {
    let mut in_fence = false;
    let mut paragraph = String::new();
    let mut first_list_item: Option<&str> = None;
    for line in markdown.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        if trimmed.is_empty() {
            if !paragraph.is_empty() {
                break;
            }
            continue;
        }
        if first_list_item.is_none() {
            first_list_item = trimmed
                .strip_prefix("- ")
                .or_else(|| trimmed.strip_prefix("* "));
        }
        let skip = trimmed.starts_with('#')
            || trimmed.starts_with('<')
            || trimmed.starts_with("{{#")
            || trimmed.starts_with('|')
            || trimmed.starts_with("- ")
            || trimmed.starts_with("* ")
            || trimmed.starts_with('>')
            || trimmed.starts_with("![");
        if skip {
            if !paragraph.is_empty() {
                break;
            }
            continue;
        }
        if !paragraph.is_empty() {
            paragraph.push(' ');
        }
        paragraph.push_str(trimmed);
    }
    if let (true, Some(item)) = (paragraph.is_empty(), first_list_item) {
        paragraph.push_str(item);
    }
    truncate_words(&strip_inline_markup(&paragraph), max_chars)
}

/// Remove inline Markdown decoration so descriptions read as plain text.
fn strip_inline_markup(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '`' | '*' | '_' => {}
            '[' => {
                // `[label](target)` -> `label`; a bare `[` passes through.
                let mut label = String::new();
                let mut closed = false;
                for inner in chars.by_ref() {
                    if inner == ']' {
                        closed = true;
                        break;
                    }
                    label.push(inner);
                }
                if closed && chars.peek() == Some(&'(') {
                    for inner in chars.by_ref() {
                        if inner == ')' {
                            break;
                        }
                    }
                }
                out.push_str(&label);
            }
            _ => out.push(c),
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn truncate_words(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let mut out = String::new();
    for word in text.split(' ') {
        let candidate_len =
            out.chars().count() + word.chars().count() + usize::from(!out.is_empty());
        if candidate_len > max_chars.saturating_sub(1) {
            break;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
    }
    if out.is_empty() {
        out = text.chars().take(max_chars.saturating_sub(1)).collect();
    }
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ctx() -> RenderContext {
        let value = json!({
            "version": "0.5.4",
            "root": "/book",
            "destination": "/book/out",
            "config": {
                "book": { "title": "ZeroClaw Docs", "description": "Docs for ZeroClaw." }
            },
            "book": { "items": [
                { "Chapter": { "name": "Introduction", "content": "# ZeroClaw\n\nPersonal assistant *you* own.\n\nMore.\n",
                    "number": [1], "sub_items": [], "path": "introduction.md", "source_path": "introduction.md", "parent_names": [] } },
                { "PartTitle": "Guides" },
                { "Chapter": { "name": "Setup", "content": "# Setup\n\n<div class=\"warning\">note</div>\n\n```sh\ncargo install\n```\n\nInstall on [Linux](./linux.md) or macOS.\n",
                    "number": [2], "sub_items": [
                        { "Chapter": { "name": "Linux", "content": "# Linux\n\n{{#include ../_snippets/install.md}}\n\nUse the script.\n",
                            "number": [2, 1], "sub_items": [], "path": "setup/linux.md", "source_path": "setup/linux.md", "parent_names": ["Setup"] } }
                    ], "path": "setup/index.md", "source_path": "setup/README.md", "parent_names": [] } },
                "Separator",
                { "Chapter": { "name": "Draft", "content": "", "number": [3], "sub_items": [], "path": null, "source_path": null, "parent_names": [] } },
                { "Chapter": { "name": "Appendix", "content": "# Appendix\n\nTrailing prefix-less chapter.\n", "number": null, "sub_items": [], "path": "appendix.md", "source_path": "appendix.md", "parent_names": [] } }
            ] }
        });
        serde_json::from_value(value).expect("test context deserializes")
    }

    #[test]
    fn index_groups_pages_by_top_level_chapter_with_absolute_urls() {
        let out = render(&ctx(), "https://docs.zeroclaw.com/v0.8.5/en").unwrap();
        let index = out.index;
        assert!(index.starts_with("# ZeroClaw Docs\n\n> Docs for ZeroClaw.\n\n"));
        assert!(index.contains("https://docs.zeroclaw.com/v0.8.5/en/llms-full.txt"));
        assert!(index.contains("## Introduction\n\n- [Introduction](https://docs.zeroclaw.com/v0.8.5/en/introduction.html): Personal assistant you own.\n"));
        assert!(index.contains("## Setup\n\n- [Setup](https://docs.zeroclaw.com/v0.8.5/en/setup/index.html): Install on Linux or macOS.\n- [Linux](https://docs.zeroclaw.com/v0.8.5/en/setup/linux.html): Use the script.\n"));
        assert!(index.contains("## Appendix\n\n- [Appendix](https://docs.zeroclaw.com/v0.8.5/en/appendix.html): Trailing prefix-less chapter.\n"));
        assert!(
            !index.contains("Draft"),
            "draft chapters have no page to link"
        );
    }

    #[test]
    fn full_dump_keeps_reading_order_and_prefixes_each_page_with_its_url() {
        let out = render(&ctx(), "https://docs.zeroclaw.com/master/en/").unwrap();
        let full = out.full;
        let intro = full.find("Source: https://docs.zeroclaw.com/master/en/introduction.html\n\n# ZeroClaw\n\nPersonal assistant *you* own.").unwrap();
        let setup = full
            .find("Source: https://docs.zeroclaw.com/master/en/setup/index.html\n\n# Setup")
            .unwrap();
        let linux = full
            .find("Source: https://docs.zeroclaw.com/master/en/setup/linux.html\n\n# Linux")
            .unwrap();
        let appendix = full
            .find("Source: https://docs.zeroclaw.com/master/en/appendix.html")
            .unwrap();
        assert!(intro < setup && setup < linux && linux < appendix);
        assert!(
            full.contains("```sh\ncargo install\n```"),
            "page bodies are verbatim markdown"
        );
        assert!(
            full.contains("Install on [Linux](./linux.md)"),
            "authored .md links are kept as written"
        );
        assert!(
            full.contains("are not rewritten to deployed URLs"),
            "the preamble must not promise working deployed links"
        );
        assert!(
            !full.contains("Source: https://docs.zeroclaw.com/master/en/\n"),
            "drafts emit nothing"
        );
    }

    #[test]
    fn first_paragraph_skips_headings_html_fences_directives_and_strips_markup() {
        let md = "# Title\n\n<div>x</div>\n\n```rust\nfn a() {}\n```\n\n{{#include foo.md}}\n\nSee **bold** and `code` and [a link](http://x).\nContinues here.\n\nNext para.\n";
        assert_eq!(
            first_paragraph(md, 200),
            "See bold and code and a link. Continues here."
        );
        assert_eq!(first_paragraph("# Only heading\n", 200), "");
        assert_eq!(
            first_paragraph(
                "# T\n\n- **Not a SaaS.** No hosted version.\n- Second.\n",
                200
            ),
            "Not a SaaS. No hosted version.",
            "a page that opens with a list uses its first item"
        );
        assert_eq!(
            first_paragraph("# T\n\n- item\n\nReal paragraph.\n", 200),
            "Real paragraph.",
            "prose still wins over a leading list"
        );
        assert_eq!(first_paragraph("", 200), "");
    }

    #[test]
    fn first_paragraph_truncates_on_word_boundary() {
        let md = "alpha beta gamma delta epsilon";
        assert_eq!(first_paragraph(md, 12), "alpha beta…");
        assert_eq!(first_paragraph(md, 100), md);
    }

    #[test]
    fn html_path_rewrites_markdown_extension_only() {
        assert_eq!(
            html_path(std::path::Path::new("setup/index.md")),
            "setup/index.html"
        );
        assert_eq!(html_path(std::path::Path::new("a/b.md")), "a/b.html");
        assert_eq!(html_path(std::path::Path::new("raw.html")), "raw.html");
    }

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn read(path: &Path) -> Option<String> {
        std::fs::read_to_string(path).ok()
    }

    /// gh-pages fixture: `master/en/` carries a pair, `stable-version.txt`
    /// names `stable`, whose `en/` dir holds `stable_files`.
    fn site(stable: &str, stable_files: &[&str]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(&root.join(STABLE_POINTER_FILE), &format!("{stable}\n"));
        write(&root.join("master/en/index.html"), "<html>");
        write(&root.join("master/en").join(INDEX_FILE), "master index");
        write(&root.join("master/en").join(FULL_FILE), "master full");
        write(&root.join(stable).join("en/index.html"), "<html>");
        for file in stable_files {
            write(
                &root.join(stable).join("en").join(file),
                &format!("{stable} {file}"),
            );
        }
        dir
    }

    #[test]
    fn sync_root_publishes_the_stable_pair_over_an_older_root_pair() {
        let dir = site("v0.9.0", &[INDEX_FILE, FULL_FILE]);
        let root = dir.path();
        write(&root.join(INDEX_FILE), "v0.8.5 llms.txt");
        write(&root.join(FULL_FILE), "v0.8.5 llms-full.txt");
        sync_root(root).unwrap();
        assert_eq!(
            read(&root.join(INDEX_FILE)).as_deref(),
            Some("v0.9.0 llms.txt")
        );
        assert_eq!(
            read(&root.join(FULL_FILE)).as_deref(),
            Some("v0.9.0 llms-full.txt")
        );
        assert!(!root.join(format!("{INDEX_FILE}.tmp")).exists());
    }

    #[test]
    fn sync_root_withdraws_the_root_pair_when_the_stable_target_has_none() {
        // The regression: a root pair from an earlier stable exists, then the
        // pointer moves to a release built without the llms backend. The root
        // must not keep serving the previous release.
        let dir = site("v0.8.5", &[]);
        let root = dir.path();
        write(&root.join(INDEX_FILE), "v0.8.4 llms.txt");
        write(&root.join(FULL_FILE), "v0.8.4 llms-full.txt");
        sync_root(root).unwrap();
        assert!(
            !root.join(INDEX_FILE).exists(),
            "stale root llms.txt removed"
        );
        assert!(
            !root.join(FULL_FILE).exists(),
            "stale root llms-full.txt removed"
        );
        assert_eq!(
            read(&root.join("master/en").join(INDEX_FILE)).as_deref(),
            Some("master index"),
            "master's own pair is untouched and not promoted to the root"
        );
    }

    #[test]
    fn sync_root_treats_a_partial_pair_as_missing() {
        let dir = site("v0.8.5", &[INDEX_FILE]);
        let root = dir.path();
        write(&root.join(FULL_FILE), "v0.8.4 llms-full.txt");
        sync_root(root).unwrap();
        assert!(
            !root.join(INDEX_FILE).exists(),
            "half a pair is not published"
        );
        assert!(
            !root.join(FULL_FILE).exists(),
            "the stale half is withdrawn"
        );
    }

    #[test]
    fn sync_root_follows_master_only_when_no_stable_pointer_resolves() {
        // No pointer: `/` redirects to master, so the root pair mirrors master.
        let dir = site("v0.8.5", &[INDEX_FILE, FULL_FILE]);
        let root = dir.path();
        std::fs::remove_file(root.join(STABLE_POINTER_FILE)).unwrap();
        sync_root(root).unwrap();
        assert_eq!(
            read(&root.join(INDEX_FILE)).as_deref(),
            Some("master index")
        );

        // Pointer names a version dir that is not on gh-pages: same fallback.
        write(&root.join(STABLE_POINTER_FILE), "v0.9.1\n");
        sync_root(root).unwrap();
        assert_eq!(read(&root.join(FULL_FILE)).as_deref(), Some("master full"));

        // Pointer names a present release: that release wins over master.
        write(&root.join(STABLE_POINTER_FILE), "v0.8.5\n");
        sync_root(root).unwrap();
        assert_eq!(
            read(&root.join(INDEX_FILE)).as_deref(),
            Some("v0.8.5 llms.txt")
        );
    }

    #[test]
    fn base_url_joins_origin_tag_and_locale() {
        assert_eq!(
            base_url_for("v0.8.5", "en"),
            "https://docs.zeroclaw.com/v0.8.5/en/"
        );
        assert_eq!(
            base_url_for("master", "en"),
            "https://docs.zeroclaw.com/master/en/"
        );
    }
}
