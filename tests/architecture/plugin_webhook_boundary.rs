//! Architecture gate: the gateway's plugin webhook route is an HTTP adapter.
//! It hands requests to the core's ingress, in process or over RPC, and must
//! not reach into the plugin host, the channel runtime, or the rest of the
//! runtime, because the route is headed for a gateway process that links
//! none of them.

use std::fs;
use std::path::PathBuf;

const MODULE_FILE: &str = "crates/zeroclaw-gateway/src/plugin_webhook.rs";
const MODULE_DIR: &str = "crates/zeroclaw-gateway/src/plugin_webhook";
const FORBIDDEN_CRATES: &[&str] = &["zeroclaw_plugins", "zeroclaw_channels", "zeroclaw_runtime"];

/// The module file and every `.rs` file under its directory, recursively.
fn module_sources() -> Vec<PathBuf> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let file = root.join(MODULE_FILE);
    assert!(
        file.is_file(),
        "{MODULE_FILE} is gone; point this gate at the gateway's plugin webhook route"
    );
    let mut nested = Vec::new();
    let mut pending = vec![root.join(MODULE_DIR)];
    while let Some(dir) = pending.pop() {
        let entries =
            fs::read_dir(&dir).unwrap_or_else(|error| panic!("read {}: {error}", dir.display()));
        for entry in entries {
            let path = entry
                .unwrap_or_else(|error| panic!("list {}: {error}", dir.display()))
                .path();
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                nested.push(path);
            }
        }
    }
    nested.sort();
    let mut sources = vec![file];
    sources.extend(nested);
    sources
}

/// Whether `line` names `crate_name` as a whole identifier.
fn names(line: &str, crate_name: &str) -> bool {
    let is_ident = |c: char| c.is_ascii_alphanumeric() || c == '_';
    line.match_indices(crate_name).any(|(start, _)| {
        let before = line[..start].chars().next_back();
        let after = line[start + crate_name.len()..].chars().next();
        !before.is_some_and(is_ident) && !after.is_some_and(is_ident)
    })
}

/// 1-based line numbers and text of every line that names a forbidden
/// crate. Comment lines are prose, not references.
fn forbidden_references(source: &str) -> Vec<(usize, String)> {
    source
        .lines()
        .enumerate()
        .filter(|(_, line)| !line.trim_start().starts_with("//"))
        .filter(|(_, line)| FORBIDDEN_CRATES.iter().any(|name| names(line, name)))
        .map(|(index, line)| (index + 1, line.trim().to_string()))
        .collect()
}

#[test]
fn gateway_plugin_webhook_module_names_no_plugin_host_or_channel_crate() {
    let sources = module_sources();
    assert!(
        sources.iter().any(|path| path.ends_with("forward.rs")),
        "the gate must cover the forwarder too: {sources:?}"
    );
    let mut hits = Vec::new();
    for path in &sources {
        let source = fs::read_to_string(path)
            .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
        for (line, text) in forbidden_references(&source) {
            hits.push(format!("{}:{line}: {text}", path.display()));
        }
    }
    assert!(
        hits.is_empty(),
        "the gateway's plugin webhook route must reach the core only through its ingress \
         (zeroclaw-api, zeroclaw-infra) or the RPC contract (zeroclaw-rpc-client, \
         zeroclaw-rpc-proto), never {FORBIDDEN_CRATES:?}:\n{}",
        hits.join("\n")
    );
}

#[test]
fn the_boundary_detector_flags_imports_and_ignores_comments() {
    let sample = "use zeroclaw_plugins::host::PluginHost;\n\
                  // zeroclaw_channels in prose\n\
                  let x = ::zeroclaw_channels::f();\n\
                  let y = zeroclaw_runtime_extra::g();\n\
                  let z = my_zeroclaw_runtime::h();\n\
                  use zeroclaw_runtime::rpc;\n";
    let lines: Vec<usize> = forbidden_references(sample)
        .into_iter()
        .map(|(line, _)| line)
        .collect();
    assert_eq!(lines, [1, 3, 6]);
}
