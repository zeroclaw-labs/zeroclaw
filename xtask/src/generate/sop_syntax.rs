//! Render the source-owned SOP syntax reference and condition operator list.
//!
//! `docs/book/src/sop/syntax.md` intentionally keeps its examples and
//! explanation hand-authored. The two lists that mirror runtime behavior are
//! sentinel-delimited so they can be regenerated and checked without making
//! the surrounding prose generated output.

use anyhow::{Context, ensure};
use std::path::PathBuf;
use zeroclaw_runtime::sop::{SOP_STEP_SYNTAX_CATALOG, condition::ConditionOp};

const SYNTAX_FILE: &str = "docs/book/src/sop/syntax.md";
const PARSER_ZONE: &str = "sop-parser-behavior";
const OPERATORS_ZONE: &str = "sop-condition-operators";

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn marker(zone: &str, end: bool) -> String {
    if end {
        format!("<!-- >>> end generated:{zone} <<< -->")
    } else {
        format!("<!-- >>> generated:{zone} by `cargo generate sop-syntax` - do not edit <<< -->")
    }
}

fn splice_markdown(current: &str, zone: &str, body: &str) -> anyhow::Result<String> {
    let begin = marker(zone, false);
    let end = marker(zone, true);
    ensure!(
        current.match_indices(&begin).count() == 1 && current.match_indices(&end).count() == 1,
        "{zone} must contain exactly one generated sentinel pair"
    );

    let begin_at = current
        .find(&begin)
        .with_context(|| format!("missing {zone} begin sentinel"))?;
    let after_begin = begin_at + begin.len();
    let end_at = current
        .find(&end)
        .with_context(|| format!("missing {zone} end sentinel"))?;
    ensure!(after_begin < end_at, "{zone} sentinels are out of order");

    let mut rendered = String::new();
    rendered.push_str(&current[..after_begin]);
    rendered.push('\n');
    rendered.push_str(body.trim_end());
    rendered.push('\n');
    rendered.push_str(&current[end_at..]);
    Ok(rendered)
}

fn render_parser_behavior() -> String {
    SOP_STEP_SYNTAX_CATALOG
        .iter()
        .map(|spec| format!("- {}\n", spec.description))
        .collect()
}

fn render_condition_operators() -> String {
    ConditionOp::catalog()
        .iter()
        .map(|spec| format!("- `{}`: {}\n", spec.token, spec.label))
        .collect()
}

fn render_file(current: &str) -> anyhow::Result<String> {
    let rendered = splice_markdown(current, PARSER_ZONE, &render_parser_behavior())?;
    splice_markdown(&rendered, OPERATORS_ZONE, &render_condition_operators())
}

/// Regenerate the source-backed regions, or fail when `check` finds drift.
pub fn run(check: bool) -> anyhow::Result<()> {
    let root = workspace_root();
    let path = root.join(SYNTAX_FILE);
    let current = std::fs::read_to_string(&path)
        .map_err(|e| anyhow::Error::msg(format!("{}: {e}", path.display())))?;
    let rendered = render_file(&current)?;

    if check {
        if current != rendered {
            anyhow::bail!("SOP syntax reference is out of sync; run `cargo generate sop-syntax`");
        }
        println!("ok: SOP syntax reference in sync");
    } else if current != rendered {
        std::fs::write(&path, rendered)?;
        println!("wrote {}", path.display());
    } else {
        println!("unchanged {}", path.display());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> String {
        format!(
            "before\n{}\n{}\nafter\n{}\n{}\nend\n",
            marker(PARSER_ZONE, false),
            marker(PARSER_ZONE, true),
            marker(OPERATORS_ZONE, false),
            marker(OPERATORS_ZONE, true),
        )
    }

    #[test]
    fn renders_every_source_owned_parser_entry() {
        let rendered = render_file(&fixture()).expect("render");
        for spec in SOP_STEP_SYNTAX_CATALOG {
            assert!(
                rendered.contains(spec.description),
                "missing {}",
                spec.description
            );
        }
    }

    #[test]
    fn renders_every_catalog_operator() {
        let rendered = render_file(&fixture()).expect("render");
        for spec in ConditionOp::catalog() {
            assert!(
                rendered.contains(&format!("- `{}`: {}", spec.token, spec.label)),
                "missing {}",
                spec.token
            );
        }
    }

    #[test]
    fn duplicate_sentinels_fail_closed() {
        let duplicate = format!("{}\n{}", fixture(), marker(PARSER_ZONE, false));
        let error = render_file(&duplicate).expect_err("duplicate sentinel must fail");
        assert!(error.to_string().contains(PARSER_ZONE));
    }
}
