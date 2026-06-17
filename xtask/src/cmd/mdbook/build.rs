use crate::cmd::mdbook::refs::{build_api, build_refs};
use crate::util::*;
use std::path::Path;
use std::process::Command;

const DEFAULT_TAG: &str = "master";

pub fn run(tag: Option<&str>) -> anyhow::Result<()> {
    let root = repo_root();
    require_tool("cargo", "https://rustup.rs")?;
    ensure_cargo_tool("mdbook", "mdbook")?;
    ensure_cargo_tool("mdbook-xgettext", "mdbook-i18n-helpers")?;
    ensure_cargo_tool("mdbook-gettext", "mdbook-i18n-helpers")?;
    ensure_cargo_tool("mdbook-mermaid", "mdbook-mermaid")?;

    build_refs(&root)?;
    build_api(&root)?;
    build_locales(&root, tag)?;
    crate::cmd::mdbook::linkcheck::check_internal_links(&root, tag.unwrap_or(DEFAULT_TAG))?;
    assemble(&root, tag)?;
    println!(
        "==> Done. Open: {}",
        book_dir(&root)
            .join("book")
            .join(tag.unwrap_or(DEFAULT_TAG))
            .join("index.html")
            .display()
    );
    Ok(())
}

pub fn build_locales(root: &std::path::Path, tag: Option<&str>) -> anyhow::Result<()> {
    let book = book_dir(root);
    let entries = locale_entries();
    println!(
        "==> Building mdBook for locales: {}",
        entries
            .iter()
            .map(|e| e.code.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    );
    inject_lang_switcher_locales(&book, &entries)?;
    crate::cmd::mdbook::themes::run(root)?;
    crate::cmd::mdbook::keymap::run(root)?;
    crate::cmd::mdbook::hardware::run(root)?;
    let mdbook = mdbook_program()?;
    let preprocessor_env = peer_groups_preprocessor_env();
    let tag_dir = tag.unwrap_or(DEFAULT_TAG);
    // Search is enabled only for the primary locale. Per-locale searchindex is
    // high-entropy (~6-7 MB raw each) and does not delta-compress across
    // versions, so building it for every locale dominates gh-pages clone size.
    // The primary locale (first in locales.toml, English) keeps full search;
    // translated locales build without a search index or search box.
    let primary_locale = entries.first().map(|e| e.code.clone());
    for entry in &entries {
        let dest = format!("book/{}/{}", tag_dir, entry.code);
        let mut cmd = Command::new(&mdbook);
        cmd.args(["build", "-d", &dest])
            .env("MDBOOK_BOOK__LANGUAGE", &entry.code)
            .current_dir(&book);
        if Some(&entry.code) != primary_locale.as_ref() {
            cmd.env("MDBOOK_OUTPUT__HTML__SEARCH__ENABLE", "false");
        }
        if let Some((key, value)) = &preprocessor_env {
            cmd.env(key, value);
        }
        run_cmd(&mut cmd)?;
    }
    Ok(())
}

/// Render `theme/lang-switcher.js.tpl` into `theme/lang-switcher.js` with the
/// `LOCALES` array filled from `locales.toml`. The `.js` output is gitignored
/// (every locale add/remove rewrites it); the `.tpl` source is the tracked
/// truth. Errors loudly when the template is missing — silently skipping
/// would let mdBook fail later with a confusing "missing additional-js"
/// message.
pub fn inject_lang_switcher_locales(book: &Path, entries: &[LocaleEntry]) -> anyhow::Result<()> {
    let tpl_path = book.join("theme/lang-switcher.js.tpl");
    let js_path = book.join("theme/lang-switcher.js");
    let src = std::fs::read_to_string(&tpl_path).map_err(|e| {
        anyhow::Error::msg(format!(
            "lang-switcher.js.tpl missing at {}: {e}. The template is the tracked source of \
             truth for the locale switcher; do not delete it.",
            tpl_path.display(),
        ))
    })?;
    let locale_lines: String = entries
        .iter()
        .map(|e| format!("    {{ code: {:?}, label: {:?} }},", e.code, e.label))
        .collect::<Vec<_>>()
        .join("\n");
    let new_block = format!("const LOCALES = [\n{locale_lines}\n  ];");

    let start = src
        .find("const LOCALES = [")
        .ok_or_else(|| anyhow::Error::msg("lang-switcher.js.tpl: LOCALES array not found"))?;
    let end = src[start..]
        .find("];")
        .ok_or_else(|| anyhow::Error::msg("lang-switcher.js.tpl: LOCALES closing ]; not found"))?;
    let updated = format!("{}{}{}", &src[..start], new_block, &src[start + end + 2..]);
    std::fs::write(&js_path, updated)?;
    Ok(())
}

pub fn print_locales() {
    let codes: Vec<String> = locale_entries().into_iter().map(|e| e.code).collect();
    println!("{}", codes.join(" "));
}

pub fn assemble(root: &std::path::Path, tag: Option<&str>) -> anyhow::Result<()> {
    println!("==> Assembling site (rustdoc + locale redirect)");
    let book = book_dir(root);
    let tag_dir = tag.unwrap_or(DEFAULT_TAG);
    let api_dest = book.join("book").join(tag_dir).join("api");
    let _ = std::fs::remove_dir_all(&api_dest);
    copy_dir_all(doc_dir(root), &api_dest)?;

    const INDEX_HTML: &str = "<!doctype html>\n<meta charset=\"utf-8\">\n<meta http-equiv=\"refresh\" content=\"0; url=./en/\">\n<link rel=\"canonical\" href=\"./en/\">\n<title>ZeroClaw Docs</title>\n";
    let out_dir = book.join("book").join(tag_dir);
    std::fs::create_dir_all(&out_dir)?;
    std::fs::write(out_dir.join("index.html"), INDEX_HTML)?;
    // Write small metadata file with the version tag
    let version_meta = format!("{}\n", tag.unwrap_or(DEFAULT_TAG));
    std::fs::write(out_dir.join("_version.txt"), version_meta)?;

    let version_dir = out_dir;
    let shared_dir = book.join("book").join("_shared");
    extract_shared_chrome(&version_dir, &shared_dir)?;
    Ok(())
}

pub fn extract_shared_chrome(version_dir: &Path, shared_dir: &Path) -> anyhow::Result<()> {
    println!("==> Extracting shared chrome layer");

    let first_locale = locale_entries()
        .into_iter()
        .next()
        .map(|e| e.code)
        .unwrap_or_else(|| "en".to_string());
    let src_dir = version_dir.join(&first_locale);
    if !src_dir.exists() {
        return Ok(());
    }

    // Map each hashed chrome file (path relative to the locale dir, e.g.
    // `theme/custom-abc12345.css`) to its unhashed `_shared`-relative path
    // (e.g. `theme/custom.css`). The `../` prefix is applied per HTML file
    // below, because a page's correct depth to the version root — and thus to
    // `_shared` at the gh-pages root — depends on how deep the page sits.
    let mut replacements = Vec::new();
    let prefixes = [
        "css/chrome",
        "theme/custom",
        "theme/version-selector",
        "theme/lang-switcher",
        "favicon",
        "theme/pc-themes",
        "theme/pc-enhance",
    ];

    let walk_dir = |dir: &Path| -> Vec<std::path::PathBuf> {
        let mut paths = Vec::new();
        let mut stack = vec![dir.to_path_buf()];
        while let Some(path) = stack.pop() {
            if let Ok(entries) = std::fs::read_dir(&path) {
                for entry in entries.flatten() {
                    if let Ok(ty) = entry.file_type() {
                        if ty.is_dir() {
                            stack.push(entry.path());
                        } else {
                            paths.push(entry.path());
                        }
                    }
                }
            }
        }
        paths
    };

    for file in walk_dir(&src_dir) {
        if let Ok(rel) = file.strip_prefix(&src_dir) {
            let rel_str = rel.to_string_lossy().replace('\\', "/");
            if !prefixes.iter().any(|p| rel_str.starts_with(p)) {
                continue;
            }
            let file_name = file.file_name().unwrap().to_string_lossy();
            if let Some(pos) = file_name.rfind('-')
                && let Some(ext_pos) = file_name.rfind('.')
                && pos < ext_pos
            {
                let hash = &file_name[pos + 1..ext_pos];
                if hash.len() == 8 && hash.chars().all(|c| c.is_ascii_hexdigit()) {
                    let unhashed_name = format!("{}{}", &file_name[..pos], &file_name[ext_pos..]);
                    let dest_rel = rel.parent().unwrap().join(unhashed_name);
                    let dest = shared_dir.join(&dest_rel);
                    std::fs::create_dir_all(dest.parent().unwrap())?;
                    std::fs::copy(&file, &dest)?;
                    let dest_rel_str = dest_rel.to_string_lossy().replace('\\', "/");
                    // Store (locale-relative hashed path, unhashed shared-relative path).
                    replacements.push((rel_str.clone(), dest_rel_str));
                }
            }
        }
    }

    for entry in locale_entries() {
        let loc_dir = version_dir.join(&entry.code);
        for file in walk_dir(&loc_dir) {
            // Depth of this HTML file below the locale dir. mdBook emits chrome
            // refs as `<../ × (locale_depth + page_depth)>theme/foo-HASH.css`
            // for an HTML page `page_depth` levels under the locale dir; the
            // matching `_shared` ref needs the same total `../` count plus one
            // to clear the version dir up to the gh-pages root where `_shared`
            // lives. Concretely: page directly in `<tag>/<locale>/` -> `../../`,
            // one level deeper -> `../../../`, and so on.
            let page_depth = file
                .strip_prefix(&loc_dir)
                .ok()
                .map(|rel| rel.components().count().saturating_sub(1))
                .unwrap_or(0);
            let up = "../".repeat(page_depth + 2);

            if file.extension().is_some_and(|e| e == "html")
                && let Ok(mut content) = std::fs::read_to_string(&file)
            {
                let mut changed = false;
                for (hashed_rel, shared_rel) in &replacements {
                    // mdBook references the chrome file relative to the page, so
                    // the on-disk ref is `<../ × page_depth><hashed_rel>`. Match
                    // and replace at that exact depth with the `_shared` ref at
                    // the correct depth — never a hardcoded prefix.
                    let page_up = "../".repeat(page_depth);
                    let from = format!("{page_up}{hashed_rel}");
                    let to = format!("{up}_shared/{shared_rel}");
                    if content.contains(&from) {
                        content = content.replace(&from, &to);
                        changed = true;
                    }
                }
                if changed {
                    let _ = std::fs::write(&file, content);
                }
            }
            if let Ok(rel) = file.strip_prefix(&loc_dir) {
                let rel_str = rel.to_string_lossy().replace('\\', "/");
                if replacements.iter().any(|(from, _)| from == &rel_str) {
                    let _ = std::fs::remove_file(&file);
                }
            }
        }
    }

    Ok(())
}
