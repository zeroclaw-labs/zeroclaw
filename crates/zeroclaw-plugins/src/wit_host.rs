//! Host capability seams provided to a sandboxed WIT tool, and the deny-by-default
//! [`WitToolHost`] that bundles them.
//!
//! Ported from `ironclaw_wasm::host`, trimmed to the pieces zeroclaw needs: the
//! five capability traits, their fail-closed default implementations, a recording
//! HTTP fixture for tests, and the `WitToolHost` builder. The concrete production
//! implementations (reqwest egress with host-side credential injection, workspace
//! reader, secret/tool-invoke bridges) are constructed by the agent runtime and
//! installed via the `with_*` builders — they are NOT part of this crate's
//! sandbox core. This keeps the security seam (a granted capability = a non-deny
//! impl) in one place.

use std::sync::{Arc, Mutex};

use crate::wit_error::WasmHostError;

// ── HTTP ──────────────────────────────────────────────────────────────────

/// HTTP request shape passed through the WIT `host.http-request` import.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WasmHttpRequest {
    pub method: String,
    pub url: String,
    pub headers_json: String,
    pub body: Option<Vec<u8>>,
    /// Already capped by the host to the smaller of the guest-requested timeout
    /// and the remaining execution deadline.
    pub timeout_ms: Option<u32>,
}

/// HTTP response returned to a WASM guest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WasmHttpResponse {
    pub status: u16,
    pub headers_json: String,
    pub body: Vec<u8>,
}

/// Host HTTP egress seam. Production composition wires this to a reqwest-backed
/// client that enforces the SSRF allowlist and injects credentials host-side.
/// The default [`DenyWasmHostHttp`] fails closed so a guest without the HTTP
/// capability can perform no network I/O.
pub trait WasmHostHttp: Send + Sync {
    fn request(&self, request: WasmHttpRequest) -> Result<WasmHttpResponse, WasmHostError>;
}

/// Fail-closed HTTP host service (the default).
#[derive(Debug, Default)]
pub struct DenyWasmHostHttp;

impl WasmHostHttp for DenyWasmHostHttp {
    fn request(&self, _request: WasmHttpRequest) -> Result<WasmHttpResponse, WasmHostError> {
        Err(WasmHostError::Unavailable(
            "WASM HTTP egress is not configured".to_string(),
        ))
    }
}

/// Recording HTTP host service for tests and fixtures: captures every request
/// and replays a canned response.
#[derive(Debug)]
pub struct RecordingWasmHostHttp {
    requests: Mutex<Vec<WasmHttpRequest>>,
    response: Result<WasmHttpResponse, WasmHostError>,
}

impl RecordingWasmHostHttp {
    pub fn ok(response: WasmHttpResponse) -> Self {
        Self {
            requests: Mutex::new(Vec::new()),
            response: Ok(response),
        }
    }

    pub fn err(error: WasmHostError) -> Self {
        Self {
            requests: Mutex::new(Vec::new()),
            response: Err(error),
        }
    }

    pub fn requests(&self) -> Result<Vec<WasmHttpRequest>, WasmHostError> {
        self.requests
            .lock()
            .map(|requests| requests.clone())
            .map_err(|_| WasmHostError::Failed("recording HTTP request log is poisoned".into()))
    }
}

impl WasmHostHttp for RecordingWasmHostHttp {
    fn request(&self, request: WasmHttpRequest) -> Result<WasmHttpResponse, WasmHostError> {
        self.requests
            .lock()
            .map_err(|_| WasmHostError::Failed("recording HTTP request log is poisoned".into()))?
            .push(request);
        self.response.clone()
    }
}

// ── Workspace ───────────────────────────────────────────────────────────────

/// Read-only workspace file access. Implementations must reject absolute paths
/// and `..` traversal before reading.
pub trait WasmHostWorkspace: Send + Sync {
    fn read(&self, path: &str) -> Option<String>;
}

#[derive(Debug, Default)]
pub struct DenyWasmHostWorkspace;

impl WasmHostWorkspace for DenyWasmHostWorkspace {
    fn read(&self, _path: &str) -> Option<String> {
        None
    }
}

// ── Secrets ─────────────────────────────────────────────────────────────────

/// Existence-only secret checks. The guest can never read secret values; real
/// credentials are injected host-side at the HTTP boundary.
pub trait WasmHostSecrets: Send + Sync {
    fn exists(&self, name: &str) -> bool;
}

#[derive(Debug, Default)]
pub struct DenyWasmHostSecrets;

impl WasmHostSecrets for DenyWasmHostSecrets {
    fn exists(&self, _name: &str) -> bool {
        false
    }
}

// ── Tool invocation ─────────────────────────────────────────────────────────

/// Alias-indirected invocation of other agent tools from inside a plugin.
pub trait WasmHostTools: Send + Sync {
    fn invoke(&self, alias: &str, params_json: &str) -> Result<String, WasmHostError>;
}

#[derive(Debug, Default)]
pub struct DenyWasmHostTools;

impl WasmHostTools for DenyWasmHostTools {
    fn invoke(&self, _alias: &str, _params_json: &str) -> Result<String, WasmHostError> {
        Err(WasmHostError::Unavailable(
            "WASM tool invocation is not configured".to_string(),
        ))
    }
}

// ── Clock ───────────────────────────────────────────────────────────────────

/// Monotonic-ish wall clock exposed to the guest. Always granted (no secret
/// surface), defaulting to the system clock.
pub trait WasmHostClock: Send + Sync {
    fn now_millis(&self) -> u64;
}

#[derive(Debug, Default)]
pub struct SystemWasmHostClock;

impl WasmHostClock for SystemWasmHostClock {
    fn now_millis(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_millis().min(u128::from(u64::MAX)) as u64)
            .unwrap_or(0)
    }
}

// ── Bundle ──────────────────────────────────────────────────────────────────

/// The host services available to one WIT tool execution.
///
/// Start from [`WitToolHost::deny_all`] and enable only the capabilities a
/// plugin was granted via the `with_*` builders. Cheap to clone (`Arc` fields),
/// so a fresh bundle can be built per invocation.
#[derive(Clone)]
pub struct WitToolHost {
    pub(crate) http: Arc<dyn WasmHostHttp>,
    pub(crate) workspace: Arc<dyn WasmHostWorkspace>,
    pub(crate) secrets: Arc<dyn WasmHostSecrets>,
    pub(crate) tools: Arc<dyn WasmHostTools>,
    pub(crate) clock: Arc<dyn WasmHostClock>,
}

impl WitToolHost {
    /// All capabilities denied except the (harmless) system clock.
    pub fn deny_all() -> Self {
        Self {
            http: Arc::new(DenyWasmHostHttp),
            workspace: Arc::new(DenyWasmHostWorkspace),
            secrets: Arc::new(DenyWasmHostSecrets),
            tools: Arc::new(DenyWasmHostTools),
            clock: Arc::new(SystemWasmHostClock),
        }
    }

    pub fn with_http<T>(mut self, http: Arc<T>) -> Self
    where
        T: WasmHostHttp + 'static,
    {
        self.http = http;
        self
    }

    pub fn with_workspace<T>(mut self, workspace: Arc<T>) -> Self
    where
        T: WasmHostWorkspace + 'static,
    {
        self.workspace = workspace;
        self
    }

    pub fn with_secrets<T>(mut self, secrets: Arc<T>) -> Self
    where
        T: WasmHostSecrets + 'static,
    {
        self.secrets = secrets;
        self
    }

    pub fn with_tools<T>(mut self, tools: Arc<T>) -> Self
    where
        T: WasmHostTools + 'static,
    {
        self.tools = tools;
        self
    }

    pub fn with_clock<T>(mut self, clock: Arc<T>) -> Self
    where
        T: WasmHostClock + 'static,
    {
        self.clock = clock;
        self
    }
}

impl Default for WitToolHost {
    fn default() -> Self {
        Self::deny_all()
    }
}
