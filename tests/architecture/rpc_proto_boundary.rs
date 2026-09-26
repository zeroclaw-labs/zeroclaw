//! `zeroclaw-rpc-proto` is the client-facing wire contract. Its value is
//! that a client can link it without linking the daemon, so its dependency
//! list is an allow-list: foundation crates and serde only. A runtime or
//! async-I/O edge here would silently turn every RPC client into a runtime
//! consumer.

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

const MANIFEST: &str = "crates/zeroclaw-rpc-proto/Cargo.toml";

/// Direct dependencies the crate may declare, normal and build.
const ALLOWED_DEPENDENCIES: &[&str] = &[
    "serde",
    "serde_json",
    "schemars",
    "zeroclaw-api",
    "zeroclaw-config",
    "zeroclaw-sop-graph",
];

fn manifest() -> toml::Table {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(MANIFEST);
    fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
        .parse()
        .expect("zeroclaw-rpc-proto Cargo.toml is valid TOML")
}

fn dependency_names(manifest: &toml::Table, table: &str) -> BTreeSet<String> {
    manifest
        .get(table)
        .and_then(|t| t.as_table())
        .map(|t| t.keys().cloned().collect())
        .unwrap_or_default()
}

#[test]
fn rpc_proto_depends_only_on_foundation_crates() {
    let manifest = manifest();
    let allowed: BTreeSet<&str> = ALLOWED_DEPENDENCIES.iter().copied().collect();
    for table in ["dependencies", "build-dependencies"] {
        for dep in dependency_names(&manifest, table) {
            assert!(
                allowed.contains(dep.as_str()),
                "zeroclaw-rpc-proto [{table}] declares `{dep}`, which is outside its allow-list. \
                 The crate must stay linkable by RPC clients without the runtime or async I/O; \
                 extend ALLOWED_DEPENDENCIES only with a foundation crate and say why in the PR."
            );
        }
    }
    assert!(
        manifest.get("target").is_none(),
        "zeroclaw-rpc-proto must not declare target-specific dependencies"
    );
}

#[test]
fn rpc_proto_is_publishable_alongside_the_runtime() {
    let manifest = manifest();
    let publish = manifest
        .get("package")
        .and_then(|p| p.get("publish"))
        .and_then(|v| v.as_bool());
    assert_ne!(
        publish,
        Some(false),
        "the runtime depends on zeroclaw-rpc-proto, so it must be publishable"
    );
}
