use crate::cmd::mdbook::refs::{build_api, build_refs};
use crate::util::*;
use std::path::Path;
use std::process::Command;

pub fn run() -> anyhow::Result<()> {
    let root = repo_root();
    require_tool("cargo", "https://rustup.rs")?;
    ensure_cargo_tool("mdbook", "mdbook")?;
    ensure_cargo_tool("mdbook-xgettext", "mdbook-i18n-helpers")?;
    ensure_cargo_tool("mdbook-gettext", "mdbook-i18n-helpers")?;
    ensure_cargo_tool("mdbook-mermaid", "mdbook-mermaid")?;

    build_refs(&root)?;
    build_api(&root)?;
    build_locales(&root)?;
    assemble(&root)?;
    println!(
        "==> Done. Open: {}",
        book_dir(&root).join("book/index.html").display()
    );
    Ok(())
}

pub fn build_locales(root: &std::path::Path) -> anyhow::Result<()> {
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
    let mdbook = mdbook_program()?;
    for entry in &entries {
        run_cmd(
            Command::new(&mdbook)
                .args(["build", "-d", &format!("book/{}", entry.code)])
                .env("MDBOOK_BOOK__LANGUAGE", &entry.code)
                .current_dir(&book),
        )?;
    }
    Ok(())
}

pub fn inject_lang_switcher_locales(book: &Path, entries: &[LocaleEntry]) -> anyhow::Result<()> {
    let js_path = book.join("theme/lang-switcher.js");
    if !js_path.exists() {
        return Ok(());
    }
    let src = std::fs::read_to_string(&js_path)?;
    let locale_lines: String = entries
        .iter()
        .map(|e| format!("    {{ code: {:?}, label: {:?} }},", e.code, e.label))
        .collect::<Vec<_>>()
        .join("\n");
    let new_block = format!("const LOCALES = [\n{locale_lines}\n  ];");

    // Replace the existing `const LOCALES = [...];` block
    let start = src
        .find("const LOCALES = [")
        .ok_or_else(|| anyhow::anyhow!("lang-switcher.js: LOCALES array not found"))?;
    let end = src[start..]
        .find("];")
        .ok_or_else(|| anyhow::anyhow!("lang-switcher.js: LOCALES closing ]; not found"))?;
    let updated = format!("{}{}{}", &src[..start], new_block, &src[start + end + 2..]);
    std::fs::write(&js_path, updated)?;
    Ok(())
}

pub fn print_locales() {
    let codes: Vec<String> = locale_entries().into_iter().map(|e| e.code).collect();
    println!("{}", codes.join(" "));
}

pub fn assemble(root: &std::path::Path) -> anyhow::Result<()> {
    println!("==> Assembling site (rustdoc + locale redirect)");
    let book = book_dir(root);
    let api_dest = book.join("book/api");
    let _ = std::fs::remove_dir_all(&api_dest);
    copy_dir_all(root.join("target/doc"), &api_dest)?;

    const INDEX_HTML: &str = "\
<!doctype html>
<meta charset=\"utf-8\">
<meta http-equiv=\"refresh\" content=\"0; url=./en/\">
<link rel=\"canonical\" href=\"./en/\">
<title>ZeroClaw Docs</title>
";
    std::fs::write(book.join("book/index.html"), INDEX_HTML)?;
    Ok(())
}
