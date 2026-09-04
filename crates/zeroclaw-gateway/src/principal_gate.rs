//! Route-layer authentication for the gateway's configuration and
//! onboarding surfaces.
//!
//! Structural enforcement: the middleware is attached via `route_layer`
//! on the route group, so a handler added to the group cannot forget the
//! check (the per-handler `require_auth` convention this replaces was
//! enforced only by reviewer vigilance).
//!
//! The layer also consumes the shared principal model (RFC 7141): a
//! paired native bearer resolves to the shared operator exactly as
//! before, while a bearer presented with the `X-ZeroClaw-Auth-Provider`
//! header naming an `oidc.<alias>` provider is verified by that provider
//! and resolved to a scoped principal whose Config grants gate the
//! request by HTTP method. Provider selection is explicit, mirroring the
//! RPC handshake's `auth_provider` field: the named provider's denial is
//! authoritative, and there is no fallback between providers.

use std::sync::Arc;

use axum::{
    Json,
    extract::{Request, State},
    http::{Method, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use parking_lot::RwLock;
use zeroclaw_api::grants::{Resource, Verb};
use zeroclaw_api::jsonrpc::error_codes::FORBIDDEN;
use zeroclaw_config::pairing::PairingGuard;
use zeroclaw_config::schema::Config;
use zeroclaw_runtime::rpc::auth::{AuthDenied, ConnectionAuth, RpcInboundAuth};
use zeroclaw_runtime::rpc::transport::TransportKind;
use zeroclaw_runtime::security::auth_provider::Credential;

/// Header naming the auth provider to verify the bearer with, mirroring
/// the RPC handshake's `auth_provider` field (e.g. `oidc.corp`). Absent
/// means the native pairing provider, exactly as before this layer.
pub const AUTH_PROVIDER_HEADER: &str = "x-zeroclaw-auth-provider";

/// The gateway's inbound-auth authority: the same provider registry and
/// principal resolver the RPC layer uses, built from the same config and
/// the daemon's canonical pairing guard.
pub struct GatewayInboundAuth {
    inner: RpcInboundAuth,
    /// The gateway's live config handle (the same Arc as
    /// `AppState.config`), so scoped resolution always sees the latest
    /// persisted-and-swapped snapshot.
    config: Arc<RwLock<Config>>,
}

impl GatewayInboundAuth {
    pub fn from_config(
        config: &Config,
        pairing: Arc<PairingGuard>,
        live_config: Arc<RwLock<Config>>,
    ) -> anyhow::Result<Self> {
        Ok(Self {
            inner: RpcInboundAuth::from_config(config, pairing)?,
            config: live_config,
        })
    }

    fn pairing(&self) -> &Arc<PairingGuard> {
        self.inner.pairing()
    }

    /// Verify a bearer against an explicitly selected provider and
    /// resolve it to a principal with grants.
    ///
    /// Resolver policy is recompiled from the LIVE config first: gateway
    /// handlers swap `state.config` on every persisted mutation, and a
    /// stale compiled policy here would keep honoring revoked profile
    /// mappings until the next daemon reload. Provider construction
    /// (issuer, keys, validation mode) still comes from startup config;
    /// changing a provider's verification settings requires the reload
    /// the mutation already flags via `pending_reload`.
    async fn authenticate_scoped(
        &self,
        token: &str,
        provider: &str,
    ) -> Result<ConnectionAuth, AuthDenied> {
        let config = self.config.read().clone();
        self.inner.refresh_from_config(&config);
        self.inner
            .authenticate(
                TransportKind::Wss,
                Credential::None,
                Some(token),
                Some(provider),
            )
            .await
    }

    /// Verify a native pairing bearer (the pre-existing gateway
    /// credential) into a shared-operator principal.
    async fn authenticate_native(&self, token: &str) -> Result<ConnectionAuth, AuthDenied> {
        self.inner
            .authenticate(TransportKind::Wss, Credential::None, Some(token), None)
            .await
    }
}

/// The exact denial the per-handler `require_auth` produced, preserved
/// for every native-path failure so clients observe no shape change.
fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(serde_json::json!({
            "error": "Unauthorized — pair first via POST /pair, then send Authorization: Bearer <token>"
        })),
    )
        .into_response()
}

fn denied_response(denied: &AuthDenied) -> Response {
    let status = if denied.code == FORBIDDEN {
        StatusCode::FORBIDDEN
    } else {
        StatusCode::UNAUTHORIZED
    };
    (status, Json(serde_json::json!({ "error": denied.message }))).into_response()
}

/// The Config-resource verb a request must hold for its HTTP method.
/// Deliberately coarse and conservative: anything that is not a plain
/// read requires Update (POST included, even for compute-only routes),
/// so a read-only principal can never reach a mutating handler.
fn required_verb(method: &Method) -> Verb {
    match *method {
        Method::GET | Method::HEAD => Verb::Read,
        Method::DELETE => Verb::Delete,
        _ => Verb::Update,
    }
}

/// Route-layer middleware for the config/onboarding route group.
pub async fn config_route_auth(
    State(auth): State<Arc<GatewayInboundAuth>>,
    mut request: Request,
    next: Next,
) -> Response {
    // CORS preflight carries no Authorization header; the per-handler
    // convention this replaces never authenticated OPTIONS either (the
    // explicit `handle_options_*` handlers carry no check).
    if request.method() == Method::OPTIONS {
        return next.run(request).await;
    }

    let provider = request
        .headers()
        .get(AUTH_PROVIDER_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);

    // Open posture preserved: with pairing disabled and no explicit
    // provider selection, the transport is trusted exactly as before
    // this layer existed.
    if provider.is_none() && !auth.pairing().require_pairing() {
        return next.run(request).await;
    }

    let Some(token) = crate::api::extract_bearer_token(request.headers()) else {
        return unauthorized();
    };
    if token.is_empty() {
        return unauthorized();
    }
    let token = token.to_owned();

    let conn = match provider {
        Some(provider) => match auth.authenticate_scoped(&token, &provider).await {
            Ok(conn) => conn,
            Err(denied) => return denied_response(&denied),
        },
        None => match auth.authenticate_native(&token).await {
            Ok(conn) => conn,
            // Preserve the historical native denial shape verbatim.
            Err(_) => return unauthorized(),
        },
    };

    if !conn
        .grants
        .permits(Resource::Config, required_verb(request.method()))
    {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({
                "error": "Principal lacks the config grant required for this method"
            })),
        )
            .into_response();
    }

    request.extensions_mut().insert(Arc::new(conn));
    next.run(request).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use axum::body::Body;
    use axum::http::Request as HttpRequest;
    use http_body_util::BodyExt as _;
    use std::collections::HashMap;
    use tower::ServiceExt as _;
    use wiremock::matchers::{method as http_method, path as http_path};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    use zeroclaw_config::schema::{OidcConfig, OidcValidation, PermissionProfileConfig};

    use crate::AppState;

    const LEGACY_DENIAL: &str =
        "Unauthorized — pair first via POST /pair, then send Authorization: Bearer <token>";

    fn paired_config() -> Config {
        let mut config = Config::default();
        config.gateway.require_pairing = true;
        config.gateway.paired_tokens = vec!["zc_paired".into()];
        config
    }

    /// The REAL route group under test, built exactly as `run_gateway`
    /// builds it (same constructor, same layer).
    fn router_for(config: Config) -> Router {
        let state = AppState {
            pairing: Arc::new(PairingGuard::new(
                config.gateway.require_pairing,
                &config.gateway.paired_tokens,
            )),
            ..crate::api::tests::test_state(config.clone())
        };
        let auth = Arc::new(
            GatewayInboundAuth::from_config(
                &config,
                Arc::clone(&state.pairing),
                Arc::clone(&state.config),
            )
            .expect("inbound auth builds from a valid config"),
        );
        crate::config_admin_router(&auth).with_state(state)
    }

    async fn send(
        router: &Router,
        http_method: &str,
        path: &str,
        bearer: Option<&str>,
        provider: Option<&str>,
        body: Option<serde_json::Value>,
    ) -> (StatusCode, serde_json::Value) {
        let mut builder = HttpRequest::builder().method(http_method).uri(path);
        if let Some(token) = bearer {
            builder = builder.header("authorization", format!("Bearer {token}"));
        }
        if let Some(provider) = provider {
            builder = builder.header(AUTH_PROVIDER_HEADER, provider);
        }
        let request = match body {
            Some(json) => builder
                .header("content-type", "application/json")
                .body(Body::from(json.to_string()))
                .unwrap(),
            None => builder.body(Body::empty()).unwrap(),
        };
        let response = router.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
        (status, json)
    }

    #[tokio::test]
    async fn unauthenticated_requests_get_the_legacy_denial_shape() {
        let router = router_for(paired_config());
        for path in [
            "/api/config",
            "/api/quickstart/state",
            "/api/config/sections",
        ] {
            let (status, body) = send(&router, "GET", path, None, None, None).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{path}");
            assert_eq!(body["error"], LEGACY_DENIAL, "{path}");
        }
    }

    #[tokio::test]
    async fn invalid_native_bearer_keeps_the_legacy_denial_shape() {
        let router = router_for(paired_config());
        let (status, body) =
            send(&router, "GET", "/api/config", Some("zc_wrong"), None, None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body["error"], LEGACY_DENIAL);
    }

    #[tokio::test]
    async fn paired_native_bearer_reaches_the_handler_with_full_access() {
        let router = router_for(paired_config());
        let (status, _) = send(
            &router,
            "GET",
            "/api/quickstart/state",
            Some("zc_paired"),
            None,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let (status, _) = send(
            &router,
            "POST",
            "/api/quickstart/fields",
            Some("zc_paired"),
            None,
            Some(serde_json::json!({"section": "channel", "type_key": "telegram"})),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "shared operator may mutate");
    }

    #[tokio::test]
    async fn options_preflight_stays_unauthenticated() {
        let router = router_for(paired_config());
        let (status, _) = send(&router, "OPTIONS", "/api/config", None, None, None).await;
        assert_ne!(status, StatusCode::UNAUTHORIZED);
    }

    fn open_config() -> Config {
        let mut config = Config::default();
        config.gateway.require_pairing = false;
        config
    }

    #[tokio::test]
    async fn open_mode_without_provider_selection_stays_open() {
        let router = router_for(open_config());
        let (status, _) = send(&router, "GET", "/api/quickstart/state", None, None, None).await;
        assert_eq!(status, StatusCode::OK);
    }

    #[tokio::test]
    async fn unknown_provider_selection_is_denied_even_in_open_mode() {
        let router = router_for(open_config());
        let (status, body) = send(
            &router,
            "GET",
            "/api/quickstart/state",
            Some("whatever"),
            Some("oidc.nope"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body["error"], "Unknown auth_provider selection");
    }

    fn now_unix() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }

    async fn introspection_idp(groups: &[&str]) -> MockServer {
        let server = MockServer::start().await;
        let issuer = server.uri();
        Mock::given(http_method("GET"))
            .and(http_path("/.well-known/openid-configuration"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "issuer": issuer,
                "introspection_endpoint": format!("{issuer}/introspect"),
            })))
            .mount(&server)
            .await;
        Mock::given(http_method("POST"))
            .and(http_path("/introspect"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "active": true,
                "iss": issuer,
                "sub": "alice",
                "aud": "zeroclaw",
                "exp": now_unix() + 600,
                "groups": groups,
            })))
            .mount(&server)
            .await;
        server
    }

    fn oidc_config_with_reader_profile(issuer: &str) -> Config {
        let mut config = paired_config();
        config.oidc.insert(
            "test".into(),
            OidcConfig {
                issuer: issuer.to_string(),
                audience: "zeroclaw".into(),
                client_id: "gw".into(),
                client_secret: Some("s3cret".into()),
                validation: OidcValidation::Introspection,
                claim_path: "groups".into(),
                profile_map: HashMap::from([("ops".to_string(), "config-reader".to_string())]),
                ..OidcConfig::default()
            },
        );
        config.permission_profiles.insert(
            "config-reader".into(),
            PermissionProfileConfig {
                grants: HashMap::from([(Resource::Config, vec![Verb::Read])]),
                ..PermissionProfileConfig::default()
            },
        );
        config
    }

    #[tokio::test]
    async fn scoped_oidc_principal_is_gated_by_config_grants() {
        let idp = introspection_idp(&["ops"]).await;
        let router = router_for(oidc_config_with_reader_profile(&idp.uri()));

        // Read passes: the profile grants Config:Read.
        let (status, _) = send(
            &router,
            "GET",
            "/api/quickstart/state",
            Some("opaque-token"),
            Some("oidc.test"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        // Mutation is refused at the layer: no Config:Update grant.
        let (status, body) = send(
            &router,
            "POST",
            "/api/quickstart/fields",
            Some("opaque-token"),
            Some("oidc.test"),
            Some(serde_json::json!({"section": "channel", "type_key": "telegram"})),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(
            body["error"],
            "Principal lacks the config grant required for this method"
        );
    }

    #[tokio::test]
    async fn oidc_identity_with_no_mapped_profile_is_refused() {
        let idp = introspection_idp(&["unmapped-group"]).await;
        let router = router_for(oidc_config_with_reader_profile(&idp.uri()));
        let (status, _) = send(
            &router,
            "GET",
            "/api/quickstart/state",
            Some("opaque-token"),
            Some("oidc.test"),
            None,
        )
        .await;
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "deny-by-default: no profile, no access"
        );
    }
}
