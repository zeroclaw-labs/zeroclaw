//! Keeps the literal tool names in the production tool sources in step with
//! `zeroclaw_tools::inventory::BUILTIN_TOOLS`.
//!
//! The scan reads every Rust file under `crates/zeroclaw-tools/src` and
//! `crates/zeroclaw-runtime/src/tools`, skipping `tests.rs` and `*_tests.rs`
//! files and cutting each file at its first inline module gated on a `cfg` that
//! names `test`. From what is left it collects the string literal returned by
//! each `fn name(&self) -> &str` that sits in an `impl ... Tool for ...` block,
//! and the literal of each `const NAME: &str` item. The `impl` check keeps the
//! literal names of observers and other non-tool traits out. A name read from a
//! field, as MCP wrappers, skill tools, and plugin tools do, is not a literal
//! and is not collected.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use regex::Regex;
use zeroclaw_tools::inventory::is_builtin_tool_name;

/// Source trees scanned, relative to the repository root.
const SOURCE_TREES: [&str; 2] = [
    "crates/zeroclaw-tools/src",
    "crates/zeroclaw-runtime/src/tools",
];

/// First-party tools with a literal name that stay out of the inventory, and why.
const OUTSIDE_INVENTORY: &[(&str, &str)] = &[
    (
        "sessions_reset",
        "implemented, but the agent registry does not register it",
    ),
    (
        "sessions_delete",
        "implemented, but the agent registry does not register it",
    ),
    (
        "skills_list",
        "only the opt-in background skill review registers it",
    ),
    (
        "skill_view",
        "only the opt-in background skill review registers it",
    ),
    (
        "skill_manage",
        "only the opt-in background skill review registers it",
    ),
    (
        "hardware_board_info",
        "a peripheral tool the hardware crate builds from the configured boards",
    ),
    (
        "hardware_memory_map",
        "a peripheral tool the hardware crate builds from the configured boards",
    ),
    (
        "hardware_memory_read",
        "a peripheral tool the hardware crate builds from the configured boards",
    ),
    (
        "vi_verify",
        "withheld from the model-visible registry until a chain verifier exists",
    ),
];

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn rust_files(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = std::fs::read_dir(&dir)
            .unwrap_or_else(|error| panic!("cannot read {}: {error}", dir.display()));
        for entry in entries {
            let path = entry.expect("directory entry is readable").path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                files.push(path);
            }
        }
    }
    files.sort();
    files
}

fn line_of(source: &str, offset: usize) -> usize {
    source[..offset].matches('\n').count() + 1
}

/// Every literal tool name in the production sources, with where it appears.
fn literal_tool_names() -> BTreeMap<String, Vec<String>> {
    let test_module = Regex::new(
        r#"(?m)^#\[cfg\([^\]\n"]*\btest\b[^\]\n]*\)\]\s*\n(?:[ \t]*#\[[^\n]*\]\s*\n)*[ \t]*(?:pub(?:\([^)\n]*\))?[ \t]+)?mod[ \t]+\w+[ \t]*\{"#,
    )
    .expect("test-module pattern compiles");
    let impl_header =
        Regex::new(r"(?m)^[ \t]*(?:unsafe[ \t]+)?impl\b[^{;]*\{").expect("impl pattern compiles");
    let tool_impl = Regex::new(r"\bTool\s+for\b").expect("tool-impl pattern compiles");
    let name_fn = Regex::new(
        r#"fn[ \t]+name[ \t]*\([ \t]*&[ \t]*self[ \t]*\)[ \t]*->[ \t]*&[ \t]*(?:'static[ \t]+)?str[ \t]*\{\s*"([^"\\]*)"\s*\}"#,
    )
    .expect("name-fn pattern compiles");
    let name_const = Regex::new(
        r#"const[ \t]+NAME[ \t]*:[ \t]*&[ \t]*(?:'static[ \t]+)?str[ \t]*=[ \t]*"([^"\\]*)"[ \t]*;"#,
    )
    .expect("name-const pattern compiles");

    let root = repo_root();
    let mut names: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for tree in SOURCE_TREES {
        for path in rust_files(&root.join(tree)) {
            let file_name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default();
            if file_name == "tests.rs" || file_name.ends_with("_tests.rs") {
                continue;
            }
            let relative = path
                .strip_prefix(&root)
                .expect("source file is under the repository root")
                .to_string_lossy()
                .replace('\\', "/");
            let source = std::fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("cannot read {relative}: {error}"));
            let production = test_module
                .find(&source)
                .map_or(source.as_str(), |module| &source[..module.start()]);

            let impls: Vec<(usize, bool)> = impl_header
                .find_iter(production)
                .map(|header| (header.start(), tool_impl.is_match(header.as_str())))
                .collect();
            let in_tool_impl = |offset: usize| {
                impls
                    .iter()
                    .rev()
                    .find(|(start, _)| *start < offset)
                    .is_some_and(|(_, is_tool)| *is_tool)
            };

            for found in name_fn.captures_iter(production) {
                let whole = found.get(0).expect("a match has a whole group");
                if in_tool_impl(whole.start()) {
                    names
                        .entry(found[1].to_string())
                        .or_default()
                        .push(format!("{relative}:{}", line_of(production, whole.start())));
                }
            }
            for found in name_const.captures_iter(production) {
                let whole = found.get(0).expect("a match has a whole group");
                names
                    .entry(found[1].to_string())
                    .or_default()
                    .push(format!("{relative}:{}", line_of(production, whole.start())));
            }
        }
    }
    names
}

#[test]
fn literal_tool_names_are_inventoried_or_excluded() {
    let unlisted: Vec<String> = literal_tool_names()
        .into_iter()
        .filter(|(name, _)| {
            !is_builtin_tool_name(name)
                && !OUTSIDE_INVENTORY
                    .iter()
                    .any(|(excluded, _)| excluded == name)
        })
        .map(|(name, locations)| format!("`{name}` at {}", locations.join(", ")))
        .collect();
    assert!(
        unlisted.is_empty(),
        "production tools with a literal name are missing from BUILTIN_TOOLS: {}. Add each one \
         to BUILTIN_TOOLS in crates/zeroclaw-tools/src/inventory.rs and to the tier tables in \
         docs/book/src/developing/tool-inventory.md, or to OUTSIDE_INVENTORY in this test with \
         the reason it stays out",
        unlisted.join("; ")
    );
}

#[test]
fn outside_inventory_entries_are_current() {
    let names = literal_tool_names();
    for (name, reason) in OUTSIDE_INVENTORY {
        assert!(
            !reason.trim().is_empty(),
            "`{name}` needs a reason in OUTSIDE_INVENTORY"
        );
        assert!(
            !is_builtin_tool_name(name),
            "`{name}` is now inventoried; drop it from OUTSIDE_INVENTORY"
        );
        assert!(
            names.contains_key(*name),
            "no production tool has the literal name `{name}` any more; drop it from \
             OUTSIDE_INVENTORY"
        );
    }
}

/// Guards the scan itself: each source tree must still yield a literal from both
/// patterns, or a moved directory or a pattern that stopped matching would make
/// the other tests pass on an empty scan.
#[test]
fn scan_reads_both_source_trees_and_patterns() {
    let names = literal_tool_names();
    for (name, tree) in [
        ("web_fetch", "crates/zeroclaw-tools/src/"),
        ("execute_pipeline", "crates/zeroclaw-tools/src/"),
        ("shell", "crates/zeroclaw-runtime/src/tools/"),
        ("delegate", "crates/zeroclaw-runtime/src/tools/"),
    ] {
        assert!(
            names
                .get(name)
                .is_some_and(|locations| locations.iter().any(|at| at.starts_with(tree))),
            "the scan no longer finds `{name}` under {tree}; its paths or patterns have drifted \
             from the sources"
        );
    }
}
