//! Wire contract for the ZeroClaw daemon RPC.
//!
//! The daemon serves NDJSON JSON-RPC 2.0 over a local socket (or Windows
//! named pipe). This crate holds the parts of that contract a client needs
//! and nothing else:
//!
//! - [`Method`]: the closed set of method names, with the single wire-name
//!   table and the per-method params/result contract;
//! - [`notification`]: the server-to-client notification names;
//! - [`types`]: every wire-stable request, response and notification
//!   payload type;
//! - [`error_codes`]: the JSON-RPC error codes the daemon returns, and
//!   [`error_reasons`] for the `data.reason` some errors carry;
//! - [`feature`]: the names of the additive extensions a core advertises
//!   on `initialize`;
//! - [`sop`]: the SOP graph projection types the `sops/*` methods return.
//!
//! The runtime depends on this crate and re-exports it from
//! `zeroclaw_runtime::rpc`; the authorization classification of each method
//! (`Method::authz`) stays in the runtime because it names runtime grants.
//! `cargo generate openrpc` renders this crate into the tracked OpenRPC
//! document and CI fails when the two drift.

pub mod method;
pub mod notification;
#[cfg(feature = "schema-export")]
pub mod schema;
pub mod types;

pub use method::{Method, MethodContract, Shape};

/// JSON-RPC error codes returned by the daemon.
///
/// The constants live in `zeroclaw-api` next to the envelope types; this
/// module re-exports them and adds the [`error_codes::ALL`] table so the
/// contract document can enumerate them.
pub mod error_codes {
    pub use zeroclaw_api::jsonrpc::error_codes::*;

    /// Every numeric error code with its constant name, in one table.
    pub const ALL: &[(&str, i32)] = &[
        ("PARSE_ERROR", PARSE_ERROR),
        ("INVALID_REQUEST", INVALID_REQUEST),
        ("METHOD_NOT_FOUND", METHOD_NOT_FOUND),
        ("INVALID_PARAMS", INVALID_PARAMS),
        ("INTERNAL_ERROR", INTERNAL_ERROR),
        ("SESSION_NOT_FOUND", SESSION_NOT_FOUND),
        ("SESSION_LIMIT_REACHED", SESSION_LIMIT_REACHED),
        ("SESSION_BUSY", SESSION_BUSY),
        ("SESSION_NOT_OWNED", SESSION_NOT_OWNED),
        ("AUTH_REQUIRED", AUTH_REQUIRED),
        ("VERSION_MISMATCH", VERSION_MISMATCH),
        ("FORBIDDEN", FORBIDDEN),
        ("SOP_ALREADY_EXISTS", SOP_ALREADY_EXISTS),
        ("SOP_NOT_FOUND", SOP_NOT_FOUND),
        ("FS_NOT_FOUND", FS_NOT_FOUND),
        ("FS_PERMISSION_DENIED", FS_PERMISSION_DENIED),
        ("FS_INVALID_PATH", FS_INVALID_PATH),
    ];

    #[cfg(test)]
    mod tests {
        use super::ALL;
        use std::collections::BTreeSet;

        #[test]
        fn error_codes_are_unique() {
            let names: BTreeSet<_> = ALL.iter().map(|(n, _)| *n).collect();
            let codes: BTreeSet<_> = ALL.iter().map(|(_, c)| *c).collect();
            assert_eq!(names.len(), ALL.len(), "duplicate error code name");
            assert_eq!(codes.len(), ALL.len(), "duplicate numeric error code");
        }
    }
}

/// `data.reason` values the daemon sets on an error when its code alone does
/// not say why.
pub mod error_reasons;

/// SOP graph projection types returned by `sops/graph` and `sops/graph-draft`.
pub mod sop {
    pub use zeroclaw_sop_graph::{GraphLegend, SopGraph};
}

/// Wire protocol version. Bump on breaking changes.
pub const RPC_PROTOCOL_VERSION: u64 = 1;

/// Names of the additive extensions a core advertises in
/// [`types::InitializeResult::features`].
///
/// An extension is an optional param or result field added to a method
/// after it shipped. Every core at one [`RPC_PROTOCOL_VERSION`] accepts the
/// same requests, so a client cannot tell from the version whether a core
/// honours an extension: an older core ignores the param or leaves the field
/// out. The core lists the extensions it supports so a client can check
/// rather than guess. A name is `<area>.<extension>` and is never reused.
pub mod feature {
    /// `tui/list` reports each connection's `client_kind`, the label its
    /// client declared on `initialize`.
    pub const TUI_CLIENT_KIND: &str = "tui.client_kind";

    /// `config/set-many` takes a JSON Patch (`ops`) and a `drift_guard`, and
    /// reports each operation's result and the validation warnings.
    pub const CONFIG_PATCH_OPS: &str = "config.patch_ops";

    /// `config/map-key-create` under `skill_bundles` also creates the
    /// bundle's directory.
    pub const CONFIG_SKILL_BUNDLE_DIR: &str = "config.skill_bundle_dir";

    /// `config/reload` accepts remote HTTP origin context and checks the
    /// live remote-admin policy in the core at reload admission.
    pub const CONFIG_REMOTE_ADMIN_RELOAD: &str = "config.remote_admin_reload";

    /// Every extension name this protocol defines. A core advertises only
    /// the ones it implements, from its own list beside the handlers.
    pub const KNOWN: &[&str] = &[
        TUI_CLIENT_KIND,
        CONFIG_PATCH_OPS,
        CONFIG_SKILL_BUNDLE_DIR,
        CONFIG_REMOTE_ADMIN_RELOAD,
    ];

    #[cfg(test)]
    mod tests {
        use super::KNOWN;
        use std::collections::BTreeSet;

        #[test]
        fn feature_names_are_unique_and_scoped_to_an_area() {
            let names: BTreeSet<_> = KNOWN.iter().collect();
            assert_eq!(names.len(), KNOWN.len(), "duplicate feature name");
            for name in KNOWN {
                let (area, extension) = name
                    .split_once('.')
                    .unwrap_or_else(|| panic!("{name} is not <area>.<extension>"));
                assert!(
                    !area.is_empty() && !extension.is_empty(),
                    "{name} is not <area>.<extension>"
                );
            }
        }
    }
}
