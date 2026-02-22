pub mod auth_routes;
pub mod channel_routes;
pub mod member_routes;
pub mod monitoring_routes;
pub mod tenant_routes;
pub mod user_routes;

use crate::state::SharedState;
use axum::{
    http::{header, Method},
    routing::{delete, get, patch, post},
    Json, Router,
};
use tower_http::cors::{Any, CorsLayer};
use tower_http::services::{ServeDir, ServeFile};

pub fn app(state: SharedState) -> Router {
    let cors = CorsLayer::new()
        .allow_origin(Any) // Restrict to specific origins in production
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::PATCH,
            Method::DELETE,
            Method::OPTIONS,
        ])
        .allow_headers([header::CONTENT_TYPE, header::AUTHORIZATION])
        .max_age(std::time::Duration::from_secs(3600));

    let api = Router::new()
        // Auth
        .route("/auth/otp/request", post(auth_routes::request_otp))
        .route("/auth/otp/verify", post(auth_routes::verify_otp))
        .route("/auth/me", get(auth_routes::me))
        .route("/auth/logout", post(auth_routes::logout))
        // Users
        .route("/users", get(user_routes::list_users))
        .route("/users", post(user_routes::create_user))
        .route(
            "/users/{id}",
            patch(user_routes::update_user).delete(user_routes::delete_user),
        )
        // Tenants
        .route(
            "/tenants",
            get(tenant_routes::list_tenants).post(tenant_routes::create_tenant),
        )
        .route("/tenants/{id}", delete(tenant_routes::delete_tenant))
        .route("/tenants/{id}/restart", post(tenant_routes::restart_tenant))
        .route("/tenants/{id}/stop", post(tenant_routes::stop_tenant))
        .route("/tenants/{id}/deploy", post(tenant_routes::deploy_tenant))
        .route(
            "/tenants/{id}/status",
            get(tenant_routes::get_tenant_status),
        )
        .route(
            "/tenants/{id}/provider/test",
            post(tenant_routes::test_provider),
        )
        .route("/tenants/{id}/exec", post(tenant_routes::exec_in_tenant))
        .route("/tenants/{id}/logs", get(tenant_routes::tenant_logs))
        .route(
            "/tenants/{id}/pairing-code",
            get(tenant_routes::get_pairing_code),
        )
        .route(
            "/tenants/{id}/reset-pairing",
            post(tenant_routes::reset_pairing),
        )
        .route(
            "/tenants/{id}/config",
            get(tenant_routes::get_tenant_config).patch(tenant_routes::update_tenant_config),
        )
        // Channels
        .route(
            "/tenants/{tenant_id}/channels",
            get(channel_routes::list_channels).post(channel_routes::create_channel),
        )
        .route(
            "/tenants/{tenant_id}/channels/{channel_id}",
            get(channel_routes::get_channel)
                .patch(channel_routes::update_channel)
                .delete(channel_routes::delete_channel),
        )
        // Members
        .route(
            "/tenants/{tenant_id}/members",
            get(member_routes::list_members).post(member_routes::add_member),
        )
        .route(
            "/tenants/{tenant_id}/members/{member_id}",
            patch(member_routes::update_member_role).delete(member_routes::remove_member),
        )
        // Monitoring
        .route("/monitoring/dashboard", get(monitoring_routes::dashboard))
        .route("/monitoring/health", get(monitoring_routes::health))
        .route("/monitoring/usage", get(monitoring_routes::usage))
        .route("/monitoring/audit", get(monitoring_routes::audit))
        .route(
            "/monitoring/resources",
            get(monitoring_routes::admin_resources),
        )
        // Resource monitoring (per-tenant)
        .route(
            "/tenants/{id}/resources",
            get(monitoring_routes::tenant_resources),
        )
        .with_state(state);

    // Top-level: health at root, API under /api, SPA fallback for everything else
    Router::new()
        .route("/health", get(health_handler))
        .nest("/api", api)
        .layer(cors)
        .fallback_service(
            ServeDir::new("static").not_found_service(ServeFile::new("static/index.html")),
        )
}

async fn health_handler() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "status": "ok",
        "service": "zcplatform"
    }))
}
