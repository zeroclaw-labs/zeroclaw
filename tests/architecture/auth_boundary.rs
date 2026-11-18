//! Architecture gate: the two auth boundaries this stack introduces are
//! enforced by placement, not by a check inside each handler. Placement is
//! invisible to the compiler, so a new route or method added in the wrong
//! spot authenticates nobody and still builds, passes clippy, and passes
//! every existing test. These detectors turn both invariants into build
//! failures.
//!
//! 1. The gateway config surface authenticates via a `route_layer` on
//!    `config_admin_router`. A handler from `api_config`, `api_sections` or
//!    `api_quickstart` registered on any other router is unauthenticated.
//! 2. Every RPC method that acts on a caller-supplied `session_id` must pass
//!    it through `authorize_session_owner`, which is what keeps one
//!    principal's sessions invisible to another.

use std::fs;
use std::path::PathBuf;

fn repo_file(rel: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// Byte range of a `fn <name>` body, found by brace balance from the first
/// `{` after the signature. `search_from` disambiguates repeated names: the
/// span must be the one at that offset, not the file's first match.
fn fn_body_span_from(src: &str, signature: &str, search_from: usize) -> (usize, usize) {
    let start = search_from
        + src[search_from..]
            .find(signature)
            .unwrap_or_else(|| panic!("{signature} not found; this gate needs updating"));
    let open = start
        + src[start..]
            .find('{')
            .expect("function signature must be followed by a body");
    let mut depth = 0usize;
    for (offset, ch) in src[open..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return (start, open + offset);
                }
            }
            _ => {}
        }
    }
    panic!("unbalanced braces after {signature}");
}

/// Handlers on the config surface are authenticated by WHERE they are
/// registered. Anything from these modules must be registered inside
/// `config_admin_router`, whose tail carries the principal `route_layer`.
#[test]
fn config_surface_handlers_are_registered_only_on_the_authenticated_router() {
    const GUARDED_MODULES: &[&str] = &["api_config::", "api_sections::", "api_quickstart::"];

    let src = repo_file("crates/zeroclaw-gateway/src/lib.rs");
    let (start, end) = fn_body_span_from(&src, "fn config_admin_router", 0);

    let mut stray = Vec::new();
    for module in GUARDED_MODULES {
        let mut from = 0usize;
        while let Some(found) = src[from..].find(module) {
            let at = from + found;
            from = at + module.len();
            if at >= start && at <= end {
                continue; // inside the authenticated group
            }
            // `use` statements and doc comments name the module without
            // registering a route; only route registration matters.
            let line_start = src[..at].rfind('\n').map_or(0, |i| i + 1);
            let line_end = src[at..].find('\n').map_or(src.len(), |i| at + i);
            let line = src[line_start..line_end].trim();
            if line.starts_with("//") || line.starts_with("use ") || line.starts_with("pub use ") {
                continue;
            }
            let line_no = src[..at].bytes().filter(|b| *b == b'\n').count() + 1;
            stray.push(format!("  lib.rs:{line_no}: {line}"));
        }
    }

    assert!(
        stray.is_empty(),
        "these config-surface handlers are registered OUTSIDE config_admin_router, so the \
         principal route_layer never runs for them and they are unauthenticated:\n{}\n\
         Move the route into config_admin_router.",
        stray.join("\n")
    );
}

/// Every RPC method that acts on a caller-supplied session id must authorize
/// ownership of that exact session. The allowlist below is for methods that
/// legitimately do not take one id: enumeration filters by principal instead,
/// and `session/new` authorizes only on its reattach path.
#[test]
fn session_targeting_rpc_methods_authorize_ownership() {
    // Enumeration filters on `scoped_principal_id` rather than authorizing a
    // single id; `session/new` creates, and gates its reattach path.
    const FILTERS_INSTEAD: &[&str] = &[
        "handle_session_list",
        "handle_session_list_acp",
        "handle_session_list_acp_for_test",
        "handle_session_new",
        "handle_session_new_for_test",
    ];

    let src = repo_file("crates/zeroclaw-runtime/src/rpc/dispatch.rs");

    let mut ungated = Vec::new();
    let mut checked = 0usize;
    let mut from = 0usize;
    while let Some(found) = src[from..].find("fn handle_session_") {
        let at = from + found;
        from = at + "fn handle_session_".len();
        let name_start = at + "fn ".len();
        let name_end = name_start
            + src[name_start..]
                .find(['(', '<'])
                .expect("a function name is followed by ( or <");
        let name = &src[name_start..name_end];
        if FILTERS_INSTEAD.contains(&name) {
            continue;
        }
        let (_, end) = fn_body_span_from(&src, &format!("fn {name}"), at);
        let body = &src[at..end];
        // Only methods that read a caller-supplied session id are in scope.
        if !body.contains("session_id") {
            continue;
        }
        checked += 1;
        if !body.contains("authorize_session_owner") {
            let line_no = src[..at].bytes().filter(|b| *b == b'\n').count() + 1;
            ungated.push(format!("  dispatch.rs:{line_no}: {name}"));
        }
    }

    assert!(
        checked >= 8,
        "expected to find the session-targeting methods; found only {checked}, so this gate is \
         no longer scanning what it thinks it is"
    );
    assert!(
        ungated.is_empty(),
        "these RPC methods act on a caller-supplied session_id without calling \
         authorize_session_owner, so one principal could reach another's session:\n{}\n\
         Add the ownership check, or add the method to FILTERS_INSTEAD with the reason it \
         filters by principal rather than authorizing one id.",
        ungated.join("\n")
    );
}
