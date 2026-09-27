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
//! request. Provider selection is explicit, mirroring the RPC handshake's
//! `auth_provider` field: the named provider's denial is authoritative,
//! and there is no fallback between providers.
//!
//! Grants are enforced in two places. The route layer applies a coarse
//! floor per HTTP method (a read needs `Read`; anything else needs some
//! mutating verb) so a read-only principal never reaches a mutating
//! handler. Each mutating handler then authorizes its complete write set
//! before its first side effect: every config path it will persist,
//! classified by effect (`Create` for a path it brings into being,
//! `Delete` for one it removes, `Update` otherwise) and matched against
//! the principal's config path selectors, with the persist boundary
//! refusing anything the handler did not authorize
//! ([`ConfigWriteAuthorization`]).
//!
//! Policy itself moves only at that persist boundary: the handler that
//! writes a configuration publishes the policy compiled from it as the
//! next accepted revision, and requests are verified and resolved against
//! the accepted snapshot as it stands. Nothing on the request path
//! compiles policy, so a request that read the configuration before a
//! concurrent persist can never reinstall the older policy over the
//! newer one.

use std::collections::HashSet;
use std::sync::Arc;

use axum::{
    Json,
    extract::{Request, State},
    http::{Method, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use zeroclaw_api::grants::{Resource, Verb, WILDCARD};
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
///
/// The accepted policy (providers, their verification settings, profile
/// mappings, grants) is compiled at construction and thereafter only by
/// `publish_persisted`, which the persist boundary calls for the
/// configuration it has just written. The daemon's RPC surface holds its
/// own live configuration and reaches the same state through the reload
/// every gateway mutation flags.
pub struct GatewayInboundAuth {
    inner: RpcInboundAuth,
}

impl GatewayInboundAuth {
    pub fn from_config(config: &Config, pairing: Arc<PairingGuard>) -> anyhow::Result<Self> {
        Ok(Self {
            inner: RpcInboundAuth::from_config(config, pairing)?,
        })
    }

    fn pairing(&self) -> &Arc<PairingGuard> {
        self.inner.pairing()
    }

    /// The authorization-policy generation currently in force.
    pub fn generation(&self) -> u64 {
        self.inner.generation()
    }

    /// Publish the policy compiled from a configuration the persist
    /// boundary has just written, as the next accepted revision.
    ///
    /// Callers hold the gateway's config write lock, so revisions are
    /// issued in persist order. A writer that was slower to publish can
    /// never reinstall policy a later persist superseded: a revision that
    /// is not newer than the accepted one is refused as a no-op.
    fn publish_persisted(&self, persisted: &Config) -> anyhow::Result<u64> {
        let revision = self.inner.accepted_revision().saturating_add(1);
        self.inner.publish_accepted(persisted, revision)
    }

    /// Verify a bearer against an explicitly selected provider and
    /// resolve it to a principal with grants, both against the accepted
    /// policy snapshot as it stands.
    async fn authenticate_scoped(
        &self,
        token: &str,
        provider: &str,
    ) -> Result<ConnectionAuth, AuthDenied> {
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

fn forbidden(message: impl Into<String>) -> Response {
    (
        StatusCode::FORBIDDEN,
        Json(serde_json::json!({ "error": message.into() })),
    )
        .into_response()
}

/// A refused config write: the 403 the handler returns in place of the
/// mutation, carrying which path or grant fell short.
#[derive(Debug)]
pub struct WriteDenied(String);

impl IntoResponse for WriteDenied {
    fn into_response(self) -> Response {
        forbidden(self.0)
    }
}

/// The coarse floor a request must clear at the route layer for its HTTP
/// method: a plain read needs `Read`; any other method must hold at least
/// one mutating verb on the Config resource, so a read-only principal
/// never reaches a mutating handler (compute-only POST routes included).
/// Which mutating verb a request really needs is decided per config path
/// by the handler's complete write set, see [`ConfigWriteSet`]. A method
/// outside the known set is refused outright.
fn method_floor(method: &Method) -> Option<&'static [Verb]> {
    match *method {
        Method::GET | Method::HEAD => Some(&[Verb::Read]),
        Method::POST | Method::PUT | Method::PATCH | Method::DELETE => {
            Some(&[Verb::Create, Verb::Update, Verb::Delete])
        }
        _ => None,
    }
}

/// What the route layer hands every admitted request, for the handlers
/// that mutate configuration: the principal the request was authenticated
/// as (`None` in the open posture, where the transport is trusted exactly
/// as before this layer existed) and the authority whose accepted policy
/// admitted it, so the persist boundary can publish the policy of the
/// configuration it writes.
#[derive(Clone)]
pub struct RequestAuth {
    principal: Option<Arc<ConnectionAuth>>,
    authority: Arc<GatewayInboundAuth>,
}

/// The extractor a mutating handler declares to receive its
/// [`RequestAuth`]. Absent only when the handler is invoked outside the
/// route group (unit tests call handlers directly); nothing is enforced
/// or published then.
pub type RequestPrincipal = Option<axum::Extension<RequestAuth>>;

/// One config mutation's complete write set: every dotted path the
/// mutation persists, each with the verb its effect requires.
#[derive(Clone, Debug, Default)]
pub struct ConfigWriteSet {
    writes: Vec<(String, Verb)>,
}

impl ConfigWriteSet {
    /// Classify `paths` by effect: `before` is the configuration being
    /// replaced and `after` the mutated working copy. A path declared only
    /// after is a `Create`, one declared only before a `Delete`, anything
    /// else an `Update`. Declaration follows the property tree, so a map
    /// entry (`agents.<alias>`) exists exactly while it has fields, and
    /// creating one implicitly (a `PUT` under a new alias) classifies as
    /// the creation it is.
    pub fn by_effect<'a>(
        before: &Config,
        after: &Config,
        paths: impl IntoIterator<Item = &'a str>,
    ) -> Self {
        let before = declared_paths(before);
        let after = declared_paths(after);
        let writes = paths
            .into_iter()
            .map(|path| {
                let verb = match (contains_path(&before, path), contains_path(&after, path)) {
                    (false, true) => Verb::Create,
                    (true, false) => Verb::Delete,
                    _ => Verb::Update,
                };
                (path.to_owned(), verb)
            })
            .collect();
        Self { writes }
    }

    /// Pin the verb for a path whose route semantics the effect diff does
    /// not show: a `DELETE` of a scalar prop clears it rather than removing
    /// the declared field, and a JSON Patch `remove` likewise.
    pub fn with(mut self, path: impl Into<String>, verb: Verb) -> Self {
        let path = path.into();
        self.writes.retain(|(existing, _)| *existing != path);
        self.writes.push((path, verb));
        self
    }

    fn covers(&self, path: &str) -> bool {
        self.writes.iter().any(|(authorized, _)| {
            path == authorized
                || path
                    .strip_prefix(authorized.as_str())
                    .is_some_and(|rest| rest.starts_with('.'))
        })
    }
}

fn declared_paths(config: &Config) -> HashSet<String> {
    config
        .prop_fields()
        .into_iter()
        .map(|info| info.name)
        .collect()
}

fn contains_path(declared: &HashSet<String>, path: &str) -> bool {
    declared.contains(path)
        || declared.iter().any(|name| {
            name.strip_prefix(path)
                .is_some_and(|rest| rest.starts_with('.'))
        })
}

fn verb_name(verb: Verb) -> &'static str {
    match verb {
        Verb::Create => "create",
        Verb::Read => "read",
        Verb::Update => "update",
        Verb::Delete => "delete",
        Verb::Execute => "execute",
    }
}

/// Proof that a mutation's complete write set was authorized before its
/// first side effect. The persist boundary requires one and re-checks that
/// nothing outside the authorized set is about to be written, then
/// publishes the policy of what it wrote through the admitting authority.
pub struct ConfigWriteAuthorization {
    /// `None` when nothing is enforced: the open posture, an admin
    /// principal, or a whole-configuration rewrite already cleared by the
    /// wildcard selector.
    enforced: Option<ConfigWriteSet>,
    authority: Option<Arc<GatewayInboundAuth>>,
}

impl ConfigWriteAuthorization {
    /// Refuse when `dirty`, the paths about to be persisted, reaches
    /// beyond the authorized write set: the handler could not identify its
    /// complete write set up front, and a scoped principal is refused
    /// rather than trusted with the remainder.
    pub fn covers_all<'a>(
        &self,
        dirty: impl IntoIterator<Item = &'a str>,
    ) -> Result<(), WriteDenied> {
        let Some(writes) = &self.enforced else {
            return Ok(());
        };
        for path in dirty {
            if !writes.covers(path) {
                return Err(WriteDenied(format!(
                    "Config path `{path}` is outside the write set authorized for this mutation"
                )));
            }
        }
        Ok(())
    }

    /// Publish the policy compiled from the configuration just persisted.
    /// The persist boundary proves the staged policy compiles before it
    /// writes, so a failure here is an invariant violation: it is logged,
    /// and the previously accepted policy stays in force.
    pub fn publish_persisted(&self, persisted: &Config) {
        let Some(authority) = &self.authority else {
            return;
        };
        if let Err(error) = authority.publish_persisted(persisted) {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({ "error": format!("{error}") })),
                "persisted configuration did not compile into an accepted policy; the previous policy stays in force"
            );
        }
    }
}

fn unenforced(authority: Option<Arc<GatewayInboundAuth>>) -> ConfigWriteAuthorization {
    ConfigWriteAuthorization {
        enforced: None,
        authority,
    }
}

/// Authorize one mutation's complete write set for the request's
/// principal, before the mutation's first side effect. Every path needs
/// both the Config verb its effect requires and a matching config path
/// selector. The open posture and an admin principal are unrestricted,
/// as everywhere else.
pub fn authorize_config_write(
    request: &RequestPrincipal,
    writes: ConfigWriteSet,
) -> Result<ConfigWriteAuthorization, WriteDenied> {
    let Some(axum::Extension(request)) = request else {
        return Ok(unenforced(None));
    };
    let authority = Some(Arc::clone(&request.authority));
    let Some(conn) = &request.principal else {
        return Ok(unenforced(authority));
    };
    if conn.grants.admin {
        return Ok(unenforced(authority));
    }
    for (path, verb) in &writes.writes {
        if !conn.grants.permits(Resource::Config, *verb) {
            return Err(WriteDenied(format!(
                "Principal lacks the config `{}` grant this mutation needs for `{path}`",
                verb_name(*verb)
            )));
        }
        if !conn.grants.may_write_config(path) {
            return Err(WriteDenied(format!(
                "Principal's config path selectors do not cover `{path}`"
            )));
        }
    }
    Ok(ConfigWriteAuthorization {
        enforced: Some(writes),
        authority,
    })
}

/// A rewrite whose write set cannot be enumerated before it runs (a schema
/// migration of the file, a Quickstart apply): a scoped principal needs
/// the wildcard selector and every verb the rewrite may exercise. Nothing
/// narrower can be honoured, so nothing narrower is accepted.
pub fn authorize_whole_config_write(
    request: &RequestPrincipal,
    verbs: &[Verb],
) -> Result<ConfigWriteAuthorization, WriteDenied> {
    let Some(axum::Extension(request)) = request else {
        return Ok(unenforced(None));
    };
    let authority = Some(Arc::clone(&request.authority));
    let Some(conn) = &request.principal else {
        return Ok(unenforced(authority));
    };
    if conn.grants.admin {
        return Ok(unenforced(authority));
    }
    for verb in verbs {
        if !conn.grants.permits(Resource::Config, *verb) {
            return Err(WriteDenied(format!(
                "Principal lacks the config `{}` grant this operation needs",
                verb_name(*verb)
            )));
        }
    }
    if !conn.grants.may_write_config(WILDCARD) {
        return Err(WriteDenied(
            "This operation rewrites the configuration as a whole; the principal's config path selectors would need `*`"
                .to_owned(),
        ));
    }
    Ok(unenforced(authority))
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

    // A provider header that is present but blank or not text is a
    // malformed selection, not an absent one: it never falls through to
    // the native provider or the open posture.
    let provider = match request.headers().get(AUTH_PROVIDER_HEADER) {
        None => None,
        Some(value) => match value.to_str().ok().map(str::trim).filter(|s| !s.is_empty()) {
            Some(name) => Some(name.to_owned()),
            None => {
                return (
                    StatusCode::UNAUTHORIZED,
                    Json(serde_json::json!({ "error": "Invalid auth_provider selection" })),
                )
                    .into_response();
            }
        },
    };

    // Open posture preserved: with pairing disabled and no explicit
    // provider selection, the transport is trusted exactly as before
    // this layer existed.
    if provider.is_none() && !auth.pairing().require_pairing() {
        request.extensions_mut().insert(RequestAuth {
            principal: None,
            authority: Arc::clone(&auth),
        });
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

    let Some(floor) = method_floor(request.method()) else {
        return forbidden("Unsupported method for the config surface");
    };
    if !floor
        .iter()
        .any(|verb| conn.grants.permits(Resource::Config, *verb))
    {
        return forbidden("Principal lacks the config grant required for this method");
    }

    request.extensions_mut().insert(RequestAuth {
        principal: Some(Arc::new(conn)),
        authority: Arc::clone(&auth),
    });
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
        router_and_authority_for(config).0
    }

    fn router_and_authority_for(config: Config) -> (Router, Arc<GatewayInboundAuth>) {
        let state = AppState {
            pairing: Arc::new(PairingGuard::new(
                config.gateway.require_pairing,
                &config.gateway.paired_tokens,
                zeroclaw_config::pairing::PairingCodePolicy::default(),
            )),
            ..crate::api::tests::test_state(config.clone())
        };
        let auth = Arc::new(
            GatewayInboundAuth::from_config(&config, Arc::clone(&state.pairing))
                .expect("inbound auth builds from a valid config"),
        );
        (crate::config_admin_router(&auth).with_state(state), auth)
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
        // A CORS preflight without credentials gets the intended answer:
        // 204 with the allow list, not a denial.
        let request = HttpRequest::builder()
            .method("OPTIONS")
            .uri("/api/config")
            .header("access-control-request-method", "PATCH")
            .body(Body::empty())
            .unwrap();
        let response = router.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert_eq!(
            response
                .headers()
                .get("access-control-allow-methods")
                .and_then(|v| v.to_str().ok()),
            Some("GET, PUT, PATCH, OPTIONS")
        );
        // A bare OPTIONS serves the schema document, also unauthenticated.
        let (status, _) = send(&router, "OPTIONS", "/api/config", None, None, None).await;
        assert_eq!(status, StatusCode::OK);
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

    #[tokio::test]
    async fn a_present_but_blank_or_non_text_provider_header_is_refused() {
        // Open posture, so an "absent" header would pass: a blank one must
        // not be read as absent.
        let router = router_for(open_config());
        let (status, body) = send(
            &router,
            "GET",
            "/api/quickstart/state",
            Some("whatever"),
            Some("   "),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body["error"], "Invalid auth_provider selection");

        let request = HttpRequest::builder()
            .method("GET")
            .uri("/api/quickstart/state")
            .header("authorization", "Bearer whatever")
            .header(
                AUTH_PROVIDER_HEADER,
                axum::http::HeaderValue::from_bytes(&[0xff]).unwrap(),
            )
            .body(Body::empty())
            .unwrap();
        let response = router.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
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
                "token_type": "Bearer",
                "client_id": "gw",
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
                // The provider classifies the actor from an operator
                // declaration; without one a verified token has no declared
                // kind and is refused. This fixture's client is interactive.
                interactive_clients: vec!["gw".into()],
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

    // ── Write sets: verbs by effect, selectors over the complete mutation ──

    const SCOPED: (Option<&str>, Option<&str>) = (Some("opaque-token"), Some("oidc.test"));
    const OPERATOR: (Option<&str>, Option<&str>) = (Some("zc_paired"), None);

    /// The mapped profile with the given Config verbs and path selectors,
    /// on a configuration that persists into `tmp` rather than the home
    /// directory.
    fn editor_config(
        tmp: &tempfile::TempDir,
        issuer: &str,
        verbs: &[Verb],
        paths: &[&str],
    ) -> Config {
        let mut config = oidc_config_with_reader_profile(issuer);
        config.permission_profiles.insert(
            "config-reader".into(),
            PermissionProfileConfig {
                grants: HashMap::from([(Resource::Config, verbs.to_vec())]),
                config_write_paths: paths.iter().map(|p| (*p).to_string()).collect(),
                ..PermissionProfileConfig::default()
            },
        );
        config.config_path = tmp.path().join("config.toml");
        config.data_dir = tmp.path().join("data");
        config
    }

    async fn create_agent(router: &Router, alias: &str) {
        let (status, body) = send(
            router,
            "POST",
            &format!("/api/config/map-key?path=agents&key={alias}"),
            OPERATOR.0,
            OPERATOR.1,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "fixture agent `{alias}`: {body}");
        assert_eq!(body["created"], true, "fixture agent `{alias}`: {body}");
    }

    async fn agent_model(router: &Router, alias: &str) -> (StatusCode, serde_json::Value) {
        let (status, body) = send(
            router,
            "GET",
            &format!("/api/config/prop?path=agents.{alias}.workspace.path"),
            OPERATOR.0,
            OPERATOR.1,
            None,
        )
        .await;
        (status, body["value"].clone())
    }

    fn put_model(alias: &str, model: &str) -> serde_json::Value {
        serde_json::json!({ "path": format!("agents.{alias}.workspace.path"), "value": model })
    }

    #[tokio::test]
    async fn verbs_follow_the_effect_of_the_mutation_not_the_http_method() {
        let tmp = tempfile::tempdir().unwrap();
        let idp = introspection_idp(&["ops"]).await;
        let router = router_for(editor_config(
            &tmp,
            &idp.uri(),
            &[Verb::Update],
            &["agents.*"],
        ));
        create_agent(&router, "alpha").await;

        // Updating an existing path is what the grant covers.
        let (status, body) = send(
            &router,
            "PUT",
            "/api/config/prop",
            SCOPED.0,
            SCOPED.1,
            Some(put_model("alpha", "m2")),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(agent_model(&router, "alpha").await.1, "m2");

        // A map-key creation is a POST, but the operation is a `create`.
        let (status, body) = send(
            &router,
            "POST",
            "/api/config/map-key?path=agents&key=beta",
            SCOPED.0,
            SCOPED.1,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert!(
            body["error"].as_str().unwrap().contains("`create`"),
            "{body}"
        );
        assert_eq!(agent_model(&router, "beta").await.0, StatusCode::NOT_FOUND);

        // So is a PUT that would bring a new map key into being.
        let (status, _) = send(
            &router,
            "PUT",
            "/api/config/prop",
            SCOPED.0,
            SCOPED.1,
            Some(put_model("gamma", "m1")),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(agent_model(&router, "gamma").await.0, StatusCode::NOT_FOUND);

        // Removing the entry is a `delete`, whatever the method.
        let (status, body) = send(
            &router,
            "DELETE",
            "/api/config/map-key?path=agents&key=alpha",
            SCOPED.0,
            SCOPED.1,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert!(
            body["error"].as_str().unwrap().contains("`delete`"),
            "{body}"
        );
        assert_eq!(agent_model(&router, "alpha").await.1, "m2");

        // A JSON Patch `remove` is a delete too, inside a PATCH.
        let (status, body) = send(
            &router,
            "PATCH",
            "/api/config",
            SCOPED.0,
            SCOPED.1,
            Some(serde_json::json!([{ "op": "remove", "path": "/agents/alpha/workspace/path" }])),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert!(
            body["error"].as_str().unwrap().contains("`delete`"),
            "{body}"
        );
        assert_eq!(agent_model(&router, "alpha").await.1, "m2");
    }

    #[tokio::test]
    async fn explicit_create_and_delete_grants_admit_those_operations() {
        let tmp = tempfile::tempdir().unwrap();
        let idp = introspection_idp(&["ops"]).await;
        let router = router_for(editor_config(
            &tmp,
            &idp.uri(),
            &[Verb::Create, Verb::Update, Verb::Delete],
            &["agents.*"],
        ));

        let (status, body) = send(
            &router,
            "POST",
            "/api/config/map-key?path=agents&key=beta",
            SCOPED.0,
            SCOPED.1,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["created"], true);

        let (status, body) = send(
            &router,
            "PATCH",
            "/api/config",
            SCOPED.0,
            SCOPED.1,
            Some(serde_json::json!([{ "op": "remove", "path": "/agents/beta/workspace/path" }])),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");

        let (status, body) = send(
            &router,
            "DELETE",
            "/api/config/map-key?path=agents&key=beta",
            SCOPED.0,
            SCOPED.1,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(agent_model(&router, "beta").await.0, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn path_selectors_are_enforced_on_the_complete_mutation() {
        let tmp = tempfile::tempdir().unwrap();
        let idp = introspection_idp(&["ops"]).await;
        let router = router_for(editor_config(
            &tmp,
            &idp.uri(),
            &[Verb::Create, Verb::Update, Verb::Delete],
            &["agents.alpha.*"],
        ));
        create_agent(&router, "alpha").await;
        create_agent(&router, "beta").await;

        let (status, body) = send(
            &router,
            "PUT",
            "/api/config/prop",
            SCOPED.0,
            SCOPED.1,
            Some(put_model("alpha", "m2")),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");

        let (status, body) = send(
            &router,
            "PUT",
            "/api/config/prop",
            SCOPED.0,
            SCOPED.1,
            Some(put_model("beta", "m2")),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert!(
            body["error"]
                .as_str()
                .unwrap()
                .contains("do not cover `agents.beta.workspace.path`"),
            "{body}"
        );

        // One out-of-scope member refuses the whole batch: the in-scope
        // member does not land either.
        let (status, _) = send(
            &router,
            "PATCH",
            "/api/config",
            SCOPED.0,
            SCOPED.1,
            Some(serde_json::json!([
                { "op": "replace", "path": "/agents/alpha/workspace/path", "value": "m3" },
                { "op": "replace", "path": "/agents/beta/workspace/path", "value": "m3" },
            ])),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(agent_model(&router, "alpha").await.1, "m2");

        // A rename writes its destination as well as its source.
        let (status, _) = send(
            &router,
            "POST",
            "/api/config/rename-map-key",
            SCOPED.0,
            SCOPED.1,
            Some(serde_json::json!({ "path": "agents", "from": "alpha", "to": "gamma" })),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(agent_model(&router, "alpha").await.0, StatusCode::OK);
        assert_eq!(agent_model(&router, "gamma").await.0, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn whole_configuration_rewrites_need_the_wildcard_selector() {
        let idp = introspection_idp(&["ops"]).await;
        let verbs = [Verb::Create, Verb::Update, Verb::Delete];

        let tmp = tempfile::tempdir().unwrap();
        let router = router_for(editor_config(&tmp, &idp.uri(), &verbs, &["agents.*"]));
        create_agent(&router, "alpha").await;
        let (status, body) = send(
            &router,
            "POST",
            "/api/config/migrate",
            SCOPED.0,
            SCOPED.1,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert!(body["error"].as_str().unwrap().contains("`*`"), "{body}");

        let tmp = tempfile::tempdir().unwrap();
        let router = router_for(editor_config(&tmp, &idp.uri(), &verbs, &["*"]));
        create_agent(&router, "alpha").await;
        let (status, body) = send(
            &router,
            "POST",
            "/api/config/migrate",
            SCOPED.0,
            SCOPED.1,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }

    #[tokio::test]
    async fn methods_outside_the_known_set_fail_closed() {
        let tmp = tempfile::tempdir().unwrap();
        let idp = introspection_idp(&["ops"]).await;
        let router = router_for(editor_config(
            &tmp,
            &idp.uri(),
            &[Verb::Read, Verb::Create, Verb::Update, Verb::Delete],
            &["*"],
        ));
        let (status, body) = send(&router, "TRACE", "/api/config", SCOPED.0, SCOPED.1, None).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(body["error"], "Unsupported method for the config surface");
    }

    // ── Policy moves only at the persist boundary ────────────────────────

    #[tokio::test]
    async fn policy_is_bound_to_the_accepted_revision_not_to_the_request() {
        let tmp = tempfile::tempdir().unwrap();
        let idp = introspection_idp(&["ops"]).await;
        let (router, authority) =
            router_and_authority_for(editor_config(&tmp, &idp.uri(), &[Verb::Read], &[]));
        let generation = authority.generation();

        // C0 admits the principal through its `groups` claim.
        let (status, _) = send(
            &router,
            "GET",
            "/api/quickstart/state",
            SCOPED.0,
            SCOPED.1,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        // C1, persisted by the operator, tightens the provider: the claim
        // the profile map reads no longer exists in the token.
        let (status, body) = send(
            &router,
            "PUT",
            "/api/config/prop",
            OPERATOR.0,
            OPERATOR.1,
            Some(serde_json::json!({ "path": "oidc.test.claim_path", "value": "roles" })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(
            authority.generation(),
            generation + 1,
            "the persist published exactly one new accepted revision"
        );

        // The same bearer is now refused against the accepted C1 policy,
        // with no request having recompiled anything.
        let (status, _) = send(
            &router,
            "GET",
            "/api/quickstart/state",
            SCOPED.0,
            SCOPED.1,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);

        // A later persist that touches no authorization input publishes no
        // new generation, and crucially does not roll C1's tightening back.
        create_agent(&router, "alpha").await;
        assert_eq!(
            authority.generation(),
            generation + 1,
            "an unrelated persist republishes without moving the generation"
        );
        let (status, _) = send(
            &router,
            "GET",
            "/api/quickstart/state",
            SCOPED.0,
            SCOPED.1,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }
}
