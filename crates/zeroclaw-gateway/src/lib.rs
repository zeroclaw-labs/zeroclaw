#![allow(
    clippy::to_string_in_format_args,
    clippy::useless_format,
    clippy::collapsible_if
)]
//! Axum-based HTTP gateway with proper HTTP/1.1 compliance, body limits, and timeouts.
//!
//! This module replaces the raw TCP implementation with axum for:
//! - Proper HTTP/1.1 parsing and compliance
//! - Content-Length validation (handled by hyper)
//! - Request body size limits (64KB max)
//! - Request timeouts (30s) to prevent slow-loris attacks
//! - Header sanitization (handled by axum/hyper)

pub mod acp;
pub mod api;
pub mod api_browse;
pub mod api_config;
pub mod api_logs;
pub mod api_pairing;
pub mod api_personality;
#[cfg(feature = "plugins-wasm")]
pub mod api_plugins;
pub mod api_quickstart;
pub mod api_sections;
pub mod api_skills;
#[cfg(feature = "webauthn")]
pub mod api_webauthn;
pub mod auth_rate_limit;
pub mod canvas;
pub mod hardware_context;
pub mod node_tool;
pub mod nodes;
pub mod openapi;
pub mod session_queue;
pub mod sse;
pub mod static_files;
pub mod tls;
#[cfg(feature = "gateway-voice-duplex")]
pub mod voice_duplex;
pub mod ws;
pub mod ws_approval;

use anyhow::{Context, Result};
#[cfg(any(
    feature = "channel-email",
    feature = "channel-linq",
    feature = "channel-nextcloud",
    feature = "channel-wati",
    feature = "channel-whatsapp-cloud"
))]
use axum::body::Bytes;
#[cfg(feature = "channel-linq")]
use axum::extract::Path;
use axum::{
    Router,
    extract::{ConnectInfo, Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Json},
    routing::{delete, get, post},
};
use parking_lot::{Mutex, RwLock};
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Backoff after a transient `accept()` error so the serve loop does not
/// hot-spin while the condition (e.g. fd exhaustion) clears.
const ACCEPT_ERROR_BACKOFF_MS: u64 = 50;

/// File-descriptor exhaustion errno values, stable across the Unix targets
/// we support (Linux, macOS, BSD).
#[cfg(unix)]
const EMFILE: i32 = 24; // too many open files (this process)
#[cfg(unix)]
const ENFILE: i32 = 23; // too many open files (system-wide)

/// Returns `true` when an error from a stream listener's `accept()` is
/// transient and the listener itself remains usable, so the serve loop
/// should log and keep running rather than terminating the daemon. Covers
/// file-descriptor exhaustion (`EMFILE`/`ENFILE`, see #7042) and the usual
/// per-connection hiccups. Mirrors the non-fatal accept handling that
/// `axum::serve` already performs on the plain-TCP path.
fn is_recoverable_accept_error(e: &std::io::Error) -> bool {
    use std::io::ErrorKind;
    if matches!(
        e.kind(),
        ErrorKind::ConnectionAborted | ErrorKind::Interrupted | ErrorKind::WouldBlock
    ) {
        return true;
    }
    #[cfg(unix)]
    if matches!(e.raw_os_error(), Some(EMFILE) | Some(ENFILE)) {
        return true;
    }
    false
}
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::timeout::TimeoutLayer;
use uuid::Uuid;
#[cfg(any(
    feature = "channel-linq",
    feature = "channel-nextcloud",
    feature = "channel-wati",
    feature = "channel-whatsapp-cloud"
))]
use zeroclaw_api::channel::{Channel, SendMessage};
use zeroclaw_api::memory_traits::MemoryStrategy;
use zeroclaw_api::tool::ToolSpec;
#[cfg(feature = "channel-email")]
use zeroclaw_channels::gmail_push::GmailPushChannel;
#[cfg(feature = "channel-linq")]
use zeroclaw_channels::linq::LinqChannel;
#[cfg(feature = "channel-nextcloud")]
use zeroclaw_channels::nextcloud_talk::NextcloudTalkChannel;
#[cfg(feature = "channel-wati")]
use zeroclaw_channels::wati::WatiChannel;
#[cfg(feature = "channel-whatsapp-cloud")]
use zeroclaw_channels::whatsapp::WhatsAppChannel;
use zeroclaw_config::policy::SecurityPolicy;
use zeroclaw_config::schema::Config;
use zeroclaw_infra::session_backend::SessionBackend;
use zeroclaw_memory::{self, Memory, MemoryCategory};
use zeroclaw_providers::{self, ModelProvider};
use zeroclaw_runtime::agent::memory_strategy::DefaultMemoryStrategy;
use zeroclaw_runtime::cost::CostTracker;
use zeroclaw_runtime::i18n;
use zeroclaw_runtime::platform;
use zeroclaw_runtime::security::pairing::{PairingGuard, constant_time_eq, is_public_bind};
use zeroclaw_runtime::tools;
use zeroclaw_runtime::tools::CanvasStore;

/// Maximum request body size (64KB) — prevents memory exhaustion
pub const MAX_BODY_SIZE: usize = 65_536;
/// Default request timeout (30s) — prevents slow-loris attacks.
pub const REQUEST_TIMEOUT_SECS: u64 = 30;

/// Default request timeout for `POST /api/cron/{id}/run` (10 minutes).
///
/// Manually-triggered cron jobs run synchronously inside the request handler
/// and frequently exceed the 30s gateway-wide default — agent jobs in
/// particular can take minutes to complete a full reasoning loop. Capping at
/// 10 minutes keeps the route from hanging indefinitely while still allowing
/// realistic workloads to finish.
pub const LONG_RUNNING_REQUEST_TIMEOUT_SECS: u64 = 600;

/// Gateway request timeout (seconds) for routes other than the long-running
/// cron-trigger endpoint. Reads from typed config.
pub fn gateway_request_timeout_secs(cfg: &zeroclaw_config::schema::GatewayConfig) -> u64 {
    cfg.request_timeout_secs
}

/// Manual cron-trigger request timeout (seconds), exempt from the
/// gateway-wide [`gateway_request_timeout_secs`] limit so synchronous agent
/// jobs can run to completion. Reads from typed config.
pub fn gateway_long_running_request_timeout_secs(
    cfg: &zeroclaw_config::schema::GatewayConfig,
) -> u64 {
    cfg.long_running_request_timeout_secs
}
/// Sliding window used by gateway rate limiting.
pub const RATE_LIMIT_WINDOW_SECS: u64 = 60;
/// Fallback max distinct client keys tracked in gateway rate limiter.
pub const RATE_LIMIT_MAX_KEYS_DEFAULT: usize = 10_000;
/// Fallback max distinct idempotency keys retained in gateway memory.
pub const IDEMPOTENCY_MAX_KEYS_DEFAULT: usize = 10_000;

fn webhook_memory_key() -> String {
    format!("webhook_msg_{}", Uuid::new_v4())
}

#[cfg(feature = "channel-whatsapp-cloud")]
fn whatsapp_memory_key(msg: &zeroclaw_api::channel::ChannelMessage) -> String {
    format!("whatsapp_{}_{}", msg.sender, msg.id)
}

#[cfg(feature = "channel-linq")]
fn linq_memory_key(msg: &zeroclaw_api::channel::ChannelMessage) -> String {
    format!("linq_{}_{}", msg.sender, msg.id)
}

#[cfg(feature = "channel-wati")]
fn wati_memory_key(msg: &zeroclaw_api::channel::ChannelMessage) -> String {
    format!("wati_{}_{}", msg.sender, msg.id)
}

#[cfg(feature = "channel-nextcloud")]
fn nextcloud_talk_memory_key(msg: &zeroclaw_api::channel::ChannelMessage) -> String {
    format!("nextcloud_talk_{}_{}", msg.sender, msg.id)
}

#[cfg(any(
    feature = "channel-linq",
    feature = "channel-nextcloud",
    feature = "channel-wati",
    feature = "channel-whatsapp-cloud"
))]
fn sender_session_id(channel: &str, msg: &zeroclaw_api::channel::ChannelMessage) -> String {
    match &msg.thread_ts {
        Some(thread_id) => format!("{channel}_{thread_id}_{}", msg.sender),
        None => format!("{channel}_{}", msg.sender),
    }
}

fn webhook_session_id(headers: &HeaderMap) -> Option<String> {
    const MAX_SESSION_ID_LEN: usize = 128;
    headers
        .get("X-Session-Id")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .filter(|value| value.len() <= MAX_SESSION_ID_LEN)
        .filter(|value| {
            value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.')
        })
        .map(str::to_owned)
}

fn hash_webhook_secret(value: &str) -> String {
    use sha2::{Digest, Sha256};

    let digest = Sha256::digest(value.as_bytes());
    hex::encode(digest)
}

/// How often the rate limiter sweeps stale IP entries from its map.
const RATE_LIMITER_SWEEP_INTERVAL_SECS: u64 = 300; // 5 minutes

#[derive(Debug)]
struct SlidingWindowRateLimiter {
    limit_per_window: u32,
    window: Duration,
    max_keys: usize,
    requests: Mutex<(HashMap<String, Vec<Instant>>, Instant)>,
}

impl SlidingWindowRateLimiter {
    fn new(limit_per_window: u32, window: Duration, max_keys: usize) -> Self {
        Self {
            limit_per_window,
            window,
            max_keys: max_keys.max(1),
            requests: Mutex::new((HashMap::new(), Instant::now())),
        }
    }

    fn prune_stale(requests: &mut HashMap<String, Vec<Instant>>, cutoff: Instant) {
        requests.retain(|_, timestamps| {
            timestamps.retain(|t| *t > cutoff);
            !timestamps.is_empty()
        });
    }

    fn allow(&self, key: &str) -> bool {
        if self.limit_per_window == 0 {
            return true;
        }

        let now = Instant::now();
        let cutoff = now.checked_sub(self.window).unwrap_or_else(Instant::now);

        let mut guard = self.requests.lock();
        let (requests, last_sweep) = &mut *guard;

        // Periodic sweep: remove keys with no recent requests
        if last_sweep.elapsed() >= Duration::from_secs(RATE_LIMITER_SWEEP_INTERVAL_SECS) {
            Self::prune_stale(requests, cutoff);
            *last_sweep = now;
        }

        if !requests.contains_key(key) && requests.len() >= self.max_keys {
            // Opportunistic stale cleanup before eviction under cardinality pressure.
            Self::prune_stale(requests, cutoff);
            *last_sweep = now;

            if requests.len() >= self.max_keys {
                let evict_key = requests
                    .iter()
                    .min_by_key(|(_, timestamps)| timestamps.last().copied().unwrap_or(cutoff))
                    .map(|(k, _)| k.clone());
                if let Some(evict_key) = evict_key {
                    requests.remove(&evict_key);
                }
            }
        }

        let entry = requests.entry(key.to_owned()).or_default();
        entry.retain(|instant| *instant > cutoff);

        if entry.len() >= self.limit_per_window as usize {
            return false;
        }

        entry.push(now);
        true
    }
}

#[derive(Debug)]
pub struct GatewayRateLimiter {
    pair: SlidingWindowRateLimiter,
    webhook: SlidingWindowRateLimiter,
}

impl GatewayRateLimiter {
    pub fn new(pair_per_minute: u32, webhook_per_minute: u32, max_keys: usize) -> Self {
        let window = Duration::from_secs(RATE_LIMIT_WINDOW_SECS);
        Self {
            pair: SlidingWindowRateLimiter::new(pair_per_minute, window, max_keys),
            webhook: SlidingWindowRateLimiter::new(webhook_per_minute, window, max_keys),
        }
    }

    fn allow_pair(&self, key: &str) -> bool {
        self.pair.allow(key)
    }

    fn allow_webhook(&self, key: &str) -> bool {
        self.webhook.allow(key)
    }
}

#[derive(Debug)]
pub struct IdempotencyStore {
    ttl: Duration,
    max_keys: usize,
    keys: Mutex<HashMap<String, Instant>>,
}

impl IdempotencyStore {
    pub fn new(ttl: Duration, max_keys: usize) -> Self {
        Self {
            ttl,
            max_keys: max_keys.max(1),
            keys: Mutex::new(HashMap::new()),
        }
    }

    /// Returns true if this key is new and is now recorded.
    fn record_if_new(&self, key: &str) -> bool {
        let now = Instant::now();
        let mut keys = self.keys.lock();

        keys.retain(|_, seen_at| now.duration_since(*seen_at) < self.ttl);

        if keys.contains_key(key) {
            return false;
        }

        if keys.len() >= self.max_keys {
            let evict_key = keys
                .iter()
                .min_by_key(|(_, seen_at)| *seen_at)
                .map(|(k, _)| k.clone());
            if let Some(evict_key) = evict_key {
                keys.remove(&evict_key);
            }
        }

        keys.insert(key.to_owned(), now);
        true
    }
}

fn parse_client_ip(value: &str) -> Option<IpAddr> {
    let value = value.trim().trim_matches('"').trim();
    if value.is_empty() {
        return None;
    }

    if let Ok(ip) = value.parse::<IpAddr>() {
        return Some(ip);
    }

    if let Ok(addr) = value.parse::<SocketAddr>() {
        return Some(addr.ip());
    }

    let value = value.trim_matches(['[', ']']);
    value.parse::<IpAddr>().ok()
}

fn dirs_data_local() -> Option<std::path::PathBuf> {
    directories::BaseDirs::new().map(|d| d.data_local_dir().to_path_buf())
}

fn forwarded_client_ip(headers: &HeaderMap) -> Option<IpAddr> {
    if let Some(xff) = headers.get("X-Forwarded-For").and_then(|v| v.to_str().ok()) {
        for candidate in xff.split(',') {
            if let Some(ip) = parse_client_ip(candidate) {
                return Some(ip);
            }
        }
    }

    headers
        .get("X-Real-IP")
        .and_then(|v| v.to_str().ok())
        .and_then(parse_client_ip)
}

fn client_key_from_request(
    peer_addr: Option<SocketAddr>,
    headers: &HeaderMap,
    trust_forwarded_headers: bool,
) -> String {
    if trust_forwarded_headers && let Some(ip) = forwarded_client_ip(headers) {
        return ip.to_string();
    }

    peer_addr
        .map(|addr| addr.ip().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

fn normalize_max_keys(configured: usize, fallback: usize) -> usize {
    if configured == 0 {
        fallback.max(1)
    } else {
        configured
    }
}

/// Shared state for all axum handlers
#[derive(Clone)]
pub struct AppState {
    pub config: Arc<RwLock<Config>>,
    pub model_provider: Arc<dyn ModelProvider>,
    pub model: String,
    /// `None` means "let the provider decide" — required for models
    /// (e.g. claude-opus-4-7) that reject the field. Always preserve
    /// `Option<f64>` end-to-end; never substitute a hardcoded default.
    pub temperature: Option<f64>,
    pub mem: Arc<dyn Memory>,
    pub memory_strategy: Arc<dyn MemoryStrategy>,
    pub auto_save: bool,
    /// SHA-256 hash of `X-Webhook-Secret` (hex-encoded), never plaintext.
    pub webhook_secret_hash: Option<Arc<str>>,
    pub pairing: Arc<PairingGuard>,
    pub trust_forwarded_headers: bool,
    pub rate_limiter: Arc<GatewayRateLimiter>,
    pub auth_limiter: Arc<auth_rate_limit::AuthRateLimiter>,
    pub idempotency_store: Arc<IdempotencyStore>,
    #[cfg(feature = "channel-whatsapp-cloud")]
    pub whatsapp: Option<Arc<WhatsAppChannel>>,
    /// `WhatsApp` app secret for webhook signature verification (`X-Hub-Signature-256`)
    #[cfg(feature = "channel-whatsapp-cloud")]
    pub whatsapp_app_secret: Option<Arc<str>>,
    #[cfg(feature = "channel-linq")]
    pub linq: HashMap<String, Arc<LinqChannel>>,
    /// Linq webhook signing secrets per alias
    #[cfg(feature = "channel-linq")]
    pub linq_signing_secrets: HashMap<String, Arc<str>>,
    #[cfg(feature = "channel-nextcloud")]
    pub nextcloud_talk: Option<Arc<NextcloudTalkChannel>>,
    /// Nextcloud Talk webhook secret for signature verification
    #[cfg(feature = "channel-nextcloud")]
    pub nextcloud_talk_webhook_secret: Option<Arc<str>>,
    #[cfg(feature = "channel-wati")]
    pub wati: Option<Arc<WatiChannel>>,
    /// Gmail Pub/Sub push notification channel
    #[cfg(feature = "channel-email")]
    pub gmail_push: Option<Arc<GmailPushChannel>>,
    /// Observability backend for metrics scraping
    pub observer: Arc<dyn zeroclaw_runtime::observability::Observer>,
    /// Registered tool specs (for web dashboard tools page)
    pub tools_registry: Arc<Vec<ToolSpec>>,
    /// Cost tracker (optional, for web dashboard cost page)
    pub cost_tracker: Option<Arc<CostTracker>>,
    /// SSE broadcast channel for real-time events
    pub event_tx: tokio::sync::broadcast::Sender<serde_json::Value>,
    /// Ring buffer of recent events for history replay
    pub event_buffer: Arc<sse::EventBuffer>,
    /// Shutdown signal sender for graceful shutdown
    pub shutdown_tx: tokio::sync::watch::Sender<bool>,
    /// Reload signal sender owned by the daemon. /admin/reload writes `true`
    /// here; the daemon's wait loop reacts and re-instantiates every
    /// subsystem in place. `None` when running standalone (`zeroclaw gateway start`)
    /// — reload then degrades to a 503 with a clear message.
    pub reload_tx: Option<tokio::sync::watch::Sender<bool>>,
    /// Registry of dynamically connected nodes
    pub node_registry: Arc<nodes::NodeRegistry>,
    /// Path prefix for reverse-proxy deployments (empty string = no prefix)
    pub path_prefix: String,
    /// Filesystem path to `web/dist/` for serving the dashboard (None = API-only)
    pub web_dist_dir: Option<std::path::PathBuf>,
    /// Session backend for persisting gateway WS chat sessions
    pub session_backend: Option<Arc<dyn SessionBackend>>,
    /// Per-session actor queue for serializing concurrent turns
    pub session_queue: Arc<session_queue::SessionActorQueue>,
    /// Device registry for paired device management
    pub device_registry: Option<Arc<api_pairing::DeviceRegistry>>,
    /// Pending pairing request store
    pub pending_pairings: Option<Arc<api_pairing::PairingStore>>,
    /// Shared canvas store for Live Canvas (A2UI) system
    pub canvas_store: CanvasStore,
    /// WebAuthn state for hardware key authentication (optional, requires `webauthn` feature)
    #[cfg(feature = "webauthn")]
    pub webauthn: Option<Arc<api_webauthn::WebAuthnState>>,
    /// Per-session cancellation tokens for aborting in-flight agent responses.
    /// Key is session_key (e.g. `gw_<session_id>`), value is the token for the
    /// current turn. Entries are inserted before each turn and removed after
    /// completion (normal or cancelled).
    pub cancel_tokens: Arc<
        std::sync::Mutex<std::collections::HashMap<String, tokio_util::sync::CancellationToken>>,
    >,
    /// Flag set whenever a config write (PATCH, init, map-key mutation) lands
    /// via `persist_and_swap`, cleared on `/admin/reload`. Distinct from disk
    /// drift (which fires only when an external editor touches the file): this
    /// signals "the operator changed config in this session, subsystems may
    /// need to be rebuilt to apply it." The dashboard polls
    /// `/api/config/reload-status` and surfaces a reload banner when true.
    pub pending_reload: Arc<std::sync::atomic::AtomicBool>,
    /// TUI session registry from the daemon (for /api/tuis endpoint).
    /// `None` when the gateway runs standalone without a daemon.
    pub tui_registry: Option<Arc<zeroclaw_runtime::rpc::tui_identity::TuiRegistry>>,
    /// Shared SOP engine from the daemon (for WS agent sessions).
    /// `None` when the gateway runs standalone — sessions build their own.
    pub sop_engine: Option<Arc<std::sync::Mutex<zeroclaw_runtime::sop::SopEngine>>>,
    /// Shared SOP audit logger from the daemon (for WS agent sessions).
    pub sop_audit: Option<Arc<zeroclaw_runtime::sop::SopAuditLogger>>,
}

/// Run the HTTP gateway using axum with proper HTTP/1.1 compliance.
#[allow(clippy::too_many_lines)]
pub async fn run_gateway(
    host: &str,
    port: u16,
    config: Config,
    external_event_tx: Option<tokio::sync::broadcast::Sender<serde_json::Value>>,
    // Reload sender owned by the daemon. /admin/reload writes `true` here;
    // the daemon's wait loop reacts via `subscribe()` and tears down to
    // re-init. Cross-platform replacement for the SIGUSR1 hack.
    reload_tx: Option<tokio::sync::watch::Sender<bool>>,
    // TUI session registry from the daemon for the /api/tuis endpoint.
    tui_registry: Option<Arc<zeroclaw_runtime::rpc::tui_identity::TuiRegistry>>,
    canvas_store: Option<CanvasStore>,
    // Shared SOP engine from the daemon. `None` when standalone — sessions build their own.
    sop_engine: Option<Arc<std::sync::Mutex<zeroclaw_runtime::sop::SopEngine>>>,
    sop_audit: Option<Arc<zeroclaw_runtime::sop::SopAuditLogger>>,
) -> Result<()> {
    // ── Security: warn on public bind without tunnel or explicit opt-in ──
    if is_public_bind(host)
        && config.tunnel.tunnel_provider == "none"
        && !config.gateway.allow_public_bind
    {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
            "⚠️  Binding to {host} — gateway will be exposed to all network interfaces.\n\
             Suggestion: use --host 127.0.0.1 (default), configure a tunnel, or set\n\
             [gateway] allow_public_bind = true in config.toml to silence this warning.\n\n\
             Docker/VM: if you are running inside a container or VM, this is expected."
        );
    }
    let config_state = Arc::new(RwLock::new(config.clone()));

    // ── Hooks ──────────────────────────────────────────────────────
    let hooks: Option<std::sync::Arc<zeroclaw_runtime::hooks::HookRunner>> = if config.hooks.enabled
    {
        Some(std::sync::Arc::new(
            zeroclaw_runtime::hooks::HookRunner::new(),
        ))
    } else {
        None
    };

    let addr: SocketAddr = match format!("{host}:{port}").parse() {
        Ok(a) => a,
        Err(e) => {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({
                        "host": host,
                        "port": port,
                        "error": format!("{e}"),
                    })),
                "Gateway: host:port did not parse as a SocketAddr; falling back to \
                 127.0.0.1 so the gateway can still boot. Fix [gateway] host and \
                 POST /admin/reload."
            );
            SocketAddr::from(([127, 0, 0, 1], port))
        }
    };
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let actual_port = listener.local_addr()?.port();
    let display_addr = format!("{host}:{actual_port}");

    let (boot_family, boot_alias, boot_entry) = config
        .providers
        .models
        .iter_entries()
        .next()
        .map(|(f, a, e)| (f.to_string(), a.to_string(), Some(e)))
        .unwrap_or_else(|| ("openrouter".to_string(), "default".to_string(), None));
    let fallback = boot_entry;
    let model_provider_name = boot_family.as_str();
    let (model_provider, boot_provider_failed): (Arc<dyn ModelProvider>, bool) =
        match zeroclaw_providers::create_resilient_model_provider_from_ref(
            &config,
            model_provider_name,
            fallback.and_then(|e| e.api_key.as_deref()),
            fallback.and_then(|e| e.uri.as_deref()),
            &config.reliability,
            &zeroclaw_providers::provider_runtime_options_for_alias(
                &config,
                &boot_family,
                &boot_alias,
            ),
        ) {
            Ok(p) => (Arc::from(p), false),
            Err(e) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note,)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({
                            "model_provider": model_provider_name,
                            "alias": boot_alias,
                            "error": format!("{e}"),
                        })),
                    "Gateway: seed model_provider failed to construct; booting in \
                     needs_quickstart mode so /quickstart and /admin/reload stay \
                     reachable. Fix the [providers.models.<type>.<alias>] entry \
                     and POST /admin/reload."
                );
                (
                    Arc::new(UnconfiguredModelProvider) as Arc<dyn ModelProvider>,
                    true,
                )
            }
        };
    // Model resolution (1) the first-model_provider's `model`,
    // (2) the first configured `[providers.models.<type>.<alias>]`
    // model with a WARN naming what to set, (3) leave the model empty so
    // the gateway boots and the dashboard can complete browser-based
    // quickstart at /quickstart. The chat-dispatch path checks
    // `state.model.is_empty()` and returns a structured needs_quickstart
    // error before any model_provider call, so the original "no silent
    // vendor-default substitution" guarantee is preserved at request-time
    // rather than at boot. V3 has no global fallback model_provider — every
    // gateway request that needs agent context resolves through its
    // `?agent=` parameter; this resolution is purely the seed value the
    // gateway uses for boot-time logging and the AppState default model
    // string.
    let model = if boot_provider_failed {
        String::new()
    } else {
        match fallback
            .and_then(|e| e.model.as_deref())
            .map(str::trim)
            .filter(|m| !m.is_empty())
        {
            Some(m) => m.to_string(),
            None => match config.resolve_default_model() {
                Some(m) => {
                    ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"model_provider": model_provider_name, "model": m})), "first model_provider has no `model` set; using first configured \
                     providers.models entry as default. Set \
                     [providers.models.<type>.<alias>] model = \"...\" to silence \
                     this warning.");
                    m
                }
                None => {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                            .with_attrs(::serde_json::json!({"display_addr": display_addr})),
                        &format!(
                            "Gateway booting without a configured model. Visit http://{display_addr}/quickstart to complete browser quickstart. Chat endpoints will return 503 needs_quickstart until at least one [providers.models.<type>.<alias>] model = \"...\" is set."
                        )
                    );
                    String::new()
                }
            },
        }
    };
    // Preserve `Option<f64>` end-to-end. Substituting a hardcoded default
    // here would clobber the "let the provider decide" intent for models
    // (e.g. claude-opus-4-7) that reject `temperature`.
    let temperature: Option<f64> = fallback.and_then(|e| e.temperature);
    // Skip the install-wide memory backend init when zero agents are
    // configured. Building a SQLite (or other) backend here would
    // synthesize `<workspace_dir>/memory/brain.db` on a fresh install
    // that has nothing to remember; per-agent memory factories under
    // `agents/<alias>/workspace/memory/` are the only legitimate
    // origin of memory state. AppState gets a NoneMemory
    // stub so endpoints that read `state.mem` keep working until an
    // agent comes online.
    let mem: Arc<dyn Memory> = if config.agents.is_empty() {
        Arc::new(zeroclaw_memory::NoneMemory::new("none"))
    } else {
        match zeroclaw_memory::create_memory_with_storage_and_routes(
            &config.memory,
            &config.embedding_routes,
            config.resolve_active_storage(),
            &config.data_dir,
            fallback.and_then(|e| e.api_key.as_deref()),
        ) {
            Ok(m) => Arc::from(m),
            Err(e) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note,)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({"error": format!("{e}")})),
                    "Gateway: memory backend failed to construct; falling back to \
                     NoneMemory so the gateway can still boot. Fix [memory] and \
                     POST /admin/reload."
                );
                Arc::new(zeroclaw_memory::NoneMemory::new("none"))
            }
        }
    };
    let runtime: Arc<dyn platform::RuntimeAdapter> = match platform::create_runtime(&config.runtime)
    {
        Ok(r) => Arc::from(r),
        Err(e) => {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note,)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({
                        "runtime_kind": config.runtime.kind,
                        "error": format!("{e}"),
                    })),
                "Gateway: runtime adapter failed to construct; falling back to \
                     NativeRuntime so the gateway can still boot. Fix [runtime] and \
                     POST /admin/reload."
            );
            Arc::new(platform::NativeRuntime::new())
        }
    };
    let memory_strategy: Arc<dyn MemoryStrategy> = Arc::new(DefaultMemoryStrategy::with_config(
        mem.clone(),
        config.memory.clone(),
        config.data_dir.clone(),
    ));
    // Gateway is infrastructure — it doesn't run as an agent. Endpoints
    // that need an agent context (`/webhook?agent=`, `/ws/chat?agent=`,
    // ACP `session/new`, agent-scoped tools/memory) take it from the
    // request. The shared SecurityPolicy / risk_profile / tools_registry
    // built here are vestiges driving the legacy single-agent
    // `/api/tools` listing and the `run_gateway_chat_with_tools` test
    // mock; `/webhook` honors `?agent=` per-request (validated against
    // `config.agents`), while SSE / pairing per-request dispatch is still
    // tracked as a follow-up.
    //
    // Agent count is unconstrained at boot. Zero agents is a valid
    // state — the gateway must come up so `/admin/reload` and
    // `/quickstart` can install one — and the legacy seed simply stays
    // empty. With one or more enabled agents, any of them seeds the
    // vestige; aliases are arbitrary so the iteration-order pick is
    // load-bearing on nothing.
    let canvas_store = canvas_store.unwrap_or_default();
    let agent_alias_opt = config
        .agents
        .iter()
        .find(|(_, a)| a.enabled)
        .map(|(alias, _)| alias.clone());

    let (composio_key, composio_entity_id) = if config.composio.enabled {
        (
            config.composio.api_key.as_deref(),
            Some(config.composio.entity_id.as_str()),
        )
    } else {
        (None, None)
    };

    // The seeded `risk_profile` + `SecurityPolicy` here drive the legacy
    // single-agent `/api/tools` listing and the `run_gateway_chat_with_tools`
    // test mock — they are not load-bearing for per-request agent dispatch.
    // When the seed agent's `risk_profile` (or any related per-agent
    // validation) fails to resolve, the gateway must still boot so the
    // operator can fix the config via `/admin/reload` or `/quickstart`
    // instead of crash-looping the daemon supervisor. Degraded boot:
    // log a warning and fall through to the empty-tools-registry branch.
    let agent_setup: Option<(
        zeroclaw_config::schema::RiskProfileConfig,
        Arc<SecurityPolicy>,
    )> = agent_alias_opt.as_ref().and_then(|agent_alias| {
        let Some(risk_profile) = config.risk_profile_for_agent(agent_alias) else {
            ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"agent": agent_alias, "agent_alias": agent_alias})), "Gateway: agents..risk_profile does not name a configured risk_profiles entry; booting with empty tools registry. Fix via /admin/reload or /quickstart.");
            return None;
        };
        let risk_profile = risk_profile.clone();
        let security = match SecurityPolicy::for_agent(&config, agent_alias) {
            Ok(s) => Arc::new(s),
            Err(e) => {
                ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"agent": agent_alias, "error": format!("{}", e), "agent_alias": agent_alias})), "Gateway: agent SecurityPolicy failed to build; booting with empty tools registry. Fix [agents.] via /admin/reload or /quickstart.");
                return None;
            }
        };
        Some((risk_profile, security))
    });

    let (mut tools_registry_raw, delegate_handle_gw) = match (&agent_alias_opt, agent_setup) {
        (Some(agent_alias), Some((risk_profile, security))) => {
            let all_tools_result = tools::all_tools_with_runtime(
                Arc::new(config.clone()),
                &security,
                &risk_profile,
                agent_alias,
                runtime,
                Arc::clone(&mem),
                composio_key,
                composio_entity_id,
                &config.browser,
                &config.http_request,
                &config.web_fetch,
                &config.data_dir,
                &config.agents,
                config
                    .model_provider_for_agent(agent_alias)
                    .and_then(|e| e.api_key.as_deref()),
                &config,
                Some(canvas_store.clone()),
                false,
                None,
                sop_engine.clone(),
                sop_audit.clone(),
            );
            // Wire channel-driven tool handles so the dashboard agent can
            // deliver messages to configured channels (same pattern as
            // orchestrator::start_channels).
            // reaction_handle_gw is PerToolChannelHandle (not Option);
            // register_channels_for_tools expects &Option for all handles.
            let reaction_handle_gw_opt = Some(all_tools_result.reaction_handle.clone());
            let channel_names = zeroclaw_channels::orchestrator::register_channels_for_tools(
                &config,
                &all_tools_result.ask_user_handle,
                &reaction_handle_gw_opt,
                &all_tools_result.poll_handle,
                &all_tools_result.escalate_handle,
            );
            if !channel_names.is_empty() {
                ::zeroclaw_log::record!(
                    INFO,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_attrs(::serde_json::json!({"count": channel_names.len()})),
                    &format!(
                        "Registered {} channel(s) for dashboard agent",
                        channel_names.len()
                    ),
                );
            }
            (all_tools_result.tools, all_tools_result.delegate_handle)
        }
        (Some(_), None) => {
            // Agent existed but its config failed to resolve. Warned
            // above; fall through to the empty-registry shape.
            (Vec::new(), None)
        }
        (None, _) => {
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(::serde_json::json!({"display_addr": display_addr})),
                &format!(
                    "Gateway: no [agents.<alias>] configured — booting with empty tools registry. Visit http://{display_addr}/quickstart to add an agent."
                )
            );
            (Vec::new(), None)
        }
    };

    // ── Wire MCP tools into the gateway tool registry (non-fatal) ───
    // Without this, the `/api/tools` endpoint misses MCP tools.
    if config.mcp.enabled && !config.mcp.servers.is_empty() {
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
            &format!(
                "Gateway: initializing MCP client — {} server(s) configured",
                config.mcp.servers.len()
            )
        );
        match tools::McpRegistry::connect_all(&config.mcp.servers).await {
            Ok(registry) => {
                let registry = std::sync::Arc::new(registry);
                if config.mcp.deferred_loading {
                    let deferred_set =
                        tools::DeferredMcpToolSet::from_registry(std::sync::Arc::clone(&registry))
                            .await;
                    ::zeroclaw_log::record!(
                        INFO,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                        &format!(
                            "Gateway MCP deferred: {} tool stub(s) from {} server(s)",
                            deferred_set.len(),
                            registry.server_count()
                        )
                    );
                    let activated =
                        std::sync::Arc::new(std::sync::Mutex::new(tools::ActivatedToolSet::new()));
                    tools_registry_raw.push(Box::new(tools::ToolSearchTool::new(
                        deferred_set,
                        activated,
                    )));
                } else {
                    let names = registry.tool_names();
                    let mut registered = 0usize;
                    for name in names {
                        if let Some(def) = registry.get_tool_def(&name).await {
                            let wrapper: std::sync::Arc<dyn tools::Tool> =
                                std::sync::Arc::new(tools::McpToolWrapper::new(
                                    name,
                                    def,
                                    std::sync::Arc::clone(&registry),
                                ));
                            if let Some(ref handle) = delegate_handle_gw {
                                handle.write().push(std::sync::Arc::clone(&wrapper));
                            }
                            tools_registry_raw.push(Box::new(tools::ArcToolRef(wrapper)));
                            registered += 1;
                        }
                    }
                    ::zeroclaw_log::record!(
                        INFO,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                        &format!(
                            "Gateway MCP: {} tool(s) registered from {} server(s)",
                            registered,
                            registry.server_count()
                        )
                    );
                }
            }
            Err(e) => {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                    "MCP registry failed to initialize"
                );
            }
        }
    }

    let tools_registry: Arc<Vec<ToolSpec>> =
        Arc::new(tools_registry_raw.iter().map(|t| t.spec()).collect());

    // Cost tracker — process-global singleton so channels share the same instance
    let cost_tracker = CostTracker::get_or_init_global(config.cost.clone(), &config.data_dir);

    // SSE broadcast channel for real-time events.
    // Use an externally provided sender (e.g. from the daemon) so that other
    // components (cron, heartbeat) can publish events to the same bus.
    let event_tx = external_event_tx.unwrap_or_else(|| {
        let (tx, _rx) = tokio::sync::broadcast::channel::<serde_json::Value>(256);
        tx
    });
    let event_buffer = Arc::new(sse::EventBuffer::new(500));
    // Extract webhook secret for authentication
    let webhook_secret_hash: Option<Arc<str>> =
        config.channels.webhook.values().next().and_then(|webhook| {
            webhook.secret.as_ref().and_then(|raw_secret| {
                let trimmed_secret = raw_secret.trim();
                (!trimmed_secret.is_empty())
                    .then(|| Arc::<str>::from(hash_webhook_secret(trimmed_secret)))
            })
        });

    // WhatsApp channel (if configured)
    #[cfg(feature = "channel-whatsapp-cloud")]
    let whatsapp_channel: Option<Arc<WhatsAppChannel>> = config
        .channels
        .whatsapp
        .get("default")
        .filter(|wa| wa.is_cloud_config())
        .map(|wa| {
            let alias = "default".to_string();
            let peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync> = {
                let cfg_arc = config_state.clone();
                let alias = alias.clone();
                Arc::new(move || cfg_arc.read().channel_external_peers("whatsapp", &alias))
            };
            Arc::new(WhatsAppChannel::new(
                wa.access_token.clone().unwrap_or_default(),
                wa.phone_number_id.clone().unwrap_or_default(),
                wa.verify_token.clone().unwrap_or_default(),
                alias,
                peer_resolver,
            ))
        });

    // WhatsApp app secret for webhook signature verification.
    #[cfg(feature = "channel-whatsapp-cloud")]
    let whatsapp_app_secret: Option<Arc<str>> = config
        .channels
        .whatsapp
        .values()
        .next()
        .and_then(|wa| {
            wa.app_secret
                .as_deref()
                .map(str::trim)
                .filter(|secret| !secret.is_empty())
                .map(ToOwned::to_owned)
        })
        .map(Arc::from);

    // Linq channel instances (multi-tenant: one per alias)
    #[cfg(feature = "channel-linq")]
    let linq_channels: HashMap<String, Arc<LinqChannel>> = config
        .channels
        .linq
        .iter()
        .filter(|(_, lq)| lq.enabled)
        .map(|(alias, lq)| {
            let peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync> = {
                let cfg_arc = config_state.clone();
                let alias = alias.clone();
                Arc::new(move || cfg_arc.read().channel_external_peers("linq", &alias))
            };
            (
                alias.clone(),
                Arc::new(LinqChannel::new(
                    lq.api_token.clone(),
                    lq.from_phone.clone(),
                    alias.clone(),
                    peer_resolver,
                )),
            )
        })
        .collect();

    // Linq signing secrets per alias.
    #[cfg(feature = "channel-linq")]
    let linq_signing_secrets: HashMap<String, Arc<str>> = config
        .channels
        .linq
        .iter()
        .filter_map(|(alias, lq)| {
            let secret = lq
                .signing_secret
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(ToOwned::to_owned)?;
            Some((alias.clone(), Arc::from(secret)))
        })
        .collect();

    // WATI channel (if configured)
    #[cfg(feature = "channel-wati")]
    let wati_channel: Option<Arc<WatiChannel>> =
        config.channels.wati.values().next().map(|wati_cfg| {
            let alias = "default".to_string();
            let peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync> = {
                let cfg_arc = config_state.clone();
                let alias = alias.clone();
                Arc::new(move || cfg_arc.read().channel_external_peers("wati", &alias))
            };
            Arc::new(
                WatiChannel::new(
                    wati_cfg.api_token.clone(),
                    wati_cfg.api_url.clone(),
                    wati_cfg.tenant_id.clone(),
                    alias,
                    peer_resolver,
                )
                .with_transcription(config.transcription.clone()),
            )
        });

    // Nextcloud Talk channel (if configured)
    #[cfg(feature = "channel-nextcloud")]
    let nextcloud_talk_channel: Option<Arc<NextcloudTalkChannel>> =
        config.channels.nextcloud_talk.values().next().map(|nc| {
            let alias = "default".to_string();
            let peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync> = {
                let cfg_arc = config_state.clone();
                let alias = alias.clone();
                Arc::new(move || {
                    cfg_arc
                        .read()
                        .channel_external_peers("nextcloud_talk", &alias)
                })
            };
            Arc::new(NextcloudTalkChannel::new(
                nc.base_url.clone(),
                nc.app_token.clone(),
                nc.bot_name.clone().unwrap_or_default(),
                alias,
                peer_resolver,
            ))
        });

    // Nextcloud Talk webhook secret for signature verification.
    #[cfg(feature = "channel-nextcloud")]
    let nextcloud_talk_webhook_secret: Option<Arc<str>> = config
        .channels
        .nextcloud_talk
        .get("default")
        .and_then(|nc| {
            nc.webhook_secret
                .as_deref()
                .map(str::trim)
                .filter(|secret| !secret.is_empty())
                .map(ToOwned::to_owned)
        })
        .map(Arc::from);

    // Gmail Push channel (if configured and referenced by an enabled agent)
    #[cfg(feature = "channel-email")]
    let gmail_push_channel: Option<Arc<GmailPushChannel>> = {
        let active: std::collections::HashSet<String> = config
            .agents
            .values()
            .filter(|a| a.enabled)
            .flat_map(|a| a.channels.iter().map(|c| c.as_str().to_string()))
            .collect();
        config
            .channels
            .gmail_push
            .iter()
            .find(|(alias, _)| active.contains(&format!("gmail_push.{alias}")))
            .map(|(alias, gp)| {
                let alias = alias.clone();
                let peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync> = {
                    let cfg_arc = config_state.clone();
                    let alias = alias.clone();
                    Arc::new(move || cfg_arc.read().channel_external_peers("gmail_push", &alias))
                };
                Arc::new(GmailPushChannel::new(gp.clone(), alias, peer_resolver))
            })
    };

    // ── Session persistence for WS chat ─────────────────────
    // Routes through `make_session_backend` so `[channels].session_backend`
    // is the single source of truth for which backend stores sessions.
    // Picking `"jsonl"` would otherwise leave gateway WS sessions writing
    // to SQLite while channel + tool reads went to JSONL — the original
    // #5769 split, just on a different backend pairing.
    let session_backend: Option<Arc<dyn SessionBackend>> = if config.gateway.session_persistence {
        match zeroclaw_infra::make_session_backend(
            &config.data_dir,
            &config.channels.session_backend,
        ) {
            Ok(backend) => {
                ::zeroclaw_log::record!(
                    INFO,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                    &format!(
                        "Gateway session persistence enabled (backend={})",
                        config.channels.session_backend
                    )
                );
                if config.gateway.session_ttl_hours > 0
                    && let Ok(cleaned) = backend.cleanup_stale(config.gateway.session_ttl_hours)
                    && cleaned > 0
                {
                    ::zeroclaw_log::record!(
                        INFO,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_attrs(::serde_json::json!({"cleaned": cleaned})),
                        "Cleaned up stale gateway sessions"
                    );
                }
                Some(backend)
            }
            Err(e) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                    "Session persistence disabled"
                );
                None
            }
        }
    } else {
        None
    };

    // ── Pairing guard ──────────────────────────────────────
    let pairing = Arc::new(PairingGuard::new(
        config.gateway.require_pairing,
        &config.gateway.paired_tokens,
    ));
    let rate_limit_max_keys = normalize_max_keys(
        config.gateway.rate_limit_max_keys,
        RATE_LIMIT_MAX_KEYS_DEFAULT,
    );
    let rate_limiter = Arc::new(GatewayRateLimiter::new(
        config.gateway.pair_rate_limit_per_minute,
        config.gateway.webhook_rate_limit_per_minute,
        rate_limit_max_keys,
    ));
    let idempotency_max_keys = normalize_max_keys(
        config.gateway.idempotency_max_keys,
        IDEMPOTENCY_MAX_KEYS_DEFAULT,
    );
    let idempotency_store = Arc::new(IdempotencyStore::new(
        Duration::from_secs(config.gateway.idempotency_ttl_secs.max(1)),
        idempotency_max_keys,
    ));

    // Resolve optional path prefix for reverse-proxy deployments.
    let path_prefix: Option<&str> = config
        .gateway
        .path_prefix
        .as_deref()
        .filter(|p| !p.is_empty());

    // ── Tunnel ────────────────────────────────────────────────
    let tunnel = match zeroclaw_runtime::tunnel::create_tunnel(&config.tunnel) {
        Ok(t) => t,
        Err(e) => {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({
                        "tunnel_provider": config.tunnel.tunnel_provider,
                        "error": format!("{e}"),
                    })),
                "Gateway: tunnel adapter failed to construct; booting without a \
                 tunnel. Fix [tunnel] and POST /admin/reload."
            );
            None
        }
    };
    let mut tunnel_url: Option<String> = None;

    if let Some(ref tun) = tunnel {
        println!("🔗 Starting {} tunnel...", tun.name());
        match tun.start(host, actual_port).await {
            Ok(url) => {
                println!("🌐 Tunnel active: {url}");
                tunnel_url = Some(url);
            }
            Err(e) => {
                println!("⚠️  Tunnel failed to start: {e}");
                println!("   Falling back to local-only mode.");
            }
        }
    }

    // Resolve web_dist_dir: explicit config (when valid) → auto-detect.
    // Treat the configured path as advisory — if it doesn't contain
    // index.html on this machine (stale/leaked path from another host,
    // typo, missing build), fall back to auto-detect rather than hard-
    // failing every dashboard request. We log the demotion so the
    // operator can spot a misconfigured path.
    let auto_detect_web_dist = || -> Option<std::path::PathBuf> {
        let mut candidates = vec![
            // Relative to CWD (development: running from repo root)
            std::path::PathBuf::from("web/dist"),
            // Relative to binary (installed alongside binary)
            std::env::current_exe()
                .ok()
                .and_then(|p| p.parent().map(|d| d.join("web/dist")))
                .unwrap_or_default(),
            // Docker / packaged layout
            std::path::PathBuf::from("/zeroclaw-data/web/dist"),
            // AUR / system package
            std::path::PathBuf::from("/usr/share/zeroclawlabs/web/dist"),
        ];
        // XDG data home (prebuilt binary installer)
        if let Some(data_dir) = dirs_data_local() {
            candidates.push(data_dir.join("zeroclaw/web/dist"));
        }
        candidates
            .into_iter()
            .find(|p| !p.as_os_str().is_empty() && p.join("index.html").is_file())
    };

    let web_dist_dir: Option<std::path::PathBuf> = match config
        .gateway
        .web_dist_dir
        .as_ref()
        .map(std::path::PathBuf::from)
    {
        Some(explicit) if explicit.join("index.html").is_file() => Some(explicit),
        Some(stale) => {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({"configured": stale.display().to_string()})),
                "gateway.web_dist_dir points at a path that doesn't contain index.html on \
                 this machine; falling back to auto-detect. Update or remove the setting in \
                 config.toml to silence this warning."
            );
            auto_detect_web_dist()
        }
        None => auto_detect_web_dist(),
    };

    if let Some(ref dir) = web_dist_dir {
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
            &format!("Web dashboard: serving from {}", dir.display().to_string())
        );
    } else if config.gateway.web_dist_dir.is_some() {
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
            "Web dashboard: not available — configured gateway.web_dist_dir is missing on \
             this machine and no fallback location was found. Reinstall with the supported \
             installer (`./install.sh --source` on Linux/macOS, `setup.bat` on Windows) to \
             build and place the dashboard where the gateway looks for it."
        );
    } else {
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
            "Web dashboard: not available — no web/dist found. Reinstall with the supported \
             installer (`./install.sh --source` on Linux/macOS, `setup.bat` on Windows) to \
             build and place the dashboard where the gateway looks for it."
        );
    }

    let pfx = path_prefix.unwrap_or("");
    println!("🦀 ZeroClaw Gateway listening on http://{display_addr}{pfx}");
    if let Some(ref url) = tunnel_url {
        println!("  🌐 Public URL: {url}");
    }
    println!("  🌐 Web Dashboard: http://{display_addr}{pfx}/");
    if let Some(code) = pairing.pairing_code() {
        println!();
        println!("  🔐 PAIRING REQUIRED — use this one-time code:");
        println!("     ┌──────────────┐");
        println!("     │  {code}  │");
        println!("     └──────────────┘");
        println!("     Send: POST {pfx}/pair with header X-Pairing-Code: {code}");
    } else if pairing.require_pairing() {
        for line in already_paired_pairing_notice(host, actual_port, pfx) {
            println!("{line}");
        }
        println!();
    } else {
        println!("  ⚠️  Pairing: DISABLED (all requests accepted)");
        println!();
    }
    println!("  POST {pfx}/pair      — pair a new client (X-Pairing-Code header)");
    println!("  POST {pfx}/webhook   — {{\"message\": \"your prompt\"}}");
    #[cfg(feature = "channel-whatsapp-cloud")]
    if whatsapp_channel.is_some() {
        println!("  GET  {pfx}/whatsapp  — Meta webhook verification");
        println!("  POST {pfx}/whatsapp  — WhatsApp message webhook");
    }
    #[cfg(feature = "channel-linq")]
    if !linq_channels.is_empty() {
        for alias in linq_channels.keys() {
            println!("  POST {pfx}/linq/{alias} — Linq SMS webhook ({alias})");
        }
    }
    #[cfg(feature = "channel-wati")]
    if wati_channel.is_some() {
        println!("  GET  {pfx}/wati      — WATI webhook verification");
        println!("  POST {pfx}/wati      — WATI message webhook");
    }
    #[cfg(feature = "channel-nextcloud")]
    if nextcloud_talk_channel.is_some() {
        println!("  POST {pfx}/nextcloud-talk — Nextcloud Talk bot webhook");
    }
    println!("  GET  {pfx}/api/*     — REST API (bearer token required)");
    println!("  GET  {pfx}/ws/chat   — WebSocket agent chat");
    if config.nodes.enabled {
        println!("  GET  {pfx}/ws/nodes  — WebSocket node discovery");
    }
    println!("  GET  {pfx}/health    — health check");
    println!("  GET  {pfx}/metrics   — Prometheus metrics");
    println!("  Press Ctrl+C to stop.\n");

    zeroclaw_runtime::health::mark_component_ok("gateway");

    // Fire gateway start hook
    if let Some(ref hooks) = hooks {
        hooks.fire_gateway_start(host, actual_port).await;
    }

    // Install the SSE broadcast hook before building any observer so that
    // events emitted by the agent's per-call observer (built inside
    // `process_message`) also reach `/api/events`. The state-level observer
    // is just the configured backend — `TeeObserver` (created by
    // `create_observer`) tees its events into the hook automatically.
    let broadcast_layer: Arc<dyn zeroclaw_runtime::observability::Observer> = Arc::new(
        sse::BroadcastObserver::new(event_tx.clone(), event_buffer.clone()),
    );
    let broadcast_hook_guard =
        zeroclaw_runtime::observability::set_scoped_broadcast_hook(broadcast_layer);

    // Install the same broadcast sender as zeroclaw-log's canonical
    // hook so that every event emitted through `record!` / `record_event`
    // also reaches `/api/events`. The Observer-trait hook above stays
    // wired for legacy `observer.record_event(ObserverEvent::...)`
    // callers that haven't migrated to `record!` yet.
    zeroclaw_log::set_broadcast_hook(event_tx.clone());

    // Bound into AppState. Not a broadcaster — the broadcaster is the
    // `broadcast_layer` installed above as the global hook. This is the
    // configured backend (Log/Prometheus/...) wrapped by `TeeObserver`,
    // which tees events into the hook on every record.
    let state_observer: Arc<dyn zeroclaw_runtime::observability::Observer> = Arc::from(
        zeroclaw_runtime::observability::create_observer(&config.observability),
    );

    let (shutdown_tx, mut shutdown_rx) = tokio::sync::watch::channel(false);

    // Node registry for dynamic node discovery
    let node_registry = Arc::new(nodes::NodeRegistry::new(config.nodes.max_nodes));

    // Device registry and pairing store (only when pairing is required)
    let device_registry = if config.gateway.require_pairing {
        Some(Arc::new(api_pairing::DeviceRegistry::new(&config.data_dir)))
    } else {
        None
    };
    let pending_pairings = if config.gateway.require_pairing {
        Some(Arc::new(api_pairing::PairingStore::new()))
    } else {
        None
    };

    let state = AppState {
        config: config_state,
        model_provider,
        model,
        temperature,
        mem,
        memory_strategy,
        auto_save: config.memory.auto_save,
        webhook_secret_hash,
        pairing,
        trust_forwarded_headers: config.gateway.trust_forwarded_headers,
        rate_limiter,
        auth_limiter: Arc::new(auth_rate_limit::AuthRateLimiter::new()),
        idempotency_store,
        #[cfg(feature = "channel-whatsapp-cloud")]
        whatsapp: whatsapp_channel,
        #[cfg(feature = "channel-whatsapp-cloud")]
        whatsapp_app_secret,
        #[cfg(feature = "channel-linq")]
        linq: linq_channels,
        #[cfg(feature = "channel-linq")]
        linq_signing_secrets,
        #[cfg(feature = "channel-nextcloud")]
        nextcloud_talk: nextcloud_talk_channel,
        #[cfg(feature = "channel-nextcloud")]
        nextcloud_talk_webhook_secret,
        #[cfg(feature = "channel-wati")]
        wati: wati_channel,
        #[cfg(feature = "channel-email")]
        gmail_push: gmail_push_channel,
        observer: state_observer,
        tools_registry,
        cost_tracker,
        event_tx,
        event_buffer,
        shutdown_tx,
        reload_tx,
        node_registry,
        session_backend,
        session_queue: Arc::new(session_queue::SessionActorQueue::new(8, 30, 600)),
        device_registry,
        pending_pairings,
        path_prefix: path_prefix.unwrap_or("").to_string(),
        web_dist_dir,
        canvas_store,
        cancel_tokens: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        pending_reload: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        tui_registry,
        sop_engine,
        sop_audit,
        #[cfg(feature = "webauthn")]
        webauthn: if config.security.webauthn.enabled {
            let secret_store = Arc::new(zeroclaw_runtime::security::SecretStore::new(
                &config.data_dir,
                true,
            ));
            let wa_config = zeroclaw_runtime::security::webauthn::WebAuthnConfig {
                enabled: true,
                rp_id: config.security.webauthn.rp_id.clone(),
                rp_origin: config.security.webauthn.rp_origin.clone(),
                rp_name: config.security.webauthn.rp_name.clone(),
            };
            Some(Arc::new(api_webauthn::WebAuthnState {
                manager: zeroclaw_runtime::security::webauthn::WebAuthnManager::new(
                    wa_config,
                    secret_store,
                    &config.data_dir,
                ),
                pending_registrations: parking_lot::Mutex::new(std::collections::HashMap::new()),
                pending_authentications: parking_lot::Mutex::new(std::collections::HashMap::new()),
            }))
        } else {
            None
        },
    };

    // Build router with middleware
    let inner = Router::new()
        // ── Admin routes (for CLI management) ──
        .route("/admin/shutdown", post(handle_admin_shutdown))
        .route("/admin/reload", post(handle_admin_reload))
        .route("/admin/paircode", get(handle_admin_paircode))
        .route("/admin/paircode/new", post(handle_admin_paircode_new))
        // ── Existing routes ──
        .route("/health", get(handle_health))
        .route("/metrics", get(handle_metrics))
        .route("/pair", post(handle_pair))
        .route("/pair/code", get(handle_pair_code))
        .route("/webhook", post(handle_webhook))
        .merge(optional_channel_routes())
        // ── Claude Code runner hooks ──
        .route("/hooks/claude-code", post(api::handle_claude_code_hook))
        // ── Web Dashboard API routes ──
        .route("/api/status", get(api::handle_api_status))
        .route("/api/logs", get(api_logs::handle_api_logs))
        .route(
            "/api/config",
            get(api_config::handle_config_get)
                .patch(api_config::handle_patch)
                .options(api_config::handle_options_config),
        )
        .route(
            "/api/config/prop",
            get(api_config::handle_prop_get)
                .put(api_config::handle_prop_put)
                .delete(api_config::handle_prop_delete)
                .options(api_config::handle_options_prop),
        )
        .route("/api/config/list", get(api_config::handle_list))
        .route("/api/config/drift", get(api_config::handle_drift))
        .route(
            "/api/config/reload-status",
            get(api_config::handle_reload_status),
        )
        .route("/api/config/templates", get(api_config::handle_templates))
        .route("/api/config/map-keys", get(api_config::handle_get_map_keys))
        .route(
            "/api/config/resolve-alias-source",
            get(api_config::handle_resolve_alias_source),
        )
        .route(
            "/api/config/map-key",
            post(api_config::handle_map_key).delete(api_config::handle_delete_map_key),
        )
        .route("/api/config/rename-map-key", post(api_config::handle_rename_map_key))
        .route("/api/config/catalog", get(api_sections::handle_catalog))
        .route(
            "/api/config/catalog/models",
            get(api_sections::handle_catalog_models),
        )
        .route("/api/config/status", get(api_sections::handle_section_status))
        .route(
            "/api/config/agent-options",
            get(api_sections::handle_agent_options),
        )
        .route("/api/config/sections", get(api_sections::handle_sections))
        .route(
            "/api/config/sections/{section}",
            get(api_sections::handle_section_picker),
        )
        .route(
            "/api/config/sections/{section}/items/{key}",
            post(api_sections::handle_section_select),
        )
        .route("/api/personality", get(api_personality::handle_index))
        .route(
            "/api/quickstart/state",
            get(api_quickstart::handle_state),
        )
        .route(
            "/api/quickstart/fields",
            post(api_quickstart::handle_fields),
        )
        .route(
            "/api/quickstart/validate",
            post(api_quickstart::handle_validate),
        )
        .route(
            "/api/quickstart/apply",
            post(api_quickstart::handle_apply),
        )
        .route(
            "/api/quickstart/dismiss",
            post(api_quickstart::handle_dismiss),
        )
        .route(
            "/api/personality/templates",
            get(api_personality::handle_templates),
        )
        .route(
            "/api/personality/{filename}",
            get(api_personality::handle_get).put(api_personality::handle_put),
        )
        .route("/api/browse", get(api_browse::handle_browse))
        .route("/api/browse/mkdir", post(api_browse::handle_browse_mkdir))
        .route("/api/browse/rmdir", delete(api_browse::handle_browse_rmdir))
        .route(
            "/api/agents/{alias}/workspace/list",
            get(api_browse::handle_agent_workspace_list),
        )
        .route(
            "/api/agents/{alias}/workspace/read",
            get(api_browse::handle_agent_workspace_read),
        )
        .route(
            "/api/agents/{alias}/workspace/path",
            delete(api_browse::handle_agent_workspace_delete),
        )
        .route(
            "/api/agents/{alias}/workspace/move",
            post(api_browse::handle_agent_workspace_move),
        )
        .route(
            "/api/agents/{alias}/workspace/mkdir",
            post(api_browse::handle_agent_workspace_mkdir),
        )
        .route("/api/skills/bundles", get(api_skills::handle_list_bundles))
        .route(
            "/api/skills/bundles/{alias}/skills",
            get(api_skills::handle_list_skills).post(api_skills::handle_create_skill),
        )
        .route(
            "/api/skills/bundles/{alias}/skills/{name}",
            get(api_skills::handle_read_skill)
                .put(api_skills::handle_write_skill)
                .delete(api_skills::handle_delete_skill),
        )
        .route("/api/config/init", post(api_config::handle_init))
        .route("/api/config/migrate", post(api_config::handle_migrate))
        .route("/api/openapi.json", get(openapi::handle_openapi_json))
        .route("/api/docs", get(openapi::handle_docs))
        .route("/api/tools", get(api::handle_api_tools))
        .route("/api/cron", get(api::handle_api_cron_list))
        .route("/api/cron", post(api::handle_api_cron_add))
        .route(
            "/api/cron/settings",
            get(api::handle_api_cron_settings_get).patch(api::handle_api_cron_settings_patch),
        )
        .route(
            "/api/cron/{id}",
            delete(api::handle_api_cron_delete).patch(api::handle_api_cron_patch),
        )
        .route("/api/cron/{id}/runs", get(api::handle_api_cron_runs))
        // Note: `/api/cron/{id}/run` is registered on a separate router below
        // with a longer TimeoutLayer — manual cron triggers run the job
        // synchronously and routinely exceed the 30s gateway-wide default.
        .route("/api/integrations", get(api::handle_api_integrations))
        .route(
            "/api/integrations/settings",
            get(api::handle_api_integrations_settings),
        )
        .route(
            "/api/doctor",
            get(api::handle_api_doctor).post(api::handle_api_doctor),
        )
        .route("/api/memory", get(api::handle_api_memory_list))
        .route("/api/memory", post(api::handle_api_memory_store))
        .route("/api/memory/{key}", delete(api::handle_api_memory_delete))
        .route("/api/cost", get(api::handle_api_cost))
        .route("/api/cli-tools", get(api::handle_api_cli_tools))
        .route("/api/channels", get(api::handle_api_channels))
        .route("/api/health", get(api::handle_api_health))
        .route("/api/tuis", get(api::handle_api_tuis))
        .route("/api/sessions", get(api::handle_api_sessions_list))
        .route("/api/sessions/running", get(api::handle_api_sessions_running))
        .route(
            "/api/sessions/{id}/messages",
            get(api::handle_api_session_messages).post(api::handle_api_session_message_post),
        )
        .route("/api/sessions/{id}", delete(api::handle_api_session_delete).put(api::handle_api_session_rename))
        .route("/api/sessions/{id}/state", get(api::handle_api_session_state))
        .route("/api/sessions/{id}/abort", post(api::handle_api_session_abort))
        // ── Pairing + Device management API ──
        .route("/api/pairing/initiate", post(api_pairing::initiate_pairing))
        .route("/api/pair", post(api_pairing::submit_pairing_enhanced))
        .route("/api/devices", get(api_pairing::list_devices))
        .route(
            "/api/devices/me/capabilities",
            post(api_pairing::update_my_capabilities),
        )
        .route("/api/devices/{id}", delete(api_pairing::revoke_device))
        .route(
            "/api/devices/{id}/token/rotate",
            post(api_pairing::rotate_token),
        )
        // ── Live Canvas (A2UI) routes ──
        .route("/api/canvas", get(canvas::handle_canvas_list))
        .route(
            "/api/canvas/{id}",
            get(canvas::handle_canvas_get)
                .post(canvas::handle_canvas_post)
                .delete(canvas::handle_canvas_clear),
        )
        .route(
            "/api/canvas/{id}/history",
            get(canvas::handle_canvas_history),
        );

    // ── WebAuthn hardware key authentication API (requires webauthn feature) ──
    #[cfg(feature = "webauthn")]
    let inner = inner
        .route(
            "/api/webauthn/register/start",
            post(api_webauthn::handle_register_start),
        )
        .route(
            "/api/webauthn/register/finish",
            post(api_webauthn::handle_register_finish),
        )
        .route(
            "/api/webauthn/auth/start",
            post(api_webauthn::handle_auth_start),
        )
        .route(
            "/api/webauthn/auth/finish",
            post(api_webauthn::handle_auth_finish),
        )
        .route(
            "/api/webauthn/credentials",
            get(api_webauthn::handle_list_credentials),
        )
        .route(
            "/api/webauthn/credentials/{id}",
            delete(api_webauthn::handle_delete_credential),
        );

    // ── Plugin management API (requires plugins-wasm feature) ──
    #[cfg(feature = "plugins-wasm")]
    let inner = inner.route(
        "/api/plugins",
        get(api_plugins::plugin_routes::list_plugins),
    );

    let inner = inner
        // ── SSE event stream ──
        .route("/api/events", get(sse::handle_sse_events))
        .route("/api/events/history", get(sse::handle_events_history))
        // ── ACP client bridge ──
        .route("/acp", get(acp::handle_ws_acp))
        // ── WebSocket agent chat ──
        .route("/ws/chat", get(ws::handle_ws_chat))
        // ── WebSocket canvas updates ──
        .route("/ws/canvas/{id}", get(canvas::handle_ws_canvas))
        // ── WebSocket node discovery ──
        .route("/ws/nodes", get(nodes::handle_ws_nodes))
        // ── Static assets (web dashboard) ──
        .route("/_app/{*path}", get(static_files::handle_static))
        // ── SPA fallback: non-API GET requests serve index.html ──
        .fallback(get(static_files::handle_spa_fallback))
        .with_state(state.clone())
        .layer(RequestBodyLimitLayer::new(MAX_BODY_SIZE))
        .layer(TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            Duration::from_secs(gateway_request_timeout_secs(&config.gateway)),
        ));

    // Manual cron-trigger route lives on its own sub-router so it can opt out
    // of the 30s gateway-wide TimeoutLayer. Layers attached here travel with
    // the route through `merge`, so only this endpoint sees the longer
    // timeout.
    let cron_run_router: Router = Router::new()
        .route("/api/cron/{id}/run", post(api::handle_api_cron_run))
        .with_state(state)
        .layer(RequestBodyLimitLayer::new(MAX_BODY_SIZE))
        .layer(TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            Duration::from_secs(gateway_long_running_request_timeout_secs(&config.gateway)),
        ));

    let inner = inner.merge(cron_run_router);

    // Nest under path prefix when configured (axum strips prefix before routing).
    // nest() at "/prefix" handles both "/prefix" and "/prefix/*" but not "/prefix/"
    // with a trailing slash, so we add a fallback redirect for that case.
    let app = if let Some(prefix) = path_prefix {
        let redirect_target = prefix.to_string();
        Router::new().nest(prefix, inner).route(
            &format!("{prefix}/"),
            get(|| async move { axum::response::Redirect::permanent(&redirect_target) }),
        )
    } else {
        inner
    };

    // ── TLS / mTLS setup ───────────────────────────────────────────
    let tls_acceptor = match &config.gateway.tls {
        Some(tls_cfg) if tls_cfg.enabled => {
            let has_mtls = tls_cfg.client_auth.as_ref().is_some_and(|ca| ca.enabled);
            if has_mtls {
                ::zeroclaw_log::record!(
                    INFO,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                    "TLS enabled with mutual TLS (mTLS) client verification"
                );
            } else {
                ::zeroclaw_log::record!(
                    INFO,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                    "TLS enabled (no client certificate requirement)"
                );
            }
            Some(tls::build_tls_acceptor(tls_cfg)?)
        }
        _ => None,
    };

    if let Some(tls_acceptor) = tls_acceptor {
        // Manual TLS accept loop — serves each connection via hyper.
        let app = app.into_make_service_with_connect_info::<SocketAddr>();
        let mut app = app;

        let mut shutdown_signal = shutdown_rx;
        loop {
            tokio::select! {
                conn = listener.accept() => {
                    let (tcp_stream, remote_addr) = match conn {
                        Ok(pair) => pair,
                        Err(e) => {
                            if is_recoverable_accept_error(&e) {
                                // Transient (e.g. EMFILE under fd pressure):
                                // the listener is still valid. Back off
                                // briefly to avoid hot-spinning, then keep
                                // serving rather than killing the daemon (#7042).
                                ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"error": format!("{}", e)})), "gateway accept() failed with a transient error; backing off and continuing");
                                tokio::time::sleep(Duration::from_millis(ACCEPT_ERROR_BACKOFF_MS)).await;
                                continue;
                            }
                            return Err(e.into());
                        }
                    };
                    let tls_acceptor = tls_acceptor.clone();
                    let svc = tower::MakeService::<
                        SocketAddr,
                        hyper::Request<hyper::body::Incoming>,
                    >::make_service(&mut app, remote_addr)
                    .await
                    .expect("infallible make_service");

                    zeroclaw_spawn::spawn!(async move {
                        let tls_stream = match tls_acceptor.accept(tcp_stream).await {
                            Ok(s) => s,
                            Err(e) => {
                                ::zeroclaw_log::record!(DEBUG, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(::serde_json::json!({"error": format!("{}", e), "remote_addr": remote_addr})), "TLS handshake failed from");
                                return;
                            }
                        };
                        let io = hyper_util::rt::TokioIo::new(tls_stream);
                        let hyper_svc = hyper::service::service_fn(move |req: hyper::Request<hyper::body::Incoming>| {
                            let mut svc = svc.clone();
                            async move {
                                tower::Service::call(&mut svc, req).await
                            }
                        });
                        if let Err(e) = hyper_util::server::conn::auto::Builder::new(
                            hyper_util::rt::TokioExecutor::new(),
                        )
                        .serve_connection(io, hyper_svc)
                        .await
                        {
                            ::zeroclaw_log::record!(DEBUG, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(::serde_json::json!({"error": format!("{}", e), "remote_addr": remote_addr})), "connection error from");
                        }
                    });
                }
                _ = shutdown_signal.changed() => {
                    ::zeroclaw_log::record!(INFO, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note), "ZeroClaw Gateway shutting down");
                    break;
                }
            }
        }
    } else {
        // Plain TCP — use axum's built-in serve.
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .with_graceful_shutdown(async move {
            let _ = shutdown_rx.changed().await;
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                "ZeroClaw Gateway shutting down"
            );
        })
        .await?;
    }

    drop(broadcast_hook_guard);

    Ok(())
}

/// Admin paircode routes are localhost-only ([`require_localhost`]), so the
/// recovery hint must never advertise a non-loopback `--host`: the CLI would
/// then target an address the admin guard rejects with `403`. We omit `--host`
/// entirely and let the CLI fall back to its loopback default. (`_host` is kept
/// for call-site symmetry with [`format_paircode_recovery_curl`].)
fn format_paircode_recovery_command(_host: &str, port: u16) -> String {
    format!("zeroclaw gateway get-paircode --new --port {port}")
}

/// Startup-banner lines for the "pairing required, but no code exists because
/// the gateway is already paired" state.
///
/// By design a fresh one-time code is NOT minted on restart once paired (see
/// [`zeroclaw_config::pairing::PairingGuard::new`]) — that would reopen a
/// standing, brute-forceable pairing window. The earlier banner ("Pairing:
/// ACTIVE (bearer token required)") never said a code was *absent*, so an
/// operator opening the dashboard hit a 6-digit prompt with no code printed
/// anywhere and no in-band way out (#5266). This notice states the absence
/// plainly and points at the commands that mint a code on demand.
///
/// Returned as lines (rather than printed inline in `run_gateway`) so the
/// wording is the single, unit-tested source of truth and can be reused by any
/// other operator-facing surface.
fn already_paired_pairing_notice(host: &str, port: u16, path_prefix: &str) -> Vec<String> {
    vec![
        "  🔒 Pairing: ACTIVE — this gateway is already paired, so no new \
         one-time code was generated on this start."
            .to_string(),
        format!(
            "     To pair another device, run: {}",
            format_paircode_recovery_command(host, port)
        ),
        format!(
            "     Fallback (localhost only): {}",
            format_paircode_recovery_curl(host, port, path_prefix)
        ),
    ]
}

fn format_paircode_recovery_curl(host: &str, port: u16, path_prefix: &str) -> String {
    // Admin paircode routes are localhost-only, so the curl fallback must point
    // at loopback. Bind-only hosts and non-loopback advertised hosts are
    // normalized to `127.0.0.1`; explicit loopback hosts are preserved.
    let recovery_host = paircode_recovery_curl_host(host);
    format!("curl -s -X POST http://{recovery_host}:{port}{path_prefix}/admin/paircode/new")
}

fn paircode_recovery_curl_host(host: &str) -> &str {
    match host {
        "127.0.0.1" | "localhost" => host,
        "::1" => "[::1]",
        _ => "127.0.0.1",
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// AXUM HANDLERS
// ══════════════════════════════════════════════════════════════════════════════

/// GET /health — always public (no secrets leaked)
async fn handle_health(State(state): State<AppState>) -> impl IntoResponse {
    let body = serde_json::json!({
        "status": "ok",
        "paired": state.pairing.is_paired(),
        "require_pairing": state.pairing.require_pairing(),
        "runtime": zeroclaw_runtime::health::snapshot_json(),
    });
    Json(body)
}

/// Prometheus content type for text exposition format.
const PROMETHEUS_CONTENT_TYPE: &str = "text/plain; version=0.0.4; charset=utf-8";

fn prometheus_disabled_hint() -> String {
    String::from(
        "# Prometheus backend not enabled. Set [observability] backend = \"prometheus\" in config.\n",
    )
}

#[cfg(feature = "observability-prometheus")]
fn prometheus_observer_from_state(
    observer: &dyn zeroclaw_runtime::observability::Observer,
) -> Option<&zeroclaw_runtime::observability::PrometheusObserver> {
    // `TeeObserver::as_any` returns the primary observer, so a single direct
    // downcast finds the PrometheusObserver whether the state observer is the
    // raw backend or wrapped by the factory tee.
    observer
        .as_any()
        .downcast_ref::<zeroclaw_runtime::observability::PrometheusObserver>()
}

/// GET /metrics — Prometheus text exposition format
async fn handle_metrics(State(state): State<AppState>) -> impl IntoResponse {
    let body = {
        #[cfg(feature = "observability-prometheus")]
        {
            if let Some(prom) = prometheus_observer_from_state(state.observer.as_ref()) {
                prom.encode()
            } else {
                prometheus_disabled_hint()
            }
        }
        #[cfg(not(feature = "observability-prometheus"))]
        {
            let _ = &state;
            prometheus_disabled_hint()
        }
    };

    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, PROMETHEUS_CONTENT_TYPE)],
        body,
    )
}

/// POST /pair — exchange one-time code for bearer token
#[axum::debug_handler]
async fn handle_pair(
    State(state): State<AppState>,
    ConnectInfo(peer_addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let rate_key =
        client_key_from_request(Some(peer_addr), &headers, state.trust_forwarded_headers);
    if !state.rate_limiter.allow_pair(&rate_key) {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
            "/pair rate limit exceeded"
        );
        let err = serde_json::json!({
            "error": "Too many pairing requests. Please retry later.",
            "retry_after": RATE_LIMIT_WINDOW_SECS,
        });
        return (StatusCode::TOO_MANY_REQUESTS, Json(err));
    }

    // ── Auth rate limiting (brute-force protection) ──
    if let Err(e) = state.auth_limiter.check_rate_limit(&rate_key) {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                .with_attrs(::serde_json::json!({"rate_key": rate_key})),
            "pairing auth rate limit exceeded"
        );
        let err = serde_json::json!({
            "error": format!("Too many auth attempts. Try again in {}s.", e.retry_after_secs),
            "retry_after": e.retry_after_secs,
        });
        return (StatusCode::TOO_MANY_REQUESTS, Json(err));
    }

    let code = headers
        .get("X-Pairing-Code")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    match state.pairing.try_pair(code, &rate_key).await {
        Ok(Some(token)) => {
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                "new client paired successfully"
            );
            if let Err(err) =
                Box::pin(persist_pairing_tokens(state.config.clone(), &state.pairing)).await
            {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"error": format!("{}", err)})),
                    "pairing succeeded but token persistence failed"
                );
                let body = serde_json::json!({
                    "paired": true,
                    "persisted": false,
                    "token": token,
                    "message": "Paired for this process, but failed to persist token to config.toml. Check config path and write permissions.",
                });
                return (StatusCode::OK, Json(body));
            }

            let body = serde_json::json!({
                "paired": true,
                "persisted": true,
                "token": token,
                "message": "Save this token — use it as Authorization: Bearer <token>"
            });
            (StatusCode::OK, Json(body))
        }
        Ok(None) => {
            state.auth_limiter.record_attempt(&rate_key);
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                "pairing attempt with invalid code"
            );
            let err = serde_json::json!({"error": "Invalid pairing code"});
            (StatusCode::FORBIDDEN, Json(err))
        }
        Err(lockout_secs) => {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({"lockout_secs": lockout_secs})),
                "pairing locked out; too many failed attempts"
            );
            let err = serde_json::json!({
                "error": format!("Too many failed attempts. Try again in {lockout_secs}s."),
                "retry_after": lockout_secs
            });
            (StatusCode::TOO_MANY_REQUESTS, Json(err))
        }
    }
}

pub(crate) async fn persist_pairing_tokens(
    config: Arc<RwLock<Config>>,
    pairing: &PairingGuard,
) -> Result<()> {
    let paired_tokens = pairing.tokens();
    // This is needed because parking_lot's guard is not Send so we clone the inner
    // this should be removed once async mutexes are used everywhere
    let mut updated_cfg = { config.read().clone() };
    updated_cfg.gateway.paired_tokens = paired_tokens;
    // Snake-case to match the prop-field name emitted by the `Configurable`
    // derive. Until #7156 the string used here was `gateway.paired-tokens`
    // (kebab); it kept working only thanks to the `-`→`_` fallback in
    // `resolve_dirty_segments`. Aligning all references to the snake form
    // removes that fallback dependency and keeps the codebase consistent.
    updated_cfg.mark_dirty("gateway.paired_tokens");
    updated_cfg
        .save_dirty()
        .await
        .context("Failed to persist paired tokens to config.toml")?;

    // Keep shared runtime config in sync with persisted tokens.
    *config.write() = updated_cfg;
    Ok(())
}

/// Result of a gateway chat turn. Carries the response text plus per-turn
/// token / cost totals captured from the cost-tracking scope (when present)
/// so callers can populate observer-event annotations without racing
/// concurrent webhook traffic that shares the same `CostTracker`.
struct GatewayChatOutcome {
    response: String,
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    cost_usd: Option<f64>,
}

struct UnconfiguredModelProvider;

#[async_trait::async_trait]
impl ModelProvider for UnconfiguredModelProvider {
    async fn chat_with_system(
        &self,
        _system_prompt: Option<&str>,
        _message: &str,
        _model: &str,
        _temperature: Option<f64>,
    ) -> anyhow::Result<String> {
        anyhow::bail!(
            "needs_quickstart: gateway booted without a working model_provider. \
             Complete browser quickstart at /quickstart, or fix \
             [providers.models.<type>.<alias>] and POST /admin/reload."
        )
    }
}

impl ::zeroclaw_api::attribution::Attributable for UnconfiguredModelProvider {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Provider(
            ::zeroclaw_api::attribution::ProviderKind::Model(
                ::zeroclaw_api::attribution::ModelProviderKind::Custom,
            ),
        )
    }
    fn alias(&self) -> &str {
        "unconfigured"
    }
}

/// Returns a structured `needs_quickstart` error when `model` is empty
/// or whitespace-only, otherwise `None`. Empty model means the gateway
/// booted with nothing configured (fresh install). Callers refuse the
/// dispatch with this marker instead of calling the provider with an
/// empty model id. Mirrors `agent::Agent::from_config` at
/// request-time so `/quickstart` stays reachable.
fn needs_quickstart_for(model: &str) -> Option<anyhow::Error> {
    if model.trim().is_empty() {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure),
            "gateway dispatch refused: no model configured (browser quickstart incomplete)"
        );
        Some(anyhow::Error::msg(
            "needs_quickstart: gateway has no model configured. Complete \
             browser quickstart at /quickstart, or set [providers.models.<type>.<alias>] \
             model = \"...\" before sending messages.",
        ))
    } else {
        None
    }
}

/// True when `e` carries the marker produced by `needs_quickstart_for`.
/// Used by chat-dispatch error paths to map the marker to a 503
/// `needs_quickstart` HTTP response or a more accurate channel-side
/// reply, instead of the generic 500 / "sorry" catch-all.
fn is_needs_quickstart_err(e: &anyhow::Error) -> bool {
    e.to_string().contains("needs_quickstart")
}

/// Reply text sent over a channel SDK when chat dispatch refuses
/// because the gateway has no model configured. Resolved through the
/// shared Fluent catalog (`channel-needs-quickstart-reply` in
/// `crates/zeroclaw-runtime/locales/<locale>/cli.ftl`) so non-English
/// operators see localized text instead of a Rust-side English literal.
fn needs_quickstart_channel_reply() -> String {
    i18n::get_required_cli_string("channel-needs-quickstart-reply")
}

/// Full-featured chat with tools for channel and webhook handlers.
///
/// `agent_override` is the caller-requested agent alias (`/webhook?agent=`),
/// already validated against `config.agents` by the handler. `None` keeps the
/// legacy default pick (migration-synthesized "default", else first enabled).
async fn run_gateway_chat_with_tools(
    state: &AppState,
    message: &str,
    session_id: Option<&str>,
    agent_override: Option<&str>,
) -> anyhow::Result<GatewayChatOutcome> {
    if let Some(err) = needs_quickstart_for(&state.model) {
        return Err(err);
    }

    // Tests exercise webhook infrastructure (idempotency, auth, autosave)
    // through handle_webhook, so dispatch to the mock model_provider directly
    // instead of bootstrapping the full agent runtime. The mock path
    // doesn't go through the cost-tracking scope, so usage stays None.
    #[cfg(test)]
    {
        let _ = (session_id, agent_override);
        let response = state
            .model_provider
            .chat_with_system(None, message, &state.model, state.temperature)
            .await?;
        Ok(GatewayChatOutcome {
            response,
            input_tokens: None,
            output_tokens: None,
            cost_usd: None,
        })
    }

    #[cfg(not(test))]
    {
        let config = state.config.read().clone();
        let agent_alias = require_gateway_chat_agent_alias(&config, agent_override)?;

        // Scope the cost tracking context so per-LLM-call usage flows into the
        // gateway's cost tracker and costs.jsonl. Without this scope, the
        // tracker exists on AppState but never receives any records from the
        // runtime tool loop. The context's per-scope `turn_usage` accumulator
        // also lets us read out this turn's tokens / cost after the scope
        // exits without racing concurrent webhook traffic that shares the
        // same tracker. Pricing comes from the V3 per-provider shape
        // (`config.providers.models[*][*].pricing`), keyed as
        // `<type>.<alias>` to match how the channels orchestrator builds
        // its `ModelProviderPricing`.
        let cost_tracking_context = state.cost_tracker.as_ref().map(|tracker| {
            let pricing: zeroclaw_runtime::agent::cost::ModelProviderPricing = config
                .providers
                .models
                .iter_entries()
                .filter(|(_, _, base)| !base.pricing.is_empty())
                .map(|(type_k, alias_k, base)| {
                    (format!("{type_k}.{alias_k}"), base.pricing.clone())
                })
                .collect();
            zeroclaw_runtime::agent::cost::ToolLoopCostTrackingContext::new(
                tracker.clone(),
                std::sync::Arc::new(pricing),
            )
            .with_agent_alias(&agent_alias)
        });
        let captured_usage = cost_tracking_context
            .as_ref()
            .map(|ctx| ctx.turn_usage.clone());
        let response = Box::pin(
            zeroclaw_runtime::agent::cost::TOOL_LOOP_COST_TRACKING_CONTEXT.scope(
                cost_tracking_context,
                zeroclaw_runtime::agent::process_message(config, &agent_alias, message, session_id),
            ),
        )
        .await?;
        let usage = captured_usage
            .map(|cell| *cell.lock())
            .filter(|u| u.input_tokens > 0 || u.output_tokens > 0);
        let (input_tokens, output_tokens, cost_usd) = match usage {
            Some(u) => (
                Some(u.input_tokens),
                Some(u.output_tokens),
                Some(u.cost_usd),
            ),
            None => (None, None, None),
        };
        Ok(GatewayChatOutcome {
            response,
            input_tokens,
            output_tokens,
            cost_usd,
        })
    }
}

fn resolve_gateway_chat_agent_alias(
    config: &Config,
    agent_override: Option<&str>,
) -> Option<String> {
    agent_override
        .map(ToString::to_string)
        .or_else(|| config.resolved_runtime_agent_alias().map(str::to_owned))
}

#[cfg(not(test))]
fn require_gateway_chat_agent_alias(
    config: &Config,
    agent_override: Option<&str>,
) -> anyhow::Result<String> {
    resolve_gateway_chat_agent_alias(config, agent_override).ok_or_else(|| {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure),
            "webhook chat rejected: no configured [agents.<alias>] entry"
        );
        anyhow::Error::msg("webhook chat requires at least one configured [agents.<alias>] entry")
    })
}

fn optional_channel_routes() -> Router<AppState> {
    let router: Router<AppState> = Router::new();
    #[cfg(feature = "channel-whatsapp-cloud")]
    let router = router
        .route("/whatsapp", get(handle_whatsapp_verify))
        .route("/whatsapp", post(handle_whatsapp_message));
    #[cfg(feature = "channel-linq")]
    let router = router.route("/linq/{alias}", post(handle_linq_webhook));
    #[cfg(feature = "channel-wati")]
    let router = router
        .route("/wati", get(handle_wati_verify))
        .route("/wati", post(handle_wati_webhook));
    #[cfg(feature = "channel-nextcloud")]
    let router = router.route("/nextcloud-talk", post(handle_nextcloud_talk_webhook));
    #[cfg(feature = "channel-email")]
    let router = router.route("/webhook/gmail", post(handle_gmail_push_webhook));
    router
}

/// Webhook request body
#[derive(serde::Deserialize)]
pub struct WebhookBody {
    pub message: String,
}

/// Webhook query parameters
#[derive(Default, serde::Deserialize)]
pub struct WebhookQuery {
    /// Configured agent alias to dispatch to. Optional — when omitted, the
    /// legacy pick applies (migration-synthesized "default" agent, else the
    /// first enabled one). Aliases mirror `WsQuery` so `/ws/chat` callers
    /// can reuse their query string verbatim.
    #[serde(default, alias = "agentAlias", alias = "agent_alias")]
    pub agent: Option<String>,
}

/// POST /webhook — main webhook endpoint
async fn handle_webhook(
    State(state): State<AppState>,
    ConnectInfo(peer_addr): ConnectInfo<SocketAddr>,
    Query(query): Query<WebhookQuery>,
    headers: HeaderMap,
    body: Result<Json<WebhookBody>, axum::extract::rejection::JsonRejection>,
) -> impl IntoResponse {
    let rate_key =
        client_key_from_request(Some(peer_addr), &headers, state.trust_forwarded_headers);
    if !state.rate_limiter.allow_webhook(&rate_key) {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
            "/webhook rate limit exceeded"
        );
        let err = serde_json::json!({
            "error": "Too many webhook requests. Please retry later.",
            "retry_after": RATE_LIMIT_WINDOW_SECS,
        });
        return (StatusCode::TOO_MANY_REQUESTS, Json(err));
    }

    // ── Bearer token auth (pairing) with auth rate limiting ──
    if state.pairing.require_pairing() {
        if let Err(e) = state.auth_limiter.check_rate_limit(&rate_key) {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({"rate_key": rate_key})),
                "webhook: auth rate limit exceeded for"
            );
            let err = serde_json::json!({
                "error": format!("Too many auth attempts. Try again in {}s.", e.retry_after_secs),
                "retry_after": e.retry_after_secs,
            });
            return (StatusCode::TOO_MANY_REQUESTS, Json(err));
        }
        let auth = headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        let token = auth.strip_prefix("Bearer ").unwrap_or("");
        if !state.pairing.is_authenticated(token) {
            state.auth_limiter.record_attempt(&rate_key);
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                "webhook: rejected — not paired / invalid bearer token"
            );
            let err = serde_json::json!({
                "error": "Unauthorized — pair first via POST /pair, then send Authorization: Bearer <token>"
            });
            return (StatusCode::UNAUTHORIZED, Json(err));
        }
    }

    // ── Webhook secret auth (optional, additional layer) ──
    if let Some(ref secret_hash) = state.webhook_secret_hash {
        let header_hash = headers
            .get("X-Webhook-Secret")
            .and_then(|v| v.to_str().ok())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(hash_webhook_secret);
        match header_hash {
            Some(val) if constant_time_eq(&val, secret_hash.as_ref()) => {}
            _ => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                    "webhook: rejected request — invalid or missing X-Webhook-Secret"
                );
                let err = serde_json::json!({"error": "Unauthorized — invalid or missing X-Webhook-Secret header"});
                return (StatusCode::UNAUTHORIZED, Json(err));
            }
        }
    }

    // ── Parse body ──
    let Json(webhook_body) = match body {
        Ok(b) => b,
        Err(e) => {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                "webhook JSON parse error"
            );
            let err = serde_json::json!({
                "error": "Invalid JSON body. Expected: {\"message\": \"...\"}"
            });
            return (StatusCode::BAD_REQUEST, Json(err));
        }
    };

    // ── Per-request agent dispatch (optional `?agent=` query param) ──
    // Validate before idempotency / autosave so a typo'd alias doesn't
    // consume the caller's idempotency key. Mirrors the `/ws/chat`
    // unknown-agent rejection.
    let agent_override = query
        .agent
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    if let Some(alias) = agent_override {
        let cfg = state.config.read();
        if cfg.agent(alias).is_none() {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"agent": alias})),
                "webhook: rejected — unknown agent alias"
            );
            let err = serde_json::json!({
                "error": format!(
                    "Unknown agent `{alias}` — no [agents.{alias}] entry configured."
                )
            });
            return (StatusCode::BAD_REQUEST, Json(err));
        }
    }

    // ── Idempotency (optional) ──
    if let Some(idempotency_key) = headers
        .get("X-Idempotency-Key")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        && !state.idempotency_store.record_if_new(idempotency_key)
    {
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_attrs(::serde_json::json!({"idempotency_key": idempotency_key})),
            "webhook duplicate ignored"
        );
        let body = serde_json::json!({
            "status": "duplicate",
            "idempotent": true,
            "message": "Request already processed for this idempotency key"
        });
        return (StatusCode::OK, Json(body));
    }

    let message = &webhook_body.message;
    let session_id = webhook_session_id(&headers);

    if state.auto_save && !zeroclaw_memory::should_skip_autosave_content(message) {
        let key = webhook_memory_key();
        let _ = state
            .mem
            .store(
                &key,
                message,
                MemoryCategory::Conversation,
                session_id.as_deref(),
            )
            .await;
    }

    let (provider_label, model_label) = {
        let cfg = state.config.read();
        let resolved_agent_alias = resolve_gateway_chat_agent_alias(&cfg, agent_override);
        let resolved_provider = resolved_agent_alias
            .as_deref()
            .and_then(|alias| cfg.resolved_model_provider_for_agent(alias));
        let provider_label = resolved_provider
            .as_ref()
            .map(|(ty, alias, _)| format!("{ty}.{alias}"))
            .unwrap_or_else(|| "unknown".to_string());
        let model_label = resolved_provider
            .and_then(|(_, _, entry)| {
                entry
                    .model
                    .as_deref()
                    .map(str::trim)
                    .filter(|model| !model.is_empty())
                    .map(ToString::to_string)
            })
            .or_else(|| cfg.resolve_default_model())
            .unwrap_or_else(|| "<unresolved>".to_string());
        (provider_label, model_label)
    };
    let started_at = Instant::now();

    state.observer.record_event(
        &zeroclaw_runtime::observability::ObserverEvent::AgentStart {
            model_provider: provider_label.clone(),
            model: model_label.clone(),
            channel: None,
            agent_alias: None,
            turn_id: None,
        },
    );
    state.observer.record_event(
        &zeroclaw_runtime::observability::ObserverEvent::LlmRequest {
            model_provider: provider_label.clone(),
            model: model_label.clone(),
            messages_count: 1,
            channel: None,
            agent_alias: None,
            turn_id: None,
        },
    );

    match run_gateway_chat_with_tools(&state, message, session_id.as_deref(), agent_override).await
    {
        Ok(GatewayChatOutcome {
            response,
            input_tokens,
            output_tokens,
            cost_usd,
        }) => {
            let duration = started_at.elapsed();
            // Per-turn token / cost annotation captured from the cost-tracking
            // scope inside `run_gateway_chat_with_tools` (None outside of test
            // / when no LLM call recorded). `TurnUsage` always carries the real
            // input/output split together, so `.zip` either gives both or
            // neither — never fabricate `output_tokens: 0` from an aggregate.
            // Cost is also persisted to /api/cost and costs.jsonl via the same
            // scope.
            let tokens_used = input_tokens.zip(output_tokens).map(|(i, o)| {
                zeroclaw_api::observability_traits::TurnTokenUsage {
                    input_tokens: i,
                    output_tokens: o,
                }
            });
            state.observer.record_event(
                &zeroclaw_runtime::observability::ObserverEvent::LlmResponse {
                    model_provider: provider_label.clone(),
                    model: model_label.clone(),
                    duration,
                    success: true,
                    error_message: None,
                    input_tokens,
                    output_tokens,
                    channel: None,
                    agent_alias: None,
                    turn_id: None,
                },
            );
            state.observer.record_metric(
                &zeroclaw_runtime::observability::traits::ObserverMetric::RequestLatency(duration),
            );
            state.observer.record_event(
                &zeroclaw_runtime::observability::ObserverEvent::AgentEnd {
                    model_provider: provider_label,
                    model: model_label.clone(),
                    duration,
                    tokens_used,
                    cost_usd,
                    channel: None,
                    agent_alias: None,
                    turn_id: None,
                },
            );

            let body = serde_json::json!({"response": response, "model": model_label});
            (StatusCode::OK, Json(body))
        }
        Err(e) => {
            let duration = started_at.elapsed();
            let sanitized = zeroclaw_providers::sanitize_api_error(&e.to_string());

            state.observer.record_event(
                &zeroclaw_runtime::observability::ObserverEvent::LlmResponse {
                    model_provider: provider_label.clone(),
                    model: model_label.clone(),
                    duration,
                    success: false,
                    error_message: Some(sanitized.clone()),
                    input_tokens: None,
                    output_tokens: None,
                    channel: None,
                    agent_alias: None,
                    turn_id: None,
                },
            );
            state.observer.record_metric(
                &zeroclaw_runtime::observability::traits::ObserverMetric::RequestLatency(duration),
            );
            state
                .observer
                .record_event(&zeroclaw_runtime::observability::ObserverEvent::Error {
                    component: "gateway".to_string(),
                    message: sanitized.clone(),
                });
            state.observer.record_event(
                &zeroclaw_runtime::observability::ObserverEvent::AgentEnd {
                    model_provider: provider_label,
                    model: model_label,
                    duration,
                    tokens_used: None,
                    cost_usd: None,
                    channel: None,
                    agent_alias: None,
                    turn_id: None,
                },
            );

            if is_needs_quickstart_err(&e) {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                    "Webhook chat refused: gateway has no model configured; \
                     visit /quickstart"
                );
                let body = serde_json::json!({
                    "error": "needs_quickstart",
                    "url": "/quickstart"
                });
                (StatusCode::SERVICE_UNAVAILABLE, Json(body))
            } else {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"error": sanitized})),
                    "webhook model_provider error"
                );
                let err = serde_json::json!({"error": "LLM request failed"});
                (StatusCode::INTERNAL_SERVER_ERROR, Json(err))
            }
        }
    }
}

/// `WhatsApp` verification query params
#[derive(serde::Deserialize)]
pub struct WhatsAppVerifyQuery {
    #[serde(rename = "hub.mode")]
    pub mode: Option<String>,
    #[serde(rename = "hub.verify_token")]
    pub verify_token: Option<String>,
    #[serde(rename = "hub.challenge")]
    pub challenge: Option<String>,
}

/// GET /whatsapp — Meta webhook verification
#[cfg(feature = "channel-whatsapp-cloud")]
async fn handle_whatsapp_verify(
    State(state): State<AppState>,
    Query(params): Query<WhatsAppVerifyQuery>,
) -> impl IntoResponse {
    let Some(ref wa) = state.whatsapp else {
        return (StatusCode::NOT_FOUND, "WhatsApp not configured".to_string());
    };

    // Verify the token matches (constant-time comparison to prevent timing attacks)
    let token_matches = params
        .verify_token
        .as_deref()
        .is_some_and(|t| constant_time_eq(t, wa.verify_token()));
    if params.mode.as_deref() == Some("subscribe") && token_matches {
        if let Some(ch) = params.challenge {
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(::serde_json::json!({"channel": "whatsapp"})),
                "webhook verified successfully"
            );
            return (StatusCode::OK, ch);
        }
        return (StatusCode::BAD_REQUEST, "Missing hub.challenge".to_string());
    }

    ::zeroclaw_log::record!(
        WARN,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
            .with_attrs(::serde_json::json!({"channel": "whatsapp"})),
        "webhook verification failed — token mismatch"
    );
    (StatusCode::FORBIDDEN, "Forbidden".to_string())
}

/// Verify `WhatsApp` webhook signature (`X-Hub-Signature-256`).
/// Returns true if the signature is valid, false otherwise.
/// See: <https://developers.facebook.com/docs/graph-api/webhooks/getting-started#verification-requests>
pub fn verify_whatsapp_signature(app_secret: &str, body: &[u8], signature_header: &str) -> bool {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;

    // Signature format: "sha256=<hex_signature>"
    let Some(hex_sig) = signature_header.strip_prefix("sha256=") else {
        return false;
    };

    // Decode hex signature
    let Ok(expected) = hex::decode(hex_sig) else {
        return false;
    };

    // Compute HMAC-SHA256
    let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(app_secret.as_bytes()) else {
        return false;
    };
    mac.update(body);

    // Constant-time comparison
    mac.verify_slice(&expected).is_ok()
}

/// POST /whatsapp — incoming message webhook
#[cfg(feature = "channel-whatsapp-cloud")]
async fn handle_whatsapp_message(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    let Some(ref wa) = state.whatsapp else {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "WhatsApp not configured"})),
        );
    };

    // ── Security: Verify X-Hub-Signature-256 if app_secret is configured ──
    if let Some(ref app_secret) = state.whatsapp_app_secret {
        let signature = headers
            .get("X-Hub-Signature-256")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");

        if !verify_whatsapp_signature(app_secret, &body, signature) {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({"channel": "whatsapp"})),
                &format!(
                    "webhook signature verification failed (signature: {})",
                    if signature.is_empty() {
                        "missing"
                    } else {
                        "invalid"
                    }
                )
            );
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({"error": "Invalid signature"})),
            );
        }
    }

    // Parse JSON body
    let Ok(payload) = serde_json::from_slice::<serde_json::Value>(&body) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "Invalid JSON payload"})),
        );
    };

    // Parse messages from the webhook payload
    let messages = wa.parse_webhook_payload(&payload);

    if messages.is_empty() {
        // Acknowledge the webhook even if no messages (could be status updates)
        return (StatusCode::OK, Json(serde_json::json!({"status": "ok"})));
    }

    // Process each message
    for msg in &messages {
        ::zeroclaw_log::record!(INFO, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(::serde_json::json!({"channel": "whatsapp", "sender": msg.sender, "content": msg.content})), "inbound webhook message");

        // Route approval replies to pending approval requests before dispatching to agent
        if let Some((token, response)) = zeroclaw_channels::util::parse_approval_reply(&msg.content)
        {
            let mut map = wa.pending_approvals().lock().await;
            if let Some(sender) = map.remove(&token) {
                let _ = sender.send(response);
                continue;
            }
        }

        let session_id = sender_session_id("whatsapp", msg);

        // Auto-save to memory
        if state.auto_save && !zeroclaw_memory::should_skip_autosave_content(&msg.content) {
            let key = whatsapp_memory_key(msg);
            let _ = state
                .mem
                .store(
                    &key,
                    &msg.content,
                    MemoryCategory::Conversation,
                    Some(&session_id),
                )
                .await;
        }

        match Box::pin(run_gateway_chat_with_tools(
            &state,
            &msg.content,
            Some(&session_id),
            None,
        ))
        .await
        {
            Ok(GatewayChatOutcome { response, .. }) => {
                // Send reply via WhatsApp
                if let Err(e) = wa
                    .send(&SendMessage::new(response, &msg.reply_target))
                    .await
                {
                    ::zeroclaw_log::record!(
                        ERROR,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                        "Failed to send WhatsApp reply"
                    );
                }
            }
            Err(e) => {
                let reply = if is_needs_quickstart_err(&e) {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                        "WhatsApp chat refused: gateway has no model configured; \
                         visit /quickstart"
                    );
                    needs_quickstart_channel_reply()
                } else {
                    ::zeroclaw_log::record!(
                        ERROR,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(
                                ::serde_json::json!({"channel": "whatsapp", "error": format!("{}", e)})
                            ),
                        "LLM error"
                    );
                    "Sorry, I couldn't process your message right now.".to_string()
                };
                let _ = wa.send(&SendMessage::new(reply, &msg.reply_target)).await;
            }
        }
    }

    // Acknowledge the webhook
    (StatusCode::OK, Json(serde_json::json!({"status": "ok"})))
}

/// POST /linq/:alias — incoming message webhook (iMessage/RCS/SMS via Linq)
#[cfg(feature = "channel-linq")]
async fn handle_linq_webhook(
    State(state): State<AppState>,
    Path(alias): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    let Some(linq) = state.linq.get(&alias) else {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": format!("Linq alias '{alias}' not configured")})),
        );
    };

    let body_str = String::from_utf8_lossy(&body);

    // ── Security: Verify X-Webhook-Signature if signing_secret is configured ──
    if let Some(signing_secret) = state.linq_signing_secrets.get(&alias) {
        let timestamp = headers
            .get("X-Webhook-Timestamp")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");

        let signature = headers
            .get("X-Webhook-Signature")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");

        if !zeroclaw_channels::linq::verify_linq_signature(
            signing_secret,
            &body_str,
            timestamp,
            signature,
        ) {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({"channel": "linq", "alias": alias})),
                &format!(
                    "Linq webhook signature verification failed for alias '{alias}' (signature: {})",
                    if signature.is_empty() {
                        "missing"
                    } else {
                        "invalid"
                    }
                )
            );
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({"error": "Invalid signature"})),
            );
        }
    }

    // Parse JSON body
    let Ok(payload) = serde_json::from_slice::<serde_json::Value>(&body) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "Invalid JSON payload"})),
        );
    };

    // Parse messages from the webhook payload
    let messages = linq.parse_webhook_payload(&payload);

    if messages.is_empty() {
        // Acknowledge the webhook even if no messages (could be status/delivery events)
        return (StatusCode::OK, Json(serde_json::json!({"status": "ok"})));
    }

    // Process each message
    for msg in &messages {
        ::zeroclaw_log::record!(INFO, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(::serde_json::json!({"channel": "linq", "alias": alias, "sender": msg.sender, "content": msg.content})), "inbound webhook message");
        let session_id = sender_session_id("linq", msg);

        // Auto-save to memory
        if state.auto_save && !zeroclaw_memory::should_skip_autosave_content(&msg.content) {
            let key = linq_memory_key(msg);
            let _ = state
                .mem
                .store(
                    &key,
                    &msg.content,
                    MemoryCategory::Conversation,
                    Some(&session_id),
                )
                .await;
        }

        // Call the LLM
        match Box::pin(run_gateway_chat_with_tools(
            &state,
            &msg.content,
            Some(&session_id),
            None,
        ))
        .await
        {
            Ok(GatewayChatOutcome { response, .. }) => {
                // Send reply via Linq
                if let Err(e) = linq
                    .send(&SendMessage::new(response, &msg.reply_target))
                    .await
                {
                    ::zeroclaw_log::record!(
                        ERROR,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                        "Failed to send Linq reply"
                    );
                }
            }
            Err(e) => {
                let reply = if is_needs_quickstart_err(&e) {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                        "Linq chat refused: gateway has no model configured; \
                         visit /quickstart"
                    );
                    needs_quickstart_channel_reply()
                } else {
                    ::zeroclaw_log::record!(
                        ERROR,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(
                                ::serde_json::json!({"channel": "linq", "error": format!("{}", e)})
                            ),
                        "LLM error"
                    );
                    "Sorry, I couldn't process your message right now.".to_string()
                };
                let _ = linq.send(&SendMessage::new(reply, &msg.reply_target)).await;
            }
        }
    }

    // Acknowledge the webhook
    (StatusCode::OK, Json(serde_json::json!({"status": "ok"})))
}

/// GET /wati — WATI webhook verification (echoes hub.challenge)
#[cfg(feature = "channel-wati")]
async fn handle_wati_verify(
    State(state): State<AppState>,
    Query(params): Query<WatiVerifyQuery>,
) -> impl IntoResponse {
    if state.wati.is_none() {
        return (StatusCode::NOT_FOUND, "WATI not configured".to_string());
    }

    // WATI may use Meta-style webhook verification; echo the challenge
    if let Some(challenge) = params.challenge {
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_attrs(::serde_json::json!({"channel": "wati"})),
            "webhook verified successfully"
        );
        return (StatusCode::OK, challenge);
    }

    (StatusCode::BAD_REQUEST, "Missing hub.challenge".to_string())
}

#[derive(Debug, serde::Deserialize)]
pub struct WatiVerifyQuery {
    #[serde(rename = "hub.challenge")]
    pub challenge: Option<String>,
}

/// POST /wati — incoming WATI WhatsApp message webhook
#[cfg(feature = "channel-wati")]
async fn handle_wati_webhook(State(state): State<AppState>, body: Bytes) -> impl IntoResponse {
    let Some(ref wati) = state.wati else {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "WATI not configured"})),
        );
    };

    // Parse JSON body
    let Ok(payload) = serde_json::from_slice::<serde_json::Value>(&body) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "Invalid JSON payload"})),
        );
    };

    // Detect audio before the synchronous parse
    let msg_type = payload.get("type").and_then(|v| v.as_str()).unwrap_or("");

    let messages = if matches!(msg_type, "audio" | "voice") {
        // Build a synthetic ChannelMessage from the audio transcript
        if let Some(transcript) = wati.try_transcribe_audio(&payload).await {
            wati.parse_audio_as_message(&payload, transcript)
        } else {
            vec![]
        }
    } else {
        wati.parse_webhook_payload(&payload)
    };

    if messages.is_empty() {
        return (StatusCode::OK, Json(serde_json::json!({"status": "ok"})));
    }

    // Process each message
    for msg in &messages {
        ::zeroclaw_log::record!(INFO, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(::serde_json::json!({"channel": "wati", "sender": msg.sender, "content": msg.content})), "inbound webhook message");
        let session_id = sender_session_id("wati", msg);

        // Auto-save to memory
        if state.auto_save && !zeroclaw_memory::should_skip_autosave_content(&msg.content) {
            let key = wati_memory_key(msg);
            let _ = state
                .mem
                .store(
                    &key,
                    &msg.content,
                    MemoryCategory::Conversation,
                    Some(&session_id),
                )
                .await;
        }

        // Call the LLM
        match Box::pin(run_gateway_chat_with_tools(
            &state,
            &msg.content,
            Some(&session_id),
            None,
        ))
        .await
        {
            Ok(GatewayChatOutcome { response, .. }) => {
                // Send reply via WATI
                if let Err(e) = wati
                    .send(&SendMessage::new(response, &msg.reply_target))
                    .await
                {
                    ::zeroclaw_log::record!(
                        ERROR,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                        "Failed to send WATI reply"
                    );
                }
            }
            Err(e) => {
                let reply = if is_needs_quickstart_err(&e) {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                        "WATI chat refused: gateway has no model configured; \
                         visit /quickstart"
                    );
                    needs_quickstart_channel_reply()
                } else {
                    ::zeroclaw_log::record!(
                        ERROR,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(
                                ::serde_json::json!({"channel": "wati", "error": format!("{}", e)})
                            ),
                        "LLM error"
                    );
                    "Sorry, I couldn't process your message right now.".to_string()
                };
                let _ = wati.send(&SendMessage::new(reply, &msg.reply_target)).await;
            }
        }
    }

    // Acknowledge the webhook
    (StatusCode::OK, Json(serde_json::json!({"status": "ok"})))
}

/// POST /nextcloud-talk — incoming message webhook (Nextcloud Talk bot API)
#[cfg(feature = "channel-nextcloud")]
async fn handle_nextcloud_talk_webhook(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    let Some(ref nextcloud_talk) = state.nextcloud_talk else {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "Nextcloud Talk not configured"})),
        );
    };

    let body_str = String::from_utf8_lossy(&body);

    // ── Security: Verify Nextcloud Talk HMAC signature if secret is configured ──
    if let Some(ref webhook_secret) = state.nextcloud_talk_webhook_secret {
        let random = headers
            .get("X-Nextcloud-Talk-Random")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");

        let signature = headers
            .get("X-Nextcloud-Talk-Signature")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");

        if !zeroclaw_channels::nextcloud_talk::verify_nextcloud_talk_signature(
            webhook_secret,
            random,
            &body_str,
            signature,
        ) {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                &format!(
                    "Nextcloud Talk webhook signature verification failed (signature: {})",
                    if signature.is_empty() {
                        "missing"
                    } else {
                        "invalid"
                    }
                )
            );
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({"error": "Invalid signature"})),
            );
        }
    }

    // Parse JSON body
    let Ok(payload) = serde_json::from_slice::<serde_json::Value>(&body) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "Invalid JSON payload"})),
        );
    };

    // Parse messages from webhook payload
    let messages = nextcloud_talk.parse_webhook_payload(&payload);
    if messages.is_empty() {
        // Acknowledge webhook even if payload does not contain actionable user messages.
        return (StatusCode::OK, Json(serde_json::json!({"status": "ok"})));
    }

    // Spawn per-message processing so the webhook returns 200 quickly.
    // Nextcloud Talk cancels webhook requests that don't complete within ~5s
    // (see #6156); slow local models routinely exceed that. Each message gets
    // its own task — the LLM call and reply are independent of the ack.
    for msg in messages {
        let state = state.clone();
        let nextcloud_talk = Arc::clone(nextcloud_talk);
        zeroclaw_spawn::spawn!(async move {
            ::zeroclaw_log::record!(INFO, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(::serde_json::json!({"channel": "nextcloud_talk", "sender": msg.sender, "content": msg.content})), "inbound webhook message");
            let session_id = sender_session_id("nextcloud_talk", &msg);

            if state.auto_save && !zeroclaw_memory::should_skip_autosave_content(&msg.content) {
                let key = nextcloud_talk_memory_key(&msg);
                let _ = state
                    .mem
                    .store(
                        &key,
                        &msg.content,
                        MemoryCategory::Conversation,
                        Some(&session_id),
                    )
                    .await;
            }

            match Box::pin(run_gateway_chat_with_tools(
                &state,
                &msg.content,
                Some(&session_id),
                None,
            ))
            .await
            {
                Ok(GatewayChatOutcome { response, .. }) => {
                    if let Err(e) = nextcloud_talk
                        .send(&SendMessage::new(response, &msg.reply_target))
                        .await
                    {
                        ::zeroclaw_log::record!(
                            ERROR,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Fail
                            )
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                            "Failed to send Nextcloud Talk reply"
                        );
                    }
                }
                Err(e) => {
                    let reply = if is_needs_quickstart_err(&e) {
                        ::zeroclaw_log::record!(
                            WARN,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Note
                            )
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                            "Nextcloud Talk chat refused: gateway has no model configured; \
                             visit /quickstart"
                        );
                        needs_quickstart_channel_reply()
                    } else {
                        ::zeroclaw_log::record!(ERROR, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail).with_outcome(::zeroclaw_log::EventOutcome::Failure).with_attrs(::serde_json::json!({"channel": "nextcloud_talk", "error": format!("{}", e)})), "LLM error");
                        "Sorry, I couldn't process your message right now.".to_string()
                    };
                    let _ = nextcloud_talk
                        .send(&SendMessage::new(reply, &msg.reply_target))
                        .await;
                }
            }
        });
    }

    (StatusCode::OK, Json(serde_json::json!({"status": "ok"})))
}

/// Maximum request body size for the Gmail webhook endpoint (1 MB).
/// Google Pub/Sub messages are typically under 10 KB.
#[cfg(feature = "channel-email")]
const GMAIL_WEBHOOK_MAX_BODY: usize = 1024 * 1024;

/// POST /webhook/gmail — incoming Gmail Pub/Sub push notification
#[cfg(feature = "channel-email")]
async fn handle_gmail_push_webhook(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    let Some(ref gmail_push) = state.gmail_push else {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "Gmail push not configured"})),
        );
    };

    // Enforce body size limit.
    if body.len() > GMAIL_WEBHOOK_MAX_BODY {
        return (
            StatusCode::PAYLOAD_TOO_LARGE,
            Json(serde_json::json!({"error": "Request body too large"})),
        );
    }

    // Authenticate the webhook request using a shared secret.
    let secret = gmail_push.config.webhook_secret.clone();
    if !secret.is_empty() {
        let provided = headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|auth| auth.strip_prefix("Bearer "))
            .unwrap_or("");

        if provided != secret {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({"channel": "gmail_push"})),
                "webhook: unauthorized request"
            );
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({"error": "Unauthorized"})),
            );
        }
    }

    let body_str = String::from_utf8_lossy(&body);
    let envelope: zeroclaw_channels::gmail_push::PubSubEnvelope =
        match serde_json::from_str(&body_str) {
            Ok(e) => e,
            Err(e) => {
                ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(
                        ::serde_json::json!({"error": format!("{}", e), "channel": "gmail_push"})
                    ),
                "webhook: invalid payload"
            );
                return (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({"error": "Invalid Pub/Sub envelope"})),
                );
            }
        };

    // Process the notification asynchronously (non-blocking for the webhook response)
    let channel = Arc::clone(gmail_push);
    zeroclaw_spawn::spawn!(async move {
        if let Err(e) = channel.handle_notification(&envelope).await {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(
                        ::serde_json::json!({"channel": "gmail_push", "error": format!("{}", e)})
                    ),
                "push notification processing failed"
            );
        }
    });

    // Acknowledge immediately — Google Pub/Sub requires a 2xx within ~10s
    (StatusCode::OK, Json(serde_json::json!({"status": "ok"})))
}

// ══════════════════════════════════════════════════════════════════════════════
// ADMIN HANDLERS (for CLI management)
// ══════════════════════════════════════════════════════════════════════════════

/// Response for admin endpoints
#[derive(serde::Serialize)]
struct AdminResponse {
    success: bool,
    message: String,
}

/// Reject requests that do not originate from a loopback address.
fn require_localhost(peer: &SocketAddr) -> Result<(), (StatusCode, Json<serde_json::Value>)> {
    if peer.ip().is_loopback() {
        Ok(())
    } else {
        Err((
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({
                "error": "Admin endpoints are restricted to localhost"
            })),
        ))
    }
}

/// POST /admin/shutdown — graceful shutdown from CLI (localhost only)
async fn handle_admin_shutdown(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
) -> Result<impl IntoResponse, (StatusCode, Json<serde_json::Value>)> {
    require_localhost(&peer)?;
    ::zeroclaw_log::record!(
        INFO,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
        "admin shutdown request received; initiating graceful shutdown"
    );

    let body = AdminResponse {
        success: true,
        message: "Gateway shutdown initiated".to_string(),
    };

    let _ = state.shutdown_tx.send(true);

    Ok((StatusCode::OK, Json(body)))
}

/// Authorization decision for `POST /admin/reload`, derived purely from the
/// caller's loopback status, the `gateway.allow_remote_admin` flag, and
/// whether pairing is enabled.
#[derive(Debug, PartialEq, Eq)]
enum AdminReloadGate {
    /// Loopback caller (the CLI) — allow without further checks.
    Allow,
    /// Non-loopback caller, opted in with pairing on — allow only if pairing
    /// auth passes.
    RequireAuth,
    /// Non-loopback caller, not opted in — reject.
    Forbidden,
    /// Non-loopback caller opted in, but pairing is disabled — reject rather
    /// than allow an unauthenticated remote reload. `require_auth` is a no-op
    /// when pairing is off, so without this guard `allow_remote_admin` would
    /// expose reload to anonymous remote callers.
    ForbiddenNoPairing,
}

/// Pure gate decision for `/admin/reload`. Auth enforcement (for the
/// `RequireAuth` case) is handled separately by the caller.
///
/// Remote access requires *both* `allow_remote_admin` and pairing: opting in
/// without pairing yields `ForbiddenNoPairing`, never an unauthenticated
/// allow.
fn admin_reload_gate(
    is_loopback: bool,
    allow_remote_admin: bool,
    require_pairing: bool,
) -> AdminReloadGate {
    if is_loopback {
        AdminReloadGate::Allow
    } else if !allow_remote_admin {
        AdminReloadGate::Forbidden
    } else if require_pairing {
        AdminReloadGate::RequireAuth
    } else {
        AdminReloadGate::ForbiddenNoPairing
    }
}

/// POST /admin/reload — reload the daemon in place.
///
/// Loopback callers (the CLI) are always allowed. Non-loopback callers are
/// rejected unless `gateway.allow_remote_admin` is enabled *and* pairing is
/// on, in which case the request must also pass pairing authentication
/// (`require_auth`). Opting in with pairing disabled is rejected rather than
/// allowing an unauthenticated remote reload.
///
/// Sends `true` on the reload channel the daemon owns. The daemon's main
/// wait loop sees the change, returns `DaemonExit::Reload`, and the outer
/// loop in `src/main.rs` re-reads config from disk and re-runs
/// `daemon::run` — re-instantiating every subsystem (gateway / channels /
/// heartbeat / scheduler / mqtt) with the fresh config.
///
/// Same PID throughout. Brief HTTP downtime while the gateway listener
/// rebinds — typically sub-second. Clients should poll `/health` to detect
/// when the new instance is ready.
///
/// Cross-platform — works identically on Linux, macOS, and Windows because
/// the channel is in-process tokio, not an OS signal. The gateway-only
/// `zeroclaw gateway start` (no daemon supervisor) returns 503 with a
/// clear message because there's nothing to signal.
async fn handle_admin_reload(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, (StatusCode, Json<serde_json::Value>)> {
    // Loopback (the CLI) is always allowed. A non-loopback caller is rejected
    // unless the operator opted in via `gateway.allow_remote_admin`, and even
    // then must pass pairing auth — which requires pairing to be enabled, so
    // opting in without pairing is rejected rather than left unauthenticated.
    let allow_remote = state.config.read().gateway.allow_remote_admin;
    // Source pairing status from the guard `require_auth` consults, not the
    // raw config field, so the gate's `RequireAuth` decision can never
    // diverge from what `require_auth` will actually enforce.
    let require_pairing = state.pairing.require_pairing();
    match admin_reload_gate(peer.ip().is_loopback(), allow_remote, require_pairing) {
        AdminReloadGate::Allow => {}
        AdminReloadGate::RequireAuth => api::require_auth(&state, &headers)?,
        AdminReloadGate::Forbidden => {
            return Err((
                StatusCode::FORBIDDEN,
                Json(serde_json::json!({
                    "error": "Remote admin reload is disabled. Call from localhost, \
                              or set gateway.allow_remote_admin = true (with pairing \
                              enabled, then pair) to allow authenticated remote reloads."
                })),
            ));
        }
        AdminReloadGate::ForbiddenNoPairing => {
            return Err((
                StatusCode::FORBIDDEN,
                Json(serde_json::json!({
                    "error": "Remote admin reload requires pairing. \
                              gateway.allow_remote_admin is enabled but \
                              gateway.require_pairing is off, so remote callers \
                              cannot be authenticated. Enable require_pairing, or \
                              call /admin/reload from localhost."
                })),
            ));
        }
    }

    let Some(reload_tx) = state.reload_tx.clone() else {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "error": "no daemon supervisor — running as standalone gateway. \
                          Restart the process to pick up config changes."
            })),
        ));
    };

    ::zeroclaw_log::record!(
        INFO,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
        "admin reload request received"
    );
    // Clear the pending-reload flag before the daemon supervisor brings up
    // the new gateway instance. The fresh instance starts with the flag
    // already false, matching its "subsystems just-loaded, no pending
    // changes" state.
    state
        .pending_reload
        .store(false, std::sync::atomic::Ordering::Relaxed);
    // Trigger graceful shutdown of THIS gateway instance's axum::serve so
    // its TcpListener releases the port before the daemon supervisor
    // spawns the new instance. Without this, daemon::run aborts the
    // gateway tokio task at the next await point — but the OLD listener
    // can stay bound briefly, racing the NEW gateway's bind. The new
    // bind then fails and spawn_component_supervisor backs off; in the
    // meantime the OLD gateway keeps serving requests with stale
    // in-memory config, and `/api/config/drift` reports drift against
    // disk because in-memory hasn't been replaced yet. Cold restart
    // (process exit + start) hits this path differently because the OS
    // fully releases the listener — that's why the user observes "shut
    // down + bring up = correct" but "/admin/reload = stale".
    let shutdown_tx = state.shutdown_tx.clone();
    // Brief delay so the HTTP response flushes before tear-down begins.
    zeroclaw_spawn::spawn!(async move {
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        // Drain axum first so the listener releases.
        let _ = shutdown_tx.send(true);
        // Then signal the daemon to re-read disk and re-spawn subsystems.
        let _ = reload_tx.send(true);
    });

    Ok((
        StatusCode::OK,
        Json(AdminResponse {
            success: true,
            message: "Daemon reload initiated".to_string(),
        }),
    ))
}

/// GET /admin/paircode — fetch current pairing code (localhost only)
async fn handle_admin_paircode(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
) -> Result<impl IntoResponse, (StatusCode, Json<serde_json::Value>)> {
    require_localhost(&peer)?;
    let code = state.pairing.pairing_code();

    let body = if let Some(c) = code {
        serde_json::json!({
            "success": true,
            "pairing_required": state.pairing.require_pairing(),
            "pairing_code": c,
            "message": "Use this one-time code to pair"
        })
    } else {
        serde_json::json!({
            "success": true,
            "pairing_required": state.pairing.require_pairing(),
            "pairing_code": null,
            "message": if state.pairing.require_pairing() {
                "Pairing is active but no new code available (already paired or code expired)"
            } else {
                "Pairing is disabled for this gateway"
            }
        })
    };

    Ok((StatusCode::OK, Json(body)))
}

/// Query parameters for `POST /admin/paircode/new`.
///
/// `rotate` distinguishes the destructive "rotate after compromise" path from
/// the default "add another client" path (#6984):
/// - absent / empty → add another client; existing tokens stay valid.
/// - `rotate=all` → revoke every paired token and clear the device registry,
///   then issue a fresh code. The only safe action when the operator does not
///   know which token leaked.
/// - `rotate=<device_id>` → revoke just that device's token, then issue a code.
#[derive(Debug, serde::Deserialize, Default)]
pub struct AdminPaircodeQuery {
    #[serde(default)]
    pub rotate: Option<String>,
}

/// POST /admin/paircode/new — generate a new pairing code (localhost only).
///
/// With `?rotate=all` or `?rotate=<device_id>` this also revokes existing
/// bearer tokens before issuing the code, so the CLI/admin surface can
/// distinguish "add another client" from "rotate after compromise" (#6984).
async fn handle_admin_paircode_new(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Query(params): Query<AdminPaircodeQuery>,
) -> Result<impl IntoResponse, (StatusCode, Json<serde_json::Value>)> {
    require_localhost(&peer)?;

    if !state.pairing.require_pairing() {
        let body = serde_json::json!({
            "success": false,
            "pairing_required": false,
            "pairing_code": null,
            "message": "Pairing is disabled for this gateway"
        });
        return Ok((StatusCode::BAD_REQUEST, Json(body)));
    }

    let rotate = params
        .rotate
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());

    let revocation_message = match rotate {
        Some("all") => {
            let revoked = state.pairing.revoke_all_tokens();
            if let Some(registry) = state.device_registry.as_ref() {
                if let Err(e) = registry.clear() {
                    let body = serde_json::json!({
                        "success": false,
                        "pairing_required": true,
                        "pairing_code": null,
                        "message": format!("Tokens revoked in memory but device registry clear failed: {e}"),
                    });
                    return Ok((StatusCode::INTERNAL_SERVER_ERROR, Json(body)));
                }
            }
            if let Err(e) = persist_pairing_tokens(state.config.clone(), &state.pairing).await {
                let body = serde_json::json!({
                    "success": false,
                    "pairing_required": true,
                    "pairing_code": null,
                    "message": format!("Tokens revoked in memory but config persist failed: {e}"),
                });
                return Ok((StatusCode::INTERNAL_SERVER_ERROR, Json(body)));
            }
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(::serde_json::json!({"revoked": revoked})),
                "all paired tokens revoked via admin endpoint"
            );
            Some(format!(
                "Revoked all {revoked} paired token(s) and cleared the device registry."
            ))
        }
        Some(device_id) => {
            let Some(registry) = state.device_registry.as_ref() else {
                let body = serde_json::json!({
                    "success": false,
                    "pairing_required": true,
                    "pairing_code": null,
                    "message": "Device registry is disabled; cannot rotate a single device.",
                });
                return Ok((StatusCode::SERVICE_UNAVAILABLE, Json(body)));
            };
            let token_hash = match registry.revoke(device_id) {
                Ok(Some(hash)) => hash,
                Ok(None) => {
                    let body = serde_json::json!({
                        "success": false,
                        "pairing_required": true,
                        "pairing_code": null,
                        "message": format!("Device '{device_id}' not found; nothing revoked."),
                    });
                    return Ok((StatusCode::NOT_FOUND, Json(body)));
                }
                Err(e) => {
                    let body = serde_json::json!({
                        "success": false,
                        "pairing_required": true,
                        "pairing_code": null,
                        "message": format!("Device registry error: {e}"),
                    });
                    return Ok((StatusCode::INTERNAL_SERVER_ERROR, Json(body)));
                }
            };
            state.pairing.revoke_token_hash(&token_hash);
            if let Err(e) = persist_pairing_tokens(state.config.clone(), &state.pairing).await {
                let body = serde_json::json!({
                    "success": false,
                    "pairing_required": true,
                    "pairing_code": null,
                    "message": format!("Token revoked in memory but config persist failed: {e}"),
                });
                return Ok((StatusCode::INTERNAL_SERVER_ERROR, Json(body)));
            }
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                "single device token revoked via admin endpoint"
            );
            Some(format!(
                "Revoked the bearer token for device '{device_id}'."
            ))
        }
        None => None,
    };

    let code = state
        .pairing
        .generate_new_pairing_code()
        .expect("require_pairing checked above");
    if rotate.is_none() {
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
            "new pairing code generated via admin endpoint"
        );
    }

    let message = match revocation_message {
        Some(revoked) => {
            format!("{revoked} Use this one-time code to re-pair.")
        }
        None => "New pairing code generated — use this one-time code to pair".to_string(),
    };

    let body = serde_json::json!({
        "success": true,
        "pairing_required": true,
        "pairing_code": code,
        "message": message,
    });
    Ok((StatusCode::OK, Json(body)))
}

/// GET /pair/code — fetch the initial pairing code (no auth, no localhost restriction).
///
/// This endpoint is intentionally public so that Docker and remote users can see
/// the pairing code on the web dashboard without needing terminal access. It only
/// returns a code when the gateway is in its initial un-paired state (no devices
/// paired yet and a pairing code exists). Once the first device pairs, this
/// endpoint stops returning a code.
async fn handle_pair_code(State(state): State<AppState>) -> impl IntoResponse {
    let require = state.pairing.require_pairing();
    let is_paired = state.pairing.is_paired();

    // Only expose the code during initial setup (before first pairing)
    let code = if require && !is_paired {
        state.pairing.pairing_code()
    } else {
        None
    };

    let body = serde_json::json!({
        "success": true,
        "pairing_required": require,
        "pairing_code": code,
    });

    (StatusCode::OK, Json(body))
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use axum::http::{HeaderValue, Uri};
    use axum::response::IntoResponse;
    use http_body_util::BodyExt;
    use parking_lot::{Mutex, RwLock};
    use std::sync::atomic::{AtomicUsize, Ordering};
    #[cfg(feature = "channel-whatsapp-cloud")]
    use zeroclaw_api::channel::ChannelMessage;
    use zeroclaw_memory::{Memory, MemoryCategory, MemoryEntry};
    use zeroclaw_providers::ModelProvider;

    /// Generate a random hex secret at runtime to avoid hard-coded cryptographic values.
    fn generate_test_secret() -> String {
        let bytes: [u8; 32] = rand::random();
        hex::encode(bytes)
    }

    #[test]
    fn security_body_limit_is_64kb() {
        assert_eq!(MAX_BODY_SIZE, 65_536);
    }

    #[test]
    fn security_timeout_default_is_30_seconds() {
        assert_eq!(REQUEST_TIMEOUT_SECS, 30);
    }

    #[test]
    fn gateway_timeout_uses_typed_config_default() {
        let cfg = zeroclaw_config::schema::GatewayConfig::default();
        assert_eq!(gateway_request_timeout_secs(&cfg), 30);
    }

    #[test]
    fn paircode_recovery_command_includes_alternate_port() {
        assert_eq!(
            format_paircode_recovery_command("127.0.0.1", 42617),
            "zeroclaw gateway get-paircode --new --port 42617"
        );
    }

    #[test]
    fn paircode_recovery_command_includes_specific_host_when_needed() {
        // Admin paircode routes are localhost-only, so the recovery hint must
        // not advertise a non-loopback `--host` (the admin guard would 403 it).
        // The CLI is left to fall back to its loopback default.
        assert_eq!(
            format_paircode_recovery_command("192.168.1.20", 42617),
            "zeroclaw gateway get-paircode --new --port 42617"
        );
    }

    #[test]
    fn paircode_recovery_command_uses_loopback_for_nonloopback_host() {
        // Regression for #6561: a gateway bound to a non-loopback interface must
        // not surface a recovery hint that the localhost-only admin guard rejects.
        let cmd = format_paircode_recovery_command("192.168.1.20", 42617);
        assert!(
            !cmd.contains("192.168.1.20"),
            "recovery command must not advertise the non-loopback bound host: {cmd}"
        );
        assert!(
            !cmd.contains("--host"),
            "recovery command should omit --host so the CLI uses its loopback default: {cmd}"
        );

        let curl = format_paircode_recovery_curl("192.168.1.20", 42617, "");
        assert_eq!(
            curl, "curl -s -X POST http://127.0.0.1:42617/admin/paircode/new",
            "curl fallback must target loopback, not the non-loopback bound host"
        );
        assert!(
            !curl.contains("192.168.1.20"),
            "curl fallback must not advertise the non-loopback bound host: {curl}"
        );

        // Path prefix is still preserved while the host is normalized.
        assert_eq!(
            format_paircode_recovery_curl("192.168.1.20", 42617, "/gw"),
            "curl -s -X POST http://127.0.0.1:42617/gw/admin/paircode/new"
        );
    }

    #[test]
    fn paircode_recovery_curl_targets_running_instance() {
        assert_eq!(
            format_paircode_recovery_curl("127.0.0.1", 42617, ""),
            "curl -s -X POST http://127.0.0.1:42617/admin/paircode/new"
        );
    }

    #[test]
    fn already_paired_notice_states_no_code_was_generated() {
        // Regression for #5266: the banner must say plainly that NO code exists
        // (already paired), not just "Pairing: ACTIVE" — otherwise the operator
        // hits the dashboard's 6-digit prompt with no code printed anywhere.
        let lines = already_paired_pairing_notice("127.0.0.1", 3001, "");
        let joined = lines.join("\n");
        assert!(
            joined.contains("already paired"),
            "notice must say the gateway is already paired: {joined}"
        );
        assert!(
            joined.contains("no new") && joined.contains("code"),
            "notice must state that no new code was generated: {joined}"
        );
    }

    #[test]
    fn already_paired_notice_includes_recovery_command_and_curl() {
        // The notice is the single source of truth for the on-demand recovery
        // commands; it must reuse the loopback-safe builders so the banner and
        // any future surface never drift from #6561's no-`--host` rule.
        let lines = already_paired_pairing_notice("192.168.1.20", 3001, "/gw");
        let joined = lines.join("\n");
        assert!(
            joined.contains(&format_paircode_recovery_command("192.168.1.20", 3001)),
            "notice must surface the get-paircode recovery command: {joined}"
        );
        assert!(
            joined.contains(&format_paircode_recovery_curl("192.168.1.20", 3001, "/gw")),
            "notice must surface the curl fallback (honoring the path prefix): {joined}"
        );
        // #6561: never advertise the non-loopback bound host in the hint.
        assert!(
            !joined.contains("192.168.1.20"),
            "notice must not advertise the non-loopback bound host: {joined}"
        );
    }

    #[test]
    fn paircode_recovery_curl_normalizes_unspecified_bind_hosts() {
        assert_eq!(
            format_paircode_recovery_curl("0.0.0.0", 42617, ""),
            "curl -s -X POST http://127.0.0.1:42617/admin/paircode/new"
        );
        assert_eq!(
            format_paircode_recovery_curl("::", 42617, ""),
            "curl -s -X POST http://127.0.0.1:42617/admin/paircode/new"
        );
    }

    #[test]
    fn paircode_recovery_curl_preserves_actual_loopback_hosts() {
        assert_eq!(
            format_paircode_recovery_curl("localhost", 42617, ""),
            "curl -s -X POST http://localhost:42617/admin/paircode/new"
        );
        assert_eq!(
            format_paircode_recovery_curl("::1", 42617, ""),
            "curl -s -X POST http://[::1]:42617/admin/paircode/new"
        );
    }

    #[test]
    fn paircode_recovery_curl_preserves_path_prefix() {
        assert_eq!(
            format_paircode_recovery_curl("127.0.0.1", 42617, "/gw"),
            "curl -s -X POST http://127.0.0.1:42617/gw/admin/paircode/new"
        );
    }

    /// Build an AppState wired with a real pairing guard, on-disk config path,
    /// and an optional device registry so the admin paircode handler's
    /// revoke + persist paths can be exercised end to end.
    fn admin_paircode_state(
        tmp: &tempfile::TempDir,
        require_pairing: bool,
        with_registry: bool,
    ) -> AppState {
        let data_dir = tmp.path().join("workspace");
        std::fs::create_dir_all(&data_dir).unwrap();
        let config = Config {
            data_dir: data_dir.clone(),
            config_path: tmp.path().join("config.toml"),
            ..Config::default()
        };
        let registry = with_registry.then(|| Arc::new(api_pairing::DeviceRegistry::new(&data_dir)));
        AppState {
            config: Arc::new(RwLock::new(config)),
            model_provider: Arc::new(MockModelProvider::default()),
            model: "test-model".into(),
            temperature: None,
            mem: Arc::new(MockMemory),
            memory_strategy: Arc::new(DefaultMemoryStrategy::with_config(
                Arc::new(MockMemory),
                zeroclaw_config::schema::MemoryConfig::default(),
                std::path::PathBuf::new(),
            )),
            auto_save: false,
            webhook_secret_hash: None,
            pairing: Arc::new(PairingGuard::new(require_pairing, &[])),
            trust_forwarded_headers: false,
            rate_limiter: Arc::new(GatewayRateLimiter::new(100, 100, 100)),
            auth_limiter: Arc::new(auth_rate_limit::AuthRateLimiter::new()),
            idempotency_store: Arc::new(IdempotencyStore::new(Duration::from_secs(300), 1000)),
            #[cfg(feature = "channel-whatsapp-cloud")]
            whatsapp: None,
            #[cfg(feature = "channel-whatsapp-cloud")]
            whatsapp_app_secret: None,
            #[cfg(feature = "channel-linq")]
            linq: HashMap::new(),
            #[cfg(feature = "channel-linq")]
            linq_signing_secrets: HashMap::new(),
            #[cfg(feature = "channel-nextcloud")]
            nextcloud_talk: None,
            #[cfg(feature = "channel-nextcloud")]
            nextcloud_talk_webhook_secret: None,
            #[cfg(feature = "channel-wati")]
            wati: None,
            #[cfg(feature = "channel-email")]
            gmail_push: None,
            observer: Arc::new(zeroclaw_runtime::observability::NoopObserver),
            tools_registry: Arc::new(Vec::new()),
            cost_tracker: None,
            event_tx: tokio::sync::broadcast::channel(16).0,
            event_buffer: Arc::new(sse::EventBuffer::new(16)),
            shutdown_tx: tokio::sync::watch::channel(false).0,
            reload_tx: None,
            node_registry: Arc::new(nodes::NodeRegistry::new(16)),
            path_prefix: String::new(),
            web_dist_dir: None,
            session_backend: None,
            session_queue: std::sync::Arc::new(crate::session_queue::SessionActorQueue::new(
                8, 30, 600,
            )),
            device_registry: registry,
            pending_pairings: None,
            canvas_store: CanvasStore::new(),
            cancel_tokens: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            pending_reload: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            tui_registry: None,
            sop_engine: None,
            sop_audit: None,
            #[cfg(feature = "webauthn")]
            webauthn: None,
        }
    }

    fn spa_fallback_state(tmp: &tempfile::TempDir) -> AppState {
        let dist_dir = tmp.path().join("web").join("dist");
        std::fs::create_dir_all(&dist_dir).unwrap();
        std::fs::write(
            dist_dir.join("index.html"),
            r#"<!DOCTYPE html><html><head></head><body>dashboard shell</body></html>"#,
        )
        .unwrap();

        let mut state = admin_paircode_state(tmp, false, false);
        state.web_dist_dir = Some(dist_dir);
        state
    }

    async fn spa_fallback_response(
        path: &'static str,
        state: AppState,
    ) -> axum::response::Response {
        static_files::handle_spa_fallback(State(state), Uri::from_static(path)).await
    }

    /// Pair a device into both the pairing guard and the device registry,
    /// returning the plaintext token so the test can assert it is revoked.
    async fn pair_device(state: &AppState, device_id: &str) -> String {
        let code = state
            .pairing
            .generate_new_pairing_code()
            .expect("pairing enabled");
        let token = state
            .pairing
            .try_pair(&code, device_id)
            .await
            .unwrap()
            .unwrap();
        state.device_registry.as_ref().unwrap().register(
            PairingGuard::token_hash(&token),
            api_pairing::DeviceInfo {
                id: device_id.to_string(),
                name: None,
                device_type: None,
                paired_at: chrono::Utc::now(),
                last_seen: chrono::Utc::now(),
                ip_address: None,
                capabilities: None,
            },
        );
        token
    }

    async fn admin_paircode_response_json(
        result: Result<impl IntoResponse, (StatusCode, Json<serde_json::Value>)>,
    ) -> (StatusCode, serde_json::Value) {
        let response = result.into_response();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        (status, json)
    }

    /// Default `?` absent path still just adds a client; existing tokens live.
    #[tokio::test]
    async fn admin_paircode_new_without_rotate_keeps_existing_tokens() {
        let tmp = tempfile::TempDir::new().unwrap();
        let state = admin_paircode_state(&tmp, true, true);
        let token = pair_device(&state, "dev-a").await;

        let (status, json) = admin_paircode_response_json(
            handle_admin_paircode_new(
                State(state.clone()),
                test_connect_info(),
                Query(AdminPaircodeQuery::default()),
            )
            .await,
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert!(json["pairing_code"].is_string());
        assert!(
            state.pairing.is_authenticated(&token),
            "add-another-client path must not revoke existing tokens"
        );
    }

    /// `?rotate=all` revokes every token, clears the registry, persists, and
    /// still issues a fresh code.
    #[tokio::test]
    async fn admin_paircode_new_rotate_all_revokes_everything() {
        let tmp = tempfile::TempDir::new().unwrap();
        let state = admin_paircode_state(&tmp, true, true);
        let token_a = pair_device(&state, "dev-a").await;
        let token_b = pair_device(&state, "dev-b").await;

        let (status, json) = admin_paircode_response_json(
            handle_admin_paircode_new(
                State(state.clone()),
                test_connect_info(),
                Query(AdminPaircodeQuery {
                    rotate: Some("all".into()),
                }),
            )
            .await,
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert!(json["pairing_code"].is_string());
        assert!(!state.pairing.is_authenticated(&token_a));
        assert!(!state.pairing.is_authenticated(&token_b));
        assert!(
            state.config.read().gateway.paired_tokens.is_empty(),
            "rotate=all must persist an empty token set"
        );
        assert!(
            state.device_registry.as_ref().unwrap().list().is_empty(),
            "rotate=all must clear the device registry"
        );
    }

    /// `?rotate=<id>` revokes only that device and leaves the rest valid.
    #[tokio::test]
    async fn admin_paircode_new_rotate_device_revokes_one() {
        let tmp = tempfile::TempDir::new().unwrap();
        let state = admin_paircode_state(&tmp, true, true);
        let token_a = pair_device(&state, "dev-a").await;
        let token_b = pair_device(&state, "dev-b").await;

        let (status, json) = admin_paircode_response_json(
            handle_admin_paircode_new(
                State(state.clone()),
                test_connect_info(),
                Query(AdminPaircodeQuery {
                    rotate: Some("dev-a".into()),
                }),
            )
            .await,
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert!(json["pairing_code"].is_string());
        assert!(!state.pairing.is_authenticated(&token_a));
        assert!(
            state.pairing.is_authenticated(&token_b),
            "targeted rotate must not touch other devices"
        );
        let old_hash = PairingGuard::token_hash(&token_a);
        assert!(
            !state
                .config
                .read()
                .gateway
                .paired_tokens
                .contains(&old_hash)
        );
    }

    /// Unknown device id returns 404 and revokes nothing.
    #[tokio::test]
    async fn admin_paircode_new_rotate_unknown_device_is_not_found() {
        let tmp = tempfile::TempDir::new().unwrap();
        let state = admin_paircode_state(&tmp, true, true);
        let token = pair_device(&state, "dev-a").await;

        let (status, _json) = admin_paircode_response_json(
            handle_admin_paircode_new(
                State(state.clone()),
                test_connect_info(),
                Query(AdminPaircodeQuery {
                    rotate: Some("ghost".into()),
                }),
            )
            .await,
        )
        .await;

        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(
            state.pairing.is_authenticated(&token),
            "a not-found rotate must not revoke any token"
        );
    }

    /// Pairing disabled returns 400 regardless of rotate intent.
    #[tokio::test]
    async fn admin_paircode_new_pairing_disabled_is_bad_request() {
        let tmp = tempfile::TempDir::new().unwrap();
        let state = admin_paircode_state(&tmp, false, false);

        let (status, json) = admin_paircode_response_json(
            handle_admin_paircode_new(
                State(state),
                test_connect_info(),
                Query(AdminPaircodeQuery {
                    rotate: Some("all".into()),
                }),
            )
            .await,
        )
        .await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(json["success"], false);
    }

    /// The on-demand mint endpoint is the recovery path advertised to operators
    /// (banner + dashboard "Generate pairing code" button) for the already-paired
    /// state in #5266. It MUST stay localhost-only: a remote peer minting a code
    /// would reopen the brute-forceable pairing window the design deliberately
    /// closes once paired. The dashboard relies on this 403 to fall back to the
    /// CLI hint for non-loopback origins.
    #[tokio::test]
    async fn admin_paircode_new_rejects_remote_peer() {
        let tmp = tempfile::TempDir::new().unwrap();
        let state = admin_paircode_state(&tmp, true, true);

        let remote = ConnectInfo(SocketAddr::from(([203, 0, 113, 7], 40_000)));
        let (status, _json) = admin_paircode_response_json(
            handle_admin_paircode_new(State(state), remote, Query(AdminPaircodeQuery::default()))
                .await,
        )
        .await;

        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "minting a pairing code must be rejected for non-loopback peers"
        );
    }

    #[test]
    fn long_running_request_timeout_default_is_ten_minutes() {
        assert_eq!(LONG_RUNNING_REQUEST_TIMEOUT_SECS, 600);
    }

    #[test]
    fn long_running_request_timeout_uses_typed_config_default() {
        let cfg = zeroclaw_config::schema::GatewayConfig::default();
        assert_eq!(gateway_long_running_request_timeout_secs(&cfg), 600);
    }

    #[test]
    fn webhook_body_requires_message_field() {
        let valid = r#"{"message": "hello"}"#;
        let parsed: Result<WebhookBody, _> = serde_json::from_str(valid);
        assert!(parsed.is_ok());
        assert_eq!(parsed.unwrap().message, "hello");

        let missing = r#"{"other": "field"}"#;
        let parsed: Result<WebhookBody, _> = serde_json::from_str(missing);
        assert!(parsed.is_err());
    }

    #[test]
    fn whatsapp_query_fields_are_optional() {
        let q = WhatsAppVerifyQuery {
            mode: None,
            verify_token: None,
            challenge: None,
        };
        assert!(q.mode.is_none());
    }

    #[test]
    fn app_state_is_clone() {
        fn assert_clone<T: Clone>() {}
        assert_clone::<AppState>();
    }

    #[tokio::test]
    async fn spa_fallback_returns_json_not_html_for_unknown_api_path() {
        let tmp = tempfile::TempDir::new().unwrap();
        let state = spa_fallback_state(&tmp);

        let response = spa_fallback_response("/api/agents", state).await;

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert!(
            response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .is_some_and(|value| value.starts_with("application/json")),
            "unknown API paths must not be served as HTML"
        );

        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["error"], "not_found");
        assert_eq!(json["path"], "/api/agents");
    }

    #[tokio::test]
    async fn spa_fallback_returns_json_for_api_root_path() {
        let tmp = tempfile::TempDir::new().unwrap();
        let state = spa_fallback_state(&tmp);

        let response = spa_fallback_response("/api", state).await;

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["path"], "/api");
    }

    #[tokio::test]
    async fn spa_fallback_returns_json_for_path_prefixed_api_miss() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut state = spa_fallback_state(&tmp);
        state.path_prefix = "/gw".to_string();

        let response = spa_fallback_response("/gw/api/agents", state).await;

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["path"], "/api/agents");
    }

    #[tokio::test]
    async fn spa_fallback_still_serves_dashboard_routes() {
        let tmp = tempfile::TempDir::new().unwrap();
        let state = spa_fallback_state(&tmp);

        let response = spa_fallback_response("/config", state).await;

        assert_eq!(response.status(), StatusCode::OK);
        assert!(
            response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .is_some_and(|value| value.starts_with("text/html")),
            "dashboard routes should still receive the SPA shell"
        );

        let body = response.into_body().collect().await.unwrap().to_bytes();
        let text = String::from_utf8(body.to_vec()).unwrap();
        assert!(text.contains("dashboard shell"));
    }

    #[tokio::test]
    async fn spa_fallback_does_not_treat_api_like_spa_paths_as_api() {
        let tmp = tempfile::TempDir::new().unwrap();
        let state = spa_fallback_state(&tmp);

        let response = spa_fallback_response("/apiary", state).await;

        assert_eq!(response.status(), StatusCode::OK);
        assert!(
            response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .is_some_and(|value| value.starts_with("text/html")),
            "similarly named SPA routes should not be reserved as API paths"
        );
    }

    /// Regression: the gateway must boot with zero configured agents so
    /// a fresh install can reach `/admin/reload` and `/quickstart` to add
    /// one. Earlier the boot path returned
    /// `gateway start requires at least one configured [agents.<alias>]
    /// entry`, which crashed the daemon supervisor before the reload
    /// channel could be exercised.
    #[tokio::test]
    async fn run_gateway_starts_with_zero_agents() {
        // Isolate data_dir so parallel nextest runs don't race on the
        // real ~/.zeroclaw/data (see #7054).
        let tmp = tempfile::TempDir::new().unwrap();
        let config = zeroclaw_config::schema::Config {
            data_dir: tmp.path().join("workspace"),
            config_path: tmp.path().join("config.toml"),
            ..zeroclaw_config::schema::Config::default()
        };
        std::fs::create_dir_all(&config.data_dir).unwrap();

        // Default Config has no [agents.*] entries — the exact shape
        // a fresh install presents on first daemon boot.
        assert!(
            config.agents.is_empty(),
            "regression assumes default Config has no agents",
        );

        // Bind to an ephemeral port on loopback. If the boot path
        // erred on the agents-required check, the join would resolve
        // immediately with that Err. We race a short delay against
        // the spawn: a still-running task at the deadline means boot
        // got far enough to start serving.
        let handle = zeroclaw_spawn::spawn!(async move {
            run_gateway("127.0.0.1", 0, config, None, None, None, None, None, None).await
        });

        match tokio::time::timeout(
            std::time::Duration::from_millis(750),
            &mut Box::pin(async {
                // We cannot await `handle` directly because the gateway
                // never returns under normal operation; instead, peek at
                // whether it has finished by polling join with a tiny
                // budget.
                let _ = tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            }),
        )
        .await
        {
            Ok(()) => {}
            Err(_) => panic!("test setup timed out before checking gateway state"),
        }

        // If the boot path errored, the task is finished and join
        // returns the error. If it's still running, abort and accept
        // boot reached the serving stage.
        if handle.is_finished() {
            let result = handle.await.expect("task did not panic");
            panic!(
                "gateway exited during boot with zero agents — must stay up for reload/quickstart: {:?}",
                result
            );
        }
        handle.abort();
    }

    /// Regression: the gateway must boot even when an enabled agent's
    /// `risk_profile` does not name a configured `risk_profiles` entry.
    /// Earlier the boot path used `config.risk_profile_for_agent(...).with_context(...)?`
    /// which propagated up through the daemon supervisor and crash-looped
    /// the gateway component, locking the operator out of `/admin/reload`
    /// and `/quickstart` — the exact endpoints they need to fix the broken
    /// risk_profile reference. The fix degrades gracefully: warn,
    /// fall through to an empty tools registry, keep serving.
    #[tokio::test]
    async fn run_gateway_starts_with_unresolved_agent_risk_profile() {
        use zeroclaw_config::schema::AliasedAgentConfig;

        // Isolate data_dir so parallel nextest runs don't race on the
        // real ~/.zeroclaw/data (see #7054).
        let tmp = tempfile::TempDir::new().unwrap();
        let mut config = zeroclaw_config::schema::Config {
            data_dir: tmp.path().join("workspace"),
            config_path: tmp.path().join("config.toml"),
            ..zeroclaw_config::schema::Config::default()
        };
        std::fs::create_dir_all(&config.data_dir).unwrap();

        // Enabled agent whose `risk_profile` does not resolve. No
        // matching [risk_profiles.<key>] entry exists.
        let agent = AliasedAgentConfig {
            enabled: true,
            risk_profile: "definitely_not_configured".into(),
            ..AliasedAgentConfig::default()
        };
        config.agents.insert("fake123".to_string(), agent);

        let handle = zeroclaw_spawn::spawn!(async move {
            run_gateway("127.0.0.1", 0, config, None, None, None, None, None, None).await
        });

        match tokio::time::timeout(
            std::time::Duration::from_millis(750),
            &mut Box::pin(async {
                let _ = tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            }),
        )
        .await
        {
            Ok(()) => {}
            Err(_) => panic!("test setup timed out before checking gateway state"),
        }

        if handle.is_finished() {
            let result = handle.await.expect("task did not panic");
            panic!(
                "gateway exited during boot when agent.risk_profile was unresolved \
                 — must stay up so operator can fix via /admin/reload or /quickstart: {:?}",
                result
            );
        }
        handle.abort();
    }

    #[tokio::test]
    async fn run_gateway_starts_with_mismatched_provider_api_key() {
        let mut config = Config::default();
        config.providers.models.anthropic.insert(
            "default".to_string(),
            zeroclaw_config::schema::AnthropicModelProviderConfig {
                base: zeroclaw_config::schema::ModelProviderConfig {
                    model: Some("anthropic/claude-sonnet-4-6".to_string()),
                    api_key: Some("sk-test-openai-shaped-key".to_string()),
                    ..Default::default()
                },
            },
        );

        let handle = zeroclaw_spawn::spawn!(async move {
            run_gateway("127.0.0.1", 0, config, None, None, None, None, None, None).await
        });

        match tokio::time::timeout(
            std::time::Duration::from_millis(750),
            &mut Box::pin(async {
                let _ = tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            }),
        )
        .await
        {
            Ok(()) => {}
            Err(_) => panic!("test setup timed out before checking gateway state"),
        }

        if handle.is_finished() {
            let result = handle.await.expect("task did not panic");
            panic!(
                "gateway exited during boot when seed provider API key was \
                 mismatched — must stay up so operator can fix via /admin/reload \
                 or /quickstart: {:?}",
                result
            );
        }
        handle.abort();
    }

    #[tokio::test]
    async fn metrics_endpoint_returns_hint_when_prometheus_is_disabled() {
        let state = AppState {
            config: Arc::new(RwLock::new(Config::default())),
            model_provider: Arc::new(MockModelProvider::default()),
            model: "test-model".into(),
            temperature: None,
            mem: Arc::new(MockMemory),
            memory_strategy: Arc::new(DefaultMemoryStrategy::with_config(
                Arc::new(MockMemory),
                zeroclaw_config::schema::MemoryConfig::default(),
                std::path::PathBuf::new(),
            )),
            auto_save: false,
            webhook_secret_hash: None,
            pairing: Arc::new(PairingGuard::new(false, &[])),
            trust_forwarded_headers: false,
            rate_limiter: Arc::new(GatewayRateLimiter::new(100, 100, 100)),
            auth_limiter: Arc::new(auth_rate_limit::AuthRateLimiter::new()),
            idempotency_store: Arc::new(IdempotencyStore::new(Duration::from_secs(300), 1000)),
            #[cfg(feature = "channel-whatsapp-cloud")]
            whatsapp: None,
            #[cfg(feature = "channel-whatsapp-cloud")]
            whatsapp_app_secret: None,
            #[cfg(feature = "channel-linq")]
            linq: HashMap::new(),
            #[cfg(feature = "channel-linq")]
            linq_signing_secrets: HashMap::new(),
            #[cfg(feature = "channel-nextcloud")]
            nextcloud_talk: None,
            #[cfg(feature = "channel-nextcloud")]
            nextcloud_talk_webhook_secret: None,
            #[cfg(feature = "channel-wati")]
            wati: None,
            #[cfg(feature = "channel-email")]
            gmail_push: None,
            observer: Arc::new(zeroclaw_runtime::observability::NoopObserver),
            tools_registry: Arc::new(Vec::new()),
            cost_tracker: None,
            event_tx: tokio::sync::broadcast::channel(16).0,
            event_buffer: Arc::new(sse::EventBuffer::new(16)),
            shutdown_tx: tokio::sync::watch::channel(false).0,
            reload_tx: None,
            node_registry: Arc::new(nodes::NodeRegistry::new(16)),
            path_prefix: String::new(),
            web_dist_dir: None,
            session_backend: None,
            session_queue: std::sync::Arc::new(crate::session_queue::SessionActorQueue::new(
                8, 30, 600,
            )),
            device_registry: None,
            pending_pairings: None,
            canvas_store: CanvasStore::new(),
            cancel_tokens: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            pending_reload: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            tui_registry: None,
            sop_engine: None,
            sop_audit: None,
            #[cfg(feature = "webauthn")]
            webauthn: None,
        };

        let response = handle_metrics(State(state)).await.into_response();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some(PROMETHEUS_CONTENT_TYPE)
        );

        let body = response.into_body().collect().await.unwrap().to_bytes();
        let text = String::from_utf8(body.to_vec()).unwrap();
        assert!(text.contains("Prometheus backend not enabled"));
    }

    #[cfg(feature = "observability-prometheus")]
    #[tokio::test]
    async fn metrics_endpoint_renders_prometheus_output() {
        let event_tx = tokio::sync::broadcast::channel(16).0;
        let prom = zeroclaw_runtime::observability::PrometheusObserver::new();
        zeroclaw_runtime::observability::Observer::record_event(
            &prom,
            &zeroclaw_runtime::observability::ObserverEvent::HeartbeatTick,
        );

        let observer: Arc<dyn zeroclaw_runtime::observability::Observer> = Arc::new(prom);
        let state = AppState {
            config: Arc::new(RwLock::new(Config::default())),
            model_provider: Arc::new(MockModelProvider::default()),
            model: "test-model".into(),
            temperature: None,
            mem: Arc::new(MockMemory),
            memory_strategy: Arc::new(DefaultMemoryStrategy::with_config(
                Arc::new(MockMemory),
                zeroclaw_config::schema::MemoryConfig::default(),
                std::path::PathBuf::new(),
            )),
            auto_save: false,
            webhook_secret_hash: None,
            pairing: Arc::new(PairingGuard::new(false, &[])),
            trust_forwarded_headers: false,
            rate_limiter: Arc::new(GatewayRateLimiter::new(100, 100, 100)),
            auth_limiter: Arc::new(auth_rate_limit::AuthRateLimiter::new()),
            idempotency_store: Arc::new(IdempotencyStore::new(Duration::from_secs(300), 1000)),
            #[cfg(feature = "channel-whatsapp-cloud")]
            whatsapp: None,
            #[cfg(feature = "channel-whatsapp-cloud")]
            whatsapp_app_secret: None,
            #[cfg(feature = "channel-linq")]
            linq: HashMap::new(),
            #[cfg(feature = "channel-linq")]
            linq_signing_secrets: HashMap::new(),
            #[cfg(feature = "channel-nextcloud")]
            nextcloud_talk: None,
            #[cfg(feature = "channel-nextcloud")]
            nextcloud_talk_webhook_secret: None,
            #[cfg(feature = "channel-wati")]
            wati: None,
            #[cfg(feature = "channel-email")]
            gmail_push: None,
            observer,
            tools_registry: Arc::new(Vec::new()),
            cost_tracker: None,
            event_tx,
            event_buffer: Arc::new(sse::EventBuffer::new(16)),
            shutdown_tx: tokio::sync::watch::channel(false).0,
            reload_tx: None,
            node_registry: Arc::new(nodes::NodeRegistry::new(16)),
            path_prefix: String::new(),
            web_dist_dir: None,
            session_backend: None,
            session_queue: std::sync::Arc::new(crate::session_queue::SessionActorQueue::new(
                8, 30, 600,
            )),
            device_registry: None,
            pending_pairings: None,
            canvas_store: CanvasStore::new(),
            cancel_tokens: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            pending_reload: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            tui_registry: None,
            sop_engine: None,
            sop_audit: None,
            #[cfg(feature = "webauthn")]
            webauthn: None,
        };

        let response = handle_metrics(State(state)).await.into_response();
        assert_eq!(response.status(), StatusCode::OK);

        let body = response.into_body().collect().await.unwrap().to_bytes();
        let text = String::from_utf8(body.to_vec()).unwrap();
        assert!(text.contains("zeroclaw_heartbeat_ticks_total 1"));
    }

    #[test]
    fn gateway_rate_limiter_blocks_after_limit() {
        let limiter = GatewayRateLimiter::new(2, 2, 100);
        assert!(limiter.allow_pair("127.0.0.1"));
        assert!(limiter.allow_pair("127.0.0.1"));
        assert!(!limiter.allow_pair("127.0.0.1"));
    }

    #[test]
    fn rate_limiter_sweep_removes_stale_entries() {
        let limiter = SlidingWindowRateLimiter::new(10, Duration::from_secs(60), 100);
        // Add entries for multiple IPs
        assert!(limiter.allow("ip-1"));
        assert!(limiter.allow("ip-2"));
        assert!(limiter.allow("ip-3"));

        {
            let guard = limiter.requests.lock();
            assert_eq!(guard.0.len(), 3);
        }

        // Force a sweep by backdating last_sweep
        {
            let mut guard = limiter.requests.lock();
            guard.1 = Instant::now()
                .checked_sub(Duration::from_secs(RATE_LIMITER_SWEEP_INTERVAL_SECS + 1))
                .unwrap();
            // Clear timestamps for ip-2 and ip-3 to simulate stale entries
            guard.0.get_mut("ip-2").unwrap().clear();
            guard.0.get_mut("ip-3").unwrap().clear();
        }

        // Next allow() call should trigger sweep and remove stale entries
        assert!(limiter.allow("ip-1"));

        {
            let guard = limiter.requests.lock();
            assert_eq!(guard.0.len(), 1, "Stale entries should have been swept");
            assert!(guard.0.contains_key("ip-1"));
        }
    }

    #[test]
    fn rate_limiter_zero_limit_always_allows() {
        let limiter = SlidingWindowRateLimiter::new(0, Duration::from_secs(60), 10);
        for _ in 0..100 {
            assert!(limiter.allow("any-key"));
        }
    }

    #[test]
    fn idempotency_store_rejects_duplicate_key() {
        let store = IdempotencyStore::new(Duration::from_secs(30), 10);
        assert!(store.record_if_new("req-1"));
        assert!(!store.record_if_new("req-1"));
        assert!(store.record_if_new("req-2"));
    }

    #[test]
    fn rate_limiter_bounded_cardinality_evicts_oldest_key() {
        let limiter = SlidingWindowRateLimiter::new(5, Duration::from_secs(60), 2);
        assert!(limiter.allow("ip-1"));
        assert!(limiter.allow("ip-2"));
        assert!(limiter.allow("ip-3"));

        let guard = limiter.requests.lock();
        assert_eq!(guard.0.len(), 2);
        assert!(guard.0.contains_key("ip-2"));
        assert!(guard.0.contains_key("ip-3"));
    }

    #[test]
    fn idempotency_store_bounded_cardinality_evicts_oldest_key() {
        let store = IdempotencyStore::new(Duration::from_secs(300), 2);
        assert!(store.record_if_new("k1"));
        std::thread::sleep(Duration::from_millis(2));
        assert!(store.record_if_new("k2"));
        std::thread::sleep(Duration::from_millis(2));
        assert!(store.record_if_new("k3"));

        let keys = store.keys.lock();
        assert_eq!(keys.len(), 2);
        assert!(!keys.contains_key("k1"));
        assert!(keys.contains_key("k2"));
        assert!(keys.contains_key("k3"));
    }

    #[test]
    fn client_key_defaults_to_peer_addr_when_untrusted_proxy_mode() {
        let peer = SocketAddr::from(([10, 0, 0, 5], 42617));
        let mut headers = HeaderMap::new();
        headers.insert(
            "X-Forwarded-For",
            HeaderValue::from_static("198.51.100.10, 203.0.113.11"),
        );

        let key = client_key_from_request(Some(peer), &headers, false);
        assert_eq!(key, "10.0.0.5");
    }

    #[test]
    fn client_key_uses_forwarded_ip_only_in_trusted_proxy_mode() {
        let peer = SocketAddr::from(([10, 0, 0, 5], 42617));
        let mut headers = HeaderMap::new();
        headers.insert(
            "X-Forwarded-For",
            HeaderValue::from_static("198.51.100.10, 203.0.113.11"),
        );

        let key = client_key_from_request(Some(peer), &headers, true);
        assert_eq!(key, "198.51.100.10");
    }

    #[test]
    fn client_key_falls_back_to_peer_when_forwarded_header_invalid() {
        let peer = SocketAddr::from(([10, 0, 0, 5], 42617));
        let mut headers = HeaderMap::new();
        headers.insert("X-Forwarded-For", HeaderValue::from_static("garbage-value"));

        let key = client_key_from_request(Some(peer), &headers, true);
        assert_eq!(key, "10.0.0.5");
    }

    #[test]
    fn normalize_max_keys_uses_fallback_for_zero() {
        assert_eq!(normalize_max_keys(0, 10_000), 10_000);
        assert_eq!(normalize_max_keys(0, 0), 1);
    }

    #[test]
    fn normalize_max_keys_preserves_nonzero_values() {
        assert_eq!(normalize_max_keys(2_048, 10_000), 2_048);
        assert_eq!(normalize_max_keys(1, 10_000), 1);
    }

    #[tokio::test]
    async fn persist_pairing_tokens_writes_config_tokens() {
        let temp = tempfile::tempdir().unwrap();
        let config_path = temp.path().join("config.toml");
        let workspace_path = temp.path().join("workspace");

        let config = Config {
            config_path: config_path.clone(),
            data_dir: workspace_path,
            ..Default::default()
        };
        config.save().await.unwrap();

        let guard = PairingGuard::new(true, &[]);
        let code = guard.pairing_code().unwrap();
        let token = guard.try_pair(&code, "test_client").await.unwrap().unwrap();
        assert!(guard.is_authenticated(&token));

        let shared_config = Arc::new(RwLock::new(config));
        Box::pin(persist_pairing_tokens(shared_config.clone(), &guard))
            .await
            .unwrap();

        // In-memory tokens should remain as plaintext 64-char hex hashes.
        let plaintext = {
            let in_memory = shared_config.read();
            assert_eq!(in_memory.gateway.paired_tokens.len(), 1);
            in_memory.gateway.paired_tokens[0].clone()
        };
        assert_eq!(plaintext.len(), 64);
        assert!(plaintext.chars().all(|c: char| c.is_ascii_hexdigit()));

        // On disk, the token should be encrypted (secrets.encrypt defaults to true).
        let saved = tokio::fs::read_to_string(config_path).await.unwrap();
        let raw_parsed: Config = toml::from_str(&saved).unwrap();
        assert_eq!(raw_parsed.gateway.paired_tokens.len(), 1);
        let on_disk = &raw_parsed.gateway.paired_tokens[0];
        assert!(
            zeroclaw_runtime::security::SecretStore::is_encrypted(on_disk),
            "paired_token should be encrypted on disk"
        );
    }

    #[test]
    fn webhook_memory_key_is_unique() {
        let key1 = webhook_memory_key();
        let key2 = webhook_memory_key();

        assert!(key1.starts_with("webhook_msg_"));
        assert!(key2.starts_with("webhook_msg_"));
        assert_ne!(key1, key2);
    }

    #[test]
    fn webhook_session_id_accepts_valid() {
        let mut headers = HeaderMap::new();
        headers.insert("X-Session-Id", HeaderValue::from_static("abc-DEF_123.foo"));
        assert_eq!(webhook_session_id(&headers), Some("abc-DEF_123.foo".into()));
    }

    #[test]
    fn webhook_session_id_trims_whitespace() {
        let mut headers = HeaderMap::new();
        headers.insert("X-Session-Id", HeaderValue::from_static("  my-session  "));
        assert_eq!(webhook_session_id(&headers), Some("my-session".into()));
    }

    #[test]
    fn webhook_session_id_rejects_empty() {
        let mut headers = HeaderMap::new();
        headers.insert("X-Session-Id", HeaderValue::from_static(""));
        assert_eq!(webhook_session_id(&headers), None);

        headers.insert("X-Session-Id", HeaderValue::from_static("   "));
        assert_eq!(webhook_session_id(&headers), None);
    }

    #[test]
    fn webhook_session_id_rejects_missing() {
        let headers = HeaderMap::new();
        assert_eq!(webhook_session_id(&headers), None);
    }

    #[test]
    fn webhook_session_id_rejects_oversized() {
        let mut headers = HeaderMap::new();
        let long = "a".repeat(129);
        headers.insert("X-Session-Id", HeaderValue::from_str(&long).unwrap());
        assert_eq!(webhook_session_id(&headers), None);

        let at_limit = "b".repeat(128);
        headers.insert("X-Session-Id", HeaderValue::from_str(&at_limit).unwrap());
        assert!(webhook_session_id(&headers).is_some());
    }

    #[test]
    fn webhook_session_id_rejects_invalid_chars() {
        let mut headers = HeaderMap::new();
        for bad in &[
            "has/slash",
            "has:colon",
            "has space",
            "has@at",
            "emoji\u{1f600}",
        ] {
            if let Ok(val) = HeaderValue::from_str(bad) {
                headers.insert("X-Session-Id", val);
                assert_eq!(webhook_session_id(&headers), None, "should reject: {bad}");
            }
        }
    }

    #[cfg(feature = "channel-whatsapp-cloud")]
    #[test]
    fn whatsapp_memory_key_includes_sender_and_message_id() {
        let msg = ChannelMessage {
            id: "wamid-123".into(),
            sender: "+1234567890".into(),
            reply_target: "+1234567890".into(),
            content: "hello".into(),
            channel: "whatsapp".into(),
            channel_alias: None,
            timestamp: 1,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![],
            subject: None,
        };

        let key = whatsapp_memory_key(&msg);
        assert_eq!(key, "whatsapp_+1234567890_wamid-123");
    }

    #[derive(Default)]
    struct MockMemory;

    #[async_trait]
    impl Memory for MockMemory {
        fn name(&self) -> &str {
            "mock"
        }

        async fn store(
            &self,
            _key: &str,
            _content: &str,
            _category: MemoryCategory,
            _session_id: Option<&str>,
        ) -> anyhow::Result<()> {
            Ok(())
        }

        async fn recall(
            &self,
            _query: &str,
            _limit: usize,
            _session_id: Option<&str>,
            _since: Option<&str>,
            _until: Option<&str>,
        ) -> anyhow::Result<Vec<MemoryEntry>> {
            Ok(Vec::new())
        }

        async fn get(&self, _key: &str) -> anyhow::Result<Option<MemoryEntry>> {
            Ok(None)
        }

        async fn list(
            &self,
            _category: Option<&MemoryCategory>,
            _session_id: Option<&str>,
        ) -> anyhow::Result<Vec<MemoryEntry>> {
            Ok(Vec::new())
        }

        async fn forget(&self, _key: &str) -> anyhow::Result<bool> {
            Ok(false)
        }

        async fn forget_for_agent(&self, _key: &str, _agent_id: &str) -> anyhow::Result<bool> {
            Ok(false)
        }

        async fn count(&self) -> anyhow::Result<usize> {
            Ok(0)
        }

        async fn health_check(&self) -> bool {
            true
        }

        async fn store_with_agent(
            &self,
            _key: &str,
            _content: &str,
            _category: MemoryCategory,
            _session_id: Option<&str>,
            _namespace: Option<&str>,
            _importance: Option<f64>,
            _agent_id: Option<&str>,
        ) -> anyhow::Result<()> {
            Ok(())
        }

        async fn recall_for_agents(
            &self,
            _allowed_agent_ids: &[&str],
            _query: &str,
            _limit: usize,
            _session_id: Option<&str>,
            _since: Option<&str>,
            _until: Option<&str>,
        ) -> anyhow::Result<Vec<MemoryEntry>> {
            Ok(Vec::new())
        }
    }
    impl ::zeroclaw_api::attribution::Attributable for MockMemory {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Memory(
                ::zeroclaw_api::attribution::MemoryKind::InMemory,
            )
        }
        fn alias(&self) -> &str {
            "MockMemory"
        }
    }

    #[derive(Default)]
    struct MockModelProvider {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl ModelProvider for MockModelProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok("ok".into())
        }
    }
    impl ::zeroclaw_api::attribution::Attributable for MockModelProvider {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Provider(
                ::zeroclaw_api::attribution::ProviderKind::Model(
                    ::zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }
        fn alias(&self) -> &str {
            "MockModelProvider"
        }
    }

    #[derive(Default)]
    struct CapturingObserver {
        events: Mutex<Vec<zeroclaw_runtime::observability::ObserverEvent>>,
    }

    impl zeroclaw_runtime::observability::Observer for CapturingObserver {
        fn record_event(&self, event: &zeroclaw_runtime::observability::ObserverEvent) {
            self.events.lock().push(event.clone());
        }

        fn record_metric(&self, _metric: &zeroclaw_runtime::observability::traits::ObserverMetric) {
        }

        fn name(&self) -> &str {
            "capturing"
        }

        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
    }

    #[derive(Default)]
    struct TrackingMemory {
        keys: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl Memory for TrackingMemory {
        fn name(&self) -> &str {
            "tracking"
        }

        async fn store(
            &self,
            key: &str,
            _content: &str,
            _category: MemoryCategory,
            _session_id: Option<&str>,
        ) -> anyhow::Result<()> {
            self.keys.lock().push(key.to_string());
            Ok(())
        }

        async fn recall(
            &self,
            _query: &str,
            _limit: usize,
            _session_id: Option<&str>,
            _since: Option<&str>,
            _until: Option<&str>,
        ) -> anyhow::Result<Vec<MemoryEntry>> {
            Ok(Vec::new())
        }

        async fn get(&self, _key: &str) -> anyhow::Result<Option<MemoryEntry>> {
            Ok(None)
        }

        async fn list(
            &self,
            _category: Option<&MemoryCategory>,
            _session_id: Option<&str>,
        ) -> anyhow::Result<Vec<MemoryEntry>> {
            Ok(Vec::new())
        }

        async fn forget(&self, _key: &str) -> anyhow::Result<bool> {
            Ok(false)
        }

        async fn forget_for_agent(&self, _key: &str, _agent_id: &str) -> anyhow::Result<bool> {
            Ok(false)
        }

        async fn count(&self) -> anyhow::Result<usize> {
            let size = self.keys.lock().len();
            Ok(size)
        }

        async fn health_check(&self) -> bool {
            true
        }

        async fn store_with_agent(
            &self,
            key: &str,
            content: &str,
            category: MemoryCategory,
            session_id: Option<&str>,
            _namespace: Option<&str>,
            _importance: Option<f64>,
            _agent_id: Option<&str>,
        ) -> anyhow::Result<()> {
            self.store(key, content, category, session_id).await
        }

        async fn recall_for_agents(
            &self,
            _allowed_agent_ids: &[&str],
            _query: &str,
            _limit: usize,
            _session_id: Option<&str>,
            _since: Option<&str>,
            _until: Option<&str>,
        ) -> anyhow::Result<Vec<MemoryEntry>> {
            Ok(Vec::new())
        }
    }
    impl ::zeroclaw_api::attribution::Attributable for TrackingMemory {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Memory(
                ::zeroclaw_api::attribution::MemoryKind::InMemory,
            )
        }
        fn alias(&self) -> &str {
            "TrackingMemory"
        }
    }

    fn test_connect_info() -> ConnectInfo<SocketAddr> {
        ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 30_300)))
    }

    #[tokio::test]
    async fn webhook_idempotency_skips_duplicate_provider_calls() {
        let provider_impl = Arc::new(MockModelProvider::default());
        let model_provider: Arc<dyn ModelProvider> = provider_impl.clone();
        let memory: Arc<dyn Memory> = Arc::new(MockMemory);

        let state = AppState {
            config: Arc::new(RwLock::new(Config::default())),
            model_provider,
            model: "test-model".into(),
            temperature: None,
            mem: memory.clone(),
            memory_strategy: Arc::new(DefaultMemoryStrategy::with_config(
                Arc::clone(&memory),
                zeroclaw_config::schema::MemoryConfig::default(),
                std::path::PathBuf::new(),
            )),
            auto_save: false,
            webhook_secret_hash: None,
            pairing: Arc::new(PairingGuard::new(false, &[])),
            trust_forwarded_headers: false,
            rate_limiter: Arc::new(GatewayRateLimiter::new(100, 100, 100)),
            auth_limiter: Arc::new(auth_rate_limit::AuthRateLimiter::new()),
            idempotency_store: Arc::new(IdempotencyStore::new(Duration::from_secs(300), 1000)),
            #[cfg(feature = "channel-whatsapp-cloud")]
            whatsapp: None,
            #[cfg(feature = "channel-whatsapp-cloud")]
            whatsapp_app_secret: None,
            #[cfg(feature = "channel-linq")]
            linq: HashMap::new(),
            #[cfg(feature = "channel-linq")]
            linq_signing_secrets: HashMap::new(),
            #[cfg(feature = "channel-nextcloud")]
            nextcloud_talk: None,
            #[cfg(feature = "channel-nextcloud")]
            nextcloud_talk_webhook_secret: None,
            #[cfg(feature = "channel-wati")]
            wati: None,
            #[cfg(feature = "channel-email")]
            gmail_push: None,
            observer: Arc::new(zeroclaw_runtime::observability::NoopObserver),
            tools_registry: Arc::new(Vec::new()),
            cost_tracker: None,
            event_tx: tokio::sync::broadcast::channel(16).0,
            event_buffer: Arc::new(sse::EventBuffer::new(16)),
            shutdown_tx: tokio::sync::watch::channel(false).0,
            reload_tx: None,
            node_registry: Arc::new(nodes::NodeRegistry::new(16)),
            path_prefix: String::new(),
            web_dist_dir: None,
            session_backend: None,
            session_queue: std::sync::Arc::new(crate::session_queue::SessionActorQueue::new(
                8, 30, 600,
            )),
            device_registry: None,
            pending_pairings: None,
            canvas_store: CanvasStore::new(),
            cancel_tokens: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            pending_reload: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            tui_registry: None,
            sop_engine: None,
            sop_audit: None,
            #[cfg(feature = "webauthn")]
            webauthn: None,
        };

        let mut headers = HeaderMap::new();
        headers.insert("X-Idempotency-Key", HeaderValue::from_static("abc-123"));

        let body = Ok(Json(WebhookBody {
            message: "hello".into(),
        }));
        let first = handle_webhook(
            State(state.clone()),
            test_connect_info(),
            Query(WebhookQuery::default()),
            headers.clone(),
            body,
        )
        .await
        .into_response();
        assert_eq!(first.status(), StatusCode::OK);

        let body = Ok(Json(WebhookBody {
            message: "hello".into(),
        }));
        let second = handle_webhook(
            State(state),
            test_connect_info(),
            Query(WebhookQuery::default()),
            headers,
            body,
        )
        .await
        .into_response();
        assert_eq!(second.status(), StatusCode::OK);

        let payload = second.into_body().collect().await.unwrap().to_bytes();
        let parsed: serde_json::Value = serde_json::from_slice(&payload).unwrap();
        assert_eq!(parsed["status"], "duplicate");
        assert_eq!(parsed["idempotent"], true);
        assert_eq!(provider_impl.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn webhook_unknown_agent_rejected_before_dispatch() {
        let provider_impl = Arc::new(MockModelProvider::default());
        let model_provider: Arc<dyn ModelProvider> = provider_impl.clone();
        let memory: Arc<dyn Memory> = Arc::new(MockMemory);

        let state = AppState {
            config: Arc::new(RwLock::new(Config::default())),
            model_provider,
            model: "test-model".into(),
            temperature: None,
            mem: memory.clone(),
            memory_strategy: Arc::new(DefaultMemoryStrategy::with_config(
                Arc::clone(&memory),
                zeroclaw_config::schema::MemoryConfig::default(),
                std::path::PathBuf::new(),
            )),
            auto_save: false,
            webhook_secret_hash: None,
            pairing: Arc::new(PairingGuard::new(false, &[])),
            trust_forwarded_headers: false,
            rate_limiter: Arc::new(GatewayRateLimiter::new(100, 100, 100)),
            auth_limiter: Arc::new(auth_rate_limit::AuthRateLimiter::new()),
            idempotency_store: Arc::new(IdempotencyStore::new(Duration::from_secs(300), 1000)),
            #[cfg(feature = "channel-whatsapp-cloud")]
            whatsapp: None,
            #[cfg(feature = "channel-whatsapp-cloud")]
            whatsapp_app_secret: None,
            #[cfg(feature = "channel-linq")]
            linq: HashMap::new(),
            #[cfg(feature = "channel-linq")]
            linq_signing_secrets: HashMap::new(),
            #[cfg(feature = "channel-nextcloud")]
            nextcloud_talk: None,
            #[cfg(feature = "channel-nextcloud")]
            nextcloud_talk_webhook_secret: None,
            #[cfg(feature = "channel-wati")]
            wati: None,
            #[cfg(feature = "channel-email")]
            gmail_push: None,
            observer: Arc::new(zeroclaw_runtime::observability::NoopObserver),
            tools_registry: Arc::new(Vec::new()),
            cost_tracker: None,
            event_tx: tokio::sync::broadcast::channel(16).0,
            event_buffer: Arc::new(sse::EventBuffer::new(16)),
            shutdown_tx: tokio::sync::watch::channel(false).0,
            reload_tx: None,
            node_registry: Arc::new(nodes::NodeRegistry::new(16)),
            path_prefix: String::new(),
            web_dist_dir: None,
            session_backend: None,
            session_queue: std::sync::Arc::new(crate::session_queue::SessionActorQueue::new(
                8, 30, 600,
            )),
            device_registry: None,
            pending_pairings: None,
            canvas_store: CanvasStore::new(),
            cancel_tokens: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            pending_reload: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            tui_registry: None,
            sop_engine: None,
            sop_audit: None,
            #[cfg(feature = "webauthn")]
            webauthn: None,
        };

        // An idempotency key on a rejected request must NOT be consumed.
        let mut headers = HeaderMap::new();
        headers.insert("X-Idempotency-Key", HeaderValue::from_static("ghost-key"));

        let response = handle_webhook(
            State(state.clone()),
            test_connect_info(),
            Query(WebhookQuery {
                agent: Some("ghost".into()),
            }),
            headers,
            Ok(Json(WebhookBody {
                message: "hello".into(),
            })),
        )
        .await
        .into_response();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let payload = response.into_body().collect().await.unwrap().to_bytes();
        let parsed: serde_json::Value = serde_json::from_slice(&payload).unwrap();
        assert!(
            parsed["error"]
                .as_str()
                .unwrap_or_default()
                .contains("Unknown agent `ghost`")
        );
        assert_eq!(provider_impl.calls.load(Ordering::SeqCst), 0);
        // Key still fresh — a corrected retry with the same key proceeds.
        assert!(state.idempotency_store.record_if_new("ghost-key"));
    }

    #[tokio::test]
    async fn webhook_explicit_agent_reports_agent_model() {
        let provider_impl = Arc::new(MockModelProvider::default());
        let model_provider: Arc<dyn ModelProvider> = provider_impl.clone();
        let memory: Arc<dyn Memory> = Arc::new(MockMemory);
        let observer_impl = Arc::new(CapturingObserver::default());
        let observer: Arc<dyn zeroclaw_runtime::observability::Observer> = observer_impl.clone();

        let mut config = Config::default();
        config.providers.models.anthropic.insert(
            "default".into(),
            zeroclaw_config::schema::AnthropicModelProviderConfig {
                base: zeroclaw_config::schema::ModelProviderConfig {
                    model: Some("agent-model".into()),
                    ..Default::default()
                },
            },
        );
        let expected_provider = "anthropic.default".to_string();
        config.agents.insert(
            "nova".to_string(),
            zeroclaw_config::schema::AliasedAgentConfig {
                enabled: true,
                model_provider: expected_provider.clone().into(),
                ..Default::default()
            },
        );

        let state = AppState {
            config: Arc::new(RwLock::new(config)),
            model_provider,
            model: "startup-model".into(),
            temperature: None,
            mem: memory,
            memory_strategy: Arc::new(DefaultMemoryStrategy::with_config(
                Arc::new(MockMemory),
                zeroclaw_config::schema::MemoryConfig::default(),
                std::path::PathBuf::new(),
            )),
            auto_save: false,
            webhook_secret_hash: None,
            pairing: Arc::new(PairingGuard::new(false, &[])),
            trust_forwarded_headers: false,
            rate_limiter: Arc::new(GatewayRateLimiter::new(100, 100, 100)),
            auth_limiter: Arc::new(auth_rate_limit::AuthRateLimiter::new()),
            idempotency_store: Arc::new(IdempotencyStore::new(Duration::from_secs(300), 1000)),
            #[cfg(feature = "channel-whatsapp-cloud")]
            whatsapp: None,
            #[cfg(feature = "channel-whatsapp-cloud")]
            whatsapp_app_secret: None,
            #[cfg(feature = "channel-linq")]
            linq: HashMap::new(),
            #[cfg(feature = "channel-linq")]
            linq_signing_secrets: HashMap::new(),
            #[cfg(feature = "channel-nextcloud")]
            nextcloud_talk: None,
            #[cfg(feature = "channel-nextcloud")]
            nextcloud_talk_webhook_secret: None,
            #[cfg(feature = "channel-wati")]
            wati: None,
            #[cfg(feature = "channel-email")]
            gmail_push: None,
            observer,
            tools_registry: Arc::new(Vec::new()),
            cost_tracker: None,
            event_tx: tokio::sync::broadcast::channel(16).0,
            event_buffer: Arc::new(sse::EventBuffer::new(16)),
            shutdown_tx: tokio::sync::watch::channel(false).0,
            reload_tx: None,
            node_registry: Arc::new(nodes::NodeRegistry::new(16)),
            path_prefix: String::new(),
            web_dist_dir: None,
            session_backend: None,
            session_queue: std::sync::Arc::new(crate::session_queue::SessionActorQueue::new(
                8, 30, 600,
            )),
            device_registry: None,
            pending_pairings: None,
            canvas_store: CanvasStore::new(),
            cancel_tokens: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            pending_reload: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            tui_registry: None,
            sop_engine: None,
            sop_audit: None,
            #[cfg(feature = "webauthn")]
            webauthn: None,
        };

        let response = handle_webhook(
            State(state),
            test_connect_info(),
            Query(WebhookQuery {
                agent: Some("nova".into()),
            }),
            HeaderMap::new(),
            Ok(Json(WebhookBody {
                message: "hello".into(),
            })),
        )
        .await
        .into_response();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(provider_impl.calls.load(Ordering::SeqCst), 1);
        let payload = response.into_body().collect().await.unwrap().to_bytes();
        let parsed: serde_json::Value = serde_json::from_slice(&payload).unwrap();
        assert_eq!(parsed["model"], "agent-model");
        let events = observer_impl.events.lock();
        assert!(
            events.iter().any(|event| matches!(
                event,
                zeroclaw_runtime::observability::ObserverEvent::AgentStart {
                    model_provider,
                    model,
                    ..
                } if model_provider == &expected_provider && model == "agent-model"
            )),
            "expected AgentStart to use the explicit agent model; events were: {events:?}"
        );
    }

    #[tokio::test]
    async fn webhook_autosave_stores_distinct_keys_per_request() {
        let provider_impl = Arc::new(MockModelProvider::default());
        let model_provider: Arc<dyn ModelProvider> = provider_impl.clone();

        let tracking_impl = Arc::new(TrackingMemory::default());
        let memory: Arc<dyn Memory> = tracking_impl.clone();

        let state = AppState {
            config: Arc::new(RwLock::new(Config::default())),
            model_provider,
            model: "test-model".into(),
            temperature: None,
            mem: memory,
            memory_strategy: Arc::new(DefaultMemoryStrategy::with_config(
                Arc::new(MockMemory),
                zeroclaw_config::schema::MemoryConfig::default(),
                std::path::PathBuf::new(),
            )),
            auto_save: true,
            webhook_secret_hash: None,
            pairing: Arc::new(PairingGuard::new(false, &[])),
            trust_forwarded_headers: false,
            rate_limiter: Arc::new(GatewayRateLimiter::new(100, 100, 100)),
            auth_limiter: Arc::new(auth_rate_limit::AuthRateLimiter::new()),
            idempotency_store: Arc::new(IdempotencyStore::new(Duration::from_secs(300), 1000)),
            #[cfg(feature = "channel-whatsapp-cloud")]
            whatsapp: None,
            #[cfg(feature = "channel-whatsapp-cloud")]
            whatsapp_app_secret: None,
            #[cfg(feature = "channel-linq")]
            linq: HashMap::new(),
            #[cfg(feature = "channel-linq")]
            linq_signing_secrets: HashMap::new(),
            #[cfg(feature = "channel-nextcloud")]
            nextcloud_talk: None,
            #[cfg(feature = "channel-nextcloud")]
            nextcloud_talk_webhook_secret: None,
            #[cfg(feature = "channel-wati")]
            wati: None,
            #[cfg(feature = "channel-email")]
            gmail_push: None,
            observer: Arc::new(zeroclaw_runtime::observability::NoopObserver),
            tools_registry: Arc::new(Vec::new()),
            cost_tracker: None,
            event_tx: tokio::sync::broadcast::channel(16).0,
            event_buffer: Arc::new(sse::EventBuffer::new(16)),
            shutdown_tx: tokio::sync::watch::channel(false).0,
            reload_tx: None,
            node_registry: Arc::new(nodes::NodeRegistry::new(16)),
            path_prefix: String::new(),
            web_dist_dir: None,
            session_backend: None,
            session_queue: std::sync::Arc::new(crate::session_queue::SessionActorQueue::new(
                8, 30, 600,
            )),
            device_registry: None,
            pending_pairings: None,
            canvas_store: CanvasStore::new(),
            cancel_tokens: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            pending_reload: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            tui_registry: None,
            sop_engine: None,
            sop_audit: None,
            #[cfg(feature = "webauthn")]
            webauthn: None,
        };

        let headers = HeaderMap::new();

        let body1 = Ok(Json(WebhookBody {
            message: "hello one".into(),
        }));
        let first = handle_webhook(
            State(state.clone()),
            test_connect_info(),
            Query(WebhookQuery::default()),
            headers.clone(),
            body1,
        )
        .await
        .into_response();
        assert_eq!(first.status(), StatusCode::OK);

        let body2 = Ok(Json(WebhookBody {
            message: "hello two".into(),
        }));
        let second = handle_webhook(
            State(state),
            test_connect_info(),
            Query(WebhookQuery::default()),
            headers,
            body2,
        )
        .await
        .into_response();
        assert_eq!(second.status(), StatusCode::OK);

        let keys = tracking_impl.keys.lock().clone();
        assert_eq!(keys.len(), 2);
        assert_ne!(keys[0], keys[1]);
        assert!(keys[0].starts_with("webhook_msg_"));
        assert!(keys[1].starts_with("webhook_msg_"));
        assert_eq!(provider_impl.calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn webhook_secret_hash_is_deterministic_and_nonempty() {
        let secret_a = generate_test_secret();
        let secret_b = generate_test_secret();
        let one = hash_webhook_secret(&secret_a);
        let two = hash_webhook_secret(&secret_a);
        let other = hash_webhook_secret(&secret_b);

        assert_eq!(one, two);
        assert_ne!(one, other);
        assert_eq!(one.len(), 64);
    }

    #[tokio::test]
    async fn webhook_secret_hash_rejects_missing_header() {
        let provider_impl = Arc::new(MockModelProvider::default());
        let model_provider: Arc<dyn ModelProvider> = provider_impl.clone();
        let memory: Arc<dyn Memory> = Arc::new(MockMemory);
        let secret = generate_test_secret();

        let state = AppState {
            config: Arc::new(RwLock::new(Config::default())),
            model_provider,
            model: "test-model".into(),
            temperature: None,
            mem: memory.clone(),
            memory_strategy: Arc::new(DefaultMemoryStrategy::with_config(
                Arc::clone(&memory),
                zeroclaw_config::schema::MemoryConfig::default(),
                std::path::PathBuf::new(),
            )),
            auto_save: false,
            webhook_secret_hash: Some(Arc::from(hash_webhook_secret(&secret))),
            pairing: Arc::new(PairingGuard::new(false, &[])),
            trust_forwarded_headers: false,
            rate_limiter: Arc::new(GatewayRateLimiter::new(100, 100, 100)),
            auth_limiter: Arc::new(auth_rate_limit::AuthRateLimiter::new()),
            idempotency_store: Arc::new(IdempotencyStore::new(Duration::from_secs(300), 1000)),
            #[cfg(feature = "channel-whatsapp-cloud")]
            whatsapp: None,
            #[cfg(feature = "channel-whatsapp-cloud")]
            whatsapp_app_secret: None,
            #[cfg(feature = "channel-linq")]
            linq: HashMap::new(),
            #[cfg(feature = "channel-linq")]
            linq_signing_secrets: HashMap::new(),
            #[cfg(feature = "channel-nextcloud")]
            nextcloud_talk: None,
            #[cfg(feature = "channel-nextcloud")]
            nextcloud_talk_webhook_secret: None,
            #[cfg(feature = "channel-wati")]
            wati: None,
            #[cfg(feature = "channel-email")]
            gmail_push: None,
            observer: Arc::new(zeroclaw_runtime::observability::NoopObserver),
            tools_registry: Arc::new(Vec::new()),
            cost_tracker: None,
            event_tx: tokio::sync::broadcast::channel(16).0,
            event_buffer: Arc::new(sse::EventBuffer::new(16)),
            shutdown_tx: tokio::sync::watch::channel(false).0,
            reload_tx: None,
            node_registry: Arc::new(nodes::NodeRegistry::new(16)),
            path_prefix: String::new(),
            web_dist_dir: None,
            session_backend: None,
            session_queue: std::sync::Arc::new(crate::session_queue::SessionActorQueue::new(
                8, 30, 600,
            )),
            device_registry: None,
            pending_pairings: None,
            canvas_store: CanvasStore::new(),
            cancel_tokens: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            pending_reload: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            tui_registry: None,
            sop_engine: None,
            sop_audit: None,
            #[cfg(feature = "webauthn")]
            webauthn: None,
        };

        let response = handle_webhook(
            State(state),
            test_connect_info(),
            Query(WebhookQuery::default()),
            HeaderMap::new(),
            Ok(Json(WebhookBody {
                message: "hello".into(),
            })),
        )
        .await
        .into_response();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(provider_impl.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn webhook_secret_hash_rejects_invalid_header() {
        let provider_impl = Arc::new(MockModelProvider::default());
        let model_provider: Arc<dyn ModelProvider> = provider_impl.clone();
        let memory: Arc<dyn Memory> = Arc::new(MockMemory);
        let valid_secret = generate_test_secret();
        let wrong_secret = generate_test_secret();

        let state = AppState {
            config: Arc::new(RwLock::new(Config::default())),
            model_provider,
            model: "test-model".into(),
            temperature: None,
            mem: memory.clone(),
            memory_strategy: Arc::new(DefaultMemoryStrategy::with_config(
                Arc::clone(&memory),
                zeroclaw_config::schema::MemoryConfig::default(),
                std::path::PathBuf::new(),
            )),
            auto_save: false,
            webhook_secret_hash: Some(Arc::from(hash_webhook_secret(&valid_secret))),
            pairing: Arc::new(PairingGuard::new(false, &[])),
            trust_forwarded_headers: false,
            rate_limiter: Arc::new(GatewayRateLimiter::new(100, 100, 100)),
            auth_limiter: Arc::new(auth_rate_limit::AuthRateLimiter::new()),
            idempotency_store: Arc::new(IdempotencyStore::new(Duration::from_secs(300), 1000)),
            #[cfg(feature = "channel-whatsapp-cloud")]
            whatsapp: None,
            #[cfg(feature = "channel-whatsapp-cloud")]
            whatsapp_app_secret: None,
            #[cfg(feature = "channel-linq")]
            linq: HashMap::new(),
            #[cfg(feature = "channel-linq")]
            linq_signing_secrets: HashMap::new(),
            #[cfg(feature = "channel-nextcloud")]
            nextcloud_talk: None,
            #[cfg(feature = "channel-nextcloud")]
            nextcloud_talk_webhook_secret: None,
            #[cfg(feature = "channel-wati")]
            wati: None,
            #[cfg(feature = "channel-email")]
            gmail_push: None,
            observer: Arc::new(zeroclaw_runtime::observability::NoopObserver),
            tools_registry: Arc::new(Vec::new()),
            cost_tracker: None,
            event_tx: tokio::sync::broadcast::channel(16).0,
            event_buffer: Arc::new(sse::EventBuffer::new(16)),
            shutdown_tx: tokio::sync::watch::channel(false).0,
            reload_tx: None,
            node_registry: Arc::new(nodes::NodeRegistry::new(16)),
            path_prefix: String::new(),
            web_dist_dir: None,
            session_backend: None,
            session_queue: std::sync::Arc::new(crate::session_queue::SessionActorQueue::new(
                8, 30, 600,
            )),
            device_registry: None,
            pending_pairings: None,
            canvas_store: CanvasStore::new(),
            cancel_tokens: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            pending_reload: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            tui_registry: None,
            sop_engine: None,
            sop_audit: None,
            #[cfg(feature = "webauthn")]
            webauthn: None,
        };

        let mut headers = HeaderMap::new();
        headers.insert(
            "X-Webhook-Secret",
            HeaderValue::from_str(&wrong_secret).unwrap(),
        );

        let response = handle_webhook(
            State(state),
            test_connect_info(),
            Query(WebhookQuery::default()),
            headers,
            Ok(Json(WebhookBody {
                message: "hello".into(),
            })),
        )
        .await
        .into_response();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(provider_impl.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn webhook_secret_hash_accepts_valid_header() {
        let provider_impl = Arc::new(MockModelProvider::default());
        let model_provider: Arc<dyn ModelProvider> = provider_impl.clone();
        let memory: Arc<dyn Memory> = Arc::new(MockMemory);
        let secret = generate_test_secret();

        let state = AppState {
            config: Arc::new(RwLock::new(Config::default())),
            model_provider,
            model: "test-model".into(),
            temperature: None,
            mem: memory.clone(),
            memory_strategy: Arc::new(DefaultMemoryStrategy::with_config(
                Arc::clone(&memory),
                zeroclaw_config::schema::MemoryConfig::default(),
                std::path::PathBuf::new(),
            )),
            auto_save: false,
            webhook_secret_hash: Some(Arc::from(hash_webhook_secret(&secret))),
            pairing: Arc::new(PairingGuard::new(false, &[])),
            trust_forwarded_headers: false,
            rate_limiter: Arc::new(GatewayRateLimiter::new(100, 100, 100)),
            auth_limiter: Arc::new(auth_rate_limit::AuthRateLimiter::new()),
            idempotency_store: Arc::new(IdempotencyStore::new(Duration::from_secs(300), 1000)),
            #[cfg(feature = "channel-whatsapp-cloud")]
            whatsapp: None,
            #[cfg(feature = "channel-whatsapp-cloud")]
            whatsapp_app_secret: None,
            #[cfg(feature = "channel-linq")]
            linq: HashMap::new(),
            #[cfg(feature = "channel-linq")]
            linq_signing_secrets: HashMap::new(),
            #[cfg(feature = "channel-nextcloud")]
            nextcloud_talk: None,
            #[cfg(feature = "channel-nextcloud")]
            nextcloud_talk_webhook_secret: None,
            #[cfg(feature = "channel-wati")]
            wati: None,
            #[cfg(feature = "channel-email")]
            gmail_push: None,
            observer: Arc::new(zeroclaw_runtime::observability::NoopObserver),
            tools_registry: Arc::new(Vec::new()),
            cost_tracker: None,
            event_tx: tokio::sync::broadcast::channel(16).0,
            event_buffer: Arc::new(sse::EventBuffer::new(16)),
            shutdown_tx: tokio::sync::watch::channel(false).0,
            reload_tx: None,
            node_registry: Arc::new(nodes::NodeRegistry::new(16)),
            path_prefix: String::new(),
            web_dist_dir: None,
            session_backend: None,
            session_queue: std::sync::Arc::new(crate::session_queue::SessionActorQueue::new(
                8, 30, 600,
            )),
            device_registry: None,
            pending_pairings: None,
            canvas_store: CanvasStore::new(),
            cancel_tokens: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            pending_reload: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            tui_registry: None,
            sop_engine: None,
            sop_audit: None,
            #[cfg(feature = "webauthn")]
            webauthn: None,
        };

        let mut headers = HeaderMap::new();
        headers.insert("X-Webhook-Secret", HeaderValue::from_str(&secret).unwrap());

        let response = handle_webhook(
            State(state),
            test_connect_info(),
            Query(WebhookQuery::default()),
            headers,
            Ok(Json(WebhookBody {
                message: "hello".into(),
            })),
        )
        .await
        .into_response();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(provider_impl.calls.load(Ordering::SeqCst), 1);
    }

    #[cfg(feature = "channel-nextcloud")]
    fn compute_nextcloud_signature_hex(secret: &str, random: &str, body: &str) -> String {
        use hmac::{Hmac, Mac};
        use sha2::Sha256;

        let payload = format!("{random}{body}");
        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(payload.as_bytes());
        hex::encode(mac.finalize().into_bytes())
    }

    #[cfg(feature = "channel-nextcloud")]
    #[tokio::test]
    async fn nextcloud_talk_webhook_returns_not_found_when_not_configured() {
        let model_provider: Arc<dyn ModelProvider> = Arc::new(MockModelProvider::default());
        let memory: Arc<dyn Memory> = Arc::new(MockMemory);

        let state = AppState {
            config: Arc::new(RwLock::new(Config::default())),
            model_provider,
            model: "test-model".into(),
            temperature: None,
            mem: memory.clone(),
            memory_strategy: Arc::new(DefaultMemoryStrategy::with_config(
                Arc::clone(&memory),
                zeroclaw_config::schema::MemoryConfig::default(),
                std::path::PathBuf::new(),
            )),
            auto_save: false,
            webhook_secret_hash: None,
            pairing: Arc::new(PairingGuard::new(false, &[])),
            trust_forwarded_headers: false,
            rate_limiter: Arc::new(GatewayRateLimiter::new(100, 100, 100)),
            auth_limiter: Arc::new(auth_rate_limit::AuthRateLimiter::new()),
            idempotency_store: Arc::new(IdempotencyStore::new(Duration::from_secs(300), 1000)),
            #[cfg(feature = "channel-whatsapp-cloud")]
            whatsapp: None,
            #[cfg(feature = "channel-whatsapp-cloud")]
            whatsapp_app_secret: None,
            #[cfg(feature = "channel-linq")]
            linq: HashMap::new(),
            #[cfg(feature = "channel-linq")]
            linq_signing_secrets: HashMap::new(),
            #[cfg(feature = "channel-nextcloud")]
            nextcloud_talk: None,
            #[cfg(feature = "channel-nextcloud")]
            nextcloud_talk_webhook_secret: None,
            #[cfg(feature = "channel-wati")]
            wati: None,
            #[cfg(feature = "channel-email")]
            gmail_push: None,
            observer: Arc::new(zeroclaw_runtime::observability::NoopObserver),
            tools_registry: Arc::new(Vec::new()),
            cost_tracker: None,
            event_tx: tokio::sync::broadcast::channel(16).0,
            event_buffer: Arc::new(sse::EventBuffer::new(16)),
            shutdown_tx: tokio::sync::watch::channel(false).0,
            reload_tx: None,
            node_registry: Arc::new(nodes::NodeRegistry::new(16)),
            path_prefix: String::new(),
            web_dist_dir: None,
            session_backend: None,
            session_queue: std::sync::Arc::new(crate::session_queue::SessionActorQueue::new(
                8, 30, 600,
            )),
            device_registry: None,
            pending_pairings: None,
            canvas_store: CanvasStore::new(),
            cancel_tokens: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            pending_reload: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            tui_registry: None,
            sop_engine: None,
            sop_audit: None,
            #[cfg(feature = "webauthn")]
            webauthn: None,
        };

        let response = Box::pin(handle_nextcloud_talk_webhook(
            State(state),
            HeaderMap::new(),
            Bytes::from_static(br#"{"type":"message"}"#),
        ))
        .await
        .into_response();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[cfg(feature = "channel-nextcloud")]
    #[tokio::test]
    async fn nextcloud_talk_webhook_rejects_invalid_signature() {
        let provider_impl = Arc::new(MockModelProvider::default());
        let model_provider: Arc<dyn ModelProvider> = provider_impl.clone();
        let memory: Arc<dyn Memory> = Arc::new(MockMemory);

        let alias = "nextcloud_talk_test_alias";
        let peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync> = Arc::new(Vec::new);
        let channel = Arc::new(NextcloudTalkChannel::new(
            "https://cloud.example.com".into(),
            "app-token".into(),
            String::new(),
            alias,
            peer_resolver,
        ));

        let secret = "nextcloud-test-secret";
        let random = "seed-value";
        let body = r#"{"type":"message","object":{"token":"room-token"},"message":{"actorType":"users","actorId":"user_a","message":"hello"}}"#;
        let _valid_signature = compute_nextcloud_signature_hex(secret, random, body);
        let invalid_signature = "deadbeef";

        let state = AppState {
            config: Arc::new(RwLock::new(Config::default())),
            model_provider,
            model: "test-model".into(),
            temperature: None,
            mem: memory.clone(),
            memory_strategy: Arc::new(DefaultMemoryStrategy::with_config(
                Arc::clone(&memory),
                zeroclaw_config::schema::MemoryConfig::default(),
                std::path::PathBuf::new(),
            )),
            auto_save: false,
            webhook_secret_hash: None,
            pairing: Arc::new(PairingGuard::new(false, &[])),
            trust_forwarded_headers: false,
            rate_limiter: Arc::new(GatewayRateLimiter::new(100, 100, 100)),
            auth_limiter: Arc::new(auth_rate_limit::AuthRateLimiter::new()),
            idempotency_store: Arc::new(IdempotencyStore::new(Duration::from_secs(300), 1000)),
            #[cfg(feature = "channel-whatsapp-cloud")]
            whatsapp: None,
            #[cfg(feature = "channel-whatsapp-cloud")]
            whatsapp_app_secret: None,
            #[cfg(feature = "channel-linq")]
            linq: HashMap::new(),
            #[cfg(feature = "channel-linq")]
            linq_signing_secrets: HashMap::new(),
            nextcloud_talk: Some(channel),
            nextcloud_talk_webhook_secret: Some(Arc::from(secret)),
            #[cfg(feature = "channel-wati")]
            wati: None,
            #[cfg(feature = "channel-email")]
            gmail_push: None,
            observer: Arc::new(zeroclaw_runtime::observability::NoopObserver),
            tools_registry: Arc::new(Vec::new()),
            cost_tracker: None,
            event_tx: tokio::sync::broadcast::channel(16).0,
            event_buffer: Arc::new(sse::EventBuffer::new(16)),
            shutdown_tx: tokio::sync::watch::channel(false).0,
            reload_tx: None,
            node_registry: Arc::new(nodes::NodeRegistry::new(16)),
            path_prefix: String::new(),
            web_dist_dir: None,
            session_backend: None,
            session_queue: std::sync::Arc::new(crate::session_queue::SessionActorQueue::new(
                8, 30, 600,
            )),
            device_registry: None,
            pending_pairings: None,
            canvas_store: CanvasStore::new(),
            cancel_tokens: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            pending_reload: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            tui_registry: None,
            sop_engine: None,
            sop_audit: None,
            #[cfg(feature = "webauthn")]
            webauthn: None,
        };

        let mut headers = HeaderMap::new();
        headers.insert(
            "X-Nextcloud-Talk-Random",
            HeaderValue::from_str(random).unwrap(),
        );
        headers.insert(
            "X-Nextcloud-Talk-Signature",
            HeaderValue::from_str(invalid_signature).unwrap(),
        );

        let response = Box::pin(handle_nextcloud_talk_webhook(
            State(state),
            headers,
            Bytes::from(body),
        ))
        .await
        .into_response();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(provider_impl.calls.load(Ordering::SeqCst), 0);
    }

    // Regression for #6156: handler must return 200 OK before the (potentially
    // slow) LLM call completes, so Nextcloud Talk doesn't cancel the webhook
    // request at its ~5s timeout.
    #[cfg(feature = "channel-nextcloud")]
    #[derive(Default)]
    struct SlowProvider {
        calls: AtomicUsize,
        started_tx: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    }

    #[cfg(feature = "channel-nextcloud")]
    #[async_trait]
    impl ModelProvider for SlowProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if let Some(tx) = self.started_tx.lock().take() {
                let _ = tx.send(());
            }
            tokio::time::sleep(Duration::from_secs(30)).await;
            Ok("slow ok".into())
        }
    }
    #[cfg(feature = "channel-nextcloud")]
    impl ::zeroclaw_api::attribution::Attributable for SlowProvider {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Provider(
                ::zeroclaw_api::attribution::ProviderKind::Model(
                    ::zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }
        fn alias(&self) -> &str {
            "SlowProvider"
        }
    }

    #[cfg(feature = "channel-nextcloud")]
    #[tokio::test]
    async fn nextcloud_talk_webhook_returns_before_llm_call_completes() {
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let provider_impl = Arc::new(SlowProvider {
            calls: AtomicUsize::new(0),
            started_tx: Mutex::new(Some(started_tx)),
        });
        let provider: Arc<dyn ModelProvider> = provider_impl.clone();
        let memory: Arc<dyn Memory> = Arc::new(MockMemory);

        let channel = Arc::new(NextcloudTalkChannel::new(
            "https://cloud.example.com".into(),
            "app-token".into(),
            String::new(),
            "default",
            Arc::new(|| vec!["*".to_string()]),
        ));

        let body = r#"{"type":"message","object":{"token":"room-token"},"actor":{"id":"user_a","name":"User A"},"message":{"actorType":"users","actorId":"user_a","message":"hello"}}"#;

        let state = AppState {
            config: Arc::new(RwLock::new(Config::default())),
            model_provider: provider,
            model: "test-model".into(),
            temperature: None,
            mem: memory.clone(),
            memory_strategy: Arc::new(DefaultMemoryStrategy::with_config(
                Arc::clone(&memory),
                zeroclaw_config::schema::MemoryConfig::default(),
                std::path::PathBuf::new(),
            )),
            auto_save: false,
            webhook_secret_hash: None,
            pairing: Arc::new(PairingGuard::new(false, &[])),
            trust_forwarded_headers: false,
            rate_limiter: Arc::new(GatewayRateLimiter::new(100, 100, 100)),
            auth_limiter: Arc::new(auth_rate_limit::AuthRateLimiter::new()),
            idempotency_store: Arc::new(IdempotencyStore::new(Duration::from_secs(300), 1000)),
            #[cfg(feature = "channel-whatsapp-cloud")]
            whatsapp: None,
            #[cfg(feature = "channel-whatsapp-cloud")]
            whatsapp_app_secret: None,
            #[cfg(feature = "channel-linq")]
            linq: HashMap::new(),
            #[cfg(feature = "channel-linq")]
            linq_signing_secrets: HashMap::new(),
            nextcloud_talk: Some(channel),
            nextcloud_talk_webhook_secret: None,
            pending_reload: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            tui_registry: None,
            #[cfg(feature = "channel-wati")]
            wati: None,
            #[cfg(feature = "channel-email")]
            gmail_push: None,
            observer: Arc::new(zeroclaw_runtime::observability::NoopObserver),
            tools_registry: Arc::new(Vec::new()),
            cost_tracker: None,
            event_tx: tokio::sync::broadcast::channel(16).0,
            event_buffer: Arc::new(sse::EventBuffer::new(16)),
            shutdown_tx: tokio::sync::watch::channel(false).0,
            reload_tx: None,
            node_registry: Arc::new(nodes::NodeRegistry::new(16)),
            path_prefix: String::new(),
            web_dist_dir: None,
            session_backend: None,
            session_queue: std::sync::Arc::new(crate::session_queue::SessionActorQueue::new(
                8, 30, 600,
            )),
            device_registry: None,
            pending_pairings: None,
            canvas_store: CanvasStore::new(),
            cancel_tokens: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            sop_engine: None,
            sop_audit: None,
            #[cfg(feature = "webauthn")]
            webauthn: None,
        };

        let start = std::time::Instant::now();
        let response = tokio::time::timeout(
            Duration::from_secs(2),
            Box::pin(handle_nextcloud_talk_webhook(
                State(state),
                HeaderMap::new(),
                Bytes::from(body),
            )),
        )
        .await
        .expect("webhook must return before 2s deadline (regression #6156)")
        .into_response();

        let elapsed = start.elapsed();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(
            elapsed < Duration::from_secs(2),
            "handler returned after {elapsed:?}; expected fast return for #6156"
        );

        // Confirm the spawned task actually started the LLM call (i.e., the
        // ack didn't just skip processing). The 30s sleep is still in flight.
        tokio::time::timeout(Duration::from_secs(2), started_rx)
            .await
            .expect("spawned LLM call did not start within 2s")
            .expect("started_tx sender was dropped");
        assert_eq!(provider_impl.calls.load(Ordering::SeqCst), 1);
    }

    // ══════════════════════════════════════════════════════════
    // WhatsApp Signature Verification Tests (CWE-345 Prevention)
    // ══════════════════════════════════════════════════════════

    #[cfg(feature = "channel-whatsapp-cloud")]
    fn compute_whatsapp_signature_hex(secret: &str, body: &[u8]) -> String {
        use hmac::{Hmac, Mac};
        use sha2::Sha256;

        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(body);
        hex::encode(mac.finalize().into_bytes())
    }

    #[cfg(feature = "channel-whatsapp-cloud")]
    fn compute_whatsapp_signature_header(secret: &str, body: &[u8]) -> String {
        format!("sha256={}", compute_whatsapp_signature_hex(secret, body))
    }

    #[cfg(feature = "channel-whatsapp-cloud")]
    #[test]
    fn whatsapp_signature_valid() {
        let app_secret = generate_test_secret();
        let body = b"test body content";

        let signature_header = compute_whatsapp_signature_header(&app_secret, body);

        assert!(verify_whatsapp_signature(
            &app_secret,
            body,
            &signature_header
        ));
    }

    #[cfg(feature = "channel-whatsapp-cloud")]
    #[test]
    fn whatsapp_signature_invalid_wrong_secret() {
        let app_secret = generate_test_secret();
        let wrong_secret = generate_test_secret();
        let body = b"test body content";

        let signature_header = compute_whatsapp_signature_header(&wrong_secret, body);

        assert!(!verify_whatsapp_signature(
            &app_secret,
            body,
            &signature_header
        ));
    }

    #[cfg(feature = "channel-whatsapp-cloud")]
    #[test]
    fn whatsapp_signature_invalid_wrong_body() {
        let app_secret = generate_test_secret();
        let original_body = b"original body";
        let tampered_body = b"tampered body";

        let signature_header = compute_whatsapp_signature_header(&app_secret, original_body);

        // Verify with tampered body should fail
        assert!(!verify_whatsapp_signature(
            &app_secret,
            tampered_body,
            &signature_header
        ));
    }

    #[cfg(feature = "channel-whatsapp-cloud")]
    #[test]
    fn whatsapp_signature_missing_prefix() {
        let app_secret = generate_test_secret();
        let body = b"test body";

        // Signature without "sha256=" prefix
        let signature_header = "abc123def456";

        assert!(!verify_whatsapp_signature(
            &app_secret,
            body,
            signature_header
        ));
    }

    #[cfg(feature = "channel-whatsapp-cloud")]
    #[test]
    fn whatsapp_signature_empty_header() {
        let app_secret = generate_test_secret();
        let body = b"test body";

        assert!(!verify_whatsapp_signature(&app_secret, body, ""));
    }

    #[cfg(feature = "channel-whatsapp-cloud")]
    #[test]
    fn whatsapp_signature_invalid_hex() {
        let app_secret = generate_test_secret();
        let body = b"test body";

        // Invalid hex characters
        let signature_header = "sha256=not_valid_hex_zzz";

        assert!(!verify_whatsapp_signature(
            &app_secret,
            body,
            signature_header
        ));
    }

    #[cfg(feature = "channel-whatsapp-cloud")]
    #[test]
    fn whatsapp_signature_empty_body() {
        let app_secret = generate_test_secret();
        let body = b"";

        let signature_header = compute_whatsapp_signature_header(&app_secret, body);

        assert!(verify_whatsapp_signature(
            &app_secret,
            body,
            &signature_header
        ));
    }

    #[cfg(feature = "channel-whatsapp-cloud")]
    #[test]
    fn whatsapp_signature_unicode_body() {
        let app_secret = generate_test_secret();
        let body = "Hello 🦀 World".as_bytes();

        let signature_header = compute_whatsapp_signature_header(&app_secret, body);

        assert!(verify_whatsapp_signature(
            &app_secret,
            body,
            &signature_header
        ));
    }

    #[cfg(feature = "channel-whatsapp-cloud")]
    #[test]
    fn whatsapp_signature_json_payload() {
        let app_secret = generate_test_secret();
        let body = br#"{"entry":[{"changes":[{"value":{"messages":[{"from":"1234567890","text":{"body":"Hello"}}]}}]}]}"#;

        let signature_header = compute_whatsapp_signature_header(&app_secret, body);

        assert!(verify_whatsapp_signature(
            &app_secret,
            body,
            &signature_header
        ));
    }

    #[cfg(feature = "channel-whatsapp-cloud")]
    #[test]
    fn whatsapp_signature_case_sensitive_prefix() {
        let app_secret = generate_test_secret();
        let body = b"test body";

        let hex_sig = compute_whatsapp_signature_hex(&app_secret, body);

        // Wrong case prefix should fail
        let wrong_prefix = format!("SHA256={hex_sig}");
        assert!(!verify_whatsapp_signature(&app_secret, body, &wrong_prefix));

        // Correct prefix should pass
        let correct_prefix = format!("sha256={hex_sig}");
        assert!(verify_whatsapp_signature(
            &app_secret,
            body,
            &correct_prefix
        ));
    }

    #[cfg(feature = "channel-whatsapp-cloud")]
    #[test]
    fn whatsapp_signature_truncated_hex() {
        let app_secret = generate_test_secret();
        let body = b"test body";

        let hex_sig = compute_whatsapp_signature_hex(&app_secret, body);
        let truncated = &hex_sig[..32]; // Only half the signature
        let signature_header = format!("sha256={truncated}");

        assert!(!verify_whatsapp_signature(
            &app_secret,
            body,
            &signature_header
        ));
    }

    #[cfg(feature = "channel-whatsapp-cloud")]
    #[test]
    fn whatsapp_signature_extra_bytes() {
        let app_secret = generate_test_secret();
        let body = b"test body";

        let hex_sig = compute_whatsapp_signature_hex(&app_secret, body);
        let extended = format!("{hex_sig}deadbeef");
        let signature_header = format!("sha256={extended}");

        assert!(!verify_whatsapp_signature(
            &app_secret,
            body,
            &signature_header
        ));
    }

    // ══════════════════════════════════════════════════════════
    // IdempotencyStore Edge-Case Tests
    // ══════════════════════════════════════════════════════════

    #[test]
    fn idempotency_store_allows_different_keys() {
        let store = IdempotencyStore::new(Duration::from_secs(60), 100);
        assert!(store.record_if_new("key-a"));
        assert!(store.record_if_new("key-b"));
        assert!(store.record_if_new("key-c"));
        assert!(store.record_if_new("key-d"));
    }

    #[test]
    fn idempotency_store_max_keys_clamped_to_one() {
        let store = IdempotencyStore::new(Duration::from_secs(60), 0);
        assert!(store.record_if_new("only-key"));
        assert!(!store.record_if_new("only-key"));
    }

    #[test]
    fn idempotency_store_rapid_duplicate_rejected() {
        let store = IdempotencyStore::new(Duration::from_secs(300), 100);
        assert!(store.record_if_new("rapid"));
        assert!(!store.record_if_new("rapid"));
    }

    #[test]
    fn idempotency_store_accepts_after_ttl_expires() {
        let store = IdempotencyStore::new(Duration::from_millis(1), 100);
        assert!(store.record_if_new("ttl-key"));
        std::thread::sleep(Duration::from_millis(10));
        assert!(store.record_if_new("ttl-key"));
    }

    #[test]
    fn idempotency_store_eviction_preserves_newest() {
        let store = IdempotencyStore::new(Duration::from_secs(300), 1);
        assert!(store.record_if_new("old-key"));
        std::thread::sleep(Duration::from_millis(2));
        assert!(store.record_if_new("new-key"));

        let keys = store.keys.lock();
        assert_eq!(keys.len(), 1);
        assert!(!keys.contains_key("old-key"));
        assert!(keys.contains_key("new-key"));
    }

    #[test]
    fn rate_limiter_allows_after_window_expires() {
        let window = Duration::from_millis(50);
        let limiter = SlidingWindowRateLimiter::new(2, window, 100);
        assert!(limiter.allow("ip-1"));
        assert!(limiter.allow("ip-1"));
        assert!(!limiter.allow("ip-1")); // blocked

        // Wait for window to expire
        std::thread::sleep(Duration::from_millis(60));

        // Should be allowed again
        assert!(limiter.allow("ip-1"));
    }

    #[test]
    fn rate_limiter_independent_keys_tracked_separately() {
        let limiter = SlidingWindowRateLimiter::new(2, Duration::from_secs(60), 100);
        assert!(limiter.allow("ip-1"));
        assert!(limiter.allow("ip-1"));
        assert!(!limiter.allow("ip-1")); // ip-1 blocked

        // ip-2 should still work
        assert!(limiter.allow("ip-2"));
        assert!(limiter.allow("ip-2"));
        assert!(!limiter.allow("ip-2")); // ip-2 now blocked
    }

    #[test]
    fn rate_limiter_exact_boundary_at_max_keys() {
        let limiter = SlidingWindowRateLimiter::new(10, Duration::from_secs(60), 3);
        assert!(limiter.allow("ip-1"));
        assert!(limiter.allow("ip-2"));
        assert!(limiter.allow("ip-3"));
        // At capacity now
        assert!(limiter.allow("ip-4")); // should evict ip-1

        let guard = limiter.requests.lock();
        assert_eq!(guard.0.len(), 3);
        assert!(
            !guard.0.contains_key("ip-1"),
            "ip-1 should have been evicted"
        );
        assert!(guard.0.contains_key("ip-2"));
        assert!(guard.0.contains_key("ip-3"));
        assert!(guard.0.contains_key("ip-4"));
    }

    #[test]
    fn gateway_rate_limiter_pair_and_webhook_are_independent() {
        let limiter = GatewayRateLimiter::new(2, 3, 100);

        // Exhaust pair limit
        assert!(limiter.allow_pair("ip-1"));
        assert!(limiter.allow_pair("ip-1"));
        assert!(!limiter.allow_pair("ip-1")); // pair blocked

        // Webhook should still work
        assert!(limiter.allow_webhook("ip-1"));
        assert!(limiter.allow_webhook("ip-1"));
        assert!(limiter.allow_webhook("ip-1"));
        assert!(!limiter.allow_webhook("ip-1")); // webhook now blocked
    }

    #[test]
    fn rate_limiter_single_key_max_allows_one_request() {
        let limiter = SlidingWindowRateLimiter::new(5, Duration::from_secs(60), 1);
        assert!(limiter.allow("ip-1"));
        assert!(limiter.allow("ip-2")); // evicts ip-1

        let guard = limiter.requests.lock();
        assert_eq!(guard.0.len(), 1);
        assert!(guard.0.contains_key("ip-2"));
        assert!(!guard.0.contains_key("ip-1"));
    }

    #[test]
    fn rate_limiter_concurrent_access_safe() {
        use std::sync::Arc;

        let limiter = Arc::new(SlidingWindowRateLimiter::new(
            1000,
            Duration::from_secs(60),
            1000,
        ));
        let mut handles = Vec::new();

        for i in 0..10 {
            let limiter = limiter.clone();
            handles.push(std::thread::spawn(move || {
                for j in 0..100 {
                    limiter.allow(&format!("thread-{i}-req-{j}"));
                }
            }));
        }

        for handle in handles {
            handle.join().unwrap();
        }

        // Should not panic or deadlock
        let guard = limiter.requests.lock();
        assert!(guard.0.len() <= 1000, "should respect max_keys");
    }

    #[test]
    fn idempotency_store_concurrent_access_safe() {
        use std::sync::Arc;

        let store = Arc::new(IdempotencyStore::new(Duration::from_secs(300), 1000));
        let mut handles = Vec::new();

        for i in 0..10 {
            let store = store.clone();
            handles.push(std::thread::spawn(move || {
                for j in 0..100 {
                    store.record_if_new(&format!("thread-{i}-key-{j}"));
                }
            }));
        }

        for handle in handles {
            handle.join().unwrap();
        }

        let keys = store.keys.lock();
        assert!(keys.len() <= 1000, "should respect max_keys");
    }

    #[test]
    fn rate_limiter_rapid_burst_then_cooldown() {
        let limiter = SlidingWindowRateLimiter::new(5, Duration::from_millis(50), 100);

        // Burst: use all 5 requests
        for _ in 0..5 {
            assert!(limiter.allow("burst-ip"));
        }
        assert!(!limiter.allow("burst-ip")); // 6th should fail

        // Cooldown
        std::thread::sleep(Duration::from_millis(60));

        // Should be allowed again
        assert!(limiter.allow("burst-ip"));
    }

    #[test]
    fn require_localhost_accepts_ipv4_loopback() {
        let peer = SocketAddr::from(([127, 0, 0, 1], 12345));
        assert!(require_localhost(&peer).is_ok());
    }

    #[test]
    fn require_localhost_accepts_ipv6_loopback() {
        let peer = SocketAddr::from((std::net::Ipv6Addr::LOCALHOST, 12345));
        assert!(require_localhost(&peer).is_ok());
    }

    #[test]
    fn require_localhost_rejects_non_loopback_ipv4() {
        let peer = SocketAddr::from(([192, 168, 1, 100], 12345));
        let err = require_localhost(&peer).unwrap_err();
        assert_eq!(err.0, StatusCode::FORBIDDEN);
    }

    #[test]
    fn require_localhost_rejects_non_loopback_ipv6() {
        let peer = SocketAddr::from((
            std::net::Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1),
            12345,
        ));
        let err = require_localhost(&peer).unwrap_err();
        assert_eq!(err.0, StatusCode::FORBIDDEN);
    }

    #[test]
    fn admin_reload_gate_loopback_always_allowed() {
        // Loopback is allowed regardless of the opt-in or pairing flags.
        assert_eq!(
            admin_reload_gate(true, false, false),
            AdminReloadGate::Allow
        );
        assert_eq!(admin_reload_gate(true, true, true), AdminReloadGate::Allow);
        assert_eq!(admin_reload_gate(true, false, true), AdminReloadGate::Allow);
        assert_eq!(admin_reload_gate(true, true, false), AdminReloadGate::Allow);
    }

    #[test]
    fn admin_reload_gate_remote_blocked_by_default() {
        // Non-loopback caller with the flag off is rejected outright,
        // regardless of pairing.
        assert_eq!(
            admin_reload_gate(false, false, true),
            AdminReloadGate::Forbidden
        );
        assert_eq!(
            admin_reload_gate(false, false, false),
            AdminReloadGate::Forbidden
        );
    }

    #[test]
    fn admin_reload_gate_remote_opt_in_requires_auth() {
        // Non-loopback caller with the flag on and pairing on must authenticate.
        assert_eq!(
            admin_reload_gate(false, true, true),
            AdminReloadGate::RequireAuth
        );
    }

    #[test]
    fn admin_reload_gate_remote_opt_in_without_pairing_is_rejected() {
        // Opting in with pairing off cannot authenticate the caller, so the
        // request is rejected rather than allowed anonymously.
        assert_eq!(
            admin_reload_gate(false, true, false),
            AdminReloadGate::ForbiddenNoPairing
        );
    }

    #[test]
    fn allow_remote_admin_defaults_off() {
        // Security default: remote admin reload is disabled until opted in.
        assert!(!zeroclaw_config::schema::GatewayConfig::default().allow_remote_admin);
    }

    // ── handle_admin_reload route-level tests ─────────────────────
    // Beyond the pure `admin_reload_gate` policy tests, these exercise the
    // real handler path (ConnectInfo + HeaderMap + PairingGuard + config),
    // proving `allow_remote_admin` cannot expose an unauthenticated remote
    // reload and that a valid paired token is required and sufficient.

    /// Build an `AppState` for `handle_admin_reload`: controls
    /// `gateway.allow_remote_admin`, pairing (and its tokens), and wires a
    /// live reload channel so the allowed path reaches `200` rather than the
    /// `503` standalone-gateway branch.
    fn admin_reload_state(
        tmp: &tempfile::TempDir,
        allow_remote_admin: bool,
        require_pairing: bool,
        tokens: &[String],
    ) -> AppState {
        let mut state = admin_paircode_state(tmp, require_pairing, false);
        state.config.write().gateway.allow_remote_admin = allow_remote_admin;
        state.pairing = Arc::new(PairingGuard::new(require_pairing, tokens));
        state.reload_tx = Some(tokio::sync::watch::channel(false).0);
        state
    }

    fn loopback_peer() -> SocketAddr {
        SocketAddr::from(([127, 0, 0, 1], 40000))
    }

    fn remote_peer() -> SocketAddr {
        // RFC 5737 TEST-NET-3 documentation address — a stable non-loopback
        // peer that is never a real host on anyone's network.
        SocketAddr::from(([203, 0, 113, 50], 40000))
    }

    fn bearer_headers(token: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {token}")).unwrap(),
        );
        headers
    }

    #[tokio::test]
    async fn admin_reload_loopback_no_token_reloads() {
        let tmp = tempfile::tempdir().unwrap();
        let state = admin_reload_state(&tmp, false, true, &[]);
        let resp =
            handle_admin_reload(State(state), ConnectInfo(loopback_peer()), HeaderMap::new())
                .await
                .unwrap()
                .into_response();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn admin_reload_remote_default_off_is_forbidden() {
        let tmp = tempfile::tempdir().unwrap();
        let state = admin_reload_state(&tmp, false, true, &[]);
        let err = handle_admin_reload(State(state), ConnectInfo(remote_peer()), HeaderMap::new())
            .await
            .err()
            .unwrap();
        assert_eq!(err.0, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn admin_reload_remote_opt_in_without_pairing_does_not_reload() {
        // The fixed hole: allow_remote_admin = true + require_pairing = false
        // must NOT permit an anonymous remote reload.
        let tmp = tempfile::tempdir().unwrap();
        let state = admin_reload_state(&tmp, true, false, &[]);
        let err = handle_admin_reload(State(state), ConnectInfo(remote_peer()), HeaderMap::new())
            .await
            .err()
            .unwrap();
        assert_eq!(err.0, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn admin_reload_remote_opt_in_missing_token_is_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let state = admin_reload_state(&tmp, true, true, &["zc_test_token".to_string()]);
        let err = handle_admin_reload(State(state), ConnectInfo(remote_peer()), HeaderMap::new())
            .await
            .err()
            .unwrap();
        assert_eq!(err.0, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn admin_reload_remote_opt_in_invalid_token_is_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let state = admin_reload_state(&tmp, true, true, &["zc_test_token".to_string()]);
        let err = handle_admin_reload(
            State(state),
            ConnectInfo(remote_peer()),
            bearer_headers("not-the-token"),
        )
        .await
        .err()
        .unwrap();
        assert_eq!(err.0, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn admin_reload_remote_opt_in_valid_token_reloads() {
        let tmp = tempfile::tempdir().unwrap();
        let state = admin_reload_state(&tmp, true, true, &["zc_test_token".to_string()]);
        let resp = handle_admin_reload(
            State(state),
            ConnectInfo(remote_peer()),
            bearer_headers("zc_test_token"),
        )
        .await
        .unwrap()
        .into_response();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[test]
    fn needs_quickstart_for_flags_empty_model() {
        let err =
            needs_quickstart_for("").expect("empty model must produce a needs_quickstart error");
        let msg = err.to_string();
        assert!(
            msg.contains("needs_quickstart"),
            "error must carry the needs_quickstart marker for callers to map to 503; got: {msg}"
        );
        assert!(
            msg.contains("/quickstart"),
            "error must point the user at /quickstart; got: {msg}"
        );
    }

    #[test]
    fn needs_quickstart_for_flags_whitespace_only_model() {
        assert!(
            needs_quickstart_for("   ").is_some(),
            "whitespace-only model must be treated as empty"
        );
        assert!(
            needs_quickstart_for("\n\t ").is_some(),
            "tabs and newlines count as empty too"
        );
    }

    #[test]
    fn needs_quickstart_for_passes_real_model() {
        assert!(
            needs_quickstart_for("anthropic/claude-sonnet-4").is_none(),
            "a real model id must not be flagged"
        );
        assert!(
            needs_quickstart_for("  gpt-4  ").is_none(),
            "leading/trailing whitespace around a real model id must not be flagged"
        );
    }

    #[test]
    fn is_needs_quickstart_err_detects_marker_from_helper() {
        let err = needs_quickstart_for("").expect("empty model produces marker");
        assert!(
            is_needs_quickstart_err(&err),
            "the marker emitted by needs_quickstart_for must be detected"
        );
    }

    #[test]
    fn is_needs_quickstart_err_ignores_unrelated_errors() {
        let err = anyhow::Error::msg("upstream timeout: provider returned 504");
        assert!(
            !is_needs_quickstart_err(&err),
            "unrelated errors must not be misclassified as needs_quickstart"
        );
        let err = anyhow::Error::msg("invalid api key");
        assert!(!is_needs_quickstart_err(&err));
    }

    #[test]
    fn is_needs_quickstart_err_detects_via_substring() {
        // Defends the contract that the substring marker is the
        // detection key — not the exact string. Wrappers (e.g.
        // anyhow::Error::context) must not break the check.
        let err =
            anyhow::Error::msg("provider call failed").context("needs_quickstart: empty model");
        assert!(is_needs_quickstart_err(&err));
    }

    #[test]
    fn needs_quickstart_channel_reply_resolves_via_fluent() {
        // The Fluent key channel-needs-quickstart-reply must resolve
        // to real text from the embedded en/cli.ftl, not the missing-
        // key fallback `{channel-needs-quickstart-reply}` that
        // `missing_cli_string` produces. Guarding this in a test
        // keeps the i18n contract from quietly drifting if the key
        // gets renamed in lib.rs without a matching ftl edit.
        let reply = needs_quickstart_channel_reply();
        assert!(
            !reply.starts_with('{') && !reply.ends_with('}'),
            "fluent missing-key fallback leaked into channel reply: {reply:?}"
        );
        assert!(
            reply.to_lowercase().contains("quickstart"),
            "channel reply must mention Quickstart so users know what's missing: {reply:?}"
        );
    }

    // ══════════════════════════════════════════════════════════
    // Linq Multi-Tenant Webhook Routing Tests
    // ══════════════════════════════════════════════════════════

    /// Helper: compute a valid Linq HMAC-SHA256 signature for the given
    /// secret, timestamp, and body.  Mirrors the verification logic in
    /// `zeroclaw_channels::linq::verify_linq_signature`.
    #[cfg(feature = "channel-linq")]
    fn compute_linq_signature_hex(secret: &str, timestamp: &str, body: &str) -> String {
        use hmac::{Hmac, Mac};
        use sha2::Sha256;

        let message = format!("{timestamp}.{body}");
        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(message.as_bytes());
        hex::encode(mac.finalize().into_bytes())
    }

    /// Helper: build a minimal Linq webhook payload that `parse_webhook_payload`
    /// recognises as a `message.received` event with one text part.
    #[cfg(feature = "channel-linq")]
    fn linq_webhook_body(sender: &str, text: &str) -> String {
        serde_json::json!({
            "event_type": "message.received",
            "data": {
                "sender": { "phone": sender },
                "message": {
                    "parts": [{ "type": "text", "value": text }]
                }
            }
        })
        .to_string()
    }

    /// Helper: build an `AppState` with one Linq channel registered under the
    /// given alias, with an allow-any peer resolver and an optional signing
    /// secret.
    #[cfg(feature = "channel-linq")]
    fn linq_test_state(alias: &str, signing_secret: Option<&str>) -> AppState {
        let model_provider: Arc<dyn ModelProvider> = Arc::new(MockModelProvider::default());
        let memory: Arc<dyn Memory> = Arc::new(MockMemory);

        let peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync> =
            Arc::new(|| vec!["*".to_string()]);
        let channel = Arc::new(LinqChannel::new(
            "test-token".into(),
            "+15550000000".into(),
            alias,
            peer_resolver,
        ));
        let mut linq = HashMap::new();
        linq.insert(alias.to_string(), channel);

        let mut linq_signing_secrets: HashMap<String, Arc<str>> = HashMap::new();
        if let Some(secret) = signing_secret {
            linq_signing_secrets.insert(alias.to_string(), Arc::from(secret));
        }

        AppState {
            config: Arc::new(RwLock::new(Config::default())),
            model_provider,
            model: "test-model".into(),
            temperature: None,
            mem: memory,
            memory_strategy: Arc::new(DefaultMemoryStrategy::with_config(
                Arc::new(MockMemory),
                zeroclaw_config::schema::MemoryConfig::default(),
                std::path::PathBuf::new(),
            )),
            auto_save: false,
            webhook_secret_hash: None,
            pairing: Arc::new(PairingGuard::new(false, &[])),
            trust_forwarded_headers: false,
            rate_limiter: Arc::new(GatewayRateLimiter::new(100, 100, 100)),
            auth_limiter: Arc::new(auth_rate_limit::AuthRateLimiter::new()),
            idempotency_store: Arc::new(IdempotencyStore::new(Duration::from_secs(300), 1000)),
            #[cfg(feature = "channel-whatsapp-cloud")]
            whatsapp: None,
            #[cfg(feature = "channel-whatsapp-cloud")]
            whatsapp_app_secret: None,
            #[cfg(feature = "channel-linq")]
            linq,
            #[cfg(feature = "channel-linq")]
            linq_signing_secrets,
            #[cfg(feature = "channel-nextcloud")]
            nextcloud_talk: None,
            #[cfg(feature = "channel-nextcloud")]
            nextcloud_talk_webhook_secret: None,
            #[cfg(feature = "channel-wati")]
            wati: None,
            #[cfg(feature = "channel-email")]
            gmail_push: None,
            observer: Arc::new(zeroclaw_runtime::observability::NoopObserver),
            tools_registry: Arc::new(Vec::new()),
            cost_tracker: None,
            event_tx: tokio::sync::broadcast::channel(16).0,
            event_buffer: Arc::new(sse::EventBuffer::new(16)),
            shutdown_tx: tokio::sync::watch::channel(false).0,
            reload_tx: None,
            node_registry: Arc::new(nodes::NodeRegistry::new(16)),
            path_prefix: String::new(),
            web_dist_dir: None,
            session_backend: None,
            session_queue: std::sync::Arc::new(crate::session_queue::SessionActorQueue::new(
                8, 30, 600,
            )),
            device_registry: None,
            pending_pairings: None,
            canvas_store: CanvasStore::new(),
            cancel_tokens: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            pending_reload: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            tui_registry: None,
            sop_engine: None,
            sop_audit: None,
            #[cfg(feature = "webauthn")]
            webauthn: None,
        }
    }

    #[cfg(feature = "channel-linq")]
    #[tokio::test]
    async fn linq_webhook_returns_not_found_for_unknown_alias() {
        // No Linq channels configured at all.
        let state = linq_test_state("production", None);

        let response = Box::pin(handle_linq_webhook(
            State(state),
            Path("staging".to_string()),
            HeaderMap::new(),
            Bytes::from_static(br#"{"event_type":"message.received"}"#),
        ))
        .await
        .into_response();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[cfg(feature = "channel-linq")]
    #[tokio::test]
    async fn linq_webhook_returns_not_found_when_no_channels_configured() {
        let model_provider: Arc<dyn ModelProvider> = Arc::new(MockModelProvider::default());
        let memory: Arc<dyn Memory> = Arc::new(MockMemory);

        let state = AppState {
            config: Arc::new(RwLock::new(Config::default())),
            model_provider,
            model: "test-model".into(),
            temperature: None,
            mem: memory,
            memory_strategy: Arc::new(DefaultMemoryStrategy::with_config(
                Arc::new(MockMemory),
                zeroclaw_config::schema::MemoryConfig::default(),
                std::path::PathBuf::new(),
            )),
            auto_save: false,
            webhook_secret_hash: None,
            pairing: Arc::new(PairingGuard::new(false, &[])),
            trust_forwarded_headers: false,
            rate_limiter: Arc::new(GatewayRateLimiter::new(100, 100, 100)),
            auth_limiter: Arc::new(auth_rate_limit::AuthRateLimiter::new()),
            idempotency_store: Arc::new(IdempotencyStore::new(Duration::from_secs(300), 1000)),
            #[cfg(feature = "channel-whatsapp-cloud")]
            whatsapp: None,
            #[cfg(feature = "channel-whatsapp-cloud")]
            whatsapp_app_secret: None,
            #[cfg(feature = "channel-linq")]
            linq: HashMap::new(),
            #[cfg(feature = "channel-linq")]
            linq_signing_secrets: HashMap::new(),
            #[cfg(feature = "channel-nextcloud")]
            nextcloud_talk: None,
            #[cfg(feature = "channel-nextcloud")]
            nextcloud_talk_webhook_secret: None,
            #[cfg(feature = "channel-wati")]
            wati: None,
            #[cfg(feature = "channel-email")]
            gmail_push: None,
            observer: Arc::new(zeroclaw_runtime::observability::NoopObserver),
            tools_registry: Arc::new(Vec::new()),
            cost_tracker: None,
            event_tx: tokio::sync::broadcast::channel(16).0,
            event_buffer: Arc::new(sse::EventBuffer::new(16)),
            shutdown_tx: tokio::sync::watch::channel(false).0,
            reload_tx: None,
            node_registry: Arc::new(nodes::NodeRegistry::new(16)),
            path_prefix: String::new(),
            web_dist_dir: None,
            session_backend: None,
            session_queue: std::sync::Arc::new(crate::session_queue::SessionActorQueue::new(
                8, 30, 600,
            )),
            device_registry: None,
            pending_pairings: None,
            canvas_store: CanvasStore::new(),
            cancel_tokens: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            pending_reload: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            tui_registry: None,
            sop_engine: None,
            sop_audit: None,
            #[cfg(feature = "webauthn")]
            webauthn: None,
        };

        let response = Box::pin(handle_linq_webhook(
            State(state),
            Path("default".to_string()),
            HeaderMap::new(),
            Bytes::from_static(br#"{"event_type":"message.received"}"#),
        ))
        .await
        .into_response();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[cfg(feature = "channel-linq")]
    #[tokio::test]
    async fn linq_webhook_accepts_valid_message_for_known_alias() {
        let state = linq_test_state("default", None);
        let body = linq_webhook_body("+15551234567", "hello from test");

        let response = Box::pin(handle_linq_webhook(
            State(state),
            Path("default".to_string()),
            HeaderMap::new(),
            Bytes::from(body),
        ))
        .await
        .into_response();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[cfg(feature = "channel-linq")]
    #[tokio::test]
    async fn linq_webhook_rejects_invalid_signature_for_alias() {
        let secret = generate_test_secret();
        let state = linq_test_state("secure-alias", Some(&secret));

        let body = linq_webhook_body("+15551234567", "hello from test");
        let mut headers = HeaderMap::new();
        headers.insert(
            "X-Webhook-Signature",
            HeaderValue::from_static("sha256=deadbeef"),
        );
        headers.insert(
            "X-Webhook-Timestamp",
            HeaderValue::from_static("9999999999"),
        );

        let response = Box::pin(handle_linq_webhook(
            State(state),
            Path("secure-alias".to_string()),
            headers,
            Bytes::from(body),
        ))
        .await
        .into_response();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[cfg(feature = "channel-linq")]
    #[tokio::test]
    async fn linq_webhook_accepts_valid_signature_for_alias() {
        let secret = generate_test_secret();
        let state = linq_test_state("secure-alias", Some(&secret));

        let body = linq_webhook_body("+15551234567", "hello from test");
        let timestamp = chrono::Utc::now().timestamp().to_string();
        let sig = compute_linq_signature_hex(&secret, &timestamp, &body);

        let mut headers = HeaderMap::new();
        headers.insert(
            "X-Webhook-Signature",
            HeaderValue::from_str(&format!("sha256={sig}")).unwrap(),
        );
        headers.insert(
            "X-Webhook-Timestamp",
            HeaderValue::from_str(&timestamp).unwrap(),
        );

        let response = Box::pin(handle_linq_webhook(
            State(state),
            Path("secure-alias".to_string()),
            headers,
            Bytes::from(body),
        ))
        .await
        .into_response();

        assert_eq!(response.status(), StatusCode::OK);
    }
}

#[cfg(test)]
mod accept_error_tests {
    use super::is_recoverable_accept_error;
    use std::io::{Error, ErrorKind};

    #[cfg(unix)]
    #[test]
    fn fd_exhaustion_accept_errors_are_recoverable() {
        // #7042: EMFILE/ENFILE must not terminate the daemon.
        assert!(is_recoverable_accept_error(&Error::from_raw_os_error(24))); // EMFILE
        assert!(is_recoverable_accept_error(&Error::from_raw_os_error(23))); // ENFILE
    }

    #[test]
    fn transient_kinds_recover_but_fatal_propagates() {
        assert!(is_recoverable_accept_error(&Error::from(
            ErrorKind::ConnectionAborted
        )));
        assert!(is_recoverable_accept_error(&Error::from(
            ErrorKind::Interrupted
        )));
        // A non-transient error is not swallowed (loop will propagate it).
        assert!(!is_recoverable_accept_error(&Error::from(
            ErrorKind::InvalidInput
        )));
    }
}
