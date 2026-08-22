//! Guard test for operator cancellation of running SOP jobs.
//!
//! Shared transports (admin HTTP, WebSocket, RPC `sops/decide`, the channel
//! orchestrator, and the `sop_approve` tool) must resolve checkpoints through the
//! DEFERRED broker entry point. The inline variant drives the resumed
//! deterministic capability tail while the engine mutex is still held, so an
//! operator cancellation cannot interleave between post-checkpoint capabilities —
//! it blocks on the mutex and lands only after every side-effecting capability has
//! already run.
//!
//! This is a source-level call-site guard rather than a behavioral test: the
//! defect is "some shared surface calls the wrong function", which is a property
//! of the call graph, not of any single run. A behavioral test can only cover the
//! surfaces it happens to enumerate; this fails for a surface added tomorrow.

use std::path::{Path, PathBuf};

/// Files that own a shared, concurrently-reachable transport into the broker.
/// None of these may call the inline resolver.
const SHARED_TRANSPORT_CALL_SITES: &[&str] = &[
    "crates/zeroclaw-gateway/src/api_sop.rs",
    "crates/zeroclaw-gateway/src/api_sop_author.rs",
    "crates/zeroclaw-gateway/src/ws.rs",
    "crates/zeroclaw-runtime/src/rpc/dispatch.rs",
    "crates/zeroclaw-runtime/src/tools/sop_approve.rs",
    "crates/zeroclaw-channels/src/orchestrator/mod.rs",
];

fn workspace_root() -> PathBuf {
    // CARGO_MANIFEST_DIR = <root>/crates/zeroclaw-runtime
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root is two levels above the crate manifest")
        .to_path_buf()
}

/// Strip `//` line comments so a doc comment mentioning the inline resolver by
/// name is not mistaken for a call. Deliberately not a full Rust parser: the
/// files under guard use ordinary line comments.
fn strip_line_comments(src: &str) -> String {
    src.lines()
        .map(|l| match l.find("//") {
            Some(i) => &l[..i],
            None => l,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn shared_transports_never_call_the_inline_broker_resolver() {
    let root = workspace_root();
    let mut offenders = Vec::new();

    for rel in SHARED_TRANSPORT_CALL_SITES {
        let path = root.join(rel);
        let src = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("guarded call site {rel} must be readable: {e}"));
        let code = strip_line_comments(&src);

        for (idx, line) in code.lines().enumerate() {
            if line.contains("resolve_via_broker_inline(") {
                offenders.push(format!("{rel}:{}", idx + 1));
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "shared transports must resolve checkpoints via the deferred broker entry \
         point so operator cancellation can interleave between post-checkpoint \
         capabilities; `resolve_via_broker_inline` holds the engine mutex across the \
         whole capability tail. Offending call sites: {offenders:?}"
    );
}

/// The companion assertion: each guarded transport must actually resolve through
/// the broker at all. Without this, deleting a call site (or renaming the method
/// out from under the guard) would make the test above pass vacuously.
#[test]
fn shared_transports_still_resolve_through_the_broker() {
    let root = workspace_root();

    for rel in SHARED_TRANSPORT_CALL_SITES {
        let path = root.join(rel);
        let src = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("guarded call site {rel} must be readable: {e}"));
        let code = strip_line_comments(&src);

        // Any broker entry point counts here, INCLUDING the inline one: this test
        // must fail only when a call site has vanished entirely, never merely
        // because it regressed onto the inline variant. Catching that regression is
        // the other test's job, and duplicating it here would make one defect
        // report as two unrelated failures.
        assert!(
            code.contains("resolve_via_broker"),
            "{rel} is listed as a shared broker transport but no longer resolves through \
             the broker; update SHARED_TRANSPORT_CALL_SITES deliberately rather than \
             letting the inline-resolver guard pass vacuously"
        );
    }
}
