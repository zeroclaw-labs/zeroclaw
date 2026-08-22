use std::{fmt::Write as _, path::Path};

use anyhow::{Context, Error, Result};
use serde_json::{Map, Value};

const REGISTRY: &str = "web/src/contexts/themes.json";

/// Render the checked-in ZeroCode theme table from the dashboard registry.
///
/// `themes.json` stays canonical. The generated Rust lives inside the
/// `zerocode` package so `cargo package` does not depend on files outside the
/// package directory; the existing installer drift gate checks the materialized
/// table on every PR.
pub fn render_file(root: &Path, _current: &str) -> Result<String> {
    render(
        &std::fs::read_to_string(root.join(REGISTRY))
            .with_context(|| format!("read canonical ZeroCode theme registry {REGISTRY}"))?,
    )
}

fn render(raw: &str) -> Result<String> {
    let themes: Value = serde_json::from_str(raw).context("parse themes.json")?;
    let themes = themes
        .as_array()
        .ok_or_else(|| Error::msg("themes.json top level must be an array"))?;

    let mut out = String::from(
        "// GENERATED from web/src/contexts/themes.json by `cargo generate installers`.\n\
         // Do not edit by hand; CI fails when this table drifts from the registry.\n\n\
         pub(crate) const GENERATED_THEMES: &[(&str, Theme)] = &[\n",
    );

    for theme in themes {
        let id = string(theme, "id", "theme")?;
        let name = snake_case(id);
        let vars = theme
            .get("vars")
            .and_then(Value::as_object)
            .ok_or_else(|| Error::msg(format!("theme {id} missing vars object")))?;
        let preview: Vec<&str> = theme
            .get("preview")
            .and_then(Value::as_array)
            .map(|values| values.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        let tui = theme.get("tui").and_then(Value::as_object);

        let var = |key: &str| -> Result<String> {
            let value = vars
                .get(key)
                .and_then(Value::as_str)
                .ok_or_else(|| Error::msg(format!("theme {id} missing {key}")))?;
            rgb_literal(value)
                .ok_or_else(|| Error::msg(format!("theme {id} {key} = {value:?} is not #rrggbb")))
        };
        let swatch = |index: usize, role: &str| -> Result<String> {
            let value = preview.get(index).ok_or_else(|| {
                Error::msg(format!(
                    "theme {id} preview missing index {index} for {role}"
                ))
            })?;
            rgb_literal(value).ok_or_else(|| {
                Error::msg(format!(
                    "theme {id} preview[{index}] = {value:?} is not #rrggbb"
                ))
            })
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

        writeln!(out, "    (")?;
        writeln!(out, "        {name:?},")?;
        writeln!(out, "        Theme {{")?;
        writeln!(out, "            title: {title},")?;
        writeln!(out, "            heading: {heading},")?;
        writeln!(out, "            body: {body},")?;
        writeln!(out, "            dim: {dim},")?;
        writeln!(out, "            accent: {accent},")?;
        writeln!(out, "            warn: {warn},")?;
        writeln!(out, "            selection_bg: {selection_bg},")?;
        writeln!(out, "            tool: {tool},")?;
        writeln!(out, "            background: {background},")?;
        writeln!(out, "        }},")?;
        writeln!(out, "    ),")?;
    }

    out.push_str("];\n");
    Ok(out)
}

fn string<'a>(value: &'a Value, key: &str, subject: &str) -> Result<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| Error::msg(format!("{subject} missing string field {key}")))
}

fn snake_case(id: &str) -> String {
    id.chars()
        .map(|character| if character == '-' { '_' } else { character })
        .collect()
}

fn role_literal<F>(
    tui: Option<&Map<String, Value>>,
    id: &str,
    key: &str,
    fallback: F,
) -> Result<String>
where
    F: FnOnce() -> Result<String>,
{
    let Some(value) = tui.and_then(|roles| roles.get(key)).and_then(Value::as_str) else {
        return fallback();
    };
    rgb_literal(value)
        .ok_or_else(|| Error::msg(format!("theme {id} tui.{key} = {value:?} is not #rrggbb")))
}

fn rgb_literal(value: &str) -> Option<String> {
    let hex = value.strip_prefix('#')?;
    if hex.len() != 6 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let red = u8::from_str_radix(&hex[0..2], 16).ok()?;
    let green = u8::from_str_radix(&hex[2..4], 16).ok()?;
    let blue = u8::from_str_radix(&hex[4..6], 16).ok()?;
    Some(format!("Color::Rgb({red}, {green}, {blue})"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const THEME: &str = r##"[
      {
        "id": "icy-blue",
        "preview": ["#010203", "#040506", "#070809", "#0a0b0c"],
        "vars": {
          "--pc-accent": "#101112",
          "--pc-accent-light": "#131415",
          "--pc-text-primary": "#161718",
          "--pc-text-muted": "#191a1b",
          "--pc-bg-elevated": "#1c1d1e",
          "--pc-bg-base": "#1f2021"
        }
      }
    ]"##;

    #[test]
    fn renders_snake_case_name_and_fallback_roles() {
        let rendered = render(THEME).unwrap();
        assert!(rendered.contains("\"icy_blue\""));
        assert!(rendered.contains("title: Color::Rgb(16, 17, 18)"));
        assert!(rendered.contains("warn: Color::Rgb(10, 11, 12)"));
        assert!(rendered.contains("tool: Color::Rgb(7, 8, 9)"));
    }

    #[test]
    fn explicit_tui_role_overrides_the_fallback() {
        let raw = THEME.replace(
            "\n      }\n    ]",
            ",\n        \"tui\": { \"accent\": \"#aabbcc\" }\n      }\n    ]",
        );
        let rendered = render(&raw).unwrap();
        assert!(rendered.contains("accent: Color::Rgb(170, 187, 204)"));
    }

    #[test]
    fn rejects_non_hex_tui_roles() {
        let raw = THEME.replace(
            "\n      }\n    ]",
            ",\n        \"tui\": { \"accent\": \"rgba(1,2,3,0.5)\" }\n      }\n    ]",
        );
        let error = render(&raw).unwrap_err().to_string();
        assert!(error.contains("tui.accent"));
        assert!(error.contains("not #rrggbb"));
    }

    #[test]
    fn rejects_a_missing_required_variable() {
        let mut value: Value = serde_json::from_str(THEME).unwrap();
        value[0]["vars"]
            .as_object_mut()
            .unwrap()
            .remove("--pc-bg-base");
        let error = render(&serde_json::to_string(&value).unwrap())
            .unwrap_err()
            .to_string();
        assert!(error.contains("missing --pc-bg-base"));
    }

    #[test]
    fn rgb_literal_rejects_the_wrong_shape() {
        assert_eq!(
            rgb_literal("#abcdef"),
            Some("Color::Rgb(171, 205, 239)".into())
        );
        assert_eq!(rgb_literal("#abcd"), None);
        assert_eq!(rgb_literal("blue"), None);
    }
}
