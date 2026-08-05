//! Architecture gate: forbid duplicate-state patterns across the codebase.

use std::fs;
use std::path::Path;

/// Field-name substrings that indicate a peer-authorization cache. These
/// concepts ALL live in `Config::peer_groups` in V3; channel handles
/// must NOT store a local copy.
const FORBIDDEN_FIELD_NAMES: &[&str] = &[
    "allowed_users",
    "allowed_contacts",
    "allowed_from",
    "allowed_numbers",
    "allowed_senders",
    "allowed_pubkeys",
    "peer_group_members",
];

/// Type signatures that combined with the field name indicate duplicate
/// state. We match `Vec<String>` literally (the most common form) plus a
/// couple of common containers.
const FORBIDDEN_TYPE_SUBSTRINGS: &[&str] = &[
    "Vec<String>",
    "Vec < String >",
    "HashSet<String>",
    "BTreeSet<String>",
];

/// Roots to scan. Channels are the hottest drift surface; we lint there
/// first. Extending the scan is one entry away.
const SCAN_ROOTS: &[&str] = &["crates/zeroclaw-channels/src"];

/// Files / paths that hold the canonical sources of truth and are
/// therefore allowed to declare these fields. Anything outside these
/// paths is treated as a potential cache.
const ALLOWED_PATHS: &[&str] = &[
    // The migration walker is allowed to read V2 field names by name —
    // those are inbound TOML keys, not struct fields. It deals in raw
    // toml::Value, not typed channel structs.
    "schema/v2.rs",
    // Peer-group external_peers/agents lists are the canonical SOT.
    "multi_agent.rs",
    // The shared allow-list helper takes the list as a parameter, not as
    // a struct field — function signatures are not state.
    "allowlist.rs",
];

#[test]
fn no_channel_handle_caches_peer_authorization_state() {
    let workspace_root = workspace_root();
    let mut violations: Vec<String> = Vec::new();
    for root in SCAN_ROOTS {
        let root_path = workspace_root.join(root);
        scan_dir(&root_path, &mut violations);
    }
    assert!(
        violations.is_empty(),
        "Duplicate peer-authorization state detected. \
         These fields cache data that lives in Config::peer_groups; \
         channel handles must resolve authorization from Config at \
         message-time (closure, &Config param, etc.) — see AGENTS.md \
         'ABSOLUTE RULE — SINGLE SOURCE OF TRUTH'. \
         To override, add `// SOT: <reason>` on the offending line.\n\n\
         Violations:\n{}",
        violations.join("\n")
    );
}

fn workspace_root() -> std::path::PathBuf {
    // `CARGO_MANIFEST_DIR` for the workspace's top-level crate (the
    // `zeroclawlabs` binary) — that's where `cargo test` invokes from.
    let here = Path::new(env!("CARGO_MANIFEST_DIR"));
    here.to_path_buf()
}

fn scan_dir(dir: &Path, violations: &mut Vec<String>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            scan_dir(&path, violations);
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        let display = path.display().to_string();
        // Skip canonical-SOT files entirely.
        if ALLOWED_PATHS
            .iter()
            .any(|allowed| display.contains(allowed))
        {
            continue;
        }
        let Ok(src) = fs::read_to_string(&path) else {
            continue;
        };
        for (lineno, line) in src.lines().enumerate() {
            // Cheap heuristic: a field declaration mentions the forbidden
            // name + a forbidden type on the same line and is missing the
            // `SOT:` escape hatch. We strip leading whitespace + check.
            let trimmed = line.trim_start();
            if !trimmed.contains(':') {
                continue;
            }
            if line.contains("// SOT:") {
                continue;
            }
            let has_bad_name = FORBIDDEN_FIELD_NAMES
                .iter()
                .any(|n| line.contains(&format!("{n}:")) || line.contains(&format!("{n} :")));
            if !has_bad_name {
                continue;
            }
            let has_bad_type = FORBIDDEN_TYPE_SUBSTRINGS.iter().any(|t| line.contains(t));
            if !has_bad_type {
                continue;
            }
            violations.push(format!("  {}:{}: {}", display, lineno + 1, trimmed));
        }
    }
}
