#![allow(
    clippy::to_string_in_format_args,
    clippy::useless_format,
    clippy::collapsible_if
)]

pub mod agent_owned_state;
pub mod api;
pub mod api_attachments;
pub mod api_attention;
pub mod api_backup_retention;
pub mod api_config;
mod api_delegation;
pub mod api_logs;
#[cfg(feature = "nodes")]
pub mod api_node_identity;
pub mod api_pairing;
pub mod api_personality;
pub mod api_review;
pub mod api_sections;
pub mod api_skills;
pub mod api_soul;
pub mod api_user_model;
mod attention;
pub mod auth_rate_limit;
mod delegation_notifications;
#[cfg(feature = "nodes")]
pub mod device_identity;
#[cfg(feature = "nodes")]
#[cfg(feature = "nodes")]
pub mod nodes;
pub mod operator_auth;
mod reminder_notifications;
mod review_notifications;
pub mod security_headers;
pub mod session_queue;
pub mod sse;
pub mod static_files;
pub mod tls;
#[cfg(feature = "gateway-voice-duplex")]
pub mod voice_duplex;
pub mod ws;
pub mod ws_approval;
pub mod ws_bridge;
pub mod ws_conversation;
mod ws_question;

use anyhow::{Context, Result};
use axum::{
    Router,
    extract::{ConnectInfo, Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Json},
    routing::{delete, get, post, put},
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
use zeroclaw_api::memory_traits::MemoryStrategy;
use zeroclaw_api::tool::ToolSpec;
use zeroclaw_config::schema::Config;
use zeroclaw_infra::session_backend::SessionBackend;
use zeroclaw_memory::{self, Memory, MemoryCategory};
use zeroclaw_policy::SecurityPolicy;
use zeroclaw_providers::{self, ModelProvider};
use zeroclaw_runtime::agent::memory_strategy::DefaultMemoryStrategy;
use zeroclaw_runtime::cost::CostTracker;
use zeroclaw_runtime::i18n;
use zeroclaw_runtime::platform;
use zeroclaw_runtime::security::pairing::{
    GATEWAY_ADMIN_TOKEN_HEADER, PairingGuard, constant_time_eq, gateway_admin_token_path,
    is_public_bind,
};

use zeroclaw_runtime::tools;
use zeroclaw_runtime::tools::scoped;

/// Maximum request body size (64KB) — prevents memory exhaustion
pub const MAX_BODY_SIZE: usize = 65_536;
/// Default request timeout (30s) — prevents slow-loris attacks.
pub const REQUEST_TIMEOUT_SECS: u64 = 30;

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

    pub(crate) fn allow_pair(&self, key: &str) -> bool {
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
    // The immediate trusted proxy appends the connecting peer on the right.
    // Only that terminal hop is vouched for. Never walk left across a loopback
    // or malformed suffix into caller-controlled entries. Decode the suffix
    // alone so opaque prefix bytes cannot hide a valid proxy-added address.
    if let Some(xff) = headers.get_all("X-Forwarded-For").iter().next_back() {
        let candidate = xff.as_bytes().rsplit(|byte| *byte == b',').next()?;
        return std::str::from_utf8(candidate)
            .ok()
            .and_then(parse_client_ip);
    }

    headers
        .get_all("X-Real-IP")
        .iter()
        .next_back()
        .and_then(|v| v.to_str().ok())
        .and_then(parse_client_ip)
}

pub(crate) fn client_key_from_request(
    peer_addr: Option<SocketAddr>,
    headers: &HeaderMap,
    trust_forwarded_headers: bool,
) -> String {
    if trust_forwarded_headers {
        if let Some(ip) = forwarded_client_ip(headers)
            && !ip.is_loopback()
        {
            return ip.to_string();
        }
        // A forwarded path must not acquire the local authentication-limiter
        // exemption from a loopback or malformed terminal hop. An actual local
        // request with no forwarding headers retains its existing behavior.
        if headers.contains_key("X-Forwarded-For") || headers.contains_key("X-Real-IP") {
            return peer_addr
                .filter(|addr| !addr.ip().is_loopback())
                .map(|addr| addr.ip().to_string())
                .unwrap_or_else(|| "unknown".to_string());
        }
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

/// Owned guard for [`AppState::config_write_lock`]. Owned (not borrowed) so
/// a handler can release it explicitly at its commit point, or pass it by
/// value into a delegated helper without lifetime coupling.
fn default_agent_alias(config: &Config) -> Option<String> {
    config
        .agents
        .iter()
        .filter(|(_, a)| a.enabled)
        .map(|(alias, _)| alias.clone())
        .min()
}

pub(crate) type ConfigWriteGuard = tokio::sync::OwnedMutexGuard<()>;

/// Retire upgraded sockets even when shutdown preceded watcher registration.
/// They are detached from Axum's listener and otherwise keep old state alive.
pub(crate) async fn gateway_shutdown(mut signal: tokio::sync::watch::Receiver<bool>) {
    loop {
        if *signal.borrow_and_update() {
            return;
        }
        if signal.changed().await.is_err() {
            return;
        }
    }
}

/// Shared state for all axum handlers
#[derive(Clone)]
pub struct AppState {
    pub config: Arc<RwLock<Config>>,

    /// Serializes the read-mutate-save-swap critical section of every HTTP
    /// handler that mutates `config` (per-property PUT/DELETE/PATCH, map-key
    /// create/delete/rename, channel bind, config migrate, section select,
    /// quickstart apply, cron settings patch, pairing-token persistence). A
    /// tokio mutex, not `parking_lot`, because the guard must survive the
    /// `.await` on config-save I/O. Mirrors
    /// `RpcContext::config_write_lock` in the RPC path.
    ///
    /// Invariant: every mutation of `config` must happen while holding this
    /// mutex, acquired before the first `config` read-for-modify and held
    /// through the swap that installs the mutated snapshot. Never acquire it
    /// while holding a `config` guard — lock order is this mutex first,
    /// `config` second, always. A writer that bypasses this lock and swaps
    /// the live config while a concurrent writer's save is in flight loses
    /// that writer's change — clobbered in memory and, if its save hadn't
    /// landed yet, on disk too.
    pub config_write_lock: Arc<tokio::sync::Mutex<()>>,
    pub model_provider: Arc<dyn ModelProvider>,
    pub model: String,
    /// `None` means "let the provider decide" — required for models
    /// (e.g. claude-opus-4-7) that reject the field. Always preserve
    /// `Option<f64>` end-to-end; never substitute a hardcoded default.
    pub temperature: Option<f64>,
    pub mem: Arc<dyn Memory>,
    pub memory_strategy: Arc<dyn MemoryStrategy>,
    /// Companion PortableKernel store. `None` when `tachi` is off or
    /// `[companion_memory].enable` is false.
    pub companion_store: Option<Arc<zeroclaw_memory::CompanionStore>>,
    pub auto_save: bool,
    /// SHA-256 hash of `X-Webhook-Secret` (hex-encoded), never plaintext.
    pub webhook_secret_hash: Option<Arc<str>>,
    pub pairing: Arc<PairingGuard>,
    pub trust_forwarded_headers: bool,
    pub rate_limiter: Arc<GatewayRateLimiter>,
    pub auth_limiter: Arc<auth_rate_limit::AuthRateLimiter>,
    pub idempotency_store: Arc<IdempotencyStore>,
    /// Observability backend for metrics scraping
    pub observer: Arc<dyn zeroclaw_runtime::observability::Observer>,
    /// Registered tool specs (for web dashboard tools page). This is the
    /// default (no `?agent=`) listing, seeded from the deterministically
    /// smallest enabled agent alias.
    pub tools_registry: Arc<Vec<ToolSpec>>,
    /// Per-agent tool-spec listings keyed by agent alias, powering the
    /// agent-aware `GET /api/tools?agent=<alias>` view so the WebUI Tools
    /// page can show each agent's scoped tool set. Falls back to
    /// `tools_registry` for an unknown or omitted alias.
    pub tools_registry_by_agent: Arc<HashMap<String, Arc<Vec<ToolSpec>>>>,
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
    #[cfg(feature = "nodes")]
    pub node_registry: Arc<nodes::NodeRegistry>,
    /// LAN-local peer hints discovered by multicast. These are informational
    /// only; they never authorize or connect a peer.
    #[cfg(feature = "nodes")]
    pub mdns_peer_registry: nodes::mdns::MdnsPeerRegistry,
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
    /// Per-session cancellation tokens for aborting in-flight agent responses.
    /// Key is session_key (e.g. `gw_<session_id>`), value is the token for the
    /// current turn. Entries are inserted before each turn and removed after
    /// completion (normal or cancelled).
    pub cancel_tokens: Arc<
        std::sync::Mutex<std::collections::HashMap<String, tokio_util::sync::CancellationToken>>,
    >,
    pub pending_reload: Arc<std::sync::atomic::AtomicBool>,
    /// Live `/ws/chat` conversations, shared by every socket on a session.
    pub ws_conversations: Arc<ws_conversation::ConversationHub<ws::WsSession>>,
    /// The live `/ws/bridge` control socket of each bridge.
    pub bridge_sockets: Arc<ws_bridge::BridgeSockets>,
}

/// The gateway's per-session turn queue, with a background reaper that
/// releases idle slots. Slots are keyed by client-chosen session ids, so
/// without eviction every distinct session would stay in memory for the
/// life of the process. The reaper stops once the queue is dropped.
fn gateway_session_queue() -> Arc<session_queue::SessionActorQueue> {
    const REAP_EVERY: std::time::Duration = std::time::Duration::from_secs(60);
    let queue = Arc::new(session_queue::SessionActorQueue::new(8, 30, 600));
    let weak = Arc::downgrade(&queue);
    zeroclaw_spawn::spawn!(async move {
        let mut interval = tokio::time::interval(REAP_EVERY);
        interval.tick().await;
        loop {
            interval.tick().await;
            let Some(queue) = weak.upgrade() else { break };
            queue.evict_idle().await;
        }
    });
    queue
}

/// Run the HTTP gateway using axum with proper HTTP/1.1 compliance.
/// How the gateway treats the address it is about to bind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PublicBindPosture {
    /// Loopback, or network-facing with pairing and an explicit opt-in or tunnel.
    Local,
    /// Network-facing with pairing on but no tunnel or opt-in: warn.
    ExposedWithoutOptIn,
    /// Network-facing, pairing off, explicit dangerous exemption set: allowed, warned loudly.
    UnauthenticatedOptedIn,
    /// Network-facing, pairing off, no opt-in: refuse to start. Without pairing
    /// every client is anonymous, so any of them could attach to any shared
    /// session by naming its id.
    RefuseUnauthenticated,
}

fn public_bind_posture(
    host: &str,
    require_pairing: bool,
    allow_public_bind: bool,
    no_tunnel: bool,
    dangerously_allow_unauthenticated_public_bind: bool,
) -> PublicBindPosture {
    if !is_public_bind(host)
        || zeroclaw_infra::parse_gateway_bind_socket_addr(host, 0)
            .is_ok_and(|addr| addr.ip().is_loopback())
    {
        return PublicBindPosture::Local;
    }
    if !require_pairing {
        return if dangerously_allow_unauthenticated_public_bind {
            PublicBindPosture::UnauthenticatedOptedIn
        } else {
            PublicBindPosture::RefuseUnauthenticated
        };
    }
    if !allow_public_bind && no_tunnel {
        PublicBindPosture::ExposedWithoutOptIn
    } else {
        PublicBindPosture::Local
    }
}

#[allow(clippy::too_many_lines, clippy::too_many_arguments)]
pub async fn run_gateway(
    host: &str,
    port: u16,
    config: Config,
    external_event_tx: Option<tokio::sync::broadcast::Sender<serde_json::Value>>,
    // Reload controls owned by the daemon for supervised runs. RPC reloads
    // write to `shutdown_tx` before signalling daemon reload so the listener
    // releases its socket before the replacement gateway binds. /admin/reload
    // writes to both controls directly. Standalone gateway passes `None`.
    reload_controls: Option<zeroclaw_runtime::daemon::GatewayReloadControls>,
    // Companion PortableKernel handle from the composition root. Daemon
    // constructs once and injects the same Arc into channels. Standalone
    // gateway constructs at `run_gateway_if_enabled`. Never opened here.
    companion_store: Option<Arc<zeroclaw_memory::CompanionStore>>,
) -> Result<()> {
    // ── Security: public bind posture ──
    match public_bind_posture(
        host,
        config.gateway.require_pairing,
        config.gateway.allow_public_bind,
        config.tunnel.tunnel_provider == "none",
        config.gateway.dangerously_allow_unauthenticated_public_bind,
    ) {
        PublicBindPosture::Local => {}
        PublicBindPosture::ExposedWithoutOptIn => {
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
        PublicBindPosture::UnauthenticatedOptedIn => {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({"host": host})),
                "⚠️  Binding to {host} with [gateway] require_pairing = false: anyone who can \
                 reach this address can drive the agent, and can attach to any session by its \
                 id (see its turns, cancel them, answer its approvals). Enable require_pairing \
                 unless the network itself is trusted."
            );
        }
        PublicBindPosture::RefuseUnauthenticated => {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"host": host})),
                "gateway refused to bind a non-loopback address with pairing disabled"
            );
            anyhow::bail!(
                "Refusing to bind the gateway to {host} with [gateway] require_pairing = false: \
                 every client on the network could drive the agent and attach to its sessions. \
                 Enable require_pairing, bind 127.0.0.1, or explicitly set [gateway] dangerously_allow_unauthenticated_public_bind = true \
                 to accept an unauthenticated network-facing gateway."
            );
        }
    }
    // Acquire before constructing tools, stores or listeners. The hub travels
    // with AppState into detached turns and keeps ownership until they stop.
    let ws_conversations = Arc::new(ws_conversation::ConversationHub::owned(&config.data_dir)?);
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

    let addr: SocketAddr = match zeroclaw_infra::parse_gateway_bind_socket_addr(host, port) {
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
            zeroclaw_infra::fallback_gateway_bind_socket_addr(port)
        }
    };
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let actual_addr = listener.local_addr()?;
    let actual_port = actual_addr.port();
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
                     needs_quickstart mode so /config and /admin/reload stay \
                     reachable. Fix the [providers.models.<type>.<alias>] entry \
                     and POST /admin/reload."
                );
                (
                    Arc::new(UnconfiguredModelProvider) as Arc<dyn ModelProvider>,
                    true,
                )
            }
        };
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
                            "Gateway booting without a configured model. Run `zeroclaw quickstart` or configure a model at http://{display_addr}/config, then POST /admin/reload. Chat endpoints will return 503 needs_quickstart until at least one [providers.models.<type>.<alias>] model = \"...\" is set."
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
    let mem: Arc<dyn Memory> = if config.agents.is_empty() {
        Arc::new(zeroclaw_memory::NoneMemory::new("none"))
    } else {
        match zeroclaw_memory::create_memory_from_config(
            &config,
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
    if let Some(store) = companion_store.as_ref() {
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(
                ::serde_json::json!({
                    "path": store.path().display().to_string(),
                })
            ),
            "gateway holding companion store"
        );
    }
    let agent_alias_opt = default_agent_alias(&config);

    let agent_setup: Option<(
        zeroclaw_config::schema::RiskProfileConfig,
        Arc<SecurityPolicy>,
    )> = agent_alias_opt.as_ref().and_then(|agent_alias| {
        let Some(risk_profile) = config.risk_profile_for_agent(agent_alias) else {
            ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"agent": agent_alias, "agent_alias": agent_alias})), "Gateway: agents..risk_profile does not name a configured risk_profiles entry; booting with empty tools registry. Fix config.toml and POST /admin/reload.");
            return None;
        };
        let risk_profile = risk_profile.clone();
        let security = match SecurityPolicy::for_agent(&config, agent_alias) {
            Ok(s) => Arc::new(s),
            Err(e) => {
                ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"agent": agent_alias, "error": format!("{}", e), "agent_alias": agent_alias})), "Gateway: agent SecurityPolicy failed to build; booting with empty tools registry. Fix [agents.] in config.toml and POST /admin/reload.");
                return None;
            }
        };
        Some((risk_profile, security))
    });

    let tools_registry_raw = match (&agent_alias_opt, agent_setup) {
        (Some(agent_alias), Some((risk_profile, security))) => {
            let all_tools_result = tools::all_tools_with_runtime(
                Arc::new(config.clone()),
                &security,
                &risk_profile,
                agent_alias,
                Arc::clone(&runtime),
                Arc::clone(&mem),
                &config.browser,
                &config.http_request,
                &config.web_fetch,
                &config.data_dir,
                &config.agents,
                config
                    .model_provider_for_agent(agent_alias)
                    .and_then(|e| e.api_key.as_deref()),
                &config,
                None,
                None,
                // Gateway request handling is a top-level origin.
                None,
                None,
            );
            let assembled = scoped::ScopedToolRegistry::assemble(scoped::ScopedAssembly {
                config: &config,
                agent_alias,
                security: &security,
                built: all_tools_result,
                // The gateway registers no skills today; unifying the two
                // skill loaders through this seam is the Epic F follow-up.
                skills: &[],
                runtime: Arc::clone(&runtime),
                caller_allowed: None,
                connect_mcp: true,
                // Gateway tool-listing path: short-lived, no cross-turn reuse
                // contract, so the per-call connect is correct.
                mcp_registry: None,
                emit_assembly_logs: false,
                exclude_memory: false,
                list_deferred_mcp_specs: true,
            })
            .await;
            // Listing-only registry: no turn runs against it, so the
            // deferred-MCP prompt section and activation handle returned by
            // `assemble` have no consumer here (live gateway chat resolves
            // its tools inside process_message).
            assembled.registry.into_inner()
        }
        (Some(_), None) => {
            // Agent existed but its config failed to resolve. Warned
            // above; fall through to the empty-registry shape.
            Vec::new()
        }
        (None, _) => {
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(::serde_json::json!({"display_addr": display_addr})),
                &format!(
                    "Gateway: no [agents.<alias>] configured — booting with empty tools registry. Run `zeroclaw quickstart` or add one at http://{display_addr}/config."
                )
            );
            Vec::new()
        }
    };

    let tools_registry: Arc<Vec<ToolSpec>> =
        Arc::new(tools_registry_raw.iter().map(|t| t.spec()).collect());

    let mut tools_registry_by_agent: HashMap<String, Arc<Vec<ToolSpec>>> = HashMap::new();
    if let Some(default_alias) = agent_alias_opt.as_ref() {
        tools_registry_by_agent.insert(default_alias.clone(), Arc::clone(&tools_registry));
    }
    let mut other_aliases: Vec<String> = config
        .agents
        .iter()
        .filter(|(alias, a)| a.enabled && Some(*alias) != agent_alias_opt.as_ref())
        .map(|(alias, _)| alias.clone())
        .collect();
    other_aliases.sort();
    for alias in other_aliases {
        let Some(risk_profile) = config.risk_profile_for_agent(&alias) else {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({"agent_alias": alias})),
                "Gateway: agent risk_profile does not resolve; skipping its /api/tools listing."
            );
            continue;
        };
        let risk_profile = risk_profile.clone();
        let security = match SecurityPolicy::for_agent(&config, &alias) {
            Ok(s) => Arc::new(s),
            Err(e) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(
                            ::serde_json::json!({"agent_alias": alias, "error": format!("{e}")})
                        ),
                    "Gateway: agent SecurityPolicy failed to build; skipping its /api/tools listing."
                );
                continue;
            }
        };
        let agent_tools_result = tools::all_tools_with_runtime(
            Arc::new(config.clone()),
            &security,
            &risk_profile,
            &alias,
            Arc::clone(&runtime),
            Arc::clone(&mem),
            &config.browser,
            &config.http_request,
            &config.web_fetch,
            &config.data_dir,
            &config.agents,
            config
                .model_provider_for_agent(&alias)
                .and_then(|e| e.api_key.as_deref()),
            &config,
            None,
            None,
            // Dashboard agent-tool enumeration: top-level origin.
            None,
            None,
        );
        // Same gated seam as the dashboard seed above, so this listing shows
        // the agent's policy-filtered set (filter + MCP). The tools are only
        // enumerated for their specs, never invoked, so the returned channel
        // handles, deferred section, and activation handle are unused.
        let assembled = scoped::ScopedToolRegistry::assemble(scoped::ScopedAssembly {
            config: &config,
            agent_alias: &alias,
            security: &security,
            built: agent_tools_result,
            // Same divergence note as the dashboard seed: no skills on the
            // gateway until Epic F unifies the loaders.
            skills: &[],
            runtime: Arc::clone(&runtime),
            caller_allowed: None,
            connect_mcp: true,
            // Gateway tool-listing path: short-lived, no cross-turn reuse
            // contract, so the per-call connect is correct.
            mcp_registry: None,
            emit_assembly_logs: false,
            exclude_memory: false,
            list_deferred_mcp_specs: true,
        })
        .await;
        let specs: Vec<ToolSpec> = assembled.registry.iter().map(|t| t.spec()).collect();
        tools_registry_by_agent.insert(alias, Arc::new(specs));
    }
    let tools_registry_by_agent: Arc<HashMap<String, Arc<Vec<ToolSpec>>>> =
        Arc::new(tools_registry_by_agent);

    // Cost tracker — process-global singleton so channels share the same instance
    let cost_tracker = CostTracker::get_or_init_global(config.cost.clone(), &config.data_dir);

    // Live model-pricing refresher (once per process; idempotent, no-op unless a
    // provider sets `live_pricing = true`). Each call re-binds the refresher's
    // config handle, so reloads that re-instantiate the config Arc are honored
    // without a restart; shares the global price snapshot the cost path reads.
    zeroclaw_providers::pricing::spawn_refresher(config_state.clone());

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

    operator_auth::warn_legacy_operator(&config.gateway.operator);

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
    if web_dist_dir.is_some() {
        println!("  🌐 Web Dashboard: http://{display_addr}{pfx}/");
    } else {
        println!(
            "  ⚠️  Web Dashboard: not available — reinstall with the supported installer \
             (`./install.sh --source` on Linux/macOS, `setup.bat` on Windows) to build it"
        );
    }
    // Start this run's admin-token generation. The pairing-code admin routes
    // accept only the token the guard holds in memory. If the file cannot be
    // written the guard holds none, so they refuse everyone (fail closed) and
    // no file left by an earlier run is honoured; the banner below stays the
    // way to read the first-run code.
    let admin_token_path = gateway_admin_token_path(&config.data_dir);
    if let Err(e) = pairing.rotate_admin_token(&config.data_dir) {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                .with_attrs(::serde_json::json!({
                    "path": admin_token_path.display().to_string(),
                    "error": e.to_string(),
                })),
            "gateway admin token could not be written; pairing-code admin routes will refuse all callers"
        );
    }
    if let Some(code) = pairing.pairing_code() {
        println!();
        println!("  🔐 PAIRING REQUIRED — use this one-time code:");
        println!("     ┌──────────────┐");
        println!("     │  {code}  │");
        println!("     └──────────────┘");
        println!("     Send: POST {pfx}/pair with header X-Pairing-Code: {code}");
    } else if pairing.require_pairing() {
        for line in already_paired_pairing_notice(host, actual_port, pfx, &admin_token_path) {
            println!("{line}");
        }
        println!();
    } else {
        println!("  ⚠️  Pairing: DISABLED (all requests accepted)");
        println!();
    }
    println!("  POST {pfx}/pair      — pair a new client (X-Pairing-Code header)");
    println!("  POST {pfx}/webhook   — {{\"message\": \"your prompt\"}}");
    println!("  GET  {pfx}/api/*     — REST API (bearer token required)");
    println!("  GET  {pfx}/ws/chat   — WebSocket agent chat");
    if !config.gateway.bridges.is_empty() {
        println!("  GET  {pfx}/ws/bridge — channel bridge control socket");
    }
    #[cfg(feature = "nodes")]
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

    let broadcast_layer: Arc<dyn zeroclaw_runtime::observability::Observer> = Arc::new(
        sse::BroadcastObserver::new(event_tx.clone(), event_buffer.clone()),
    );
    let broadcast_hook_guard =
        zeroclaw_runtime::observability::set_scoped_broadcast_hook(broadcast_layer);

    install_log_broadcast_hook(event_tx.clone());

    // Bound into AppState. Not a broadcaster — the broadcaster is the
    // `broadcast_layer` installed above as the global hook. This is the
    // configured backend (Log/Prometheus/...) wrapped by `TeeObserver`,
    // which tees events into the hook on every record.
    let state_observer: Arc<dyn zeroclaw_runtime::observability::Observer> = Arc::from(
        zeroclaw_runtime::observability::create_observer(&config.observability),
    );

    let (owned_shutdown_tx, _) = tokio::sync::watch::channel(false);
    let (shutdown_tx, reload_tx) = reload_controls
        .map(|controls| (controls.shutdown_tx, Some(controls.reload_tx)))
        .unwrap_or((owned_shutdown_tx, None));
    let mut shutdown_rx = shutdown_tx.subscribe();

    // Node registry for dynamic node discovery
    #[cfg(feature = "nodes")]
    let node_registry = Arc::new({
        let mut registry = nodes::NodeRegistry::new(config.nodes.max_nodes);
        match crate::device_identity::DeviceIdentityStore::open(
            &config.data_dir,
            config.nodes.max_nodes,
        ) {
            Ok(store) => registry = registry.with_identities(store),
            Err(err) => {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"error": format!("{err}")})),
                    "device identity store failed to open; refusing node identity service"
                );
                registry = registry.without_identities();
            }
        }
        registry
    });
    #[cfg(feature = "nodes")]
    {
        zeroclaw_api::node_tools::install(&config.data_dir, node_registry.clone());
        let registry = node_registry.clone();
        let mut shutdown = shutdown_tx.subscribe();
        ::zeroclaw_spawn::spawn!(async move {
            let _ = shutdown.changed().await;
            registry.shutdown();
        });
    }
    #[cfg(feature = "nodes")]
    let mdns_config_state = Arc::clone(&config_state);
    #[cfg(feature = "nodes")]
    let mdns_peer_registry =
        nodes::mdns::MdnsPeerRegistry::new(move || mdns_config_state.read().nodes.mdns.max_peers);
    #[cfg(feature = "nodes")]
    let mdns_task = if config.nodes.mdns.enabled
        && nodes::mdns::is_advertisable_gateway_addr(&actual_addr)
    {
        let mdns_config = config.nodes.mdns.clone();
        let advertised_gateway = nodes::mdns::MdnsAdvertisedGateway::new(actual_port, path_prefix);
        let mdns_registry = mdns_peer_registry.clone();
        let mdns_shutdown_rx = shutdown_tx.subscribe();
        Some(zeroclaw_spawn::spawn!(async move {
            if let Err(err) = nodes::mdns::run_peer_discovery(
                mdns_config,
                advertised_gateway,
                mdns_registry,
                mdns_shutdown_rx,
            )
            .await
            {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"error": format!("{err}")})),
                    "mDNS local peer discovery stopped"
                );
            }
        }))
    } else if config.nodes.mdns.enabled {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                .with_attrs(::serde_json::json!({"bind_addr": actual_addr.to_string()})),
            "mDNS local peer discovery skipped because the gateway is bound to a loopback-only host"
        );
        None
    } else {
        None
    };

    // Device registry and pairing store (only when pairing is required)
    let device_registry = if config.gateway.require_pairing {
        let registry = Arc::new(api_pairing::DeviceRegistry::new(&config.data_dir));
        // Reconcile the registry against the canonical paired-token set so that
        // tokens paired via the legacy `/pair` route (and any other historical
        // orphans) become visible and revocable in the management UI. The token
        // set itself stays owned by `PairingGuard`/`gateway.paired_tokens`.
        match registry.reconcile_from_token_hashes(&pairing.tokens()) {
            Ok(0) => {}
            Ok(n) => ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(::serde_json::json!({ "backfilled": n })),
                "backfilled legacy paired token(s) into the device registry"
            ),
            Err(e) => ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({ "error": format!("{e}") })),
                "device registry backfill from paired_tokens failed"
            ),
        }
        Some(registry)
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
        config_write_lock: Arc::new(tokio::sync::Mutex::new(())),
        model_provider,
        model,
        temperature,
        mem,
        memory_strategy,
        companion_store,
        auto_save: config.memory.auto_save,
        webhook_secret_hash,
        pairing,
        trust_forwarded_headers: config.gateway.trust_forwarded_headers,
        rate_limiter,
        auth_limiter: Arc::new(auth_rate_limit::AuthRateLimiter::new()),
        idempotency_store,
        observer: state_observer,
        tools_registry,
        tools_registry_by_agent,
        cost_tracker,
        event_tx,
        event_buffer,
        shutdown_tx,
        reload_tx,
        #[cfg(feature = "nodes")]
        node_registry,
        #[cfg(feature = "nodes")]
        mdns_peer_registry,
        session_backend,
        session_queue: gateway_session_queue(),
        device_registry,
        pending_pairings,
        path_prefix: path_prefix.unwrap_or("").to_string(),
        web_dist_dir,
        cancel_tokens: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        pending_reload: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        ws_conversations,
        bridge_sockets: Default::default(),
    };

    let review_notifications_task = review_notifications::start(
        Arc::clone(&state.config),
        state.path_prefix.clone(),
        state.shutdown_tx.subscribe(),
    );
    let delegation_notifications_task =
        delegation_notifications::start(Arc::clone(&state.config), state.shutdown_tx.subscribe());
    let reminder_notifications_task =
        reminder_notifications::start(Arc::clone(&state.config), state.shutdown_tx.subscribe());

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
        .route(
            "/api/config/model-providers/{type}/{alias}/refresh-context-window",
            post(api_config::handle_refresh_context_window),
        )
        .route("/api/config/delete-plan", get(api_config::handle_delete_plan))
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
        .route("/api/config/init", post(api_config::handle_init))
        .route("/api/config/migrate", post(api_config::handle_migrate))
        .route(
            "/api/channels/bind",
            post(api_config::handle_api_channel_bind),
        )
        .route("/api/personality", get(api_personality::handle_index))
        .route(
            "/api/personality/templates",
            get(api_personality::handle_templates),
        )
        .route(
            "/api/personality/{filename}",
            get(api_personality::handle_get).put(api_personality::handle_put),
        )
        .route(
            "/api/agents/{alias}/skills",
            get(api_skills::handle_agent_skills),
        )
        .route("/api/skills/bundles", get(api_skills::handle_list_bundles))
        .route(
            "/api/skills/slash-option-kinds",
            get(api_skills::handle_slash_option_kinds),
        )
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
        .route("/api/memory", get(api::handle_api_memory_list))
        .route("/api/memory", post(api::handle_api_memory_store))
        .route("/api/memory/{key}", delete(api::handle_api_memory_delete))
        .route("/api/cost", get(api::handle_api_cost))
        .route("/api/channels", get(api::handle_api_channels))
        .route(
            "/api/channels/{channel}/relink",
            post(api::handle_api_channel_relink),
        )
        .route("/api/health", get(api::handle_api_health))
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
        );

    let inner = inner
        .merge(api_review::routes())
        .merge(api_attention::routes())
        // ── User Model operator review surface ──
        .route("/api/user-model/candidates", get(api_user_model::list_candidates))
        .route("/api/user-model/candidates/{id}", get(api_user_model::candidate_history))
        .route("/api/user-model/heads", get(api_user_model::list_heads))
        .route(
            "/api/user-model/candidates/{id}/review",
            post(api_user_model::review_candidate),
        )
        .route("/api/user-model/statements", post(api_user_model::create_statement))
        // ── Governed Soul owner surface (ADR-015) ──
        .route("/api/soul", get(api_soul::get_soul))
        .route("/api/soul/history", get(api_soul::get_history))
        .route("/api/soul/export", get(api_soul::get_export))
        .route("/api/soul/identity", put(api_soul::put_identity))
        .route("/api/soul/principles", put(api_soul::put_principles))
        .route("/api/soul/rollback", post(api_soul::post_rollback))
        .route("/api/soul/drift", get(api_soul::get_drift))
        .route("/api/soul/anchor", post(api_soul::post_anchor))
        .route(
            "/api/soul/anchor/rollback",
            post(api_soul::post_anchor_rollback),
        )
        .route("/api/soul/story", put(api_soul::put_story))
        .route("/api/soul/tone", put(api_soul::put_tone))
        .route(
            "/api/soul/tone/exemplars",
            put(api_soul::put_tone_exemplars),
        )
        .route("/api/soul/proposals", get(api_soul::get_proposals))
        .route(
            "/api/soul/proposals/{id}/resolve",
            post(api_soul::post_resolve_proposal),
        )
        // ── Backup / data-retention operator surface ──
        // Thin operator-bearer-gated entries over the same BackupTool /
        // DataManagementTool command methods the model-visible tools use;
        // restore keeps its confirm/dry-run guard, purge keeps its
        // dry-run default. Route table lives with the handlers.
        .merge(api_backup_retention::routes())
        // ── SSE event stream ──
        .route("/api/events", get(sse::handle_sse_events))
        .route("/api/events/history", get(sse::handle_events_history))
        // ── WebSocket agent chat ──
        .route("/ws/chat", get(ws::handle_ws_chat))
        .route("/ws/bridge", get(ws_bridge::handle_ws_bridge));
    // ── WebSocket node discovery (nodes feature) ──
    #[cfg(feature = "nodes")]
    let inner = inner
        .route("/ws/nodes", get(nodes::handle_ws_nodes))
        .route(
            "/api/node-identities/pairing",
            post(api_node_identity::issue_pairing),
        )
        .route(
            "/api/node-identities",
            post(api_node_identity::enroll_identity),
        )
        .route(
            "/api/node-identities/{id}",
            delete(api_node_identity::revoke_identity),
        );
    let inner = inner
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

    // The manual cron-trigger route lives on its own sub-router so
    // it can opt out of the 30s gateway-wide TimeoutLayer. It runs a
    // synchronous agent turn inline. Layers attached here travel with the
    // route through `merge`, so only these endpoints see the longer timeout.
    let long_running_router: Router<AppState> =
        Router::new().route("/api/cron/{id}/run", post(api::handle_api_cron_run));
    let long_running_router: Router = long_running_router
        .with_state(state.clone())
        .layer(RequestBodyLimitLayer::new(MAX_BODY_SIZE))
        .layer(TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            Duration::from_secs(gateway_long_running_request_timeout_secs(&config.gateway)),
        ));

    let inner = inner
        .merge(api_delegation::routes(state.clone()))
        .merge(api_attachments::routes(
            state.clone(),
            gateway_request_timeout_secs(&config.gateway),
        ))
        .merge(long_running_router);

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

    let tls_enabled = config
        .gateway
        .tls
        .as_ref()
        .is_some_and(|tls_cfg| tls_cfg.enabled);
    let app = if tls_enabled {
        app.layer(axum::middleware::from_fn(security_headers::apply_with_hsts))
    } else {
        app.layer(axum::middleware::from_fn(security_headers::apply))
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
                                // serving rather than killing the daemon
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

    #[cfg(feature = "nodes")]
    if let Some(task) = mdns_task {
        let mut task = task;
        tokio::select! {
            result = &mut task => {
                if let Err(err) = result {
                    ::zeroclaw_log::record!(
                        DEBUG,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({"error": format!("{err}")})),
                        "LAN peer discovery task join failed"
                    );
                }
            }
            _ = tokio::time::sleep(Duration::from_secs(2)) => {
                task.abort();
            }
        }
    }

    drop(review_notifications_task);
    drop(delegation_notifications_task);
    drop(reminder_notifications_task);
    drop(broadcast_hook_guard);
    Ok(())
}

fn format_paircode_recovery_command(_host: &str, port: u16) -> String {
    format!("zeroclaw gateway get-paircode --new --port {port}")
}

fn already_paired_pairing_notice(
    host: &str,
    port: u16,
    path_prefix: &str,
    admin_token_path: &std::path::Path,
) -> Vec<String> {
    vec![
        "  🔒 Pairing: ACTIVE — this gateway is already paired, so no new \
         one-time code was generated on this start."
            .to_string(),
        format!(
            "     To pair another device, run: {}",
            format_paircode_recovery_command(host, port)
        ),
        format!(
            "     Fallback (on this host, as this user): {}",
            format_paircode_recovery_curl(host, port, path_prefix, admin_token_path)
        ),
    ]
}

fn format_paircode_recovery_curl(
    host: &str,
    port: u16,
    path_prefix: &str,
    admin_token_path: &std::path::Path,
) -> String {
    // Admin paircode routes are localhost-only, so the curl fallback must point
    // at loopback. Bind-only hosts and non-loopback advertised hosts are
    // normalized to `127.0.0.1`; explicit loopback hosts are preserved. They
    // also require this run's admin secret, read from its owner-only file.
    // Never send that secret through a proxy configured in the owner's shell.
    let recovery_host = paircode_recovery_curl_host(host);
    format!(
        "curl --noproxy '*' -s -X POST -H \"{GATEWAY_ADMIN_TOKEN_HEADER}: $(cat '{}')\" \
         http://{recovery_host}:{port}{path_prefix}/admin/paircode/new",
        admin_token_path
            .display()
            .to_string()
            .replace('\'', "'\\''")
    )
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
            let token_hash = PairingGuard::token_hash(&token);
            if let Some(ref registry) = state.device_registry {
                if let Err(e) = registry.register(
                    token_hash.clone(),
                    api_pairing::DeviceInfo {
                        id: uuid::Uuid::new_v4().to_string(),
                        name: None,
                        device_type: None,
                        paired_at: chrono::Utc::now(),
                        last_seen: chrono::Utc::now(),
                        ip_address: Some(rate_key.clone()),
                        capabilities: None,
                    },
                ) {
                    ::zeroclaw_log::record!(
                        ERROR,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({"error": format!("{e}")})),
                        "device registry insert failed after successful legacy /pair; rolling back in-process token"
                    );
                    state.pairing.revoke_token_hash(&token_hash);
                    let body = serde_json::json!({
                        "paired": false,
                        "persisted": false,
                        "error": format!("Device registry error: {e}"),
                        "message": "Pairing failed; the in-process token was not retained.",
                    });
                    return (StatusCode::INTERNAL_SERVER_ERROR, Json(body));
                }
            }
            if let Err(err) = Box::pin(persist_pairing_tokens(
                state.config.clone(),
                &state.pairing,
                state.config_write_lock.clone(),
            ))
            .await
            {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"error": format!("{}", err)})),
                    "pairing token persistence failed; rolling back in-process token"
                );
                state.pairing.revoke_token_hash(&token_hash);
                let body = serde_json::json!({
                    "paired": false,
                    "persisted": false,
                    "error": format!("Token persistence error: {err}"),
                    "message": "Pairing failed; the in-process token was not retained.",
                });
                return (StatusCode::INTERNAL_SERVER_ERROR, Json(body));
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
    config_write_lock: Arc<tokio::sync::Mutex<()>>,
) -> Result<()> {
    // Self-contained: no caller pre-reads config for modify, so this
    // acquires the witness itself rather than taking it as a param. Held
    // across the whole read-modify-save-swap below.
    let _guard = Arc::clone(&config_write_lock).lock_owned().await;
    debug_assert!(
        config_write_lock.try_lock().is_err(),
        "persist_pairing_tokens must hold config_write_lock across its read-modify-save-swap"
    );
    let paired_tokens = pairing.tokens();
    // This is needed because parking_lot's guard is not Send so we clone the inner
    // this should be removed once async mutexes are used everywhere
    let mut updated_cfg = { config.read().clone() };
    updated_cfg.gateway.paired_tokens = paired_tokens;
    updated_cfg.mark_dirty("gateway.paired_tokens");
    updated_cfg
        .save_dirty()
        .await
        .context("Failed to persist paired tokens to config.toml")?;

    // Keep shared runtime config in sync with persisted tokens.
    *config.write() = updated_cfg;
    Ok(())
}

/// Result of a gateway chat turn.
struct GatewayChatOutcome {
    response: String,
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
             Run `zeroclaw quickstart`, or fix \
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

fn needs_quickstart_for(model: &str) -> Option<anyhow::Error> {
    if model.trim().is_empty() {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure),
            "gateway dispatch refused: no model configured"
        );
        Some(anyhow::Error::msg(
            "needs_quickstart: gateway has no model configured. Run \
             `zeroclaw quickstart`, or set [providers.models.<type>.<alias>] \
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

fn needs_quickstart_channel_reply() -> String {
    i18n::get_required_cli_string("channel-needs-quickstart-reply")
}

pub(crate) async fn run_gateway_chat_with_tools(
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
    // doesn't go through the cost-tracking scope.
    #[cfg(test)]
    {
        let _ = (session_id, agent_override);
        let response = state
            .model_provider
            .chat_with_system(None, message, &state.model, state.temperature)
            .await?;
        Ok(GatewayChatOutcome { response })
    }

    #[cfg(not(test))]
    {
        let config = state.config.read().clone();
        let agent_alias = require_gateway_chat_agent_alias(&config, agent_override)?;

        // Scope the cost tracking context so per-LLM-call usage flows into
        // the gateway's cost tracker and costs.jsonl. A separate
        // `TOOL_LOOP_TURN_USAGE` task-local accumulates this turn's totals so
        // the runtime-owned lifecycle guard can annotate its `AgentEnd`
        // without racing concurrent requests sharing the same tracker.
        // Pricing is built from the
        // unified `build_model_provider_pricing` (alias-keyed, `cost.rates`
        // wins over legacy per-alias pricing).
        let cost_tracking_context = state.cost_tracker.as_ref().map(|tracker| {
            let pricing = zeroclaw_runtime::agent::cost::build_model_provider_pricing(&config);
            zeroclaw_runtime::agent::cost::ToolLoopCostTrackingContext::new(
                tracker.clone(),
                std::sync::Arc::new(pricing),
            )
            .with_agent_alias(&agent_alias)
        });
        let turn_usage = state.cost_tracker.as_ref().map(|_| {
            std::sync::Arc::new(parking_lot::Mutex::new(
                zeroclaw_runtime::agent::cost::TurnUsage::default(),
            ))
        });
        let response = Box::pin(zeroclaw_runtime::agent::cost::TOOL_LOOP_TURN_USAGE.scope(
            turn_usage.clone(),
            zeroclaw_runtime::agent::cost::TOOL_LOOP_COST_TRACKING_CONTEXT.scope(
                cost_tracking_context,
                zeroclaw_runtime::agent::process_message(
                    config,
                    &agent_alias,
                    message,
                    session_id,
                    zeroclaw_api::ingress::TurnOrigin::Interactive,
                ),
            ),
        ))
        .await?;
        Ok(GatewayChatOutcome { response })
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

    let model_label = {
        let cfg = state.config.read();
        let resolved_agent_alias = resolve_gateway_chat_agent_alias(&cfg, agent_override);
        let resolved_provider = resolved_agent_alias
            .as_deref()
            .and_then(|alias| cfg.resolved_model_provider_for_agent(alias));
        resolved_provider
            .and_then(|(_, _, entry)| {
                entry
                    .model
                    .as_deref()
                    .map(str::trim)
                    .filter(|model| !model.is_empty())
                    .map(ToString::to_string)
            })
            .or_else(|| cfg.resolve_default_model())
            .unwrap_or_else(|| "<unresolved>".to_string())
    };
    // HTTP transport owns request latency and response mapping. The production
    // dispatch below enters `process_message`, whose runtime turn guard is the
    // sole owner of lifecycle and LLM events. Emitting another bracket here
    // gives one webhook prompt two unrelated turn IDs.
    let started_at = Instant::now();

    match run_gateway_chat_with_tools(&state, message, session_id.as_deref(), agent_override).await
    {
        Ok(GatewayChatOutcome { response, .. }) => {
            let duration = started_at.elapsed();
            state.observer.record_metric(
                &zeroclaw_runtime::observability::traits::ObserverMetric::RequestLatency(duration),
            );

            let body = serde_json::json!({"response": response, "model": model_label});
            (StatusCode::OK, Json(body))
        }
        Err(e) => {
            let duration = started_at.elapsed();
            let sanitized = zeroclaw_providers::sanitize_api_error(&e.to_string());
            state.observer.record_metric(
                &zeroclaw_runtime::observability::traits::ObserverMetric::RequestLatency(duration),
            );
            state
                .observer
                .record_event(&zeroclaw_runtime::observability::ObserverEvent::Error {
                    component: "gateway".to_string(),
                    message: sanitized.clone(),
                });
            if is_needs_quickstart_err(&e) {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                    "Webhook chat refused: gateway has no model configured; \
                     run `zeroclaw quickstart`"
                );
                let body = serde_json::json!({
                    "error": "needs_quickstart",
                    "url": "/config"
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

/// Reject a pairing-code admin request that does not present this run's admin
/// secret. [`require_localhost`] alone is not enough for these routes: a reverse
/// proxy or tunnel on the same host relays remote callers from loopback, and
/// a code read or minted here is exchanged at `/pair` for a shared-operator
/// bearer. The secret reaches local clients through an owner-only file, so
/// presenting it proves the caller runs as the gateway's user on its host.
/// Admission compares against the token the pairing guard holds for this
/// run, never the file, so a stale file or a failed rotation matches nothing.
fn require_gateway_admin_token(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<(), (StatusCode, Json<serde_json::Value>)> {
    let presented = headers
        .get(GATEWAY_ADMIN_TOKEN_HEADER)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if state.pairing.admin_token_matches(presented) {
        Ok(())
    } else {
        Err((
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({
                "error": "Pairing-code admin requests need this gateway's admin token. \
                          Run `zeroclaw gateway get-paircode` on the gateway host, as the \
                          user that runs the gateway."
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

/// GET /admin/paircode — fetch current pairing code (localhost and owner token only)
async fn handle_admin_paircode(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, (StatusCode, Json<serde_json::Value>)> {
    require_localhost(&peer)?;
    require_gateway_admin_token(&state, &headers)?;
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

#[derive(Debug, serde::Deserialize, Default)]
pub struct AdminPaircodeQuery {
    #[serde(default)]
    pub rotate: Option<String>,
}

async fn handle_admin_paircode_new(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Query(params): Query<AdminPaircodeQuery>,
) -> Result<impl IntoResponse, (StatusCode, Json<serde_json::Value>)> {
    require_localhost(&peer)?;
    require_gateway_admin_token(&state, &headers)?;

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
            if let Err(e) = persist_pairing_tokens(
                state.config.clone(),
                &state.pairing,
                state.config_write_lock.clone(),
            )
            .await
            {
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
            if let Err(e) = persist_pairing_tokens(
                state.config.clone(),
                &state.pairing,
                state.config_write_lock.clone(),
            )
            .await
            {
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

/// GET /pair/code — whether pairing is required. It never returns the code.
///
/// No HTTP caller can prove it is on this host: a reverse proxy or tunnel on
/// the same host relays remote callers from loopback, with or without
/// forwarding headers, and whoever reads a first-run code can pair as the
/// shared operator. The code reaches operators only through the startup
/// banner in the gateway log and `zeroclaw gateway get-paircode`, which
/// presents the owner-only admin token. `pairing_code` stays in the response,
/// always `null`, so existing dashboard clients fall back to manual entry.
async fn handle_pair_code(State(state): State<AppState>) -> impl IntoResponse {
    let body = serde_json::json!({
        "success": true,
        "pairing_required": state.pairing.require_pairing(),
        "pairing_code": serde_json::Value::Null,
    });

    (StatusCode::OK, Json(body))
}

#[cfg(test)]
mod tests;

/// Test-only: a full `run_gateway` start replaces the process-global log
/// broadcast hook. Unit tests that subscribe the broadcast and drive turns
/// (see ws::device_tests::turn_control_tests) take this lock so a concurrent
/// gateway start cannot swap the sender under them. Compiled out of
/// production builds.
#[cfg(test)]
pub(crate) static TEST_GATEWAY_START_LOCK: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

/// Install the gateway's log broadcast sender. In TEST builds the swap is
/// serialized by TEST_GATEWAY_START_LOCK (scoped: the parking_lot guard never
/// crosses an await, keeping run_gateway's future Send). Defined here — after
/// the file's first `#[cfg(test)]` region — so the config-save-isolation
/// architecture guard still starts its scan region at the original marker,
/// not at an earlier inline `cfg(test)` block near production code.
#[cfg(test)]
fn install_log_broadcast_hook(sender: zeroclaw_log::LogBroadcastSender) {
    let _guard = TEST_GATEWAY_START_LOCK.lock();
    zeroclaw_log::set_broadcast_hook(sender);
}

#[cfg(not(test))]
fn install_log_broadcast_hook(sender: zeroclaw_log::LogBroadcastSender) {
    zeroclaw_log::set_broadcast_hook(sender);
}

#[cfg(test)]
mod accept_error_tests {
    use super::is_recoverable_accept_error;
    use std::io::{Error, ErrorKind};

    #[cfg(unix)]
    #[test]
    fn fd_exhaustion_accept_errors_are_recoverable() {
        // EMFILE/ENFILE must not terminate the daemon.
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
