//! The preview `zeroclaw-gw` process: the gateway running outside the
//! daemon, reaching the core only through the daemon's local socket.
//!
//! This is a stretch preview, not a supported deployment. It serves the
//! static dashboard, the OpenAPI document, its own health, a core-link
//! diagnostic, its own shutdown, the Claude Code hook, and the dashboard
//! routes ported onto the core's RPC surface, with the same bodies the
//! in-process gateway answers when a request reaches the core. Every other route the in-process gateway serves
//! answers a distinct JSON refusal naming the route; nothing falls through
//! to in-process state or to the dashboard's page fallback. Routes join as
//! they are ported.
//!
//! Every route sits behind the in-process gateway's request limits: a body
//! of at most [`crate::MAX_BODY_SIZE`] bytes (`413` beyond it) and an answer
//! within the request timeout (`408` after it), so a slow or oversized
//! request is bounded here as it is there, shutdown included.
//!
//! It serves only through a core of its own version: a core of another
//! version speaks the same protocol but may ignore what this gateway asks
//! for, so every core-backed route answers `503 core_version_mismatch`
//! instead, while its own routes keep answering. `--allow-version-skew`
//! lifts that, for development only.
//!
//! It is configured from flags and the environment only and never reads
//! `config.toml`. It runs as the same OS account as the core: every
//! connection verifies through the kernel that this account serves the
//! endpoint before the caller's credential is written, and every request
//! needs a credential of its own. Windows cannot make that check yet, so
//! the preview refuses to start there.

use std::future::Future;
use std::io::Write as _;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::Router;
use axum::extract::rejection::{JsonRejection, PathRejection, QueryRejection};
use axum::extract::{ConnectInfo, Path as UrlPath, Query, State};
use axum::http::{Method as HttpMethod, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::{MethodFilter, MethodRouter, get, on, post};
use serde_json::json;
use tokio::sync::watch;
use zeroclaw_rpc_client::{
    ClientError, EndpointOwner, EndpointRejection, Method, RPC_PROTOCOL_VERSION, RpcClient,
};

use crate::api::CostQuery;
use crate::api_personality::{AgentQuery, PersonalityPutBody, TemplateQuery};
use crate::api_skills::{DeleteQuery, SkillWriteBody};
use crate::core_rpc::{CoreAccess, CoreCall, CoreError, CoreRpc, VersionSkew};

/// Where the dashboard reaches when no `--listen` is given: the address the
/// in-process gateway uses.
pub const DEFAULT_LISTEN: &str = "127.0.0.1:42617";

/// The preview's core-link diagnostic: which principal the caller's
/// credential binds and which core answers.
pub const CORE_LINK_PATH: &str = "/api/gateway/core";

/// How long the health probe waits to reach the core's endpoint.
const HEALTH_PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// What to do when the core is down or busy.
const HINT_START_CORE: &str =
    "Start the core with `zeroclaw daemon`, or point zeroclaw-gw at its socket with --socket PATH.";
/// What to do when the endpoint fails the same-account check.
const HINT_SAME_ACCOUNT: &str = "Run zeroclaw-gw as the same OS account as the core, and keep the \
     core's socket in a directory only that account can write.";

pub const USAGE: &str = "\
zeroclaw-gw: preview of the gateway as its own process

Usage: zeroclaw-gw [--socket PATH | --data-dir DIR] [options]

The core's endpoint (one is required; zeroclaw-gw never reads config.toml):
  --socket PATH         the core's local socket (also ZEROCLAW_SOCKET)
  --data-dir DIR        the core's data directory; the socket is DIR/daemon.sock

Options:
  --listen ADDR         address to serve on [default: 127.0.0.1:42617]
  --allow-public-bind   allow a non-loopback --listen address
  --web-dist DIR        serve the dashboard from DIR (must hold index.html)
  --tls-cert PEM        serve HTTPS with this certificate (needs --tls-key)
  --tls-key PEM         the certificate's private key (needs --tls-cert)
  --request-timeout SECS
                        answer 408 to a request not done within SECS
                        [default: 30, the in-process gateway's default]
  --allow-version-skew  serve through a core of another version (development
                        only: answers may silently lack what was asked for)
  -h, --help            print this help
  -V, --version         print the version

Runs as the same OS account as the core, on Unix. Serves only through a
core of its own version unless --allow-version-skew is given. On start it
prints `READY <url>` on stdout once it is serving. It stops on SIGINT or
SIGTERM, or on `POST /admin/shutdown` from this machine.";

/// Everything the preview needs to start, from flags and the environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bootstrap {
    pub listen: SocketAddr,
    pub endpoint: PathBuf,
    pub web_dist: Option<PathBuf>,
    pub tls: Option<TlsFiles>,
    /// How long a request may take before it is answered `408`.
    pub request_timeout: Duration,
    /// Refused unless `--allow-version-skew` is given.
    pub version_skew: VersionSkew,
}

/// The PEM files for serving HTTPS.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TlsFiles {
    pub cert: PathBuf,
    pub key: PathBuf,
}

/// What an invocation asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Invocation {
    Serve(Bootstrap),
    Help,
    Version,
}

/// Parse the command line (without the program name). `socket_env` is the
/// value of `ZEROCLAW_SOCKET`, used when no `--socket` is given.
pub fn parse_args(
    args: impl IntoIterator<Item = String>,
    socket_env: Option<String>,
) -> Result<Invocation, String> {
    let mut listen: Option<String> = None;
    let mut socket: Option<PathBuf> = None;
    let mut data_dir: Option<PathBuf> = None;
    let mut web_dist: Option<PathBuf> = None;
    let mut tls_cert: Option<PathBuf> = None;
    let mut tls_key: Option<PathBuf> = None;
    let mut request_timeout = Duration::from_secs(crate::REQUEST_TIMEOUT_SECS);
    let mut allow_public_bind = false;
    let mut version_skew = VersionSkew::Refuse;

    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        let mut value = |flag: &str| {
            args.next()
                .filter(|v| !v.is_empty())
                .ok_or_else(|| format!("{flag} needs a value"))
        };
        match arg.as_str() {
            "-h" | "--help" => return Ok(Invocation::Help),
            "-V" | "--version" => return Ok(Invocation::Version),
            "--listen" => listen = Some(value("--listen")?),
            "--socket" => socket = Some(value("--socket")?.into()),
            "--data-dir" => data_dir = Some(value("--data-dir")?.into()),
            "--web-dist" => web_dist = Some(value("--web-dist")?.into()),
            "--tls-cert" => tls_cert = Some(value("--tls-cert")?.into()),
            "--tls-key" => tls_key = Some(value("--tls-key")?.into()),
            "--request-timeout" => {
                let secs = value("--request-timeout")?;
                request_timeout = secs
                    .parse::<u64>()
                    .ok()
                    .filter(|secs| *secs > 0)
                    .map(Duration::from_secs)
                    .ok_or_else(|| {
                        format!("--request-timeout {secs:?} is not a positive number of seconds")
                    })?;
            }
            "--allow-public-bind" => allow_public_bind = true,
            "--allow-version-skew" => version_skew = VersionSkew::Allow,
            "--config" | "--config-dir" => {
                return Err(format!(
                    "{arg}: zeroclaw-gw never reads config.toml; pass --socket PATH or \
                     --data-dir DIR"
                ));
            }
            other => return Err(format!("unknown argument {other:?}; see --help")),
        }
    }

    let endpoint = socket
        .or_else(|| {
            socket_env
                .filter(|v| !v.trim().is_empty())
                .map(PathBuf::from)
        })
        .or_else(|| data_dir.map(|dir| zeroclaw_rpc_client::endpoint::default_endpoint(&dir)))
        .ok_or_else(|| {
            "zeroclaw-gw needs the core's endpoint: pass --socket PATH (or set \
             ZEROCLAW_SOCKET) or --data-dir DIR; it never reads config.toml"
                .to_string()
        })?;

    let listen_text = listen.as_deref().unwrap_or(DEFAULT_LISTEN);
    let listen: SocketAddr = listen_text
        .parse()
        .map_err(|e| format!("--listen {listen_text:?} is not an address: {e}"))?;
    if !listen.ip().is_loopback() && !allow_public_bind {
        return Err(format!(
            "--listen {listen} is not a loopback address; pass --allow-public-bind to serve \
             beyond this machine"
        ));
    }

    let tls = match (tls_cert, tls_key) {
        (Some(cert), Some(key)) => Some(TlsFiles { cert, key }),
        (None, None) => None,
        _ => return Err("--tls-cert and --tls-key go together".into()),
    };

    Ok(Invocation::Serve(Bootstrap {
        listen,
        endpoint,
        web_dist,
        tls,
        request_timeout,
        version_skew,
    }))
}

/// Why the preview refuses a route it does not serve.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Refusal {
    /// A dashboard route not ported onto the core's RPC surface yet. The
    /// caller's credential is checked with the core first, so a caller
    /// without one is told to sign in rather than that the feature is
    /// missing.
    NotPorted,
    /// A route the preview defers by design (pairing exchange, OIDC,
    /// WebAuthn, ACP, nodes, webhook ingress, admin endpoints).
    /// These carry no bearer by nature, so no credential is asked for.
    Deferred(&'static str),
}

/// Every route the in-process gateway registers that the preview does not
/// serve, by path, with its HTTP methods and why it is refused. A test
/// checks this list against the in-process router's source, so a route
/// added there is refused here until someone classifies it.
const REFUSED: &[(&str, &str, Refusal)] = &[
    // Pairing exchange and device enrollment.
    ("/pair", "POST", Refusal::Deferred("pairing")),
    ("/pair/code", "GET", Refusal::Deferred("pairing")),
    ("/api/pair", "POST", Refusal::Deferred("pairing")),
    (
        "/api/pairing/initiate",
        "POST",
        Refusal::Deferred("pairing"),
    ),
    // OIDC sign-in.
    ("/api/oidc/providers", "GET", Refusal::Deferred("oidc")),
    (
        "/api/oidc/{alias}/device/start",
        "POST",
        Refusal::Deferred("oidc"),
    ),
    (
        "/api/oidc/{alias}/device/poll",
        "POST",
        Refusal::Deferred("oidc"),
    ),
    ("/oidc/login/{alias}", "GET", Refusal::Deferred("oidc")),
    ("/oidc/callback", "GET", Refusal::Deferred("oidc")),
    // WebAuthn.
    (
        "/api/webauthn/register/start",
        "POST",
        Refusal::Deferred("webauthn"),
    ),
    (
        "/api/webauthn/register/finish",
        "POST",
        Refusal::Deferred("webauthn"),
    ),
    (
        "/api/webauthn/auth/start",
        "POST",
        Refusal::Deferred("webauthn"),
    ),
    (
        "/api/webauthn/auth/finish",
        "POST",
        Refusal::Deferred("webauthn"),
    ),
    (
        "/api/webauthn/credentials",
        "GET",
        Refusal::Deferred("webauthn"),
    ),
    (
        "/api/webauthn/credentials/{id}",
        "DELETE",
        Refusal::Deferred("webauthn"),
    ),
    // ACP and node discovery.
    ("/acp", "GET", Refusal::Deferred("acp")),
    ("/ws/nodes", "GET", Refusal::Deferred("nodes")),
    // Webhook and channel ingress, A2A.
    ("/webhook", "POST", Refusal::Deferred("webhook ingress")),
    (
        "/webhook/gmail",
        "POST",
        Refusal::Deferred("webhook ingress"),
    ),
    ("/sop/{*rest}", "POST", Refusal::Deferred("webhook ingress")),
    (
        "/plugin/{path}",
        "GET,POST",
        Refusal::Deferred("webhook ingress"),
    ),
    (
        "/whatsapp",
        "GET,POST",
        Refusal::Deferred("webhook ingress"),
    ),
    (
        "/whatsapp/{alias}",
        "GET,POST",
        Refusal::Deferred("webhook ingress"),
    ),
    ("/linq", "POST", Refusal::Deferred("webhook ingress")),
    (
        "/linq/{alias}",
        "POST",
        Refusal::Deferred("webhook ingress"),
    ),
    (
        "/nextcloud-talk",
        "POST",
        Refusal::Deferred("webhook ingress"),
    ),
    (
        "/nextcloud-talk/{alias}",
        "POST",
        Refusal::Deferred("webhook ingress"),
    ),
    ("/a2a/{alias}", "POST", Refusal::Deferred("a2a")),
    (
        "/.well-known/agents-card.json",
        "GET",
        Refusal::Deferred("a2a"),
    ),
    (
        "/a2a/.well-known/agents-card.json",
        "GET",
        Refusal::Deferred("a2a"),
    ),
    (
        "/a2a/{alias}/.well-known/agent-card.json",
        "GET",
        Refusal::Deferred("a2a"),
    ),
    // Local administration of the core: use the zeroclaw CLI against it.
    ("/admin/reload", "POST", Refusal::Deferred("admin")),
    ("/admin/sop/pending", "GET", Refusal::Deferred("admin")),
    ("/admin/sop/logs", "GET", Refusal::Deferred("admin")),
    ("/admin/sop/approve", "POST", Refusal::Deferred("admin")),
    ("/admin/sop/deny", "POST", Refusal::Deferred("admin")),
    ("/admin/paircode", "GET", Refusal::Deferred("admin")),
    ("/admin/paircode/new", "POST", Refusal::Deferred("admin")),
    ("/metrics", "GET", Refusal::Deferred("metrics")),
    // Dashboard routes not yet served through the core.
    ("/api/status", "GET", Refusal::NotPorted),
    ("/api/logs", "GET", Refusal::NotPorted),
    ("/api/doctor", "GET,POST", Refusal::NotPorted),
    ("/api/events", "GET", Refusal::NotPorted),
    ("/api/version/check", "GET", Refusal::NotPorted),
    ("/api/version/upgrade", "POST", Refusal::NotPorted),
    ("/api/version/upgrade/status", "GET", Refusal::NotPorted),
    ("/api/sessions/running", "GET", Refusal::NotPorted),
    ("/api/sessions/{id}", "DELETE,PUT", Refusal::NotPorted),
    (
        "/api/sessions/{id}/messages",
        "GET,POST",
        Refusal::NotPorted,
    ),
    ("/api/sessions/{id}/state", "GET", Refusal::NotPorted),
    ("/api/sessions/{id}/abort", "POST", Refusal::NotPorted),
    ("/ws/chat", "GET", Refusal::NotPorted),
    ("/ws/sops/runs", "GET", Refusal::NotPorted),
    ("/ws/canvas/{id}", "GET", Refusal::NotPorted),
    ("/api/config", "GET,PATCH,OPTIONS", Refusal::NotPorted),
    (
        "/api/config/prop",
        "GET,PUT,DELETE,OPTIONS",
        Refusal::NotPorted,
    ),
    ("/api/config/list", "GET", Refusal::NotPorted),
    ("/api/config/drift", "GET", Refusal::NotPorted),
    ("/api/config/reload-status", "GET", Refusal::NotPorted),
    ("/api/config/templates", "GET", Refusal::NotPorted),
    ("/api/config/map-keys", "GET", Refusal::NotPorted),
    (
        "/api/config/resolve-alias-source",
        "GET",
        Refusal::NotPorted,
    ),
    ("/api/config/map-key", "POST,DELETE", Refusal::NotPorted),
    ("/api/config/rename-map-key", "POST", Refusal::NotPorted),
    (
        "/api/config/model-providers/{type}/{alias}/refresh-context-window",
        "POST",
        Refusal::NotPorted,
    ),
    ("/api/config/delete-plan", "GET", Refusal::NotPorted),
    ("/api/config/catalog", "GET", Refusal::NotPorted),
    ("/api/config/catalog/models", "GET", Refusal::NotPorted),
    ("/api/config/status", "GET", Refusal::NotPorted),
    ("/api/config/agent-options", "GET", Refusal::NotPorted),
    ("/api/config/sections", "GET", Refusal::NotPorted),
    ("/api/config/sections/{section}", "GET", Refusal::NotPorted),
    (
        "/api/config/sections/{section}/items/{key}",
        "POST",
        Refusal::NotPorted,
    ),
    ("/api/config/init", "POST", Refusal::NotPorted),
    ("/api/config/migrate", "POST", Refusal::NotPorted),
    ("/api/quickstart/state", "GET", Refusal::NotPorted),
    ("/api/quickstart/fields", "POST", Refusal::NotPorted),
    ("/api/quickstart/validate", "POST", Refusal::NotPorted),
    ("/api/quickstart/apply", "POST", Refusal::NotPorted),
    ("/api/quickstart/dismiss", "POST", Refusal::NotPorted),
    ("/api/channels", "GET", Refusal::NotPorted),
    ("/api/channels/bind", "POST", Refusal::NotPorted),
    ("/api/channels/{channel}/relink", "POST", Refusal::NotPorted),
    ("/api/sops", "GET,POST", Refusal::NotPorted),
    ("/api/sops/{name}", "PUT,DELETE", Refusal::NotPorted),
    ("/api/sops/{name}/graph", "GET", Refusal::NotPorted),
    ("/api/sops/{name}/run", "POST", Refusal::NotPorted),
    ("/api/sops/{name}/rename", "POST", Refusal::NotPorted),
    ("/api/sops/runs", "GET", Refusal::NotPorted),
    ("/api/sops/{name}/full", "GET", Refusal::NotPorted),
    ("/api/sops/wire-draft", "POST", Refusal::NotPorted),
    ("/api/sops/graph-draft", "POST", Refusal::NotPorted),
    ("/api/sops/trigger-sources", "GET", Refusal::NotPorted),
    ("/api/sops/decision-models", "GET", Refusal::NotPorted),
    ("/api/sops/graph-legend", "GET", Refusal::NotPorted),
    (
        "/api/sops/{name}/runs/{run_id}/overlay",
        "GET",
        Refusal::NotPorted,
    ),
    (
        "/api/sops/{name}/runs/{run_id}/decide",
        "POST",
        Refusal::NotPorted,
    ),
    (
        "/api/sops/{name}/runs/{run_id}/cancel",
        "POST",
        Refusal::NotPorted,
    ),
    ("/api/tools", "GET", Refusal::NotPorted),
    ("/api/tools/param-options", "POST", Refusal::NotPorted),
    ("/api/browse", "GET", Refusal::NotPorted),
    ("/api/browse/mkdir", "POST", Refusal::NotPorted),
    ("/api/browse/rmdir", "DELETE", Refusal::NotPorted),
    (
        "/api/agents/{alias}/workspace/list",
        "GET",
        Refusal::NotPorted,
    ),
    (
        "/api/agents/{alias}/workspace/read",
        "GET",
        Refusal::NotPorted,
    ),
    (
        "/api/agents/{alias}/workspace/path",
        "DELETE",
        Refusal::NotPorted,
    ),
    (
        "/api/agents/{alias}/workspace/move",
        "POST",
        Refusal::NotPorted,
    ),
    (
        "/api/agents/{alias}/workspace/mkdir",
        "POST",
        Refusal::NotPorted,
    ),
    // The core has no method yet for an agent's effective skills, the slash
    // option kinds, or creating a skill; the P4 parity methods add them.
    ("/api/agents/{alias}/skills", "GET", Refusal::NotPorted),
    ("/api/skills/slash-option-kinds", "GET", Refusal::NotPorted),
    // `GET` on this path is served.
    (
        "/api/skills/bundles/{alias}/skills",
        "POST",
        Refusal::NotPorted,
    ),
    ("/api/cron", "GET,POST", Refusal::NotPorted),
    ("/api/cron/settings", "GET,PATCH", Refusal::NotPorted),
    ("/api/cron/{id}", "DELETE,PATCH", Refusal::NotPorted),
    ("/api/cron/{id}/runs", "GET", Refusal::NotPorted),
    ("/api/cron/{id}/run", "POST", Refusal::NotPorted),
    ("/api/integrations", "GET", Refusal::NotPorted),
    ("/api/integrations/settings", "GET", Refusal::NotPorted),
    ("/api/memory", "GET,POST", Refusal::NotPorted),
    ("/api/memory/{key}", "DELETE", Refusal::NotPorted),
    ("/api/cli-tools", "GET", Refusal::NotPorted),
    ("/api/devices", "GET", Refusal::NotPorted),
    ("/api/devices/me/capabilities", "POST", Refusal::NotPorted),
    ("/api/devices/{id}", "DELETE", Refusal::NotPorted),
    ("/api/devices/{id}/token/rotate", "POST", Refusal::NotPorted),
    ("/api/canvas", "GET", Refusal::NotPorted),
    ("/api/canvas/{id}", "GET,POST,DELETE", Refusal::NotPorted),
    ("/api/canvas/{id}/history", "GET", Refusal::NotPorted),
    ("/api/plugins", "GET", Refusal::NotPorted),
    ("/api/upload", "POST", Refusal::NotPorted),
];

/// Path prefixes that belong to the API or to machine clients. An unknown
/// path under one of them is a JSON 404, never the dashboard page.
const API_PREFIXES: &[&str] = &[
    "/api/",
    "/ws/",
    "/acp/",
    "/pair/",
    "/admin/",
    "/oidc/",
    "/.well-known/",
    "/a2a/",
    "/plugin/",
    "/sop/",
    "/hooks/",
    "/whatsapp/",
    "/linq/",
    "/nextcloud-talk/",
    "/webhook/",
    "/_app/",
];

fn method_filter(methods: &str) -> MethodFilter {
    methods
        .split(',')
        .map(|method| match method {
            "GET" => MethodFilter::GET,
            "POST" => MethodFilter::POST,
            "PUT" => MethodFilter::PUT,
            "PATCH" => MethodFilter::PATCH,
            "DELETE" => MethodFilter::DELETE,
            "OPTIONS" => MethodFilter::OPTIONS,
            other => panic!("unhandled method {other} in the preview route table"),
        })
        .reduce(MethodFilter::or)
        .expect("every refused route names a method")
}

#[derive(Clone)]
struct PreviewState {
    endpoint: Arc<PathBuf>,
    web_dist: Option<Arc<PathBuf>>,
    /// Set by `POST /admin/shutdown` to stop this process.
    shutdown: watch::Sender<bool>,
}

/// The preview's router. `core` must be attached to the core's socket
/// (see [`CoreRpc::local`]); `endpoint` is that socket, for the health
/// probe; `web_dist` holds the built dashboard. `POST /admin/shutdown` sets
/// `shutdown`, which the caller serving the router stops on. Serve it with
/// the peer's address (`into_make_service_with_connect_info`), which that
/// route checks. Every route answers `413` to a body over
/// [`crate::MAX_BODY_SIZE`] and `408` to a request not done within
/// `request_timeout`, as the in-process gateway's routes do.
pub fn router(
    core: CoreRpc,
    endpoint: PathBuf,
    web_dist: Option<PathBuf>,
    shutdown: watch::Sender<bool>,
    request_timeout: Duration,
) -> Router {
    let state = PreviewState {
        endpoint: Arc::new(endpoint),
        web_dist: web_dist.map(Arc::new),
        shutdown,
    };
    let mut router: Router<PreviewState> = Router::new()
        .route("/health", get(health))
        .route(
            "/api/openapi.json",
            get(crate::openapi::handle_openapi_json),
        )
        .route("/api/docs", get(crate::openapi::handle_docs))
        .route(CORE_LINK_PATH, get(core_link))
        .route("/api/health", get(api_health))
        .route("/api/tuis", get(api_tuis))
        .route("/api/cost", get(api_cost))
        .route("/api/events/history", get(api_events_history))
        .route("/api/sessions", get(api_sessions_list))
        .route("/api/skills/bundles", get(skills_bundles))
        .route("/api/skills/bundles/{alias}/skills", get(skills_list))
        .route(
            "/api/skills/bundles/{alias}/skills/{name}",
            get(skill_read).put(skill_write).delete(skill_delete),
        )
        .route("/api/personality", get(personality_index))
        .route("/api/personality/templates", get(personality_templates))
        .route(
            "/api/personality/{filename}",
            get(personality_get).put(personality_put),
        )
        .route("/admin/shutdown", post(admin_shutdown))
        .route("/hooks/claude-code", post(claude_code_hook));
    for &(path, methods, refusal) in REFUSED {
        let handler: MethodRouter<PreviewState> = on(
            method_filter(methods),
            move |method: HttpMethod, access: Result<CoreAccess, CoreError>| {
                refuse(refusal, format!("{method} {path}"), access)
            },
        );
        router = router.route(path, handler);
    }
    // The dashboard build references its files as `/_app/<path>` and keeps
    // them at `<dist>/<path>`, as the in-process gateway serves them.
    if let Some(dist) = &state.web_dist {
        router = router.nest_service("/_app", tower_http::services::ServeDir::new(dist.as_path()));
    }
    router
        .fallback(fallback)
        .layer(axum::Extension(core))
        .layer(tower_http::limit::RequestBodyLimitLayer::new(
            crate::MAX_BODY_SIZE,
        ))
        .layer(tower_http::timeout::TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            request_timeout,
        ))
        .layer(axum::middleware::from_fn(crate::security_headers::apply))
        .with_state(state)
}

async fn refuse(
    refusal: Refusal,
    route: String,
    access: Result<CoreAccess, CoreError>,
) -> Response {
    match refusal {
        Refusal::Deferred(area) => capability_missing(
            &route,
            &format!("{route} is not available in the preview gateway ({area} is deferred)"),
            true,
        ),
        Refusal::NotPorted => match access {
            Err(error) => explain(error),
            Ok(_) => capability_missing(
                &route,
                &format!("{route} is not served through the core yet"),
                false,
            ),
        },
    }
}

fn capability_missing(route: &str, message: &str, deferred: bool) -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({
            "code": "capability_missing",
            "route": route,
            "deferred": deferred,
            "error": message,
        })),
    )
        .into_response()
}

/// A core refusal with what the operator can do about it, in the shape the
/// dashboard shows as a banner: `{error, code, hint}`, plus `versions`
/// (`{core, gateway}`) for a version refusal. A classified core refusal
/// keeps the in-process route's status and body.
pub(crate) fn explain(error: CoreError) -> Response {
    if error.reason().is_some() {
        return error.into_response();
    }
    let (status, code) = error.status();
    let mut versions = None;
    let message = match error {
        CoreError::AuthRequired(message)
        | CoreError::Forbidden(message)
        | CoreError::Unavailable(message)
        | CoreError::UntrustedEndpoint(message) => message,
        CoreError::Busy => "every core connection this gateway may hold is in use".into(),
        CoreError::Timeout => "the core did not answer in time".into(),
        CoreError::VersionMismatch { core, gateway } => {
            report_refused_core(&core, &gateway);
            let message = crate::core_rpc::version_mismatch_message(&core, &gateway);
            versions = Some(json!({ "core": core, "gateway": gateway }));
            message
        }
        CoreError::Rpc(error) => error.message,
    };
    let hint = match code {
        "auth_required" => {
            "Sign in again: send Authorization: Bearer <token> with a token the core accepts."
        }
        "core_unavailable" => HINT_START_CORE,
        "core_untrusted_endpoint" => HINT_SAME_ACCOUNT,
        "core_busy" => "Every core connection this gateway may hold is in use: retry shortly.",
        "core_timeout" => {
            "The core is up but did not answer in time: retry, and check the core's log if it \
             keeps happening."
        }
        "core_incompatible" => {
            "zeroclaw-gw and the core speak different protocol versions: install matching \
             versions of zeroclaw and zeroclaw-gw."
        }
        "core_version_mismatch" => {
            "zeroclaw-gw serves only through a core of its own version: install matching \
             versions of zeroclaw and zeroclaw-gw. A restarted core is picked up on the next \
             request."
        }
        "forbidden" => "Your principal lacks the grant for this operation.",
        _ => "The core refused the request.",
    };
    let mut body = json!({ "error": message, "code": code, "hint": hint });
    if let Some(versions) = versions {
        body["versions"] = versions;
    }
    (status, Json(body)).into_response()
}

/// The core version this process last reported refusing, so a dashboard
/// polling a refused core puts one line on stderr rather than one per
/// request.
static REPORTED_REFUSAL: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// Say on stderr, this process's log, that a core was refused for its
/// version, once per core version.
fn report_refused_core(core: &str, gateway: &str) {
    let mut reported = REPORTED_REFUSAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if reported.as_deref() != Some(core) {
        // Written so that a closed stderr cannot panic the request handler.
        let _ = writeln!(std::io::stderr(), "{}", refused_core_notice(core, gateway));
        *reported = Some(core.to_owned());
    }
}

/// The operator's note on stderr when a core of another version is refused.
fn refused_core_notice(core: &str, gateway: &str) -> String {
    zeroclaw_runtime::i18n::get_required_cli_string_with_args(
        "cli-gw-core-version-refused",
        &[("core", core), ("gateway", gateway)],
    )
}

/// The operator's note on stderr at start under `--allow-version-skew`.
fn version_skew_notice() -> String {
    zeroclaw_runtime::i18n::get_required_cli_string("cli-gw-version-skew-allowed")
}

/// Why the health probe could not vouch for the core's endpoint.
enum ProbeFailure {
    /// Nothing accepted the connection.
    Unreachable(String),
    /// Something accepted it, but it failed the same-account check.
    Untrusted(&'static str),
}

/// Whether the core's endpoint accepts a connection from this account and
/// passes the same kernel checks a credential-bearing dial makes. Nothing is
/// written on it: a probe that sent a handshake without a credential would
/// be the credential-less connection this gateway never opens.
async fn probe_endpoint(endpoint: &Path) -> Result<(), ProbeFailure> {
    match tokio::time::timeout(
        HEALTH_PROBE_TIMEOUT,
        RpcClient::probe_local(endpoint, EndpointOwner::SameAccount),
    )
    .await
    {
        Ok(Ok(())) => Ok(()),
        Ok(Err(ClientError::UntrustedEndpoint { rejection, .. })) => {
            Err(ProbeFailure::Untrusted(untrusted_reason(&rejection)))
        }
        Ok(Err(error)) => Err(ProbeFailure::Unreachable(error.to_string())),
        Err(_) => Err(ProbeFailure::Unreachable(
            "the endpoint did not accept within the probe timeout".into(),
        )),
    }
}

/// A refusal in words that name no path or account, for the unauthenticated
/// health route. The authenticated core-link route gives the details.
fn untrusted_reason(rejection: &EndpointRejection) -> &'static str {
    match rejection {
        EndpointRejection::PeerUid { .. } => "another OS account serves it",
        EndpointRejection::PeerUnknown(_) => "the kernel did not report which account serves it",
        EndpointRejection::DirectoryOwner { .. } => "its directory belongs to another OS account",
        EndpointRejection::DirectoryWritable { .. } => "other accounts can write to its directory",
        EndpointRejection::DirectoryUnreadable { .. } => "its directory could not be inspected",
        EndpointRejection::Unsupported => "this platform cannot verify which account serves it",
    }
}

/// How a dashboard signs in to the preview. There is no pairing-code
/// exchange, so the dashboard asks for an existing token and checks it
/// against the core-link route.
fn sign_in() -> serde_json::Value {
    json!({ "pairing_code": false, "bearer": true, "verify": CORE_LINK_PATH })
}

/// `GET /health`: this process is up, and whether the core's endpoint
/// accepts connections from this account. `503` with a banner-ready message
/// when it does not. The body names no path.
///
/// `require_pairing` is always `true`: every request needs a bearer, and a
/// dashboard written for the in-process gateway must never read a missing
/// field as "pairing off".
async fn health(State(state): State<PreviewState>) -> Response {
    match probe_endpoint(&state.endpoint).await {
        Ok(()) => (
            StatusCode::OK,
            Json(json!({
                "status": "ok",
                "gateway": "zeroclaw-gw preview",
                "version": env!("CARGO_PKG_VERSION"),
                "require_pairing": true,
                "sign_in": sign_in(),
                "core": { "link": "reachable" },
            })),
        )
            .into_response(),
        Err(failure) => {
            let (code, link, error, hint) = match failure {
                ProbeFailure::Unreachable(detail) => (
                    "core_unavailable",
                    "unreachable",
                    format!("The ZeroClaw core is not reachable: {detail}"),
                    HINT_START_CORE,
                ),
                ProbeFailure::Untrusted(reason) => (
                    "core_untrusted_endpoint",
                    "untrusted",
                    format!("The core's endpoint failed the same-account check: {reason}"),
                    HINT_SAME_ACCOUNT,
                ),
            };
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({
                    "status": "degraded",
                    "code": code,
                    "error": error,
                    "hint": hint,
                    "require_pairing": true,
                    "sign_in": sign_in(),
                    "core": { "link": link },
                })),
            )
                .into_response()
        }
    }
}

/// The caller's own core connection, or why it has none.
fn attached(access: Result<CoreAccess, CoreError>) -> Result<CoreCall, CoreError> {
    match access? {
        CoreAccess::Core(call) => Ok(call),
        // A gateway attached to a socket never serves in-process.
        CoreAccess::InProcess => Err(CoreError::Unavailable(
            "this gateway has no core attached".into(),
        )),
    }
}

/// A dashboard route the core serves: `body` is the route's core path, the
/// one the in-process gateway runs when the same request reaches the core.
/// A refusal carries the hint the dashboard shows.
async fn served<F, Fut>(access: Result<CoreAccess, CoreError>, body: F) -> Response
where
    F: FnOnce(CoreCall) -> Fut,
    Fut: Future<Output = Result<Response, CoreError>>,
{
    let answered = match attached(access) {
        Ok(call) => body(call).await,
        Err(error) => Err(error),
    };
    answered.unwrap_or_else(explain)
}

/// `GET /api/health`
async fn api_health(access: Result<CoreAccess, CoreError>) -> Response {
    served(access, |call| async move {
        crate::api::api_health_through_core(&call).await
    })
    .await
}

/// `GET /api/tuis`
async fn api_tuis(access: Result<CoreAccess, CoreError>) -> Response {
    served(access, |call| async move {
        crate::api::api_tuis_through_core(&call).await
    })
    .await
}

/// `GET /api/cost`
async fn api_cost(
    Query(query): Query<CostQuery>,
    access: Result<CoreAccess, CoreError>,
) -> Response {
    served(access, |call| async move {
        crate::api::api_cost_through_core(&call, &query).await
    })
    .await
}

/// `GET /api/events/history`
async fn api_events_history(access: Result<CoreAccess, CoreError>) -> Response {
    served(access, |call| async move {
        crate::sse::events_history_through_core(&call).await
    })
    .await
}

/// `GET /api/sessions`
async fn api_sessions_list(access: Result<CoreAccess, CoreError>) -> Response {
    served(access, |call| async move {
        crate::api::api_sessions_list_through_core(&call).await
    })
    .await
}

/// The caller's core connection, for a route that reads a request body. A
/// refused credential is answered while the request's head is extracted,
/// before the body is awaited, as the in-process gateway's authentication
/// answers before anything it parses.
struct BodyRouteCall(CoreCall);

impl<S: Send + Sync> axum::extract::FromRequestParts<S> for BodyRouteCall {
    type Rejection = Response;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        state: &S,
    ) -> Result<Self, Self::Rejection> {
        let access =
            <CoreAccess as axum::extract::FromRequestParts<S>>::from_request_parts(parts, state)
                .await;
        attached(access).map(Self).map_err(explain)
    }
}

/// A dashboard route the core serves whose input the request carries (path,
/// query or body). The credential is checked before the input, as the
/// in-process gateway's authentication runs before anything it parses is
/// used.
async fn served_with<T, F, Fut>(
    access: Result<CoreAccess, CoreError>,
    input: Result<T, Response>,
    body: F,
) -> Response
where
    F: FnOnce(CoreCall, T) -> Fut,
    Fut: Future<Output = Result<Response, CoreError>>,
{
    let call = match attached(access) {
        Ok(call) => call,
        Err(error) => return explain(error),
    };
    match input {
        Ok(input) => body(call, input).await.unwrap_or_else(explain),
        Err(rejection) => rejection,
    }
}

/// `GET /api/skills/bundles`
async fn skills_bundles(access: Result<CoreAccess, CoreError>) -> Response {
    served(access, |call| async move {
        crate::api_skills::list_bundles_through_core(&call).await
    })
    .await
}

/// `GET /api/skills/bundles/{alias}/skills`
async fn skills_list(
    access: Result<CoreAccess, CoreError>,
    path: Result<UrlPath<String>, PathRejection>,
) -> Response {
    let input = path
        .map(|UrlPath(alias)| alias)
        .map_err(IntoResponse::into_response);
    served_with(access, input, |call, alias| async move {
        crate::api_skills::list_skills_through_core(&call, &alias).await
    })
    .await
}

/// `GET /api/skills/bundles/{alias}/skills/{name}`
async fn skill_read(
    access: Result<CoreAccess, CoreError>,
    path: Result<UrlPath<(String, String)>, PathRejection>,
) -> Response {
    let input = path
        .map(|UrlPath(path)| path)
        .map_err(IntoResponse::into_response);
    served_with(access, input, |call, (alias, name)| async move {
        crate::api_skills::read_skill_through_core(&call, &alias, &name).await
    })
    .await
}

/// `PUT /api/skills/bundles/{alias}/skills/{name}`
async fn skill_write(
    BodyRouteCall(call): BodyRouteCall,
    path: Result<UrlPath<(String, String)>, PathRejection>,
    body: Result<Json<SkillWriteBody>, JsonRejection>,
) -> Response {
    let ((alias, name), body) = match (path, body) {
        (Ok(UrlPath(path)), Ok(Json(body))) => (path, body),
        (Err(rejection), _) => return rejection.into_response(),
        (_, Err(rejection)) => return rejection.into_response(),
    };
    crate::api_skills::write_skill_through_core(&call, &alias, &name, &body)
        .await
        .unwrap_or_else(explain)
}

/// `DELETE /api/skills/bundles/{alias}/skills/{name}`
async fn skill_delete(
    access: Result<CoreAccess, CoreError>,
    path: Result<UrlPath<(String, String)>, PathRejection>,
    query: Result<Query<DeleteQuery>, QueryRejection>,
) -> Response {
    let input = match (path, query) {
        (Ok(UrlPath(path)), Ok(Query(query))) => Ok((path, query)),
        (Err(rejection), _) => Err(rejection.into_response()),
        (_, Err(rejection)) => Err(rejection.into_response()),
    };
    served_with(access, input, |call, ((alias, name), query)| async move {
        crate::api_skills::delete_skill_through_core(&call, &alias, &name, &query).await
    })
    .await
}

/// `GET /api/personality`
async fn personality_index(
    access: Result<CoreAccess, CoreError>,
    query: Result<Query<AgentQuery>, QueryRejection>,
) -> Response {
    let input = query
        .map(|Query(query)| query)
        .map_err(IntoResponse::into_response);
    served_with(access, input, |call, query| async move {
        crate::api_personality::index_through_core(&call, &query).await
    })
    .await
}

/// `GET /api/personality/templates`
async fn personality_templates(
    access: Result<CoreAccess, CoreError>,
    query: Result<Query<TemplateQuery>, QueryRejection>,
) -> Response {
    let input = query
        .map(|Query(query)| query)
        .map_err(IntoResponse::into_response);
    served_with(access, input, |call, query| async move {
        crate::api_personality::templates_through_core(&call, &query).await
    })
    .await
}

/// `GET /api/personality/{filename}`
async fn personality_get(
    access: Result<CoreAccess, CoreError>,
    path: Result<UrlPath<String>, PathRejection>,
    query: Result<Query<AgentQuery>, QueryRejection>,
) -> Response {
    let input = match (path, query) {
        (Ok(UrlPath(filename)), Ok(Query(query))) => Ok((filename, query)),
        (Err(rejection), _) => Err(rejection.into_response()),
        (_, Err(rejection)) => Err(rejection.into_response()),
    };
    served_with(access, input, |call, (filename, query)| async move {
        crate::api_personality::get_through_core(&call, &filename, &query).await
    })
    .await
}

/// `PUT /api/personality/{filename}`
async fn personality_put(
    BodyRouteCall(call): BodyRouteCall,
    path: Result<UrlPath<String>, PathRejection>,
    query: Result<Query<AgentQuery>, QueryRejection>,
    body: Result<Json<PersonalityPutBody>, JsonRejection>,
) -> Response {
    let (filename, query, body) = match (path, query, body) {
        (Ok(UrlPath(filename)), Ok(Query(query)), Ok(Json(body))) => (filename, query, body),
        (Err(rejection), _, _) => return rejection.into_response(),
        (_, Err(rejection), _) => return rejection.into_response(),
        (_, _, Err(rejection)) => return rejection.into_response(),
    };
    crate::api_personality::put_through_core(&call, &filename, &query, &body)
        .await
        .unwrap_or_else(explain)
}

/// `POST /admin/shutdown`: stop this process, for a caller on loopback, as
/// the in-process route stops the in-process gateway. No core call: the
/// core keeps running.
async fn admin_shutdown(
    State(state): State<PreviewState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
) -> Response {
    crate::admin_shutdown(&peer, &state.shutdown)
}

/// `POST /hooks/claude-code`: log the event and acknowledge it, as the
/// in-process route does. No core call.
async fn claude_code_hook(
    Json(payload): Json<zeroclaw_tools::claude_code_runner::ClaudeCodeHookEvent>,
) -> Json<serde_json::Value> {
    crate::api::claude_code_hook(&payload)
}

/// `GET /api/gateway/core`: which principal the caller's credential binds,
/// which core answers and the extensions it advertised, over the caller's
/// own core connection.
async fn core_link(access: Result<CoreAccess, CoreError>) -> Response {
    let call = match attached(access) {
        Ok(call) => call,
        Err(error) => return explain(error),
    };
    let principal = call.principal_id().map(str::to_owned);
    match call.request(Method::Status, json!({})).await {
        Ok(status) => (
            StatusCode::OK,
            Json(json!({
                "principal_id": principal,
                "core": {
                    "server_version": status["server_version"],
                    "protocol_version": status["protocol_version"],
                    "features": call.core_features(),
                },
                "gateway": {
                    "version": env!("CARGO_PKG_VERSION"),
                    "protocol_version": RPC_PROTOCOL_VERSION,
                },
            })),
        )
            .into_response(),
        Err(error) => explain(error),
    }
}

/// Anything no route matched: the dashboard's page for a page path, a JSON
/// `404` for anything under an API or machine prefix.
async fn fallback(State(state): State<PreviewState>, method: HttpMethod, uri: Uri) -> Response {
    let path = uri.path();
    // The namespace root itself (`/api`, `/ws`) is as much the API's as
    // anything under it.
    let api = API_PREFIXES
        .iter()
        .any(|prefix| path.starts_with(prefix) || path == prefix.trim_end_matches('/'));
    if api || method != HttpMethod::GET {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({
                "code": "not_found",
                "route": format!("{method} {path}"),
                "error": format!("{method} {path} is not a gateway route"),
            })),
        )
            .into_response();
    }
    let Some(dist) = &state.web_dist else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({
                "code": "dashboard_unavailable",
                "error": "this gateway serves no dashboard; start it with --web-dist DIR",
            })),
        )
            .into_response();
    };
    match tokio::fs::read(dist.join("index.html")).await {
        Ok(page) => (
            [(axum::http::header::CONTENT_TYPE, "text/html; charset=utf-8")],
            page,
        )
            .into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({
                "code": "dashboard_unavailable",
                "error": format!("the dashboard page could not be read: {error}"),
            })),
        )
            .into_response(),
    }
}

/// The operator's note on stderr once the preview serves `url`.
fn serving_notice(url: &str, endpoint: &Path) -> String {
    zeroclaw_runtime::i18n::get_required_cli_string_with_args(
        "cli-gw-preview-serving",
        &[("url", url), ("endpoint", &endpoint.display().to_string())],
    )
}

/// Run the preview until interrupted. Prints `READY <url>` on stdout once
/// it is serving.
pub async fn serve(bootstrap: Bootstrap) -> anyhow::Result<()> {
    if cfg!(not(unix)) {
        anyhow::bail!(
            "the zeroclaw-gw preview runs on Unix only: the client cannot yet verify which \
             account serves a Windows named pipe, so it would refuse to send any credential"
        );
    }
    if let Some(dist) = &bootstrap.web_dist
        && !dist.join("index.html").is_file()
    {
        anyhow::bail!(
            "--web-dist {} holds no index.html; build the dashboard first",
            dist.display()
        );
    }
    let tls = match &bootstrap.tls {
        Some(files) => {
            let _ = rustls::crypto::ring::default_provider().install_default();
            Some(crate::tls::build_tls_acceptor(
                &zeroclaw_config::schema::GatewayTlsConfig {
                    enabled: true,
                    cert_path: files.cert.display().to_string(),
                    key_path: files.key.display().to_string(),
                    client_auth: None,
                },
            )?)
        }
        None => None,
    };

    let core = CoreRpc::local(
        bootstrap.endpoint.clone(),
        EndpointOwner::SameAccount,
        bootstrap.version_skew,
    );
    let (shutdown, shutdown_requested) = watch::channel(false);
    let mut app = router(
        core,
        bootstrap.endpoint.clone(),
        bootstrap.web_dist.clone(),
        shutdown,
        bootstrap.request_timeout,
    );
    if tls.is_some() {
        app = app.layer(axum::middleware::from_fn(
            crate::security_headers::apply_with_hsts,
        ));
    }
    let listener = tokio::net::TcpListener::bind(bootstrap.listen).await?;
    let address = listener.local_addr()?;
    let scheme = if tls.is_some() { "https" } else { "http" };
    // i18n-exempt: `READY <url>` is the startup line a supervisor parses, not prose
    println!("READY {scheme}://{address}");
    eprintln!(
        "{}",
        serving_notice(&format!("{scheme}://{address}"), &bootstrap.endpoint)
    );
    if bootstrap.version_skew == VersionSkew::Allow {
        let _ = writeln!(std::io::stderr(), "{}", version_skew_notice());
    }

    match tls {
        None => {
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .with_graceful_shutdown(stop_requested(shutdown_requested))
            .await?;
        }
        Some(acceptor) => {
            serve_tls(
                listener,
                acceptor,
                app,
                shutdown_requested,
                bootstrap.request_timeout,
            )
            .await?;
        }
    }
    Ok(())
}

/// Serve HTTPS until asked to stop, then let the requests in flight finish
/// and their answers (the shutdown acknowledgement among them) reach the
/// caller, as the plain listener does: each connection closes once its
/// request is answered. A connection that does not finish within
/// `request_timeout` is dropped, so stopping stays bounded.
async fn serve_tls(
    listener: tokio::net::TcpListener,
    acceptor: tokio_rustls::TlsAcceptor,
    app: Router,
    shutdown_requested: watch::Receiver<bool>,
    request_timeout: Duration,
) -> anyhow::Result<()> {
    let shutdown = stop_requested(shutdown_requested);
    tokio::pin!(shutdown);
    let connections = hyper_util::server::graceful::GracefulShutdown::new();
    loop {
        let (tcp, peer) = tokio::select! {
            accepted = listener.accept() => accepted?,
            () = &mut shutdown => break,
        };
        let acceptor = acceptor.clone();
        let app = app.clone();
        let watcher = connections.watcher();
        zeroclaw_spawn::spawn!(async move {
            let Ok(stream) = acceptor.accept(tcp).await else {
                return;
            };
            let service = hyper::service::service_fn(move |mut request: hyper::Request<_>| {
                request.extensions_mut().insert(ConnectInfo(peer));
                tower::ServiceExt::oneshot(app.clone(), request)
            });
            let builder =
                hyper_util::server::conn::auto::Builder::new(hyper_util::rt::TokioExecutor::new());
            let _ = watcher
                .watch(builder.serve_connection(hyper_util::rt::TokioIo::new(stream), service))
                .await;
        });
    }
    let _ = tokio::time::timeout(request_timeout, connections.shutdown()).await;
    Ok(())
}

/// Resolves when this process should stop: on a signal, or once
/// `POST /admin/shutdown` asks.
async fn stop_requested(mut requested: watch::Receiver<bool>) {
    tokio::select! {
        () = shutdown_signal() => {}
        _ = requested.wait_for(|stop| *stop) => {}
    }
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut terminate =
            match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                Ok(signal) => signal,
                Err(_) => {
                    let _ = tokio::signal::ctrl_c().await;
                    return;
                }
            };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = terminate.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

#[cfg(test)]
mod tests;
