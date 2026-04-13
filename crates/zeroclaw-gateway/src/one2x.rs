//! One2X gateway extensions.
//!
//! Provides `extend_router()` to add custom API routes for the agent SSE
//! endpoint and WebSocket channel handler.
//!
//! The actual handler implementations are in the root crate's `src/one2x/`
//! module. This stub provides the router extension point; handlers will be
//! wired once the one2x module is fully adapted for v6 crate architecture.

use axum::Router;

use crate::AppState;

/// Extend the gateway router with One2X-specific routes.
///
/// Called from `run_gateway()` via `#[cfg(feature = "one2x")]`.
/// Routes added:
/// - `POST /agent` — Agent SSE endpoint
/// - `POST /agent/clear` — Clear agent session
/// - `GET /ws/channel` — WebSocket real-time channel
pub fn extend_router(router: Router<AppState>) -> Router<AppState> {
    // Routes will be connected once the one2x handler modules are
    // adapted for the v6 crate architecture. For now, pass through.
    router
}
