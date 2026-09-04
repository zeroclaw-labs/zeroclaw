//! Cross-surface OIDC enrollment API.
//!
//! Design: `docs/security/oidc-browser-pkce-design-8289.md`. These routes
//! are unauthenticated by necessity (enrollment precedes authentication),
//! rate limited, and grant nothing: they relay what the IdP grants after
//! the user approves. The gateway holds the `[oidc.<alias>]` client
//! credentials so browsers and zerocode need none.
//!
//! - `GET  /api/oidc/providers` lists configured aliases.
//! - `POST /api/oidc/{alias}/device/start` proxies the RFC 8628 start.
//! - `POST /api/oidc/{alias}/device/poll` proxies one token poll.
//! - `GET  /oidc/login/{alias}` starts Authorization Code + PKCE (302).
//! - `GET  /oidc/callback` finishes it: the one-time page hands the token
//!   to `window.opener` via `postMessage` (gateway origin only) with a
//!   manual copy fallback. No cookies, no session: clients keep
//!   authenticating per request with the route-layer headers.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::{
    Extension, Json, Router,
    extract::{ConnectInfo, Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
};
use serde::Deserialize;
use zeroclaw_runtime::security::auth_provider::{DevicePollOutcome, Enrollment, PkceFlow};

use crate::AppState;

const FLOW_TTL: Duration = Duration::from_secs(600);
const FLOW_CAP: usize = 32;

struct PendingPkce {
    alias: String,
    flow: PkceFlow,
    created: Instant,
}

/// In-flight PKCE flows keyed by `state`, following the pairing-store
/// posture: in-memory, single-use consume-on-arrival, short TTL, capped.
#[derive(Default)]
pub struct OidcFlowStore {
    flows: parking_lot::Mutex<HashMap<String, PendingPkce>>,
}

impl OidcFlowStore {
    fn insert(&self, pending: PendingPkce) -> Result<(), &'static str> {
        let mut flows = self.flows.lock();
        flows.retain(|_, p| p.created.elapsed() < FLOW_TTL);
        if flows.len() >= FLOW_CAP {
            return Err("too many in-flight sign-ins; retry shortly");
        }
        flows.insert(pending.flow.state.clone(), pending);
        Ok(())
    }

    fn consume(&self, state_key: &str) -> Option<PendingPkce> {
        let mut flows = self.flows.lock();
        flows.retain(|_, p| p.created.elapsed() < FLOW_TTL);
        flows.remove(state_key)
    }
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/oidc/providers", get(handle_providers))
        .route("/api/oidc/{alias}/device/start", post(handle_device_start))
        .route("/api/oidc/{alias}/device/poll", post(handle_device_poll))
        .route("/oidc/login/{alias}", get(handle_pkce_login))
        .route("/oidc/callback", get(handle_pkce_callback))
        .layer(Extension(Arc::new(OidcFlowStore::default())))
}

/// Rate limiting for the enrollment surface. `record` distinguishes
/// flow-starting requests (counted) from polls (checked but not counted,
/// or a legitimate device flow would exhaust its own budget; the IdP
/// throttles polling itself via `slow_down`).
fn rate_limit(
    state: &AppState,
    peer: SocketAddr,
    headers: &HeaderMap,
    record: bool,
) -> Result<(), Box<Response>> {
    let key = crate::client_key_from_request(Some(peer), headers, state.trust_forwarded_headers);
    if let Err(e) = state.auth_limiter.check_rate_limit(&key) {
        return Err(Box::new(
            (
                StatusCode::TOO_MANY_REQUESTS,
                Json(serde_json::json!({
                    "error": format!(
                        "Too many enrollment attempts. Try again in {}s.",
                        e.retry_after_secs
                    ),
                    "retry_after": e.retry_after_secs,
                })),
            )
                .into_response(),
        ));
    }
    if record {
        state.auth_limiter.record_attempt(&key);
    }
    Ok(())
}

fn error_json(status: StatusCode, message: &str) -> Response {
    (status, Json(serde_json::json!({ "error": message }))).into_response()
}

/// Build the enrollment client for a configured alias.
fn enrollment_for(state: &AppState, alias: &str) -> Result<Enrollment, Box<Response>> {
    let entry = {
        let config = state.config.read();
        config.oidc.get(alias).cloned()
    };
    let Some(entry) = entry else {
        return Err(Box::new(error_json(
            StatusCode::NOT_FOUND,
            "unknown oidc provider alias",
        )));
    };
    Enrollment::new(entry).map_err(|_| {
        Box::new(error_json(
            StatusCode::INTERNAL_SERVER_ERROR,
            "enrollment client construction failed",
        ))
    })
}

async fn handle_providers(State(state): State<AppState>) -> Response {
    let mut aliases: Vec<String> = state.config.read().oidc.keys().cloned().collect();
    aliases.sort();
    let providers: Vec<serde_json::Value> = aliases
        .iter()
        .map(|alias| {
            serde_json::json!({
                "alias": alias,
                "provider": format!("oidc.{alias}"),
            })
        })
        .collect();
    Json(serde_json::json!({ "providers": providers })).into_response()
}

async fn handle_device_start(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path(alias): Path<String>,
    headers: HeaderMap,
) -> Response {
    if let Err(denied) = rate_limit(&state, peer, &headers, true) {
        return *denied;
    }
    let enrollment = match enrollment_for(&state, &alias) {
        Ok(enrollment) => enrollment,
        Err(response) => return *response,
    };
    match enrollment.device_grant_start().await {
        Ok(start) => Json(start).into_response(),
        Err(e) => {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"alias": alias, "error": format!("{e}")})),
                "oidc device enrollment start failed"
            );
            error_json(StatusCode::BAD_GATEWAY, &format!("{e}"))
        }
    }
}

#[derive(Debug, Deserialize)]
struct DevicePollBody {
    device_code: String,
}

async fn handle_device_poll(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path(alias): Path<String>,
    headers: HeaderMap,
    Json(body): Json<DevicePollBody>,
) -> Response {
    if let Err(denied) = rate_limit(&state, peer, &headers, false) {
        return *denied;
    }
    let enrollment = match enrollment_for(&state, &alias) {
        Ok(enrollment) => enrollment,
        Err(response) => return *response,
    };
    match enrollment.device_grant_poll(&body.device_code).await {
        Ok(DevicePollOutcome::Pending) => {
            Json(serde_json::json!({ "status": "pending" })).into_response()
        }
        Ok(DevicePollOutcome::SlowDown) => {
            Json(serde_json::json!({ "status": "slow_down" })).into_response()
        }
        Ok(DevicePollOutcome::Token(token)) => Json(serde_json::json!({
            "status": "granted",
            "provider": format!("oidc.{alias}"),
            "token": *token,
        }))
        .into_response(),
        Err(e) => error_json(StatusCode::BAD_GATEWAY, &format!("{e}")),
    }
}

/// The callback URI this deployment's browser flow lands on, derived
/// from the request the user's browser just made. A spoofed Host cannot
/// redeem anything: the IdP only accepts registered redirect URIs, and
/// the exchange later uses the exact URI stored at flow start.
fn callback_uri(state: &AppState, headers: &HeaderMap) -> Result<String, Box<Response>> {
    let forwarded_proto = state
        .trust_forwarded_headers
        .then(|| headers.get("x-forwarded-proto"))
        .flatten()
        .and_then(|v| v.to_str().ok());
    let scheme = match forwarded_proto {
        Some(proto) => proto.to_string(),
        None => {
            let tls_on = state.config.read().gateway.tls.is_some();
            if tls_on {
                "https".into()
            } else {
                "http".into()
            }
        }
    };
    let host = headers
        .get(axum::http::header::HOST)
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| Box::new(error_json(StatusCode::BAD_REQUEST, "missing Host header")))?;
    Ok(format!(
        "{scheme}://{host}{}/oidc/callback",
        state.path_prefix
    ))
}

async fn handle_pkce_login(
    State(state): State<AppState>,
    Extension(flows): Extension<Arc<OidcFlowStore>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path(alias): Path<String>,
    headers: HeaderMap,
) -> Response {
    if let Err(denied) = rate_limit(&state, peer, &headers, true) {
        return *denied;
    }
    let enrollment = match enrollment_for(&state, &alias) {
        Ok(enrollment) => enrollment,
        Err(response) => return *response,
    };
    let redirect_uri = match callback_uri(&state, &headers) {
        Ok(uri) => uri,
        Err(response) => return *response,
    };
    let flow = match enrollment.pkce_start(&redirect_uri).await {
        Ok(flow) => flow,
        Err(e) => return error_json(StatusCode::BAD_GATEWAY, &format!("{e}")),
    };
    let authorize_url = flow.authorize_url.clone();
    if let Err(full) = flows.insert(PendingPkce {
        alias,
        flow,
        created: Instant::now(),
    }) {
        return error_json(StatusCode::TOO_MANY_REQUESTS, full);
    }
    Redirect::temporary(&authorize_url).into_response()
}

#[derive(Debug, Deserialize)]
struct CallbackQuery {
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    error_description: Option<String>,
}

/// Fixed failure page: never echoes request content. Details go to the
/// server log only.
fn failure_page(status: StatusCode) -> Response {
    (
        status,
        Html(
            "<!doctype html><meta charset=\"utf-8\"><title>Sign-in not completed</title>\
             <body style=\"font-family:system-ui;margin:3rem\"><h1>Sign-in not completed</h1>\
             <p>This request did not complete an enrollment. Start again from the \
             application, or check the gateway log for details.</p>",
        ),
    )
        .into_response()
}

/// One-time success page (decision 4 of the design note): hands the
/// token to `window.opener` via `postMessage` with this page's own
/// origin as the target, plus a manual copy fallback. The token reaches
/// the page only as a JSON string literal inside the inline script.
fn success_page(provider: &str, access_token: &str, expires_in: Option<u64>) -> Response {
    // `</` cannot appear inside the script element, whatever the IdP
    // returned; JSON string escaping handles the rest.
    let token_json = serde_json::to_string(access_token)
        .unwrap_or_else(|_| "\"\"".into())
        .replace("</", "<\\/");
    let provider_json = serde_json::to_string(provider)
        .unwrap_or_else(|_| "\"\"".into())
        .replace("</", "<\\/");
    let expires_note = expires_in
        .map(|secs| format!("<p>The token expires in {secs} seconds.</p>"))
        .unwrap_or_default();
    Html(format!(
        "<!doctype html><meta charset=\"utf-8\"><title>Signed in</title>\
         <body style=\"font-family:system-ui;margin:3rem\">\
         <h1>Signed in</h1>\
         <p>Enrollment is complete. If the application does not pick the token up \
         automatically, copy it below and close this tab.</p>{expires_note}\
         <p><code id=\"token\" style=\"word-break:break-all\"></code></p>\
         <script>(function () {{\
           var token = {token_json};\
           var provider = {provider_json};\
           document.getElementById(\"token\").textContent = token;\
           try {{\
             if (window.opener) {{\
               window.opener.postMessage(\
                 {{ type: \"zeroclaw-oidc-token\", provider: provider, access_token: token }},\
                 window.location.origin\
               );\
             }}\
           }} catch (e) {{}}\
         }})();</script>",
    ))
    .into_response()
}

async fn handle_pkce_callback(
    State(state): State<AppState>,
    Extension(flows): Extension<Arc<OidcFlowStore>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Query(query): Query<CallbackQuery>,
) -> Response {
    if let Err(denied) = rate_limit(&state, peer, &headers, true) {
        return *denied;
    }
    // State gates everything: without a live matching flow there is
    // nothing to fail, let alone finish.
    let Some(pending) = query.state.as_deref().and_then(|s| flows.consume(s)) else {
        return failure_page(StatusCode::BAD_REQUEST);
    };
    if let Some(error) = query.error {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                .with_attrs(::serde_json::json!({
                    "alias": pending.alias,
                    "error": error,
                    "error_description": query.error_description,
                })),
            "oidc browser sign-in denied by the identity provider"
        );
        return failure_page(StatusCode::BAD_REQUEST);
    }
    let Some(code) = query.code else {
        return failure_page(StatusCode::BAD_REQUEST);
    };
    let enrollment = match enrollment_for(&state, &pending.alias) {
        Ok(enrollment) => enrollment,
        // The alias was removed while the flow was in flight: fail closed.
        Err(_) => return failure_page(StatusCode::BAD_REQUEST),
    };
    match enrollment.pkce_exchange(&pending.flow, &code).await {
        Ok(token) => success_page(
            &format!("oidc.{}", pending.alias),
            &token.access_token,
            token.expires_in,
        ),
        Err(e) => {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "alias": pending.alias,
                        "error": format!("{e}"),
                    })),
                "oidc browser sign-in code exchange failed"
            );
            failure_page(StatusCode::BAD_GATEWAY)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request as HttpRequest;
    use http_body_util::BodyExt as _;
    use tower::ServiceExt as _;
    use wiremock::matchers::{body_string_contains, method as http_method, path as http_path};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    use zeroclaw_config::schema::{Config, OidcConfig, OidcValidation};

    async fn idp() -> MockServer {
        let server = MockServer::start().await;
        let issuer = server.uri();
        Mock::given(http_method("GET"))
            .and(http_path("/.well-known/openid-configuration"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "issuer": issuer,
                "authorization_endpoint": format!("{issuer}/authorize"),
                "token_endpoint": format!("{issuer}/token"),
                "device_authorization_endpoint": format!("{issuer}/device"),
                "code_challenge_methods_supported": ["S256"],
            })))
            .mount(&server)
            .await;
        server
    }

    fn config_with_alias(issuer: &str, alias: &str) -> Config {
        let mut config = Config::default();
        config.oidc.insert(
            alias.to_string(),
            OidcConfig {
                issuer: issuer.to_string(),
                audience: "zeroclaw".into(),
                client_id: "gw".into(),
                client_secret: Some("s3cret".into()),
                validation: OidcValidation::Introspection,
                claim_path: "groups".into(),
                profile_map: HashMap::from([("ops".to_string(), "operator".to_string())]),
                ..OidcConfig::default()
            },
        );
        config
    }

    fn router_for(config: Config) -> Router {
        routes().with_state(crate::api::tests::test_state(config))
    }

    async fn send(
        router: &Router,
        http_method: &str,
        path: &str,
        host: &str,
        body: Option<serde_json::Value>,
    ) -> axum::response::Response {
        let mut builder = HttpRequest::builder()
            .method(http_method)
            .uri(path)
            .header("host", host);
        if body.is_some() {
            builder = builder.header("content-type", "application/json");
        }
        let mut request = match body {
            Some(json) => builder.body(Body::from(json.to_string())).unwrap(),
            None => builder.body(Body::empty()).unwrap(),
        };
        request
            .extensions_mut()
            .insert(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 39999))));
        router.clone().oneshot(request).await.unwrap()
    }

    async fn body_text(response: axum::response::Response) -> String {
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        String::from_utf8_lossy(&bytes).into_owned()
    }

    #[tokio::test]
    async fn providers_lists_configured_aliases_sorted() {
        let mut config = config_with_alias("https://a.example.com", "zeta");
        config
            .oidc
            .insert("alpha".into(), config.oidc.get("zeta").cloned().unwrap());
        let router = router_for(config);
        let response = send(&router, "GET", "/api/oidc/providers", "gw.local", None).await;
        assert_eq!(response.status(), StatusCode::OK);
        let json: serde_json::Value = serde_json::from_str(&body_text(response).await).unwrap();
        assert_eq!(json["providers"][0]["alias"], "alpha");
        assert_eq!(json["providers"][1]["provider"], "oidc.zeta");
    }

    #[tokio::test]
    async fn unknown_alias_is_a_404() {
        let router = router_for(Config::default());
        let response = send(
            &router,
            "POST",
            "/api/oidc/nope/device/start",
            "gw.local",
            None,
        )
        .await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn device_proxy_start_and_poll_round_trip() {
        let server = idp().await;
        Mock::given(http_method("POST"))
            .and(http_path("/device"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "device_code": "dev-9",
                "user_code": "WXYZ-1234",
                "verification_uri": "https://sso.example.com/activate",
                "expires_in": 600,
                "interval": 5,
            })))
            .mount(&server)
            .await;
        Mock::given(http_method("POST"))
            .and(http_path("/token"))
            .and(body_string_contains("device_code=still-pending"))
            .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "error": "authorization_pending",
            })))
            .mount(&server)
            .await;
        Mock::given(http_method("POST"))
            .and(http_path("/token"))
            .and(body_string_contains("device_code=dev-9"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "at-device",
                "expires_in": 3600,
            })))
            .mount(&server)
            .await;
        let router = router_for(config_with_alias(&server.uri(), "corp"));

        let response = send(
            &router,
            "POST",
            "/api/oidc/corp/device/start",
            "gw.local",
            None,
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let start: serde_json::Value = serde_json::from_str(&body_text(response).await).unwrap();
        assert_eq!(start["user_code"], "WXYZ-1234");

        let response = send(
            &router,
            "POST",
            "/api/oidc/corp/device/poll",
            "gw.local",
            Some(serde_json::json!({"device_code": "still-pending"})),
        )
        .await;
        let poll: serde_json::Value = serde_json::from_str(&body_text(response).await).unwrap();
        assert_eq!(poll["status"], "pending");

        let response = send(
            &router,
            "POST",
            "/api/oidc/corp/device/poll",
            "gw.local",
            Some(serde_json::json!({"device_code": "dev-9"})),
        )
        .await;
        let poll: serde_json::Value = serde_json::from_str(&body_text(response).await).unwrap();
        assert_eq!(poll["status"], "granted");
        assert_eq!(poll["provider"], "oidc.corp");
        assert_eq!(poll["token"]["access_token"], "at-device");
    }

    #[tokio::test]
    async fn pkce_login_redirects_to_the_idp_with_s256_and_our_callback() {
        let server = idp().await;
        let router = router_for(config_with_alias(&server.uri(), "corp"));
        let response = send(&router, "GET", "/oidc/login/corp", "gw.local:9443", None).await;
        assert_eq!(response.status(), StatusCode::TEMPORARY_REDIRECT);
        let location = response
            .headers()
            .get("location")
            .and_then(|v| v.to_str().ok())
            .unwrap()
            .to_string();
        assert!(location.starts_with(&format!("{}/authorize", server.uri())));
        assert!(location.contains("code_challenge_method=S256"));
        assert!(
            location.contains("gw.local%3A9443%2Foidc%2Fcallback"),
            "redirect_uri derives from the request Host: {location}"
        );
    }

    #[tokio::test]
    async fn callback_without_a_live_flow_is_refused() {
        let router = router_for(Config::default());
        let response = send(
            &router,
            "GET",
            "/oidc/callback?code=x&state=never-issued",
            "gw.local",
            None,
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert!(body_text(response).await.contains("Sign-in not completed"));
    }

    fn state_from_location(location: &str) -> String {
        let query = location.split('?').nth(1).unwrap();
        query
            .split('&')
            .find_map(|pair| pair.strip_prefix("state="))
            .unwrap()
            .to_string()
    }

    #[tokio::test]
    async fn pkce_callback_exchanges_once_and_only_once() {
        let server = idp().await;
        Mock::given(http_method("POST"))
            .and(http_path("/token"))
            .and(body_string_contains("grant_type=authorization_code"))
            .and(body_string_contains("code_verifier="))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "at-browser",
                "expires_in": 3600,
            })))
            .mount(&server)
            .await;
        let router = router_for(config_with_alias(&server.uri(), "corp"));

        let response = send(&router, "GET", "/oidc/login/corp", "gw.local", None).await;
        let location = response
            .headers()
            .get("location")
            .and_then(|v| v.to_str().ok())
            .unwrap()
            .to_string();
        let flow_state = state_from_location(&location);

        let response = send(
            &router,
            "GET",
            &format!("/oidc/callback?code=auth-1&state={flow_state}"),
            "gw.local",
            None,
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let page = body_text(response).await;
        assert!(page.contains("zeroclaw-oidc-token"), "postMessage handoff");
        assert!(page.contains("\"at-browser\""), "token as a JSON literal");
        assert!(page.contains("oidc.corp"));

        // Single use: the same state cannot be redeemed twice.
        let response = send(
            &router,
            "GET",
            &format!("/oidc/callback?code=auth-1&state={flow_state}"),
            "gw.local",
            None,
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn idp_error_on_callback_consumes_the_flow_and_shows_the_fixed_page() {
        let server = idp().await;
        let router = router_for(config_with_alias(&server.uri(), "corp"));
        let response = send(&router, "GET", "/oidc/login/corp", "gw.local", None).await;
        let location = response
            .headers()
            .get("location")
            .and_then(|v| v.to_str().ok())
            .unwrap()
            .to_string();
        let flow_state = state_from_location(&location);
        let response = send(
            &router,
            "GET",
            &format!(
                "/oidc/callback?error=access_denied&error_description=nope&state={flow_state}"
            ),
            "gw.local",
            None,
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let page = body_text(response).await;
        assert!(page.contains("Sign-in not completed"));
        assert!(
            !page.contains("access_denied") && !page.contains("nope"),
            "the failure page never echoes request content"
        );
    }
}
