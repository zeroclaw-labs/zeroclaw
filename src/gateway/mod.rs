//! Axum-based HTTP gateway with proper HTTP/1.1 compliance, body limits, and timeouts.
//!
//! This module replaces the raw TCP implementation with axum for:
//! - Proper HTTP/1.1 parsing and compliance
//! - Content-Length validation (handled by hyper)
//! - Request body size limits (64KB max)
//! - Request timeouts (30s) to prevent slow-loris attacks
//! - Header sanitization (handled by axum/hyper)

pub mod api;
pub mod api_pairing;
#[cfg(feature = "plugins-wasm")]
pub mod api_plugins;
#[cfg(feature = "webauthn")]
pub mod api_webauthn;
pub mod auth_rate_limit;
pub mod canvas;
pub mod nodes;
pub mod session_queue;
pub mod sse;
pub mod static_files;
pub mod tls;
pub mod ws;

use crate::channels::{session_backend::SessionBackend, session_sqlite::SqliteSessionBackend};
use crate::config::Config;
use crate::cost::CostTracker;
use crate::memory::{self, Memory, MemoryCategory};
use crate::providers::{self, ChatMessage, Provider};
use crate::runtime;
use crate::security::SecurityPolicy;
use crate::security::pairing::{PairingGuard, constant_time_eq, is_public_bind};
use crate::tools;
use crate::tools::canvas::CanvasStore;
use crate::tools::traits::ToolSpec;
use anyhow::{Context, Result};
use axum::{
    Router,
    extract::{ConnectInfo, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Json},
    routing::{delete, get, post, put},
};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::timeout::TimeoutLayer;
use uuid::Uuid;

/// Maximum request body size (64KB) — prevents memory exhaustion
pub const MAX_BODY_SIZE: usize = 65_536;
/// Default request timeout (30s) — prevents slow-loris attacks.
pub const REQUEST_TIMEOUT_SECS: u64 = 30;

/// Read gateway request timeout from `ZEROCLAW_GATEWAY_TIMEOUT_SECS` env var
/// at runtime, falling back to [`REQUEST_TIMEOUT_SECS`].
///
/// Agentic workloads with tool use (web search, MCP tools, sub-agent
/// delegation) regularly exceed 30 seconds. This allows operators to
/// increase the timeout without recompiling.
pub fn gateway_request_timeout_secs() -> u64 {
    std::env::var("ZEROCLAW_GATEWAY_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(REQUEST_TIMEOUT_SECS)
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

fn sender_session_id(channel: &str, msg: &crate::channels::traits::ChannelMessage) -> String {
    match &msg.thread_ts {
        Some(thread_id) => format!("{channel}_{thread_id}_{}", msg.sender),
        None => format!("{channel}_{}", msg.sender),
    }
}

fn webhook_session_id(headers: &HeaderMap) -> Option<String> {
    headers
        .get("X-Session-Id")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
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
    fn new(pair_per_minute: u32, webhook_per_minute: u32, max_keys: usize) -> Self {
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
    fn new(ttl: Duration, max_keys: usize) -> Self {
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
    if trust_forwarded_headers {
        if let Some(ip) = forwarded_client_ip(headers) {
            return ip.to_string();
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

/// Shared state for all axum handlers
#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Mutex<Config>>,
    pub provider: Arc<dyn Provider>,
    pub model: String,
    pub temperature: f64,
    pub mem: Arc<dyn Memory>,
    pub auto_save: bool,
    /// SHA-256 hash of `X-Webhook-Secret` (hex-encoded), never plaintext.
    pub webhook_secret_hash: Option<Arc<str>>,
    pub pairing: Arc<PairingGuard>,
    pub trust_forwarded_headers: bool,
    pub rate_limiter: Arc<GatewayRateLimiter>,
    pub auth_limiter: Arc<auth_rate_limit::AuthRateLimiter>,
    pub idempotency_store: Arc<IdempotencyStore>,
    /// Observability backend for metrics scraping
    pub observer: Arc<dyn crate::observability::Observer>,
    /// Registered tool specs (for web dashboard tools page)
    pub tools_registry: Arc<Vec<ToolSpec>>,
    /// Cost tracker (optional, for web dashboard cost page)
    pub cost_tracker: Option<Arc<CostTracker>>,
    /// SSE broadcast channel for real-time events
    pub event_tx: tokio::sync::broadcast::Sender<serde_json::Value>,
    /// Shutdown signal sender for graceful shutdown
    pub shutdown_tx: tokio::sync::watch::Sender<bool>,
    /// Registry of dynamically connected nodes
    pub node_registry: Arc<nodes::NodeRegistry>,
    /// Path prefix for reverse-proxy deployments (empty string = no prefix)
    pub path_prefix: String,
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
}

/// Run the HTTP gateway using axum with proper HTTP/1.1 compliance.
#[allow(clippy::too_many_lines)]
pub async fn run_gateway(host: &str, port: u16, config: Config) -> Result<()> {
    // ── Security: warn on public bind without tunnel or explicit opt-in ──
    if is_public_bind(host) && config.tunnel.provider == "none" && !config.gateway.allow_public_bind
    {
        tracing::warn!(
            "⚠️  Binding to {host} — gateway will be exposed to all network interfaces.\n\
             Suggestion: use --host 127.0.0.1 (default), configure a tunnel, or set\n\
             [gateway] allow_public_bind = true in config.toml to silence this warning.\n\n\
             Docker/VM: if you are running inside a container or VM, this is expected."
        );
    }
    let config_state = Arc::new(Mutex::new(config.clone()));

    // ── Hooks ──────────────────────────────────────────────────────
    let hooks: Option<std::sync::Arc<crate::hooks::HookRunner>> = if config.hooks.enabled {
        Some(std::sync::Arc::new(crate::hooks::HookRunner::new()))
    } else {
        None
    };

    let addr: SocketAddr = format!("{host}:{port}").parse()?;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let actual_port = listener.local_addr()?.port();
    let display_addr = format!("{host}:{actual_port}");

    let provider: Arc<dyn Provider> = Arc::from(providers::create_resilient_provider_with_options(
        config.default_provider.as_deref().unwrap_or("openrouter"),
        config.api_key.as_deref(),
        config.api_url.as_deref(),
        &config.reliability,
        &providers::ProviderRuntimeOptions {
            auth_profile_override: None,
            provider_api_url: config.api_url.clone(),
            zeroclaw_dir: config.config_path.parent().map(std::path::PathBuf::from),
            secrets_encrypt: config.secrets.encrypt,
            reasoning_enabled: config.runtime.reasoning_enabled,
            reasoning_effort: config.runtime.reasoning_effort.clone(),
            provider_timeout_secs: Some(config.provider_timeout_secs),
            extra_headers: config.extra_headers.clone(),
            api_path: config.api_path.clone(),
            provider_max_tokens: config.provider_max_tokens,
        },
    )?);
    let model = config
        .default_model
        .clone()
        .unwrap_or_else(|| "anthropic/claude-sonnet-4".into());
    let temperature = config.default_temperature;
    let mem: Arc<dyn Memory> = Arc::from(memory::create_memory_with_storage_and_routes(
        &config.memory,
        &config.embedding_routes,
        Some(&config.storage.provider.config),
        &config.workspace_dir,
        config.api_key.as_deref(),
    )?);
    let runtime: Arc<dyn runtime::RuntimeAdapter> =
        Arc::from(runtime::create_runtime(&config.runtime)?);
    let security = Arc::new(SecurityPolicy::from_config(
        &config.autonomy,
        &config.workspace_dir,
    ));

    let (composio_key, composio_entity_id) = if config.composio.enabled {
        (
            config.composio.api_key.as_deref(),
            Some(config.composio.entity_id.as_str()),
        )
    } else {
        (None, None)
    };

    let canvas_store = tools::CanvasStore::new();

    let (
        mut tools_registry_raw,
        delegate_handle_gw,
        _reaction_handle_gw,
        _channel_map_handle,
        _ask_user_handle_gw,
        _escalate_handle_gw,
    ) = tools::all_tools_with_runtime(
        Arc::new(config.clone()),
        &security,
        runtime,
        Arc::clone(&mem),
        composio_key,
        composio_entity_id,
        &config.browser,
        &config.http_request,
        &config.web_fetch,
        &config.workspace_dir,
        &config.agents,
        config.api_key.as_deref(),
        &config,
        Some(canvas_store.clone()),
    );

    // ── Wire MCP tools into the gateway tool registry (non-fatal) ───
    // Without this, the `/api/tools` endpoint misses MCP tools.
    if config.mcp.enabled && !config.mcp.servers.is_empty() {
        tracing::info!(
            "Gateway: initializing MCP client — {} server(s) configured",
            config.mcp.servers.len()
        );
        match tools::McpRegistry::connect_all(&config.mcp.servers).await {
            Ok(registry) => {
                let registry = std::sync::Arc::new(registry);
                if config.mcp.deferred_loading {
                    let deferred_set =
                        tools::DeferredMcpToolSet::from_registry(std::sync::Arc::clone(&registry))
                            .await;
                    tracing::info!(
                        "Gateway MCP deferred: {} tool stub(s) from {} server(s)",
                        deferred_set.len(),
                        registry.server_count()
                    );
                    let activated =
                        std::sync::Arc::new(std::sync::Mutex::new(tools::ActivatedToolSet::new()));
                    let builtin_specs: Vec<tools::ToolSpec> =
                        tools_registry_raw.iter().map(|t| t.spec()).collect();
                    tools_registry_raw.push(Box::new(tools::ToolSearchTool::new(
                        deferred_set,
                        activated,
                        builtin_specs,
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
                    tracing::info!(
                        "Gateway MCP: {} tool(s) registered from {} server(s)",
                        registered,
                        registry.server_count()
                    );
                }
            }
            Err(e) => {
                tracing::error!("Gateway MCP registry failed to initialize: {e:#}");
            }
        }
    }

    tools::tool_search::ensure_registered(&mut tools_registry_raw);

    let tools_registry: Arc<Vec<ToolSpec>> =
        Arc::new(tools_registry_raw.iter().map(|t| t.spec()).collect());

    // Cost tracker — process-global singleton so channels share the same instance
    let cost_tracker = CostTracker::get_or_init_global(config.cost.clone(), &config.workspace_dir);

    // SSE broadcast channel for real-time events
    let (event_tx, _event_rx) = tokio::sync::broadcast::channel::<serde_json::Value>(256);
    // Extract webhook secret for authentication from env var
    let webhook_secret_hash: Option<Arc<str>> = std::env::var("ZEROCLAW_WEBHOOK_SECRET")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .map(|s| Arc::<str>::from(hash_webhook_secret(&s)));

    // ── Session persistence for WS chat ─────────────────────
    let session_backend: Option<Arc<dyn SessionBackend>> = if config.gateway.session_persistence {
        match SqliteSessionBackend::new(&config.workspace_dir) {
            Ok(b) => {
                tracing::info!("Gateway session persistence enabled (SQLite)");
                if config.gateway.session_ttl_hours > 0 {
                    if let Ok(cleaned) = b.cleanup_stale(config.gateway.session_ttl_hours) {
                        if cleaned > 0 {
                            tracing::info!("Cleaned up {cleaned} stale gateway sessions");
                        }
                    }
                }
                Some(Arc::new(b))
            }
            Err(e) => {
                tracing::warn!("Session persistence disabled: {e}");
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
    let tunnel = crate::tunnel::create_tunnel(&config.tunnel)?;
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
        println!("  🔒 Pairing: ACTIVE (bearer token required)");
        println!("     To pair a new device: zeroclaw gateway get-paircode --new");
        println!();
    } else {
        println!("  ⚠️  Pairing: DISABLED (all requests accepted)");
        println!();
    }
    println!("  POST {pfx}/pair      — pair a new client (X-Pairing-Code header)");
    println!("  POST {pfx}/webhook   — {{\"message\": \"your prompt\"}}");
    println!("  GET  {pfx}/api/*     — REST API (bearer token required)");
    println!("  GET  {pfx}/ws/chat   — WebSocket agent chat");
    if config.nodes.enabled {
        println!("  GET  {pfx}/ws/nodes  — WebSocket node discovery");
    }
    println!("  GET  {pfx}/health    — health check");
    println!("  GET  {pfx}/metrics   — Prometheus metrics");
    println!("  Press Ctrl+C to stop.\n");

    crate::health::mark_component_ok("gateway");

    // Fire gateway start hook
    if let Some(ref hooks) = hooks {
        hooks.fire_gateway_start(host, actual_port).await;
    }

    // Wrap observer with broadcast capability for SSE
    let broadcast_observer: Arc<dyn crate::observability::Observer> =
        Arc::new(sse::BroadcastObserver::new(
            crate::observability::create_observer(&config.observability),
            event_tx.clone(),
        ));

    let (shutdown_tx, mut shutdown_rx) = tokio::sync::watch::channel(false);

    // Node registry for dynamic node discovery
    let node_registry = Arc::new(nodes::NodeRegistry::new(config.nodes.max_nodes));

    // Device registry and pairing store (only when pairing is required)
    let device_registry = if config.gateway.require_pairing {
        Some(Arc::new(api_pairing::DeviceRegistry::new(
            &config.workspace_dir,
        )))
    } else {
        None
    };
    let pending_pairings = if config.gateway.require_pairing {
        Some(Arc::new(api_pairing::PairingStore::new(
            config.gateway.pairing_dashboard.max_pending_codes,
        )))
    } else {
        None
    };

    let state = AppState {
        config: config_state,
        provider,
        model,
        temperature,
        mem,
        auto_save: config.memory.auto_save,
        webhook_secret_hash,
        pairing,
        trust_forwarded_headers: config.gateway.trust_forwarded_headers,
        rate_limiter,
        auth_limiter: Arc::new(auth_rate_limit::AuthRateLimiter::new()),
        idempotency_store,
        observer: broadcast_observer,
        tools_registry,
        cost_tracker,
        event_tx,
        shutdown_tx,
        node_registry,
        session_backend,
        session_queue: Arc::new(session_queue::SessionActorQueue::new(8, 30, 600)),
        device_registry,
        pending_pairings,
        path_prefix: path_prefix.unwrap_or("").to_string(),
        canvas_store,
        #[cfg(feature = "webauthn")]
        webauthn: if config.security.webauthn.enabled {
            let secret_store = Arc::new(crate::security::SecretStore::new(
                &config.workspace_dir,
                true,
            ));
            let wa_config = crate::security::webauthn::WebAuthnConfig {
                enabled: true,
                rp_id: config.security.webauthn.rp_id.clone(),
                rp_origin: config.security.webauthn.rp_origin.clone(),
                rp_name: config.security.webauthn.rp_name.clone(),
            };
            Some(Arc::new(api_webauthn::WebAuthnState {
                manager: crate::security::webauthn::WebAuthnManager::new(
                    wa_config,
                    secret_store,
                    &config.workspace_dir,
                ),
                pending_registrations: parking_lot::Mutex::new(std::collections::HashMap::new()),
                pending_authentications: parking_lot::Mutex::new(std::collections::HashMap::new()),
            }))
        } else {
            None
        },
    };

    // Config PUT needs larger body limit (1MB)
    let config_put_router = Router::new()
        .route("/api/config", put(api::handle_api_config_put))
        .layer(RequestBodyLimitLayer::new(1_048_576));

    // Build router with middleware
    let inner = Router::new()
        // ── Admin routes (for CLI management) ──
        .route("/admin/shutdown", post(handle_admin_shutdown))
        .route("/admin/paircode", get(handle_admin_paircode))
        .route("/admin/paircode/new", post(handle_admin_paircode_new))
        // ── Existing routes ──
        .route("/health", get(handle_health))
        .route("/metrics", get(handle_metrics))
        .route("/pair", post(handle_pair))
        .route("/pair/code", get(handle_pair_code))
        .route("/webhook", post(handle_webhook))
        // ── Claude Code runner hooks ──
        .route("/hooks/claude-code", post(api::handle_claude_code_hook))
        // ── Web Dashboard API routes ──
        .route("/api/status", get(api::handle_api_status))
        .route("/api/config", get(api::handle_api_config_get))
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
        .route("/api/health", get(api::handle_api_health))
        .route("/api/sessions", get(api::handle_api_sessions_list))
        .route("/api/sessions/running", get(api::handle_api_sessions_running))
        .route(
            "/api/sessions/{id}/messages",
            get(api::handle_api_session_messages),
        )
        .route("/api/sessions/{id}", delete(api::handle_api_session_delete).put(api::handle_api_session_rename))
        .route("/api/sessions/{id}/state", get(api::handle_api_session_state))
        // ── Pairing + Device management API ──
        .route("/api/pairing/initiate", post(api_pairing::initiate_pairing))
        .route("/api/pair", post(api_pairing::submit_pairing_enhanced))
        .route("/api/devices", get(api_pairing::list_devices))
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
        // ── WebSocket agent chat ──
        .route("/ws/chat", get(ws::handle_ws_chat))
        // ── WebSocket canvas updates ──
        .route("/ws/canvas/{id}", get(canvas::handle_ws_canvas))
        // ── WebSocket node discovery ──
        .route("/ws/nodes", get(nodes::handle_ws_nodes))
        // ── Static assets (web dashboard) ──
        .route("/_app/{*path}", get(static_files::handle_static))
        // ── Config PUT with larger body limit ──
        .merge(config_put_router)
        // ── SPA fallback: non-API GET requests serve index.html ──
        .fallback(get(static_files::handle_spa_fallback))
        .with_state(state)
        .layer(RequestBodyLimitLayer::new(MAX_BODY_SIZE))
        .layer(TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            Duration::from_secs(gateway_request_timeout_secs()),
        ));

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
                tracing::info!("TLS enabled with mutual TLS (mTLS) client verification");
            } else {
                tracing::info!("TLS enabled (no client certificate requirement)");
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
                    let (tcp_stream, remote_addr) = conn?;
                    let tls_acceptor = tls_acceptor.clone();
                    let svc = tower::MakeService::<
                        SocketAddr,
                        hyper::Request<hyper::body::Incoming>,
                    >::make_service(&mut app, remote_addr)
                    .await
                    .expect("infallible make_service");

                    tokio::spawn(async move {
                        let tls_stream = match tls_acceptor.accept(tcp_stream).await {
                            Ok(s) => s,
                            Err(e) => {
                                tracing::debug!("TLS handshake failed from {remote_addr}: {e}");
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
                            tracing::debug!("connection error from {remote_addr}: {e}");
                        }
                    });
                }
                _ = shutdown_signal.changed() => {
                    tracing::info!("🦀 ZeroClaw Gateway shutting down...");
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
            tracing::info!("🦀 ZeroClaw Gateway shutting down...");
        })
        .await?;
    }

    Ok(())
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
        "runtime": crate::health::snapshot_json(),
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
    observer: &dyn crate::observability::Observer,
) -> Option<&crate::observability::PrometheusObserver> {
    observer
        .as_any()
        .downcast_ref::<crate::observability::PrometheusObserver>()
        .or_else(|| {
            observer
                .as_any()
                .downcast_ref::<sse::BroadcastObserver>()
                .and_then(|broadcast| {
                    broadcast
                        .inner()
                        .as_any()
                        .downcast_ref::<crate::observability::PrometheusObserver>()
                })
        })
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
        tracing::warn!("/pair rate limit exceeded");
        let err = serde_json::json!({
            "error": "Too many pairing requests. Please retry later.",
            "retry_after": RATE_LIMIT_WINDOW_SECS,
        });
        return (StatusCode::TOO_MANY_REQUESTS, Json(err));
    }

    // ── Auth rate limiting (brute-force protection) ──
    if let Err(e) = state.auth_limiter.check_rate_limit(&rate_key) {
        tracing::warn!("🔐 Pairing auth rate limit exceeded for {rate_key}");
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
            tracing::info!("🔐 New client paired successfully");
            if let Err(err) =
                Box::pin(persist_pairing_tokens(state.config.clone(), &state.pairing)).await
            {
                tracing::error!("🔐 Pairing succeeded but token persistence failed: {err:#}");
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
            tracing::warn!("🔐 Pairing attempt with invalid code");
            let err = serde_json::json!({"error": "Invalid pairing code"});
            (StatusCode::FORBIDDEN, Json(err))
        }
        Err(lockout_secs) => {
            tracing::warn!(
                "🔐 Pairing locked out — too many failed attempts ({lockout_secs}s remaining)"
            );
            let err = serde_json::json!({
                "error": format!("Too many failed attempts. Try again in {lockout_secs}s."),
                "retry_after": lockout_secs
            });
            (StatusCode::TOO_MANY_REQUESTS, Json(err))
        }
    }
}

async fn persist_pairing_tokens(config: Arc<Mutex<Config>>, pairing: &PairingGuard) -> Result<()> {
    let paired_tokens = pairing.tokens();
    // This is needed because parking_lot's guard is not Send so we clone the inner
    // this should be removed once async mutexes are used everywhere
    let mut updated_cfg = { config.lock().clone() };
    updated_cfg.gateway.paired_tokens = paired_tokens;
    updated_cfg
        .save()
        .await
        .context("Failed to persist paired tokens to config.toml")?;

    // Keep shared runtime config in sync with persisted tokens.
    *config.lock() = updated_cfg;
    Ok(())
}

/// Simple chat for webhook endpoint (no tools, for backward compatibility and testing).
async fn run_gateway_chat_simple(state: &AppState, message: &str) -> anyhow::Result<String> {
    let user_messages = vec![ChatMessage::user(message)];

    // Keep webhook/gateway prompts aligned with channel behavior by injecting
    // workspace-aware system context before model invocation.
    let system_prompt = {
        let config_guard = state.config.lock();
        crate::channels::build_system_prompt(
            &config_guard.workspace_dir,
            &state.model,
            &[], // tools - empty for simple chat
            &[], // skills
            Some(&config_guard.identity),
            None, // bootstrap_max_chars - use default
        )
    };

    let mut messages = Vec::with_capacity(1 + user_messages.len());
    messages.push(ChatMessage::system(system_prompt));
    messages.extend(user_messages);

    let multimodal_config = state.config.lock().multimodal.clone();
    let prepared =
        crate::multimodal::prepare_messages_for_provider(&messages, &multimodal_config).await?;

    state
        .provider
        .chat_with_history(&prepared.messages, &state.model, state.temperature)
        .await
}

/// Full-featured chat with tools for channel handlers (WhatsApp, Linq, Nextcloud Talk).
async fn run_gateway_chat_with_tools(
    state: &AppState,
    message: &str,
    session_id: Option<&str>,
) -> anyhow::Result<String> {
    let config = state.config.lock().clone();
    Box::pin(crate::agent::process_message(config, message, session_id)).await
}

/// Webhook request body
#[derive(serde::Deserialize)]
pub struct WebhookBody {
    pub message: String,
}

/// POST /webhook — main webhook endpoint
async fn handle_webhook(
    State(state): State<AppState>,
    ConnectInfo(peer_addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: Result<Json<WebhookBody>, axum::extract::rejection::JsonRejection>,
) -> impl IntoResponse {
    let rate_key =
        client_key_from_request(Some(peer_addr), &headers, state.trust_forwarded_headers);
    if !state.rate_limiter.allow_webhook(&rate_key) {
        tracing::warn!("/webhook rate limit exceeded");
        let err = serde_json::json!({
            "error": "Too many webhook requests. Please retry later.",
            "retry_after": RATE_LIMIT_WINDOW_SECS,
        });
        return (StatusCode::TOO_MANY_REQUESTS, Json(err));
    }

    // ── Bearer token auth (pairing) with auth rate limiting ──
    if state.pairing.require_pairing() {
        if let Err(e) = state.auth_limiter.check_rate_limit(&rate_key) {
            tracing::warn!("Webhook: auth rate limit exceeded for {rate_key}");
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
            tracing::warn!("Webhook: rejected — not paired / invalid bearer token");
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
                tracing::warn!("Webhook: rejected request — invalid or missing X-Webhook-Secret");
                let err = serde_json::json!({"error": "Unauthorized — invalid or missing X-Webhook-Secret header"});
                return (StatusCode::UNAUTHORIZED, Json(err));
            }
        }
    }

    // ── Parse body ──
    let Json(webhook_body) = match body {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!("Webhook JSON parse error: {e}");
            let err = serde_json::json!({
                "error": "Invalid JSON body. Expected: {\"message\": \"...\"}"
            });
            return (StatusCode::BAD_REQUEST, Json(err));
        }
    };

    // ── Idempotency (optional) ──
    if let Some(idempotency_key) = headers
        .get("X-Idempotency-Key")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        if !state.idempotency_store.record_if_new(idempotency_key) {
            tracing::info!("Webhook duplicate ignored (idempotency key: {idempotency_key})");
            let body = serde_json::json!({
                "status": "duplicate",
                "idempotent": true,
                "message": "Request already processed for this idempotency key"
            });
            return (StatusCode::OK, Json(body));
        }
    }

    let message = &webhook_body.message;
    let session_id = webhook_session_id(&headers);

    if state.auto_save && !memory::should_skip_autosave_content(message) {
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

    let provider_label = state
        .config
        .lock()
        .default_provider
        .clone()
        .unwrap_or_else(|| "unknown".to_string());
    let model_label = state.model.clone();
    let started_at = Instant::now();

    state
        .observer
        .record_event(&crate::observability::ObserverEvent::AgentStart {
            provider: provider_label.clone(),
            model: model_label.clone(),
        });
    state
        .observer
        .record_event(&crate::observability::ObserverEvent::LlmRequest {
            provider: provider_label.clone(),
            model: model_label.clone(),
            messages_count: 1,
        });

    match run_gateway_chat_simple(&state, message).await {
        Ok(response) => {
            let duration = started_at.elapsed();
            state
                .observer
                .record_event(&crate::observability::ObserverEvent::LlmResponse {
                    provider: provider_label.clone(),
                    model: model_label.clone(),
                    duration,
                    success: true,
                    error_message: None,
                    input_tokens: None,
                    output_tokens: None,
                    cached_input_tokens: None,
                });
            state.observer.record_metric(
                &crate::observability::traits::ObserverMetric::RequestLatency(duration),
            );
            state
                .observer
                .record_event(&crate::observability::ObserverEvent::AgentEnd {
                    provider: provider_label,
                    model: model_label,
                    duration,
                    tokens_used: None,
                    cost_usd: None,
                });

            let body = serde_json::json!({"response": response, "model": state.model});
            (StatusCode::OK, Json(body))
        }
        Err(e) => {
            let duration = started_at.elapsed();
            let sanitized = providers::sanitize_api_error(&e.to_string());

            state
                .observer
                .record_event(&crate::observability::ObserverEvent::LlmResponse {
                    provider: provider_label.clone(),
                    model: model_label.clone(),
                    duration,
                    success: false,
                    error_message: Some(sanitized.clone()),
                    input_tokens: None,
                    output_tokens: None,
                    cached_input_tokens: None,
                });
            state.observer.record_metric(
                &crate::observability::traits::ObserverMetric::RequestLatency(duration),
            );
            state
                .observer
                .record_event(&crate::observability::ObserverEvent::Error {
                    component: "gateway".to_string(),
                    message: sanitized.clone(),
                });
            state
                .observer
                .record_event(&crate::observability::ObserverEvent::AgentEnd {
                    provider: provider_label,
                    model: model_label,
                    duration,
                    tokens_used: None,
                    cost_usd: None,
                });

            tracing::error!("Webhook provider error: {}", sanitized);
            let err = serde_json::json!({"error": "LLM request failed"});
            (StatusCode::INTERNAL_SERVER_ERROR, Json(err))
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

/// POST /admin/shutdown — graceful shutdown from CLI (localhost only)
async fn handle_admin_shutdown(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
) -> Result<impl IntoResponse, (StatusCode, Json<serde_json::Value>)> {
    require_localhost(&peer)?;
    tracing::info!("🔌 Admin shutdown request received — initiating graceful shutdown");

    let body = AdminResponse {
        success: true,
        message: "Gateway shutdown initiated".to_string(),
    };

    let _ = state.shutdown_tx.send(true);

    Ok((StatusCode::OK, Json(body)))
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

/// POST /admin/paircode/new — generate a new pairing code (localhost only)
async fn handle_admin_paircode_new(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
) -> Result<impl IntoResponse, (StatusCode, Json<serde_json::Value>)> {
    require_localhost(&peer)?;
    match state.pairing.generate_new_pairing_code() {
        Some(code) => {
            tracing::info!("🔐 New pairing code generated via admin endpoint");
            let body = serde_json::json!({
                "success": true,
                "pairing_required": state.pairing.require_pairing(),
                "pairing_code": code,
                "message": "New pairing code generated — use this one-time code to pair"
            });
            Ok((StatusCode::OK, Json(body)))
        }
        None => {
            let body = serde_json::json!({
                "success": false,
                "pairing_required": false,
                "pairing_code": null,
                "message": "Pairing is disabled for this gateway"
            });
            Ok((StatusCode::BAD_REQUEST, Json(body)))
        }
    }
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
    use crate::memory::{Memory, MemoryCategory, MemoryEntry};
    use crate::providers::Provider;
    use async_trait::async_trait;
    use axum::http::HeaderValue;
    use axum::response::IntoResponse;
    use http_body_util::BodyExt;
    use parking_lot::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

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
    fn gateway_timeout_falls_back_to_default() {
        // When env var is not set, should return the default constant
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::remove_var("ZEROCLAW_GATEWAY_TIMEOUT_SECS") };
        assert_eq!(gateway_request_timeout_secs(), 30);
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
    fn app_state_is_clone() {
        fn assert_clone<T: Clone>() {}
        assert_clone::<AppState>();
    }

    #[tokio::test]
    async fn metrics_endpoint_returns_hint_when_prometheus_is_disabled() {
        let state = AppState {
            config: Arc::new(Mutex::new(Config::default())),
            provider: Arc::new(MockProvider::default()),
            model: "test-model".into(),
            temperature: 0.0,
            mem: Arc::new(MockMemory),
            auto_save: false,
            webhook_secret_hash: None,
            pairing: Arc::new(PairingGuard::new(false, &[])),
            trust_forwarded_headers: false,
            rate_limiter: Arc::new(GatewayRateLimiter::new(100, 100, 100)),
            auth_limiter: Arc::new(auth_rate_limit::AuthRateLimiter::new()),
            idempotency_store: Arc::new(IdempotencyStore::new(Duration::from_secs(300), 1000)),
            observer: Arc::new(crate::observability::NoopObserver),
            tools_registry: Arc::new(Vec::new()),
            cost_tracker: None,
            event_tx: tokio::sync::broadcast::channel(16).0,
            shutdown_tx: tokio::sync::watch::channel(false).0,
            node_registry: Arc::new(nodes::NodeRegistry::new(16)),
            path_prefix: String::new(),
            session_backend: None,
            session_queue: std::sync::Arc::new(
                crate::gateway::session_queue::SessionActorQueue::new(8, 30, 600),
            ),
            device_registry: None,
            pending_pairings: None,
            canvas_store: CanvasStore::new(),
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
        let wrapped = sse::BroadcastObserver::new(
            Box::new(crate::observability::PrometheusObserver::new()),
            event_tx.clone(),
        );
        crate::observability::Observer::record_event(
            &wrapped,
            &crate::observability::ObserverEvent::HeartbeatTick,
        );

        let observer: Arc<dyn crate::observability::Observer> = Arc::new(wrapped);
        let state = AppState {
            config: Arc::new(Mutex::new(Config::default())),
            provider: Arc::new(MockProvider::default()),
            model: "test-model".into(),
            temperature: 0.0,
            mem: Arc::new(MockMemory),
            auto_save: false,
            webhook_secret_hash: None,
            pairing: Arc::new(PairingGuard::new(false, &[])),
            trust_forwarded_headers: false,
            rate_limiter: Arc::new(GatewayRateLimiter::new(100, 100, 100)),
            auth_limiter: Arc::new(auth_rate_limit::AuthRateLimiter::new()),
            idempotency_store: Arc::new(IdempotencyStore::new(Duration::from_secs(300), 1000)),
            observer,
            tools_registry: Arc::new(Vec::new()),
            cost_tracker: None,
            event_tx,
            shutdown_tx: tokio::sync::watch::channel(false).0,
            node_registry: Arc::new(nodes::NodeRegistry::new(16)),
            path_prefix: String::new(),
            session_backend: None,
            session_queue: std::sync::Arc::new(
                crate::gateway::session_queue::SessionActorQueue::new(8, 30, 600),
            ),
            device_registry: None,
            pending_pairings: None,
            canvas_store: CanvasStore::new(),
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

        let mut config = Config::default();
        config.config_path = config_path.clone();
        config.workspace_dir = workspace_path;
        config.save().await.unwrap();

        let guard = PairingGuard::new(true, &[]);
        let code = guard.pairing_code().unwrap();
        let token = guard.try_pair(&code, "test_client").await.unwrap().unwrap();
        assert!(guard.is_authenticated(&token));

        let shared_config = Arc::new(Mutex::new(config));
        Box::pin(persist_pairing_tokens(shared_config.clone(), &guard))
            .await
            .unwrap();

        // In-memory tokens should remain as plaintext 64-char hex hashes.
        let plaintext = {
            let in_memory = shared_config.lock();
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
            crate::security::SecretStore::is_encrypted(on_disk),
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

        async fn count(&self) -> anyhow::Result<usize> {
            Ok(0)
        }

        async fn health_check(&self) -> bool {
            true
        }
    }

    #[derive(Default)]
    struct MockProvider {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl Provider for MockProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: f64,
        ) -> anyhow::Result<String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok("ok".into())
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

        async fn count(&self) -> anyhow::Result<usize> {
            let size = self.keys.lock().len();
            Ok(size)
        }

        async fn health_check(&self) -> bool {
            true
        }
    }

    fn test_connect_info() -> ConnectInfo<SocketAddr> {
        ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 30_300)))
    }

    #[tokio::test]
    async fn webhook_idempotency_skips_duplicate_provider_calls() {
        let provider_impl = Arc::new(MockProvider::default());
        let provider: Arc<dyn Provider> = provider_impl.clone();
        let memory: Arc<dyn Memory> = Arc::new(MockMemory);

        let state = AppState {
            config: Arc::new(Mutex::new(Config::default())),
            provider,
            model: "test-model".into(),
            temperature: 0.0,
            mem: memory,
            auto_save: false,
            webhook_secret_hash: None,
            pairing: Arc::new(PairingGuard::new(false, &[])),
            trust_forwarded_headers: false,
            rate_limiter: Arc::new(GatewayRateLimiter::new(100, 100, 100)),
            auth_limiter: Arc::new(auth_rate_limit::AuthRateLimiter::new()),
            idempotency_store: Arc::new(IdempotencyStore::new(Duration::from_secs(300), 1000)),
            observer: Arc::new(crate::observability::NoopObserver),
            tools_registry: Arc::new(Vec::new()),
            cost_tracker: None,
            event_tx: tokio::sync::broadcast::channel(16).0,
            shutdown_tx: tokio::sync::watch::channel(false).0,
            node_registry: Arc::new(nodes::NodeRegistry::new(16)),
            path_prefix: String::new(),
            session_backend: None,
            session_queue: std::sync::Arc::new(
                crate::gateway::session_queue::SessionActorQueue::new(8, 30, 600),
            ),
            device_registry: None,
            pending_pairings: None,
            canvas_store: CanvasStore::new(),
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
            headers.clone(),
            body,
        )
        .await
        .into_response();
        assert_eq!(first.status(), StatusCode::OK);

        let body = Ok(Json(WebhookBody {
            message: "hello".into(),
        }));
        let second = handle_webhook(State(state), test_connect_info(), headers, body)
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
    async fn webhook_autosave_stores_distinct_keys_per_request() {
        let provider_impl = Arc::new(MockProvider::default());
        let provider: Arc<dyn Provider> = provider_impl.clone();

        let tracking_impl = Arc::new(TrackingMemory::default());
        let memory: Arc<dyn Memory> = tracking_impl.clone();

        let state = AppState {
            config: Arc::new(Mutex::new(Config::default())),
            provider,
            model: "test-model".into(),
            temperature: 0.0,
            mem: memory,
            auto_save: true,
            webhook_secret_hash: None,
            pairing: Arc::new(PairingGuard::new(false, &[])),
            trust_forwarded_headers: false,
            rate_limiter: Arc::new(GatewayRateLimiter::new(100, 100, 100)),
            auth_limiter: Arc::new(auth_rate_limit::AuthRateLimiter::new()),
            idempotency_store: Arc::new(IdempotencyStore::new(Duration::from_secs(300), 1000)),
            observer: Arc::new(crate::observability::NoopObserver),
            tools_registry: Arc::new(Vec::new()),
            cost_tracker: None,
            event_tx: tokio::sync::broadcast::channel(16).0,
            shutdown_tx: tokio::sync::watch::channel(false).0,
            node_registry: Arc::new(nodes::NodeRegistry::new(16)),
            path_prefix: String::new(),
            session_backend: None,
            session_queue: std::sync::Arc::new(
                crate::gateway::session_queue::SessionActorQueue::new(8, 30, 600),
            ),
            device_registry: None,
            pending_pairings: None,
            canvas_store: CanvasStore::new(),
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
            headers.clone(),
            body1,
        )
        .await
        .into_response();
        assert_eq!(first.status(), StatusCode::OK);

        let body2 = Ok(Json(WebhookBody {
            message: "hello two".into(),
        }));
        let second = handle_webhook(State(state), test_connect_info(), headers, body2)
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
        let provider_impl = Arc::new(MockProvider::default());
        let provider: Arc<dyn Provider> = provider_impl.clone();
        let memory: Arc<dyn Memory> = Arc::new(MockMemory);
        let secret = generate_test_secret();

        let state = AppState {
            config: Arc::new(Mutex::new(Config::default())),
            provider,
            model: "test-model".into(),
            temperature: 0.0,
            mem: memory,
            auto_save: false,
            webhook_secret_hash: Some(Arc::from(hash_webhook_secret(&secret))),
            pairing: Arc::new(PairingGuard::new(false, &[])),
            trust_forwarded_headers: false,
            rate_limiter: Arc::new(GatewayRateLimiter::new(100, 100, 100)),
            auth_limiter: Arc::new(auth_rate_limit::AuthRateLimiter::new()),
            idempotency_store: Arc::new(IdempotencyStore::new(Duration::from_secs(300), 1000)),
            observer: Arc::new(crate::observability::NoopObserver),
            tools_registry: Arc::new(Vec::new()),
            cost_tracker: None,
            event_tx: tokio::sync::broadcast::channel(16).0,
            shutdown_tx: tokio::sync::watch::channel(false).0,
            node_registry: Arc::new(nodes::NodeRegistry::new(16)),
            path_prefix: String::new(),
            session_backend: None,
            session_queue: std::sync::Arc::new(
                crate::gateway::session_queue::SessionActorQueue::new(8, 30, 600),
            ),
            device_registry: None,
            pending_pairings: None,
            canvas_store: CanvasStore::new(),
            #[cfg(feature = "webauthn")]
            webauthn: None,
        };

        let response = handle_webhook(
            State(state),
            test_connect_info(),
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
        let provider_impl = Arc::new(MockProvider::default());
        let provider: Arc<dyn Provider> = provider_impl.clone();
        let memory: Arc<dyn Memory> = Arc::new(MockMemory);
        let valid_secret = generate_test_secret();
        let wrong_secret = generate_test_secret();

        let state = AppState {
            config: Arc::new(Mutex::new(Config::default())),
            provider,
            model: "test-model".into(),
            temperature: 0.0,
            mem: memory,
            auto_save: false,
            webhook_secret_hash: Some(Arc::from(hash_webhook_secret(&valid_secret))),
            pairing: Arc::new(PairingGuard::new(false, &[])),
            trust_forwarded_headers: false,
            rate_limiter: Arc::new(GatewayRateLimiter::new(100, 100, 100)),
            auth_limiter: Arc::new(auth_rate_limit::AuthRateLimiter::new()),
            idempotency_store: Arc::new(IdempotencyStore::new(Duration::from_secs(300), 1000)),
            observer: Arc::new(crate::observability::NoopObserver),
            tools_registry: Arc::new(Vec::new()),
            cost_tracker: None,
            event_tx: tokio::sync::broadcast::channel(16).0,
            shutdown_tx: tokio::sync::watch::channel(false).0,
            node_registry: Arc::new(nodes::NodeRegistry::new(16)),
            path_prefix: String::new(),
            session_backend: None,
            session_queue: std::sync::Arc::new(
                crate::gateway::session_queue::SessionActorQueue::new(8, 30, 600),
            ),
            device_registry: None,
            pending_pairings: None,
            canvas_store: CanvasStore::new(),
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
        let provider_impl = Arc::new(MockProvider::default());
        let provider: Arc<dyn Provider> = provider_impl.clone();
        let memory: Arc<dyn Memory> = Arc::new(MockMemory);
        let secret = generate_test_secret();

        let state = AppState {
            config: Arc::new(Mutex::new(Config::default())),
            provider,
            model: "test-model".into(),
            temperature: 0.0,
            mem: memory,
            auto_save: false,
            webhook_secret_hash: Some(Arc::from(hash_webhook_secret(&secret))),
            pairing: Arc::new(PairingGuard::new(false, &[])),
            trust_forwarded_headers: false,
            rate_limiter: Arc::new(GatewayRateLimiter::new(100, 100, 100)),
            auth_limiter: Arc::new(auth_rate_limit::AuthRateLimiter::new()),
            idempotency_store: Arc::new(IdempotencyStore::new(Duration::from_secs(300), 1000)),
            observer: Arc::new(crate::observability::NoopObserver),
            tools_registry: Arc::new(Vec::new()),
            cost_tracker: None,
            event_tx: tokio::sync::broadcast::channel(16).0,
            shutdown_tx: tokio::sync::watch::channel(false).0,
            node_registry: Arc::new(nodes::NodeRegistry::new(16)),
            path_prefix: String::new(),
            session_backend: None,
            session_queue: std::sync::Arc::new(
                crate::gateway::session_queue::SessionActorQueue::new(8, 30, 600),
            ),
            device_registry: None,
            pending_pairings: None,
            canvas_store: CanvasStore::new(),
            #[cfg(feature = "webauthn")]
            webauthn: None,
        };

        let mut headers = HeaderMap::new();
        headers.insert("X-Webhook-Secret", HeaderValue::from_str(&secret).unwrap());

        let response = handle_webhook(
            State(state),
            test_connect_info(),
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
}
