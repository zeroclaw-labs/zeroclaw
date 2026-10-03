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
//! It is configured from flags and the environment only and never reads
//! `config.toml`. It runs as the same OS account as the core: every
//! connection verifies through the kernel that this account serves the
//! endpoint before the caller's credential is written, and every request
//! needs a credential of its own. Windows cannot make that check yet, so
//! the preview refuses to start there.

use std::future::Future;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::Router;
use axum::extract::rejection::JsonRejection;
use axum::extract::{ConnectInfo, Path as UrlPath, Query, State};
use axum::http::{Method as HttpMethod, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::{MethodFilter, MethodRouter, get, on, post};
use serde_json::json;
use tokio::sync::watch;
use zeroclaw_rpc_client::{
    ClientError, EndpointOwner, EndpointRejection, Method, RPC_PROTOCOL_VERSION, RpcClient,
};

use crate::api::{CostQuery, CronRunsQuery, MemoryDeleteQuery, MemoryQuery, MemoryStoreBody};
use crate::core_rpc::{CoreAccess, CoreCall, CoreError, CoreRpc, DedicatedCoreAccess};

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
  --long-running-request-timeout SECS
                        the same for a manual cron run, which waits for the
                        job [default: 600, the in-process gateway's default]
  -h, --help            print this help
  -V, --version         print the version

Runs as the same OS account as the core, on Unix. On start it prints
`READY <url>` on stdout once it is serving. It stops on SIGINT or SIGTERM,
or on `POST /admin/shutdown` from this machine.";

/// Everything the preview needs to start, from flags and the environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bootstrap {
    pub listen: SocketAddr,
    pub endpoint: PathBuf,
    pub web_dist: Option<PathBuf>,
    pub tls: Option<TlsFiles>,
    /// How long a request may take before it is answered `408`.
    pub request_timeout: Duration,
    /// The same, for the routes that wait on long work (a manual cron run),
    /// as the in-process gateway's long-running routes do.
    pub long_running_request_timeout: Duration,
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
    let mut long_running_request_timeout =
        Duration::from_secs(crate::LONG_RUNNING_REQUEST_TIMEOUT_SECS);
    let mut allow_public_bind = false;

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
                request_timeout = positive_secs("--request-timeout", &value("--request-timeout")?)?;
            }
            "--long-running-request-timeout" => {
                long_running_request_timeout = positive_secs(
                    "--long-running-request-timeout",
                    &value("--long-running-request-timeout")?,
                )?;
            }
            "--allow-public-bind" => allow_public_bind = true,
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
        long_running_request_timeout,
    }))
}

/// A flag's value as a positive number of seconds.
fn positive_secs(flag: &str, secs: &str) -> Result<Duration, String> {
    secs.parse::<u64>()
        .ok()
        .filter(|secs| *secs > 0)
        .map(Duration::from_secs)
        .ok_or_else(|| format!("{flag} {secs:?} is not a positive number of seconds"))
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
    ("/api/personality", "GET", Refusal::NotPorted),
    ("/api/personality/templates", "GET", Refusal::NotPorted),
    ("/api/personality/{filename}", "GET,PUT", Refusal::NotPorted),
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
    ("/api/agents/{alias}/skills", "GET", Refusal::NotPorted),
    ("/api/skills/bundles", "GET", Refusal::NotPorted),
    ("/api/skills/slash-option-kinds", "GET", Refusal::NotPorted),
    (
        "/api/skills/bundles/{alias}/skills",
        "GET,POST",
        Refusal::NotPorted,
    ),
    (
        "/api/skills/bundles/{alias}/skills/{name}",
        "GET,PUT,DELETE",
        Refusal::NotPorted,
    ),
    // Creating and editing a job wait on the core's `cron/add` and
    // `cron/patch` taking the agent-job fields and policy approval; the other
    // cron methods on these paths are served.
    ("/api/cron", "POST", Refusal::NotPorted),
    ("/api/cron/{id}", "PATCH", Refusal::NotPorted),
    ("/api/integrations", "GET", Refusal::NotPorted),
    ("/api/integrations/settings", "GET", Refusal::NotPorted),
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
    /// The long-running routes' budget, which their core call gets too.
    long_running_timeout: Duration,
}

/// The preview's router. `core` must be attached to the core's socket
/// (see [`CoreRpc::local`]); `endpoint` is that socket, for the health
/// probe; `web_dist` holds the built dashboard. `POST /admin/shutdown` sets
/// `shutdown`, which the caller serving the router stops on. Serve it with
/// the peer's address (`into_make_service_with_connect_info`), which that
/// route checks. Every route answers `413` to a body over
/// [`crate::MAX_BODY_SIZE`] and `408` to a request not done within
/// `request_timeout`, as the in-process gateway's routes do; a manual cron
/// run has `long_running_timeout` instead, as on the in-process gateway's
/// long-running router.
pub fn router(
    core: CoreRpc,
    endpoint: PathBuf,
    web_dist: Option<PathBuf>,
    shutdown: watch::Sender<bool>,
    request_timeout: Duration,
    long_running_timeout: Duration,
) -> Router {
    let state = PreviewState {
        endpoint: Arc::new(endpoint),
        web_dist: web_dist.map(Arc::new),
        shutdown,
        long_running_timeout,
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
        .route("/api/cron", get(api_cron_list))
        .route(
            "/api/cron/settings",
            get(api_cron_settings).patch(api_cron_settings_patch),
        )
        .route("/api/cron/{id}", axum::routing::delete(api_cron_delete))
        .route("/api/cron/{id}/runs", get(api_cron_runs))
        .route("/api/memory", get(api_memory_list).post(api_memory_store))
        .route(
            "/api/memory/{key}",
            axum::routing::delete(api_memory_delete),
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
    // A manual cron run waits for the job, so it gets the long-running
    // budget instead of the request timeout, as on the in-process gateway's
    // long-running router. Merged after the request timeout's layer, which
    // therefore does not wrap it.
    let long_running: Router<PreviewState> = Router::new()
        .route("/api/cron/{id}/run", post(api_cron_run))
        .layer(axum::Extension(core.clone()))
        .layer(tower_http::limit::RequestBodyLimitLayer::new(
            crate::MAX_BODY_SIZE,
        ))
        .layer(tower_http::timeout::TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            long_running_timeout,
        ));
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
        .merge(long_running)
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
/// dashboard shows as a banner: `{error, code, hint}`. A refusal the core
/// classified is the route's own answer, not a fault to explain: it answers
/// with the in-process route's status and body.
pub(crate) fn explain(error: CoreError) -> Response {
    if error.reason().is_some() {
        return error.into_response();
    }
    let (status, code) = error.status();
    let message = match error {
        CoreError::AuthRequired(message)
        | CoreError::Forbidden(message)
        | CoreError::Unavailable(message)
        | CoreError::UntrustedEndpoint(message) => message,
        CoreError::Busy => "every core connection this gateway may hold is in use".into(),
        CoreError::Timeout => "the core did not answer in time".into(),
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
        "forbidden" => "Your principal lacks the grant for this operation.",
        _ => "The core refused the request.",
    };
    (
        status,
        Json(json!({ "error": message, "code": code, "hint": hint })),
    )
        .into_response()
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

/// `GET /api/cron`
async fn api_cron_list(access: Result<CoreAccess, CoreError>) -> Response {
    served(access, |call| async move {
        crate::api::api_cron_list_through_core(&call).await
    })
    .await
}

/// `GET /api/cron/settings`
async fn api_cron_settings(access: Result<CoreAccess, CoreError>) -> Response {
    served(access, |call| async move {
        crate::api::api_cron_settings_through_core(&call).await
    })
    .await
}

/// `PATCH /api/cron/settings`. A malformed body is answered only after the
/// credential, as the in-process gateway's authentication runs first.
async fn api_cron_settings_patch(
    access: Result<CoreAccess, CoreError>,
    body: Result<Json<serde_json::Value>, JsonRejection>,
) -> Response {
    served(access, |call| async move {
        match body {
            Ok(Json(body)) => crate::api::api_cron_settings_patch_through_core(&call, &body).await,
            Err(rejection) => Ok(rejection.into_response()),
        }
    })
    .await
}

/// `DELETE /api/cron/{id}`
async fn api_cron_delete(
    UrlPath(id): UrlPath<String>,
    access: Result<CoreAccess, CoreError>,
) -> Response {
    served(access, |call| async move {
        crate::api::api_cron_delete_through_core(&call, &id).await
    })
    .await
}

/// `POST /api/cron/{id}/run`, on a connection of its own and within the
/// long-running budget, so the run is neither cut short nor holds the
/// caller's other requests behind it.
async fn api_cron_run(
    State(state): State<PreviewState>,
    UrlPath(id): UrlPath<String>,
    access: Result<DedicatedCoreAccess, CoreError>,
) -> Response {
    let budget = state.long_running_timeout;
    served(
        access.map(|DedicatedCoreAccess(access)| access),
        |call| async move { crate::api::api_cron_run_through_core(&call, &id, budget).await },
    )
    .await
}

/// `GET /api/cron/{id}/runs`
async fn api_cron_runs(
    UrlPath(id): UrlPath<String>,
    Query(params): Query<CronRunsQuery>,
    access: Result<CoreAccess, CoreError>,
) -> Response {
    served(access, |call| async move {
        crate::api::api_cron_runs_through_core(&call, &id, &params).await
    })
    .await
}

/// `GET /api/memory`
async fn api_memory_list(
    Query(params): Query<MemoryQuery>,
    access: Result<CoreAccess, CoreError>,
) -> Response {
    served(access, |call| async move {
        crate::api::api_memory_list_through_core(&call, &params).await
    })
    .await
}

/// `POST /api/memory`. A malformed body is answered only after the
/// credential, as on `PATCH /api/cron/settings`.
async fn api_memory_store(
    access: Result<CoreAccess, CoreError>,
    body: Result<Json<MemoryStoreBody>, JsonRejection>,
) -> Response {
    served(access, |call| async move {
        match body {
            Ok(Json(body)) => crate::api::api_memory_store_through_core(&call, &body).await,
            Err(rejection) => Ok(rejection.into_response()),
        }
    })
    .await
}

/// `DELETE /api/memory/{key}`
async fn api_memory_delete(
    UrlPath(key): UrlPath<String>,
    Query(query): Query<MemoryDeleteQuery>,
    access: Result<CoreAccess, CoreError>,
) -> Response {
    served(access, |call| async move {
        crate::api::api_memory_delete_through_core(&call, &key, &query).await
    })
    .await
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

/// `GET /api/gateway/core`: which principal the caller's credential binds
/// and which core answers, over the caller's own core connection.
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

    let core = CoreRpc::local(bootstrap.endpoint.clone(), EndpointOwner::SameAccount);
    let (shutdown, shutdown_requested) = watch::channel(false);
    let mut app = router(
        core,
        bootstrap.endpoint.clone(),
        bootstrap.web_dist.clone(),
        shutdown,
        bootstrap.request_timeout,
        bootstrap.long_running_request_timeout,
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
