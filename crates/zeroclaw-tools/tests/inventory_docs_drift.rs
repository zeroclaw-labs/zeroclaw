//! Keeps the tier tables of the built-in tool inventory docs page in step with
//! `zeroclaw_tools::inventory::BUILTIN_TOOLS`.
//!
//! The tool names of a tier are the backticked tokens in the `Tool` or `Tools`
//! column of its table's body rows; the other columns are prose. A failure
//! names the tools on each side so the editor can tell whether the docs page or
//! the inventory is the side to fix.

use std::collections::BTreeSet;
use std::path::Path;

use zeroclaw_tools::inventory::{ToolTier, builtin_tools_in};

/// Paths relative to the repository root, two levels above this crate.
const DOCS_PAGE: &str = "docs/book/src/developing/tool-inventory.md";
const INVENTORY_SOURCE: &str = "crates/zeroclaw-tools/src/inventory.rs";
const TIERS_SECTION: &str = "## Tiers and the retained core set";
const TIER_TABLES: [(&str, ToolTier); 3] = [
    ("### Tier 1: core", ToolTier::Core),
    ("### Tier 2: host-coupled", ToolTier::Host),
    ("### Tier 3: optional", ToolTier::Optional),
];

fn docs_page() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(DOCS_PAGE);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()))
}

/// The lines of the tiers section, up to the next `##` heading.
fn tiers_section(page: &str) -> Vec<&str> {
    let mut lines = page.lines().map(str::trim_end);
    assert!(
        lines.by_ref().any(|line| line == TIERS_SECTION),
        "{DOCS_PAGE} has no `{TIERS_SECTION}` heading"
    );
    lines.take_while(|line| !line.starts_with("## ")).collect()
}

/// The lines under `heading` inside the tiers section, up to the next `##` or
/// `###` heading.
fn subsection<'a>(section: &[&'a str], heading: &str) -> Vec<&'a str> {
    let start = section
        .iter()
        .position(|line| *line == heading)
        .unwrap_or_else(|| panic!("the tiers section of {DOCS_PAGE} has no `{heading}` heading"));
    section[start + 1..]
        .iter()
        .copied()
        .take_while(|line| !line.starts_with("## ") && !line.starts_with("### "))
        .collect()
}

/// The trimmed cells of a Markdown table row.
fn cells(row: &str) -> Vec<&str> {
    let inner = row.strip_prefix('|').unwrap_or(row);
    let inner = inner.strip_suffix('|').unwrap_or(inner);
    inner.split('|').map(str::trim).collect()
}

fn is_separator_row(row: &str) -> bool {
    row.contains('-') && row.chars().all(|c| matches!(c, '|' | '-' | ':' | ' '))
}

/// The backticked tokens in the `Tool` or `Tools` column of the body rows of
/// the Markdown tables in `lines`.
fn documented_names(lines: &[&str], heading: &str) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    let mut row_in_table = 0_usize;
    let mut tool_column = 0_usize;
    let mut width = 0_usize;
    for line in lines {
        let row = line.trim();
        if !row.starts_with('|') {
            row_in_table = 0;
            continue;
        }
        row_in_table += 1;
        let row_cells = cells(row);
        if row_in_table == 1 {
            tool_column = row_cells
                .iter()
                .position(|cell| *cell == "Tool" || *cell == "Tools")
                .unwrap_or_else(|| {
                    panic!("the table under `{heading}` has no `Tool` or `Tools` column: {row}")
                });
            width = row_cells.len();
            continue;
        }
        if row_in_table == 2 {
            assert!(
                is_separator_row(row),
                "the table under `{heading}` has no `|---|` separator after its header: {row}"
            );
            continue;
        }
        assert_eq!(
            row_cells.len(),
            width,
            "a row under `{heading}` does not have the header's {width} cells: {row}"
        );
        let spans: Vec<&str> = row_cells[tool_column].split('`').collect();
        assert!(
            spans.len() % 2 == 1,
            "a row under `{heading}` has an unbalanced backtick in its tool column: {row}"
        );
        names.extend(spans.iter().skip(1).step_by(2).map(|name| name.to_string()));
    }
    assert!(
        !names.is_empty(),
        "no tool names found in the table under `{heading}`"
    );
    names
}

#[test]
fn docs_tier_tables_match_the_inventory() {
    let page = docs_page();
    let section = tiers_section(&page);
    let mut problems = Vec::new();
    for (heading, tier) in TIER_TABLES {
        let documented = documented_names(&subsection(&section, heading), heading);
        let inventoried: BTreeSet<String> = builtin_tools_in(tier)
            .map(|spec| spec.name.to_string())
            .collect();
        let only_in_docs: Vec<&String> = documented.difference(&inventoried).collect();
        let only_in_inventory: Vec<&String> = inventoried.difference(&documented).collect();
        if !only_in_docs.is_empty() {
            problems.push(format!(
                "`{heading}` lists tools that are not {tier:?} rows in the inventory: {only_in_docs:?}"
            ));
        }
        if !only_in_inventory.is_empty() {
            problems.push(format!(
                "{tier:?} rows in the inventory that `{heading}` does not list: {only_in_inventory:?}"
            ));
        }
    }
    assert!(
        problems.is_empty(),
        "the tier tables in {DOCS_PAGE} and BUILTIN_TOOLS in {INVENTORY_SOURCE} disagree; \
         fix whichever side is wrong:\n{}",
        problems.join("\n")
    );
}
