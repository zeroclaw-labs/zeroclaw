use crate::cmd::mdbook::refs::{build_api, build_refs};
use crate::util::*;
use anyhow::Context as _;
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
    prepare_generated_book_inputs(root, &entries)?;
    let mdbook = mdbook_program()?;
    let preprocessor_env = mdbook_xtask_preprocessor_env();
    let tag_dir = tag.unwrap_or(DEFAULT_TAG);
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
        if let Some(env) = &preprocessor_env {
            cmd.envs(env.iter().map(|(key, value)| (key, value)));
        }
        run_cmd(&mut cmd)?;
    }
    if let Some(primary) = primary_locale.as_deref() {
        build_llms(root, tag, primary)?;
    }
    Ok(())
}

/// Emit `llms.txt` and `llms-full.txt` beside the primary locale's HTML by
/// running mdBook once more with only the in-tree `llms` backend. The
/// preprocessors that support that renderer (gettext, peer-groups,
/// placeholders) run again so the text matches the rendered pages; mermaid
/// serves the HTML renderer only, so diagram source stays fenced. No HTML
/// backend runs, so the output already in `dest` is untouched. `serve` does
/// not call this, and an HTML watch rebuild of `dest` discards the pair.
pub fn build_llms(root: &Path, tag: Option<&str>, locale: &str) -> anyhow::Result<()> {
    use crate::cmd::mdbook::llms;
    let tag_dir = tag.unwrap_or(DEFAULT_TAG);
    let dest = format!("book/{tag_dir}/{locale}");
    println!(
        "==> Writing {} and {} for {locale}",
        llms::INDEX_FILE,
        llms::FULL_FILE
    );
    let exe = std::env::current_exe().context("resolve xtask binary for the llms backend")?;
    let mut cmd = Command::new(mdbook_program()?);
    cmd.args(["build", "-d", &dest])
        .env("MDBOOK_BOOK__LANGUAGE", locale)
        .env("MDBOOK_OUTPUT", llms_output_table(&exe.to_string_lossy()))
        .env(llms::BASE_URL_ENV, llms::base_url_for(tag_dir, locale))
        .current_dir(book_dir(root));
    if let Some(env) = mdbook_xtask_preprocessor_env() {
        cmd.envs(env.iter().map(|(key, value)| (key, value)));
    }
    run_cmd(&mut cmd)
}

/// JSON for `MDBOOK_OUTPUT`: replaces the whole `output` table from
/// `book.toml` so this pass runs the `llms` backend alone. With a single
/// backend mdBook writes straight into `-d`, not `<dest>/llms/`.
fn llms_output_table(helper_path: &str) -> String {
    let quoted =
        shlex::try_quote(helper_path).map_or_else(|_| helper_path.to_string(), |q| q.into_owned());
    serde_json::json!({ "llms": { "command": format!("{quoted} llms") } }).to_string()
}

/// Generate every gitignored input that mdBook sources or hashes while
/// rendering. Keep all entrypoints on this helper so a warm working tree cannot
/// hide a missing generator from clean-checkout builds.
pub fn prepare_generated_book_inputs(root: &Path, entries: &[LocaleEntry]) -> anyhow::Result<()> {
    build_refs(root)?;
    inject_lang_switcher_locales(&book_dir(root), entries)?;
    crate::cmd::mdbook::themes::run(root)?;
    crate::cmd::mdbook::keymap::run(root)?;
    crate::cmd::mdbook::hardware::run(root)?;
    crate::cmd::mdbook::feature_matrix::run(root)?;
    crate::cmd::mdbook::plugins::run(root)?;
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
    let api_dest = book.join("book").join("api");
    let _ = std::fs::remove_dir_all(&api_dest);
    copy_dir_all(doc_dir(root), &api_dest)?;
    prune_rustdoc_source_view(&api_dest)?;

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

pub fn prune_rustdoc_source_view(api_dir: &Path) -> anyhow::Result<()> {
    let src_view = api_dir.join("src");
    if !src_view.exists() {
        return Ok(());
    }
    let mut stack = vec![api_dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        if dir == src_view {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            match entry.file_type() {
                Ok(ty) if ty.is_dir() => stack.push(path),
                Ok(_) if path.extension().is_some_and(|e| e == "html") => {
                    if let Ok(content) = std::fs::read_to_string(&path) {
                        let stripped = strip_source_anchors(&content);
                        if stripped != content {
                            std::fs::write(&path, stripped)?;
                        }
                    }
                }
                _ => {}
            }
        }
    }
    std::fs::remove_dir_all(&src_view)?;
    Ok(())
}

pub fn strip_source_anchors(html: &str) -> String {
    let needle = "<a class=\"src\"";
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(start) = rest.find(needle) {
        let after = &rest[start..];
        let Some(end) = after.find("</a>") else {
            break;
        };
        out.push_str(&rest[..start]);
        rest = &after[end + "</a>".len()..];
    }
    out.push_str(rest);
    out
}

pub fn strip_chrome_hash(file_name: &str) -> Option<String> {
    let pos = file_name.rfind('-')?;
    let rel_dot = file_name[pos + 1..].find('.')?;
    let ext_pos = pos + 1 + rel_dot;
    let hash = &file_name[pos + 1..ext_pos];
    if hash.len() == 8 && hash.chars().all(|c| c.is_ascii_hexdigit()) {
        Some(format!("{}{}", &file_name[..pos], &file_name[ext_pos..]))
    } else {
        None
    }
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

    let mut replacements = Vec::new();
    let prefixes = [
        "css/chrome",
        "theme/custom",
        "theme/version-selector",
        "theme/lang-switcher",
        "favicon",
        "theme/pc-themes",
        "theme/pc-enhance",
        "mermaid",
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
            let file_name = file
                .file_name()
                .with_context(|| format!("asset path has no file name: {}", file.display()))?
                .to_string_lossy();
            if let Some(unhashed_name) = strip_chrome_hash(&file_name) {
                let dest_rel = rel
                    .parent()
                    .with_context(|| format!("asset path has no parent: {}", rel.display()))?
                    .join(unhashed_name);
                let dest = shared_dir.join(&dest_rel);
                let dest_parent = dest.parent().with_context(|| {
                    format!("shared asset destination has no parent: {}", dest.display())
                })?;
                std::fs::create_dir_all(dest_parent)?;
                std::fs::copy(&file, &dest)?;
                let dest_rel_str = dest_rel.to_string_lossy().replace('\\', "/");
                // Store (locale-relative hashed path, unhashed shared-relative path).
                replacements.push((rel_str.clone(), dest_rel_str));
            }
        }
    }

    for entry in locale_entries() {
        let loc_dir = version_dir.join(&entry.code);
        for file in walk_dir(&loc_dir) {
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

#[cfg(test)]
mod tests {
    use super::{llms_output_table, strip_chrome_hash, strip_source_anchors};

    #[test]
    fn llms_output_table_names_only_the_llms_backend() {
        let table: serde_json::Value =
            serde_json::from_str(&llms_output_table("/t/x task")).unwrap();
        assert_eq!(table["llms"]["command"], "'/t/x task' llms");
        assert_eq!(
            table.as_object().unwrap().len(),
            1,
            "html must not run in this pass"
        );
    }

    #[test]
    fn strips_single_extension_hash() {
        assert_eq!(
            strip_chrome_hash("custom-abc12345.css").as_deref(),
            Some("custom.css")
        );
    }

    #[test]
    fn strips_compound_extension_hash() {
        assert_eq!(
            strip_chrome_hash("mermaid-193ae996.min.js").as_deref(),
            Some("mermaid.min.js")
        );
    }

    #[test]
    fn ignores_unhashed_names() {
        assert_eq!(strip_chrome_hash("mermaid-init.js"), None);
        assert_eq!(strip_chrome_hash("chrome.css"), None);
    }

    #[test]
    fn ignores_non_hex_or_wrong_length() {
        assert_eq!(strip_chrome_hash("foo-1234567.js"), None);
        assert_eq!(strip_chrome_hash("foo-zzzzzzzz.js"), None);
    }

    #[test]
    fn strips_rustdoc_source_anchor() {
        let html = r#"<div><a class="src" href="../../src/zeroclaw_tools/x.rs.html#146-177">Source</a></div>"#;
        assert_eq!(strip_source_anchors(html), "<div></div>");
    }

    #[test]
    fn strips_multiple_source_anchors() {
        let html = concat!(
            r#"<a class="src" href="../src/a.rs.html#1">Source</a>"#,
            "middle",
            r#"<a class="src" href="../src/b.rs.html#2">Source</a>"#,
        );
        assert_eq!(strip_source_anchors(html), "middle");
    }

    #[test]
    fn leaves_non_source_anchors_intact() {
        let html = r#"<a href="../../src/foo">keep</a> and <a class="docblock">keep</a>"#;
        assert_eq!(strip_source_anchors(html), html);
    }
}
