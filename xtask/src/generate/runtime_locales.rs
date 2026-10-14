//! Render the runtime locale table from the canonical repo-root `locales.toml`.
//!
//! `locales.toml` stays the single place a locale is added. The runtime used to
//! embed it with `include_str!("../../../locales.toml")`, which reaches outside
//! the crate directory. `cargo package` copies only the package directory, so
//! that read makes `zeroclaw-runtime` unpublishable. Generating a committed
//! table inside the crate keeps the registry canonical while putting the data
//! where packaging can see it, the same trade the installer surfaces make.

use std::path::Path;

const HEADER: &str = "\
// GENERATED from locales.toml by `cargo generate installers` - do not edit by hand.
//
// `locales.toml` at the repo root stays the single source of truth for locale
// codes and labels. Regenerate with `cargo generate installers runtime-locales`;
// CI fails on drift via `cargo generate installers --check`.

/// One selectable locale: its `code` (e.g. `ja`) and display `label`
/// (e.g. \u{65e5}\u{672c}\u{8a9e}).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocaleOption {
    pub code: &'static str,
    pub label: &'static str,
}

/// Locales this build knows about, in `locales.toml` order. The first entry is
/// the primary locale.
pub const AVAILABLE_LOCALES: &[LocaleOption] = &[
";

/// Parse `locales.toml` and render the generated Rust table.
pub fn render_file(root: &Path, _current: &str) -> anyhow::Result<String> {
    let path = root.join("locales.toml");
    let raw = std::fs::read_to_string(&path)
        .map_err(|e| anyhow::Error::msg(format!("{}: {e}", path.display())))?;
    render(&raw)
}

fn render(raw: &str) -> anyhow::Result<String> {
    let table: toml::Value = toml::from_str(raw)?;
    let entries = table
        .get("locale")
        .and_then(|v| v.as_array())
        .ok_or_else(|| anyhow::Error::msg("locales.toml has no [[locale]] array"))?;

    let mut out = String::from(HEADER);
    for entry in entries {
        let code = entry
            .get("code")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::Error::msg("a [[locale]] entry is missing `code`"))?;
        let label = entry
            .get("label")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::Error::msg(format!("locale `{code}` is missing `label`")))?;
        // Emit the shape rustfmt produces, so the committed file is
        // fmt-stable and `cargo fmt --check` does not fight the generator.
        out.push_str(&format!(
            "    LocaleOption {{\n        code: {},\n        label: {},\n    }},\n",
            quote(code),
            quote(label)
        ));
    }
    out.push_str("];\n");
    Ok(out)
}

/// Emit a Rust string literal. Labels carry non-ASCII text, so escape only what
/// Rust source requires rather than rewriting the label.
fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            _ => out.push(ch),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_codes_and_labels_in_order() {
        let out = render(
            r#"
[[locale]]
code = "en"
label = "English"

[[locale]]
code = "ja"
label = "日本語"
"#,
        )
        .expect("render");
        assert!(out.contains(r#"code: "en","#) && out.contains(r#"label: "English","#));
        assert!(out.contains(r#"code: "ja","#) && out.contains(r#"label: "日本語","#));
        let en = out.find(r#"code: "en""#).expect("en present");
        let ja = out.find(r#"code: "ja""#).expect("ja present");
        assert!(en < ja, "locales.toml order must be preserved");
    }

    #[test]
    fn a_label_with_a_quote_stays_valid_rust() {
        let out = render(
            r#"
[[locale]]
code = "xx"
label = "a \"quoted\" label"
"#,
        )
        .expect("render");
        assert!(out.contains(r#"label: "a \"quoted\" label","#));
    }

    #[test]
    fn a_locale_missing_its_label_is_an_error() {
        let err = render(
            r#"
[[locale]]
code = "xx"
"#,
        )
        .expect_err("missing label must fail");
        assert!(err.to_string().contains("xx"), "error names the locale");
    }
}
