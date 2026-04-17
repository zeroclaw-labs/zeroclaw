//! One2X custom extensions for ZeroClaw (root-crate parts).
//!
//! All custom functionality lives under `one2x/` (both here and in
//! sub-crates), minimizing upstream file changes. New files in this
//! module have **zero merge conflict risk**.
//!
//! ## Architecture (v6)
//!
//! In v6, most one2x code was extracted to workspace crates. The canonical
//! implementations live in sub-crate `one2x` modules:
//!
//! - **Runtime hooks**: `zeroclaw-runtime/src/one2x.rs` +
//!   `zeroclaw-runtime/src/one2x/compaction.rs` — tool pairing, multi-stage
//!   compaction, planning detection.
//! - **Channel hooks**: `zeroclaw-channels/src/one2x.rs` — session hygiene,
//!   fast-approval optimization, channel-side tool pairing.
//! - **Gateway route IoC**: `zeroclaw-gateway/src/one2x.rs` — closure-based
//!   route extender the root crate registers into at startup.
//!
//! This root-crate module retains only root-crate-specific code:
//!
//! - [`agent_sse`]   — SSE-streamed `POST /agent` handler (depends on root
//!   `approval`, `tools`, `memory` modules that cannot easily move to a
//!   sub-crate).
//! - [`web_channel`] — WebSocket channel state + message types for the
//!   `GET /ws/channel` handler.
//! - [`gateway_ext`] — pairing-aware `handle_ws_channel` wrapper.
//! - [`register_gateway_routes`] — wires the three handlers into
//!   `zeroclaw_gateway::one2x::register_extra_routes` at process startup.
//!
//! ## History (2026-04-16 cleanup)
//!
//! The following v5-era siblings used to live here and were removed once
//! their canonical v6 equivalents in the sub-crates were confirmed to own
//! every live call path:
//!
//! - `agent_hooks.rs`    → `zeroclaw-runtime/src/one2x.rs`
//! - `session_hygiene.rs` → `zeroclaw-channels/src/one2x.rs`
//! - `tool_pairing.rs`   → `zeroclaw-runtime/src/one2x.rs`
//! - `compaction.rs`     → `zeroclaw-runtime/src/one2x/compaction.rs`
//! - `config.rs`         (thin `pub use` re-export; callers now import
//!   directly from `zeroclaw_config::scattered_types::WebChannelConfig`)
//!
//! Git history preserves the old implementations for reference. When
//! syncing with upstream, consult the corresponding sub-crate `one2x`
//! module rather than trying to resurrect the v5 root-crate copy.
//!
//! ## Upstream Integration Points
//!
//! Upstream files still carry a handful of tiny integration patches,
//! mostly `#[cfg(feature = "one2x")]`-gated. See `dev/UPSTREAM-SYNC-SOP.md`
//! for the current list and conflict-risk ranking.

// These three sibling modules are all wired into the live gateway/channel
// stacks from `src/main.rs`. From the *library* compilation unit they look
// like dead code because the lib never invokes `register_gateway_routes()`.
// The bin compilation unit (`mod one2x;` in main.rs) does invoke it.
#[allow(dead_code)]
pub mod agent_sse;
#[allow(dead_code)]
pub mod gateway_ext;
#[allow(dead_code)]
pub mod web_channel;

/// Register all One2X gateway routes with the gateway crate's IoC hook.
///
/// Must be called once at process startup, before [`zeroclaw_gateway::run_gateway`].
/// Calling it more than once is harmless (the registration uses `OnceLock::set`,
/// which silently ignores subsequent calls and logs at warn level).
///
/// Routes registered:
/// - `POST /agent`        → SSE-streamed chat with the agent (F-05)
/// - `POST /agent/clear`  → Clear an agent SSE session
/// - `GET  /ws/channel`   → WebSocket entry point for the web channel (F-04)
#[cfg(feature = "gateway")]
#[allow(dead_code)] // invoked from src/main.rs (separate compilation unit)
pub fn register_gateway_routes() {
    use axum::routing::{get, post};

    zeroclaw_gateway::one2x::register_extra_routes(Box::new(|router| {
        router
            .route("/agent", post(agent_sse::handle_agent_sse))
            .route("/agent/clear", post(agent_sse::handle_agent_clear))
            .route("/ws/channel", get(gateway_ext::handle_ws_channel))
    }));
}
