//! A2A protocol constants and JSON-RPC error shapes.
//!
//! Pure protocol, no host dependency: well-known paths, protocol version, and the
//! error envelopes specified by spec §§5.4/9.5 (including `google.rpc.ErrorInfo`
//! details). Host handlers reuse these directly to avoid hand-built `Value` drift.

use serde_json::Value;

use crate::a2a::a2a_wire::SendMessageParams;

/// Well-known agent card path (A2A §14.3, RFC 8615).
pub const WELL_KNOWN_AGENT_CARD_PATH: &str = "/.well-known/agent-card.json";
/// Catalog (plural) agent-card path served at the origin root.
pub const CATALOG_CARD_PATH: &str = "/.well-known/agents-card.json";
/// Prefixed alias of the catalog under `/a2a/`.
pub const CATALOG_CARD_PREFIXED_PATH: &str = "/a2a/.well-known/agents-card.json";
/// A2A protocol version this server implements (spec §5.x).
pub const A2A_PROTOCOL_VERSION: &str = "1.0";
/// Protocol binding marker for a discovery catalog. A catalog is not a runnable
/// agent; a client uses this to tell an aggregate catalog from a single agent.
pub const CATALOG_PROTOCOL_BINDING: &str = "catalog";
/// The A2A `google.rpc.ErrorInfo` `domain` used by the official SDK.
pub const A2A_ERROR_DOMAIN: &str = "a2a-protocol.org";

/// Whether a parsed [`SendMessageParams`] asks for `returnImmediately: true`
/// (spec §4: return right after task creation without a terminal state).
#[must_use]
pub fn wants_return_immediately(params: &SendMessageParams) -> bool {
    params
        .configuration
        .as_ref()
        .map(|c| c.return_immediately)
        .unwrap_or(false)
}

/// Generic JSON-RPC 2.0 error envelope.
#[must_use]
pub fn jsonrpc_error(id: Value, code: i64, message: &str) -> Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message },
    })
}

/// A2A `TaskNotFoundError` (`-32001`) in the conforming JSON-RPC shape.
#[must_use]
pub fn task_not_found_error(id: Value, task_id: &str) -> Value {
    error_info(
        id,
        -32001,
        &format!(
            "A2A task '{task_id}' does not exist or is not accessible on this server (no durable task store)"
        ),
        "TASK_NOT_FOUND",
        serde_json::json!({ "task_id": task_id }),
    )
}

/// A2A `UnsupportedOperationError` (`-32004`) with the conforming `data` array
/// of `@type`-tagged `google.rpc.ErrorInfo` details.
#[must_use]
pub fn unsupported_operation_error(id: Value, message: &str) -> Value {
    error_info(id, -32004, message, "UNSUPPORTED_OPERATION", Value::Null)
}

/// A2A `VersionNotSupportedError` (`-32009`) in the conforming JSON-RPC shape.
#[must_use]
pub fn version_not_supported_error(id: Value, advertised_version: &str) -> Value {
    error_info(
        id,
        -32009,
        &format!(
            "Unsupported A2A version '{advertised_version}': this server only speaks A2A {A2A_PROTOCOL_VERSION}"
        ),
        "VERSION_NOT_SUPPORTED",
        serde_json::json!({ "requested_version": advertised_version }),
    )
}

/// Build an A2A error envelope carrying one `google.rpc.ErrorInfo` detail.
fn error_info(id: Value, code: i64, message: &str, reason: &str, metadata: Value) -> Value {
    let mut info = serde_json::json!({
        "@type": "type.googleapis.com/google.rpc.ErrorInfo",
        "reason": reason,
        "domain": A2A_ERROR_DOMAIN,
    });
    if !metadata.is_null() {
        info["metadata"] = metadata;
    }
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message, "data": [info] },
    })
}
