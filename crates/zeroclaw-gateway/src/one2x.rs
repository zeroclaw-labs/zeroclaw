//! One2X gateway extensions.
//!
//! Provides `extend_router()` to add custom API routes for the agent SSE
//! endpoint and WebSocket channel handler.
//!
//! ## Architecture
//!
//! The actual route handlers (`handle_agent_sse`, `handle_ws_connection`)
//! live in the root crate (`src/one2x/agent_sse.rs`, `src/one2x/web_channel.rs`)
//! because they depend on root-crate-only types (`crate::approval`,
//! `crate::tools`, etc.). The gateway crate cannot import the root crate
//! (circular dependency), so we use an inversion-of-control pattern:
//!
//! 1. The root crate calls [`register_extra_routes`] once at startup with
//!    a closure that knows how to extend the [`axum::Router`].
//! 2. [`extend_router`] (called from `run_gateway`) consults that closure
//!    when building the router.
//!
//! When no closure is registered (e.g. in tests, or when one2x routes are
//! intentionally disabled), the router is returned unchanged.

use std::sync::OnceLock;

use axum::Router;

use crate::AppState;

/// Closure type for registering one2x routes from outside the gateway crate.
pub type RouteExtender =
    Box<dyn Fn(Router<AppState>) -> Router<AppState> + Send + Sync + 'static>;

static EXTRA_ROUTES: OnceLock<RouteExtender> = OnceLock::new();

/// Register a closure that extends the gateway router with one2x routes.
///
/// Call this once from the root crate at process startup, before
/// `run_gateway()` is invoked. Subsequent calls are ignored and emit a
/// warning: silently dropping a registration is usually a programming
/// error (e.g., two subsystems both trying to own the route table), and
/// the caller should know their closure did not take effect.
pub fn register_extra_routes(extender: RouteExtender) {
    if EXTRA_ROUTES.set(extender).is_err() {
        tracing::warn!(
            "one2x::register_extra_routes called more than once — only the first \
             registration takes effect. This is likely a programming error; check for \
             duplicate initialization paths."
        );
    }
}

/// Extend the gateway router with One2X-specific routes.
///
/// Called from `run_gateway()` via `#[cfg(feature = "one2x")]`.
pub fn extend_router(router: Router<AppState>) -> Router<AppState> {
    match EXTRA_ROUTES.get() {
        Some(extender) => {
            tracing::debug!("one2x::extend_router: applying registered extra routes");
            extender(router)
        }
        None => {
            tracing::debug!(
                "one2x::extend_router: no extra routes registered, returning router unchanged"
            );
            router
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::routing::get;
    use tower::ServiceExt;

    /// Building a router with a registered extender should add the extra routes.
    /// Note: `OnceLock` is process-global, so we cannot reliably exercise
    /// `register_extra_routes` across tests — only one will win. We verify
    /// the closure-composition contract directly below.
    #[test]
    fn extend_router_passthrough_when_unregistered() {
        let r: Router<AppState> = Router::new();
        let _ = extend_router(r);
        // No panic is the assertion.
    }

    #[test]
    fn route_extender_signature_compiles() {
        let _extender: RouteExtender = Box::new(|router| router.route("/test", get(|| async {})));
    }

    /// End-to-end: a hand-built extender closure that adds a simple route
    /// should produce a [`Router`] that serves that route when driven
    /// through [`tower::ServiceExt::oneshot`]. Uses [`Router<()>`] so we
    /// can skip the real `AppState` construction (which needs the full
    /// daemon stack). This verifies the closure-composition pattern that
    /// the IoC hook relies on.
    #[tokio::test]
    async fn extender_closure_composition_serves_injected_route() {
        let extender: Box<dyn Fn(Router<()>) -> Router<()> + Send + Sync> =
            Box::new(|r| r.route("/_one2x_test", get(|| async { "ok-one2x" })));
        let router: Router<()> = extender(Router::new());

        let response = router
            .oneshot(
                Request::builder()
                    .uri("/_one2x_test")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .expect("request should dispatch");
        assert_eq!(response.status(), StatusCode::OK);

        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body readable");
        assert_eq!(&bytes[..], b"ok-one2x");
    }

    /// Multiple routes registered by one extender should all be reachable —
    /// exercises the "register 3 routes at once" pattern used by the root
    /// crate's `register_gateway_routes` (POST /agent, POST /agent/clear,
    /// GET /ws/channel).
    #[tokio::test]
    async fn extender_closure_can_add_multiple_routes() {
        let extender: Box<dyn Fn(Router<()>) -> Router<()> + Send + Sync> = Box::new(|r| {
            r.route("/alpha", get(|| async { "a" }))
                .route("/beta", get(|| async { "b" }))
                .route("/gamma", get(|| async { "c" }))
        });
        let router: Router<()> = extender(Router::new());

        for (path, expected) in [("/alpha", "a"), ("/beta", "b"), ("/gamma", "c")] {
            let response = router
                .clone()
                .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
                .await
                .expect("request should dispatch");
            assert_eq!(response.status(), StatusCode::OK, "path={path}");
            let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap();
            assert_eq!(&bytes[..], expected.as_bytes(), "path={path}");
        }
    }

    /// Requests to routes the extender did NOT add should 404 — confirms
    /// the extender doesn't magically add catch-all handlers.
    #[tokio::test]
    async fn extender_does_not_add_unspecified_routes() {
        let extender: Box<dyn Fn(Router<()>) -> Router<()> + Send + Sync> =
            Box::new(|r| r.route("/only-this", get(|| async { "x" })));
        let router: Router<()> = extender(Router::new());

        let response = router
            .oneshot(
                Request::builder()
                    .uri("/does-not-exist")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
}
