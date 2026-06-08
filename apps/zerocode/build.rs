//! Generate the zerocode TUI theme preset table from the dashboard theme
//! registry (`web/src/contexts/themes.json`) — the single source of truth
//! shared with the React dashboard and the mdBook docs. The TUI mirrors it so
//! all three surfaces expose the same named themes without a second hardcoded
//! list.
//!
//! The web registry carries ~25 CSS custom properties per theme; the TUI needs
//! nine `Theme` roles. The mapping below is the documented bridge. Two roles
//! the `--pc-*` vars do not express (`warn`, `tool`) are taken from the theme's
//! `preview` swatch array, which every registry entry provides as
//! `[bg, accent, accent2, accent3, fg]`.
//!
//! Output: `$OUT_DIR/theme_presets.rs`, `include!`d by `src/theme.rs`. Never
//! committed — regenerated on every build so the registry cannot drift from the
//! compiled table.

use std::path::Path;

use serde_json::Value;

fn main() {
    let manifest = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    let registry = Path::new(&manifest).join("../../web/src/contexts/themes.json");

    println!("cargo:rerun-if-changed={}", registry.display());
    println!("cargo:rerun-if-changed=build.rs");

    let raw = std::fs::read_to_string(&registry)
        .unwrap_or_else(|e| panic!("read theme registry {}: {e}", registry.display()));
    let themes: Value =
        serde_json::from_str(&raw).unwrap_or_else(|e| panic!("parse {}: {e}", registry.display()));
    let arr = themes
        .as_array()
        .expect("themes.json top level is not an array");

    let mut out = String::from(
        "// GENERATED from web/src/contexts/themes.json by build.rs — DO NOT EDIT BY HAND.\n\n",
    );
    out.push_str("pub(crate) const GENERATED_THEMES: &[(&str, Theme)] = &[\n");

    for t in arr {
        let id = t
            .get("id")
            .and_then(Value::as_str)
            .expect("theme missing id");
        let name = snake_case(id);
        let vars = t
            .get("vars")
            .and_then(Value::as_object)
            .unwrap_or_else(|| panic!("theme {id} missing vars object"));
        let preview: Vec<&str> = t
            .get("preview")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();

        let var = |key: &str| -> String {
            let v = vars
                .get(key)
                .and_then(Value::as_str)
                .unwrap_or_else(|| panic!("theme {id} missing {key}"));
            rgb_literal(v).unwrap_or_else(|| panic!("theme {id} {key} = {v:?} is not #rrggbb"))
        };
        let swatch = |idx: usize, role: &str| -> String {
            let v = preview
                .get(idx)
                .unwrap_or_else(|| panic!("theme {id} preview missing index {idx} for {role}"));
            rgb_literal(v)
                .unwrap_or_else(|| panic!("theme {id} preview[{idx}] = {v:?} is not #rrggbb"))
        };

        let title = var("--pc-accent");
        let heading = var("--pc-accent-light");
        let body = var("--pc-text-primary");
        let dim = var("--pc-text-muted");
        let accent = var("--pc-accent");
        let warn = swatch(3, "warn");
        let selection_bg = var("--pc-bg-elevated");
        let tool = swatch(2, "tool");
        let background = var("--pc-bg-base");

        out.push_str(&format!(
            "    (\"{name}\", Theme {{ title: {title}, heading: {heading}, body: {body}, \
             dim: {dim}, accent: {accent}, warn: {warn}, selection_bg: {selection_bg}, \
             tool: {tool}, background: {background} }}),\n"
        ));
    }

    out.push_str("];\n");

    let dest = Path::new(&std::env::var("OUT_DIR").expect("OUT_DIR")).join("theme_presets.rs");
    std::fs::write(&dest, out).unwrap_or_else(|e| panic!("write {}: {e}", dest.display()));
}

/// Translate a kebab-case registry id to the snake_case the TUI uses
/// exclusively for theme names.
fn snake_case(id: &str) -> String {
    id.chars().map(|c| if c == '-' { '_' } else { c }).collect()
}

/// Convert a `#rrggbb` hex string to a `Color::Rgb(r, g, b)` literal. Returns
/// `None` for any value that is not a six-digit hex colour, so non-hex registry
/// values (rgba(), bare names) fail the build loudly rather than emit garbage.
fn rgb_literal(s: &str) -> Option<String> {
    let hex = s.strip_prefix('#')?;
    if hex.len() != 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
    let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
    let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
    Some(format!("Color::Rgb({r}, {g}, {b})"))
}
