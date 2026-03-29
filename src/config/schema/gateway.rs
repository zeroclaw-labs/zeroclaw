use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::default_true;

// ── Gateway security ─────────────────────────────────────────────

/// Gateway server configuration (`[gateway]` section).
///
/// Controls the HTTP gateway for webhook and pairing endpoints.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[allow(clippy::struct_excessive_bools)]
pub struct GatewayConfig {
    /// Gateway port (default: 42617)
    #[serde(default = "default_gateway_port")]
    pub port: u16,
    /// Gateway host (default: 127.0.0.1)
    #[serde(default = "default_gateway_host")]
    pub host: String,
    /// Require pairing before accepting requests (default: true)
    #[serde(default = "default_true")]
    pub require_pairing: bool,
    /// Allow binding to non-localhost without a tunnel (default: false)
    #[serde(default)]
    pub allow_public_bind: bool,
    /// Paired bearer tokens (managed automatically, not user-edited)
    #[serde(default)]
    pub paired_tokens: Vec<String>,

    /// Max `/pair` requests per minute per client key.
    #[serde(default = "default_pair_rate_limit")]
    pub pair_rate_limit_per_minute: u32,

    /// Max `/webhook` requests per minute per client key.
    #[serde(default = "default_webhook_rate_limit")]
    pub webhook_rate_limit_per_minute: u32,

    /// Trust proxy-forwarded client IP headers (`X-Forwarded-For`, `X-Real-IP`).
    /// Disabled by default; enable only behind a trusted reverse proxy.
    #[serde(default)]
    pub trust_forwarded_headers: bool,

    /// Optional URL path prefix for reverse-proxy deployments.
    /// When set, all gateway routes are served under this prefix.
    /// Must start with `/` and must not end with `/`.
    #[serde(default)]
    pub path_prefix: Option<String>,

    /// Maximum distinct client keys tracked by gateway rate limiter maps.
    #[serde(default = "default_gateway_rate_limit_max_keys")]
    pub rate_limit_max_keys: usize,

    /// TTL for webhook idempotency keys.
    #[serde(default = "default_idempotency_ttl_secs")]
    pub idempotency_ttl_secs: u64,

    /// Maximum distinct idempotency keys retained in memory.
    #[serde(default = "default_gateway_idempotency_max_keys")]
    pub idempotency_max_keys: usize,

    /// Persist gateway WebSocket chat sessions to SQLite. Default: true.
    #[serde(default = "default_true")]
    pub session_persistence: bool,

    /// Auto-archive stale gateway sessions older than N hours. 0 = disabled. Default: 0.
    #[serde(default)]
    pub session_ttl_hours: u32,

    /// Pairing dashboard configuration
    #[serde(default)]
    pub pairing_dashboard: PairingDashboardConfig,

    /// TLS configuration for the gateway server (`[gateway.tls]`).
    #[serde(default)]
    pub tls: Option<GatewayTlsConfig>,
}

fn default_gateway_port() -> u16 {
    42617
}

fn default_gateway_host() -> String {
    "127.0.0.1".into()
}

fn default_pair_rate_limit() -> u32 {
    10
}

fn default_webhook_rate_limit() -> u32 {
    60
}

fn default_idempotency_ttl_secs() -> u64 {
    300
}

fn default_gateway_rate_limit_max_keys() -> usize {
    10_000
}

fn default_gateway_idempotency_max_keys() -> usize {
    10_000
}

impl Default for GatewayConfig {
    fn default() -> Self {
        Self {
            port: default_gateway_port(),
            host: default_gateway_host(),
            require_pairing: true,
            allow_public_bind: false,
            paired_tokens: Vec::new(),
            pair_rate_limit_per_minute: default_pair_rate_limit(),
            webhook_rate_limit_per_minute: default_webhook_rate_limit(),
            trust_forwarded_headers: false,
            path_prefix: None,
            rate_limit_max_keys: default_gateway_rate_limit_max_keys(),
            idempotency_ttl_secs: default_idempotency_ttl_secs(),
            idempotency_max_keys: default_gateway_idempotency_max_keys(),
            session_persistence: true,
            session_ttl_hours: 0,
            pairing_dashboard: PairingDashboardConfig::default(),
            tls: None,
        }
    }
}

/// Pairing dashboard configuration (`[gateway.pairing_dashboard]`).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PairingDashboardConfig {
    /// Length of pairing codes (default: 8)
    #[serde(default = "default_pairing_code_length")]
    pub code_length: usize,
    /// Time-to-live for pending pairing codes in seconds (default: 3600)
    #[serde(default = "default_pairing_ttl")]
    pub code_ttl_secs: u64,
    /// Maximum concurrent pending pairing codes (default: 3)
    #[serde(default = "default_max_pending_codes")]
    pub max_pending_codes: usize,
    /// Maximum failed pairing attempts before lockout (default: 5)
    #[serde(default = "default_max_failed_attempts")]
    pub max_failed_attempts: u32,
    /// Lockout duration in seconds after max attempts (default: 300)
    #[serde(default = "default_pairing_lockout_secs")]
    pub lockout_secs: u64,
}

fn default_pairing_code_length() -> usize {
    8
}
fn default_pairing_ttl() -> u64 {
    3600
}
fn default_max_pending_codes() -> usize {
    3
}
fn default_max_failed_attempts() -> u32 {
    5
}
fn default_pairing_lockout_secs() -> u64 {
    300
}

impl Default for PairingDashboardConfig {
    fn default() -> Self {
        Self {
            code_length: default_pairing_code_length(),
            code_ttl_secs: default_pairing_ttl(),
            max_pending_codes: default_max_pending_codes(),
            max_failed_attempts: default_max_failed_attempts(),
            lockout_secs: default_pairing_lockout_secs(),
        }
    }
}

/// TLS configuration for the gateway server (`[gateway.tls]`).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct GatewayTlsConfig {
    /// Enable TLS for the gateway (default: false).
    #[serde(default)]
    pub enabled: bool,
    /// Path to the PEM-encoded server certificate file.
    pub cert_path: String,
    /// Path to the PEM-encoded server private key file.
    pub key_path: String,
    /// Client certificate authentication (mutual TLS) settings.
    #[serde(default)]
    pub client_auth: Option<GatewayClientAuthConfig>,
}

/// Client certificate authentication (mTLS) configuration (`[gateway.tls.client_auth]`).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct GatewayClientAuthConfig {
    /// Enable client certificate verification (default: false).
    #[serde(default)]
    pub enabled: bool,
    /// Path to the PEM-encoded CA certificate used to verify client certs.
    pub ca_cert_path: String,
    /// Reject connections that do not present a valid client certificate (default: true).
    #[serde(default = "default_true")]
    pub require_client_cert: bool,
    /// Optional SHA-256 fingerprints for certificate pinning.
    /// When non-empty, only client certs matching one of these fingerprints are accepted.
    #[serde(default)]
    pub pinned_certs: Vec<String>,
}

/// Secure transport configuration for inter-node communication (`[node_transport]`).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct NodeTransportConfig {
    /// Enable the secure transport layer.
    #[serde(default = "default_node_transport_enabled")]
    pub enabled: bool,
    /// Shared secret for HMAC authentication between nodes.
    #[serde(default)]
    pub shared_secret: String,
    /// Maximum age of signed requests in seconds (replay protection).
    #[serde(default = "default_max_request_age")]
    pub max_request_age_secs: i64,
    /// Require HTTPS for all node communication.
    #[serde(default = "default_require_https")]
    pub require_https: bool,
    /// Allow specific node IPs/CIDRs.
    #[serde(default)]
    pub allowed_peers: Vec<String>,
    /// Path to TLS certificate file.
    #[serde(default)]
    pub tls_cert_path: Option<String>,
    /// Path to TLS private key file.
    #[serde(default)]
    pub tls_key_path: Option<String>,
    /// Require client certificates (mutual TLS).
    #[serde(default)]
    pub mutual_tls: bool,
    /// Maximum number of connections per peer.
    #[serde(default = "default_connection_pool_size")]
    pub connection_pool_size: usize,
}

fn default_node_transport_enabled() -> bool {
    true
}
fn default_max_request_age() -> i64 {
    300
}
fn default_require_https() -> bool {
    true
}
fn default_connection_pool_size() -> usize {
    4
}

impl Default for NodeTransportConfig {
    fn default() -> Self {
        Self {
            enabled: default_node_transport_enabled(),
            shared_secret: String::new(),
            max_request_age_secs: default_max_request_age(),
            require_https: default_require_https(),
            allowed_peers: Vec::new(),
            tls_cert_path: None,
            tls_key_path: None,
            mutual_tls: false,
            connection_pool_size: default_connection_pool_size(),
        }
    }
}
