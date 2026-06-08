//! Generate the mdBook dashboard-theme CSS and switcher button list from the
//! web dashboard's theme registry (`web/src/contexts/themes.json`). That JSON
//! is the single source of truth shared with the React dashboard; the docs
//! mirror it so both surfaces expose the same named themes without a second
//! hardcoded list.
//!
//! Outputs (gitignored, derived):
//!   docs/book/theme/pc-themes.css       — one `html.<id>` block per theme
//!   docs/book/theme/pc-theme-list.html  — `<li>` switcher buttons (reference)

use crate::util::book_dir;
use anyhow::{Context, Result};
use serde_json::Value;
use std::path::Path;

const PC_VAR_KEYS: &[(&str, &str)] = &[
    ("--bg", "--pc-bg-base"),
    ("--fg", "--pc-text-primary"),
    ("--sidebar-bg", "--pc-bg-sidebar"),
    ("--sidebar-fg", "--pc-text-secondary"),
    ("--sidebar-non-existant", "--pc-text-faint"),
    ("--sidebar-active", "--pc-accent"),
    ("--sidebar-spacer", "--pc-separator"),
    ("--scrollbar", "--pc-scrollbar-thumb"),
    ("--icons", "--pc-text-muted"),
    ("--icons-hover", "--pc-accent"),
    ("--links", "--pc-accent-light"),
    ("--inline-code-color", "--pc-accent-light"),
    ("--theme-popup-bg", "--pc-bg-elevated"),
    ("--theme-popup-border", "--pc-border-strong"),
    ("--theme-hover", "--pc-hover-strong"),
    ("--quote-bg", "--pc-bg-surface"),
    ("--quote-border", "--pc-accent-dim"),
    ("--table-border-color", "--pc-border"),
    ("--table-header-bg", "--pc-bg-elevated"),
    ("--table-alternate-bg", "--pc-bg-surface"),
    ("--searchbar-border-color", "--pc-border-strong"),
    ("--searchbar-bg", "--pc-bg-input"),
    ("--searchbar-fg", "--pc-text-primary"),
    ("--searchbar-shadow-color", "--pc-accent-dim"),
    ("--searchresults-header-fg", "--pc-text-muted"),
    ("--searchresults-border-color", "--pc-border"),
    ("--searchresults-li-bg", "--pc-bg-elevated"),
    ("--search-mark-bg", "--pc-accent-dim"),
];

pub fn run(root: &Path) -> Result<()> {
    let themes_path = root.join("web/src/contexts/themes.json");
    let raw = std::fs::read_to_string(&themes_path)
        .with_context(|| format!("read theme registry at {}", themes_path.display()))?;
    let themes: Value = serde_json::from_str(&raw).context("parse web/src/contexts/themes.json")?;
    let arr = themes
        .as_array()
        .context("themes.json top level is not an array")?;

    let book = book_dir(root);
    let css = render_css(arr)?;
    let list = render_list(arr)?;
    std::fs::write(book.join("theme/pc-themes.css"), css)?;
    // Inject the switcher buttons into index.hbs between markers. They must be
    // present at parse time because mdBook's bundled book.js reads the theme
    // list synchronously on load; injecting later would abort its handler.
    // index.hbs is tracked but the marker region is regenerated from
    // themes.json, so themes.json stays the single source of truth.
    inject_theme_list(&book.join("theme/index.hbs"), &list)?;
    // Write the zerocode TUI theme-name list to a derived fragment that
    // themes.md `{{#include}}`s at build time. Gitignored, like cli.md/config.md
    // — the names stay sourced from the same registry the TUI build generates
    // its preset table from, and nothing generated lands in the committed doc.
    std::fs::write(
        root.join("docs/book/src/zerocode/zerocode-theme-list.md"),
        render_doc_theme_names(arr)?,
    )?;
    println!(
        "==> Generated pc-themes.css + injected {} theme buttons into index.hbs",
        arr.len()
    );
    Ok(())
}

const LIST_START: &str = "<!-- PC_THEME_LIST_START: generated from themes.json by `cargo xtask mdbook themes`; do not edit between markers -->";
const LIST_END: &str = "<!-- PC_THEME_LIST_END -->";

fn inject_theme_list(hbs_path: &Path, list: &str) -> Result<()> {
    let src = std::fs::read_to_string(hbs_path)
        .with_context(|| format!("read {}", hbs_path.display()))?;
    let start = src
        .find(LIST_START)
        .context("index.hbs missing PC_THEME_LIST_START marker")?;
    let end = src
        .find(LIST_END)
        .context("index.hbs missing PC_THEME_LIST_END marker")?;
    let after_start = start + LIST_START.len();
    let updated = format!(
        "{}\n{}\n                            {}",
        &src[..after_start],
        list.trim_end(),
        &src[end..],
    );
    std::fs::write(hbs_path, updated).with_context(|| format!("write {}", hbs_path.display()))?;
    Ok(())
}

/// Render the dark/light grouped backtick list of snake_case theme names the
/// TUI exposes. Mirrors `apps/zerocode/build.rs::snake_case`.
fn render_doc_theme_names(themes: &[Value]) -> Result<String> {
    let names = |want: &str| -> Result<Vec<String>> {
        themes
            .iter()
            .filter(|t| t.get("scheme").and_then(Value::as_str) == Some(want))
            .map(|t| {
                let id = t
                    .get("id")
                    .and_then(Value::as_str)
                    .context("theme missing id")?;
                Ok(format!("`{}`", id.replace('-', "_")))
            })
            .collect()
    };
    let dark = names("dark")?;
    let light = names("light")?;
    Ok(format!(
        "**Dark:** {}\n\n**Light:** {}",
        dark.join(", "),
        light.join(", "),
    ))
}

fn render_css(themes: &[Value]) -> Result<String> {
    let mut out = String::from(
        "/* GENERATED by `cargo xtask mdbook` from web/src/contexts/themes.json \
         — DO NOT EDIT BY HAND. */\n\n",
    );
    for t in themes {
        let id = t
            .get("id")
            .and_then(Value::as_str)
            .context("theme missing id")?;
        let vars = t
            .get("vars")
            .and_then(Value::as_object)
            .context("theme missing vars object")?;
        // Sanitize the registry-supplied id and values before they enter the
        // CSS file. The id forms a selector (`html.<id>`); values land in
        // declarations. Stripping CSS-structural characters prevents a hostile
        // theme from breaking out of the rule (defense in depth).
        let id = css_ident(id);
        out.push_str(&format!("html.{id} {{\n"));
        // Re-emit the raw --pc-* tokens so component rules resolve per theme.
        for (k, v) in vars {
            if let Some(s) = v.as_str()
                && is_css_custom_prop(k)
            {
                out.push_str(&format!("  {k}: {};\n", css_value(s)));
            }
        }
        // Bridge onto mdBook's own variables.
        for (mdbook_key, pc_key) in PC_VAR_KEYS {
            if let Some(val) = vars.get(*pc_key).and_then(Value::as_str) {
                out.push_str(&format!("  {mdbook_key}: {};\n", css_value(val)));
            }
        }
        out.push_str("}\n\n");
    }
    Ok(out)
}

/// CSS identifier (theme id used as a class selector): keep alphanumerics and
/// hyphens only, so it cannot terminate the selector or inject a rule.
fn css_ident(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect()
}

/// Accept only well-formed `--custom-property` names from the registry.
fn is_css_custom_prop(k: &str) -> bool {
    k.starts_with("--")
        && k.len() > 2
        && k[2..]
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// Strip CSS-structural characters from a declaration value so a value cannot
/// close the declaration/rule and inject further CSS.
fn css_value(s: &str) -> String {
    s.chars()
        .filter(|c| !matches!(c, ';' | '{' | '}'))
        .collect()
}

fn render_list(themes: &[Value]) -> Result<String> {
    let scheme_is = |t: &Value, want: &str| t.get("scheme").and_then(Value::as_str) == Some(want);
    let li = |t: &Value| -> Result<String> {
        let id = t
            .get("id")
            .and_then(Value::as_str)
            .context("theme missing id")?;
        let name = t.get("name").and_then(Value::as_str).unwrap_or(id);
        let preview: Vec<&str> = t
            .get("preview")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        // Escape all theme-registry values before they enter HTML/CSS. The
        // registry is maintainer-controlled, but escaping keeps a malformed or
        // hostile theme name/colour from breaking out of the attribute, text,
        // or style context (defense in depth).
        let id = html_attr_escape(id);
        let name = html_text_escape(name);
        let s0 = css_color(preview.first().copied().unwrap_or("#000"));
        let s1 = css_color(preview.get(1).copied().unwrap_or("#888"));
        let s2 = css_color(preview.get(2).copied().unwrap_or("#888"));
        let s3 = css_color(preview.get(3).copied().unwrap_or("#888"));
        let swatch = format!(
            "<span class=\"pc-theme-swatch\" aria-hidden=\"true\" \
             style=\"--s0:{s0};--s1:{s1};--s2:{s2};--s3:{s3}\"></span>"
        );
        Ok(format!(
            "                            <li role=\"none\" class=\"pc-theme-item\">\
             <button role=\"menuitem\" class=\"theme\" id=\"mdbook-theme-{id}\">\
             {swatch}<span class=\"pc-theme-name\">{name}</span></button></li>"
        ))
    };

    let mut out = String::from(
        "                            <li role=\"none\" class=\"pc-theme-group\">Dark</li>\n",
    );
    for t in themes.iter().filter(|t| scheme_is(t, "dark")) {
        out.push_str(&li(t)?);
        out.push('\n');
    }
    out.push_str(
        "                            <li role=\"none\" class=\"pc-theme-group\">Light</li>\n",
    );
    for t in themes.iter().filter(|t| scheme_is(t, "light")) {
        out.push_str(&li(t)?);
        out.push('\n');
    }
    Ok(out)
}

/// Escape text destined for HTML element content.
fn html_text_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Escape text destined for a double-quoted HTML attribute value.
fn html_attr_escape(s: &str) -> String {
    html_text_escape(s).replace('"', "&quot;")
}

/// Reduce a preview swatch value to a safe CSS colour token. Theme previews are
/// hex (`#rrggbb`) or simple identifiers; anything outside `[#0-9a-zA-Z]` is
/// dropped so the value cannot break out of the `style` attribute / declaration.
fn css_color(s: &str) -> String {
    let cleaned: String = s
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '#')
        .collect();
    if cleaned.is_empty() {
        "#888".to_string()
    } else {
        cleaned
    }
}
