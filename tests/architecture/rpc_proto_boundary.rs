//! `zeroclaw-rpc-proto` is the client-facing wire contract and
//! `zeroclaw-rpc-client` is the client built on it. Their value is that a
//! client can link them without linking the daemon, so each dependency list
//! is an allow-list. A runtime edge in either would silently turn every RPC
//! client into a runtime consumer, and a client edge in the runtime would
//! make the server link its own client.

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

const MANIFEST: &str = "crates/zeroclaw-rpc-proto/Cargo.toml";
const CLIENT_MANIFEST: &str = "crates/zeroclaw-rpc-client/Cargo.toml";
const RUNTIME_MANIFEST: &str = "crates/zeroclaw-runtime/Cargo.toml";

/// Direct dependencies the crate may declare, normal and build.
const ALLOWED_DEPENDENCIES: &[&str] = &[
    "serde",
    "serde_json",
    "schemars",
    "zeroclaw-api",
    "zeroclaw-config",
    "zeroclaw-sop-graph",
];

/// Direct dependencies the client crate may declare: the proto allow-list
/// plus the async runtime it needs to drive a byte stream.
const CLIENT_ALLOWED_DEPENDENCIES: &[&str] = &[
    "serde",
    "serde_json",
    "tokio",
    "zeroclaw-api",
    "zeroclaw-rpc-proto",
];

fn read_manifest(relative: &str) -> toml::Table {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(relative);
    fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
        .parse()
        .unwrap_or_else(|e| panic!("{relative} is valid TOML: {e}"))
}

fn manifest() -> toml::Table {
    read_manifest(MANIFEST)
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

#[test]
fn rpc_client_depends_only_on_foundation_crates_and_tokio() {
    let manifest = read_manifest(CLIENT_MANIFEST);
    let allowed: BTreeSet<&str> = CLIENT_ALLOWED_DEPENDENCIES.iter().copied().collect();
    for table in ["dependencies", "build-dependencies"] {
        for dep in dependency_names(&manifest, table) {
            assert!(
                allowed.contains(dep.as_str()),
                "zeroclaw-rpc-client [{table}] declares `{dep}`, which is outside its allow-list. \
                 The client must stay linkable without the runtime; extend \
                 CLIENT_ALLOWED_DEPENDENCIES only with a foundation crate and say why in the PR."
            );
        }
    }
}

#[test]
fn runtime_never_links_the_rpc_client() {
    let manifest = read_manifest(RUNTIME_MANIFEST);
    for table in ["dependencies", "build-dependencies"] {
        assert!(
            !dependency_names(&manifest, table).contains("zeroclaw-rpc-client"),
            "zeroclaw-runtime [{table}] must not depend on zeroclaw-rpc-client: the server \
             must not link its own client (a dev-dependency is fine)"
        );
    }
}
