//! Regression guard for the `main.rs` / `lib.rs` dual-compilation layout.
//!
//! # Why this exists
//!
//! This crate's source tree is unusual: both `src/main.rs` and `src/lib.rs`
//! declare the same set of shared modules (`gateway`, `channels`, `agent`,
//! etc.) and therefore Rust compiles each shared module's source TWICE —
//! once rooted at the lib crate and once rooted at the bin crate. Inside
//! shared modules, `crate::X::Y` resolves relative to *whichever crate
//! root is compiling the code*. A module that lives only in `lib.rs` is
//! invisible when the same file is re-compiled for the bin, which manifests
//! as a cryptic E0433 "unresolved import".
//!
//! We hit this twice already (2026-04-17 `local_llm` + 2026-04-18
//! `host_probe`). Both times `cargo test --lib` stayed green even while
//! `cargo check --all-targets` was broken on the bin target.
//!
//! This test parses `src/main.rs` and `src/lib.rs`, extracts their module
//! declarations, then scans every shared module's source for
//! `crate::<module>::` references. The assertion: any `<module>` named
//! that way must appear in both crate roots' module lists. A lib-only
//! module referenced from a shared file fails this test *before* CI runs
//! a build, giving an immediate, human-readable error instead of
//! `error[E0433]: failed to resolve: unresolved import`.
//!
//! The test intentionally does not require main.rs to mirror every
//! lib.rs module — only those actually reachable via a `crate::` path
//! from shared source files. That lets truly bin-internal modules
//! (e.g. `auth`, `cost`, `cron`) stay in whichever crate root owns them
//! without triggering noise.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Parse `mod X;`, `pub mod X;`, and `pub(crate) mod X;` declarations from a file.
fn extract_module_names(path: &Path) -> BTreeSet<String> {
    let src = fs::read_to_string(path).unwrap_or_else(|e| panic!("read {path:?}: {e}"));
    let mut out = BTreeSet::new();
    for line in src.lines() {
        let trimmed = line.trim_start();
        // Strip visibility prefixes.
        let rest = if let Some(r) = trimmed.strip_prefix("pub(crate) mod ") {
            r
        } else if let Some(r) = trimmed.strip_prefix("pub mod ") {
            r
        } else if let Some(r) = trimmed.strip_prefix("mod ") {
            r
        } else {
            continue;
        };
        // Accept `foo;` or `foo { ... }` but skip path-style `mod foo = ...`.
        let name_end = rest
            .find(|c: char| c == ';' || c == '{' || c.is_whitespace())
            .unwrap_or(rest.len());
        let name = rest[..name_end].trim();
        // Skip inline modules (`mod foo { ... }`) — they're not file-backed
        // top-level declarations, so symmetry doesn't apply.
        if !rest.trim_start().starts_with(|c: char| c == ';' || c == '{' || c.is_alphanumeric() || c == '_') {
            continue;
        }
        if name.is_empty() || name.contains(char::is_whitespace) {
            continue;
        }
        // We only care about file-backed top-level modules. Check that the
        // declaration closes with a `;` on the same line — inline modules
        // (`mod foo { ... }`) aren't subject to symmetry.
        if !line.contains(';') {
            continue;
        }
        out.insert(name.to_string());
    }
    out
}

/// Recursively collect all `.rs` files under a directory (depth-first).
fn rust_files_in(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if !dir.is_dir() {
        return out;
    }
    for entry in fs::read_dir(dir).unwrap_or_else(|e| panic!("read_dir {dir:?}: {e}")) {
        let entry = entry.expect("dir entry");
        let path = entry.path();
        if path.is_dir() {
            out.extend(rust_files_in(&path));
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(path);
        }
    }
    out
}

/// Extract module names used via `crate::<name>::` paths in a file.
///
/// Only catches simple top-level segments — enough to detect the class of
/// bug we care about (lib-only module referenced from a shared file).
fn extract_crate_references(path: &Path) -> BTreeSet<String> {
    let Ok(src) = fs::read_to_string(path) else {
        return BTreeSet::new();
    };
    let mut out = BTreeSet::new();
    // Slide a cursor through the file looking for `crate::`.
    let bytes = src.as_bytes();
    let needle = b"crate::";
    let mut i = 0;
    while i + needle.len() <= bytes.len() {
        if &bytes[i..i + needle.len()] == needle {
            let start = i + needle.len();
            let mut end = start;
            while end < bytes.len() {
                let c = bytes[end] as char;
                if c.is_alphanumeric() || c == '_' {
                    end += 1;
                } else {
                    break;
                }
            }
            if end > start {
                // Avoid catching `crate::` in string literals / comments by
                // requiring the segment to be followed by `::` (a module-like
                // continuation) — trailing `crate::FOO` used as a bare const
                // import is rare here and doesn't trigger the bug class.
                if end + 2 <= bytes.len() && &bytes[end..end + 2] == b"::" {
                    out.insert(
                        std::str::from_utf8(&bytes[start..end])
                            .expect("ascii segment")
                            .to_string(),
                    );
                }
            }
            i = end;
        } else {
            i += 1;
        }
    }
    out
}

#[test]
fn shared_modules_only_reference_mirrored_modules() {
    let root = repo_root();
    let main_modules = extract_module_names(&root.join("src/main.rs"));
    let lib_modules = extract_module_names(&root.join("src/lib.rs"));

    assert!(
        !main_modules.is_empty(),
        "failed to parse main.rs module list"
    );
    assert!(
        !lib_modules.is_empty(),
        "failed to parse lib.rs module list"
    );

    // A "shared" module is one declared in BOTH crate roots. Its source
    // is compiled twice, once per crate — so `crate::X::…` inside it
    // must resolve in both contexts.
    let shared: BTreeSet<&String> = main_modules.intersection(&lib_modules).collect();

    // Gather all `crate::<X>::` references from every shared module's
    // source tree (src/<X>/**/*.rs, plus the top-level file if present).
    let mut referenced: BTreeSet<String> = BTreeSet::new();
    for module in &shared {
        let module_dir = root.join("src").join(module.as_str());
        for file in rust_files_in(&module_dir) {
            for name in extract_crate_references(&file) {
                referenced.insert(name);
            }
        }
        let single_file = root.join("src").join(format!("{module}.rs"));
        if single_file.exists() {
            for name in extract_crate_references(&single_file) {
                referenced.insert(name);
            }
        }
    }

    // Cross-reference against the two crate roots. Any module that
    // shared code dereferences through `crate::X::` must exist in both
    // crate roots — otherwise the bin target fails to compile with
    // E0433 while `cargo check --lib` stays green.
    let mut asymmetric_refs: Vec<String> = Vec::new();
    for name in &referenced {
        let in_main = main_modules.contains(name);
        let in_lib = lib_modules.contains(name);
        // A reference to a module that doesn't exist in either root is
        // probably a top-level item (`crate::util::HOME_DIR`, say) that
        // is defined directly in lib.rs / main.rs as a function or const.
        // We only flag the cases where the module exists in ONE root but
        // not the other.
        if in_main ^ in_lib {
            asymmetric_refs.push(format!(
                "  * `crate::{name}::…` — present in {}, missing from {}",
                if in_lib { "lib.rs" } else { "main.rs" },
                if in_lib { "main.rs" } else { "lib.rs" },
            ));
        }
    }

    assert!(
        asymmetric_refs.is_empty(),
        "\n\
         Dual-compile symmetry violation: the modules listed below are\n\
         referenced via `crate::<module>::…` from code shared by main.rs\n\
         and lib.rs, but are declared in only one of the two crate roots.\n\
         This class of mismatch breaks `cargo check --bin zeroclaw` while\n\
         leaving `cargo check --lib` green — exactly the regression this\n\
         test guards against.\n\n\
         Fix: mirror each listed module in the *other* crate root.\n\
         See the `Dual-compile symmetry block` in `src/main.rs` for the\n\
         canonical pattern (with `#[allow(unused_imports)]` to silence\n\
         re-export noise).\n\n\
         {}\n",
        asymmetric_refs.join("\n"),
    );
}

#[test]
fn lib_and_main_both_declare_at_least_one_module() {
    // Cheap sanity check — if our parser breaks, both sets would go empty
    // and the real symmetry test above would silently pass.
    let root = repo_root();
    let main = extract_module_names(&root.join("src/main.rs"));
    let lib = extract_module_names(&root.join("src/lib.rs"));
    assert!(main.len() > 10, "main.rs parse returned {} modules", main.len());
    assert!(lib.len() > 10, "lib.rs parse returned {} modules", lib.len());
}
