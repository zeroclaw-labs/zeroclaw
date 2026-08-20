//! Generate the zerocode TUI theme preset table from the dashboard theme
//! registry (`web/src/contexts/themes.json`) — the single source of truth
//! shared with the React dashboard and the mdBook docs. The TUI mirrors it so
//! all three surfaces expose the same named themes without a second hardcoded

use std::path::Path;

use serde_json::Value;

fn main() -> Result<(), String> {
    let manifest = std::env::var("CARGO_MANIFEST_DIR")
        .map_err(|error| format!("CARGO_MANIFEST_DIR is unavailable: {error}"))?;
    let registry = Path::new(&manifest).join("../../web/src/contexts/themes.json");

    println!("cargo:rerun-if-changed={}", registry.display());
    println!("cargo:rerun-if-changed=build.rs");

    let raw = std::fs::read_to_string(&registry)
        .map_err(|error| format!("read theme registry {}: {error}", registry.display()))?;
    let themes: Value = serde_json::from_str(&raw)
        .map_err(|error| format!("parse {}: {error}", registry.display()))?;
    let arr = themes
        .as_array()
        .ok_or_else(|| "themes.json top level is not an array".to_string())?;

    let mut out = String::from(
        "// GENERATED from web/src/contexts/themes.json by build.rs — DO NOT EDIT BY HAND.\n\n",
    );
    out.push_str("pub(crate) const GENERATED_THEMES: &[(&str, Theme)] = &[\n");

    for t in arr {
        let id = t
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| "theme missing id".to_string())?;
        let name = snake_case(id);
        let vars = t
            .get("vars")
            .and_then(Value::as_object)
            .ok_or_else(|| format!("theme {id} missing vars object"))?;
        let preview: Vec<&str> = t
            .get("preview")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        let tui = t.get("tui").and_then(Value::as_object);

        let var = |key: &str| -> Result<String, String> {
            let v = vars
                .get(key)
                .and_then(Value::as_str)
                .ok_or_else(|| format!("theme {id} missing {key}"))?;
            rgb_literal(v).ok_or_else(|| format!("theme {id} {key} = {v:?} is not #rrggbb"))
        };
        let swatch = |idx: usize, role: &str| -> Result<String, String> {
            let v = preview
                .get(idx)
                .ok_or_else(|| format!("theme {id} preview missing index {idx} for {role}"))?;
            rgb_literal(v)
                .ok_or_else(|| format!("theme {id} preview[{idx}] = {v:?} is not #rrggbb"))
        };
        let title = role_literal(tui, id, "title", || var("--pc-accent"))?;
        let heading = role_literal(tui, id, "heading", || var("--pc-accent-light"))?;
        let body = role_literal(tui, id, "body", || var("--pc-text-primary"))?;
        let dim = role_literal(tui, id, "dim", || var("--pc-text-muted"))?;
        let accent = role_literal(tui, id, "accent", || var("--pc-accent"))?;
        let warn = role_literal(tui, id, "warn", || swatch(3, "warn"))?;
        let selection_bg = role_literal(tui, id, "selection_bg", || var("--pc-bg-elevated"))?;
        let tool = role_literal(tui, id, "tool", || swatch(2, "tool"))?;
        let background = role_literal(tui, id, "background", || var("--pc-bg-base"))?;

        out.push_str(&format!(
            "    (\"{name}\", Theme {{ title: {title}, heading: {heading}, body: {body}, \
             dim: {dim}, accent: {accent}, warn: {warn}, selection_bg: {selection_bg}, \
             tool: {tool}, background: {background} }}),\n"
        ));
    }

    out.push_str("];\n");

    let out_dir =
        std::env::var("OUT_DIR").map_err(|error| format!("OUT_DIR is unavailable: {error}"))?;
    let dest = Path::new(&out_dir).join("theme_presets.rs");
    std::fs::write(&dest, out).map_err(|error| format!("write {}: {error}", dest.display()))?;
    Ok(())
}

/// Translate a kebab-case registry id to the snake_case the TUI uses
/// exclusively for theme names.
fn snake_case(id: &str) -> String {
    id.chars().map(|c| if c == '-' { '_' } else { c }).collect()
}

fn role_literal<F>(
    tui: Option<&serde_json::Map<String, Value>>,
    id: &str,
    key: &str,
    fallback: F,
) -> Result<String, String>
where
    F: FnOnce() -> Result<String, String>,
{
    let Some(v) = tui.and_then(|roles| roles.get(key)).and_then(Value::as_str) else {
        return fallback();
    };
    rgb_literal(v).ok_or_else(|| format!("theme {id} tui.{key} = {v:?} is not #rrggbb"))
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
