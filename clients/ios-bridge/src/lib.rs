//! ZeroClaw iOS Bridge — C-FFI for Swift interop
//!
//! This crate provides a C-compatible API for ZeroClaw on iOS.
//! iOS cannot launch sidecar processes, so ZeroClaw runs in-process
//! as a static library linked into the MoA app.
//!
//! ## Architecture
//! ```text
//! MoA iOS App (SwiftUI)
//!   └── Tauri 2 WKWebView (frontend)
//!   └── zeroclaw_ios.a (this library)
//!         ├── start_gateway()   → spawns local HTTP gateway
//!         ├── send_message()    → POST /webhook to local gateway
//!         ├── get_status()      → health check
//!         └── stop()            → graceful shutdown
//! ```
//!
//! ## Swift Usage (via Bridging Header)
//! ```swift
//! import ZeroClawBridge
//! zeroclaw_start("~/.zeroclaw", "gemini", "your-api-key", 3000)
//! let response = zeroclaw_send_message("Hello")
//! zeroclaw_stop()
//! ```

use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::sync::{Arc, Mutex, OnceLock};
use tokio::runtime::Runtime;
use tokio::sync::watch;

/// Global runtime for async operations.
static RUNTIME: OnceLock<Runtime> = OnceLock::new();

fn runtime() -> &'static Runtime {
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("Failed to create Tokio runtime")
    })
}

/// Internal state for the bridge.
struct BridgeState {
    gateway_url: String,
    token: Option<String>,
    running: bool,
    /// Shutdown signal sender — dropping or sending triggers graceful stop.
    shutdown_tx: Option<watch::Sender<bool>>,
}

static STATE: OnceLock<Arc<Mutex<BridgeState>>> = OnceLock::new();

fn state() -> &'static Arc<Mutex<BridgeState>> {
    STATE.get_or_init(|| {
        Arc::new(Mutex::new(BridgeState {
            gateway_url: "http://127.0.0.1:3000".to_string(),
            token: None,
            running: false,
            shutdown_tx: None,
        }))
    })
}

// ── C-FFI Functions ──────────────────────────────────────────────

/// Start the ZeroClaw gateway in-process.
///
/// # Parameters
/// - `data_dir`: Path to the data directory (e.g. app sandbox Documents path)
/// - `provider`: LLM provider name (e.g. "gemini", "openai", "anthropic")
/// - `api_key`: API key for the provider (can be NULL if using operator key)
/// - `port`: Gateway port (default: 3000)
///
/// # Returns
/// 0 on success, -1 on error.
///
/// # Safety
/// Caller must ensure `data_dir` and `provider` are valid C strings.
#[no_mangle]
pub unsafe extern "C" fn zeroclaw_start(
    data_dir: *const c_char,
    provider: *const c_char,
    api_key: *const c_char,
    port: u16,
) -> i32 {
    if data_dir.is_null() || provider.is_null() {
        return -1;
    }

    let data_dir_str = match unsafe { CStr::from_ptr(data_dir) }.to_str() {
        Ok(s) => s.to_string(),
        Err(_) => return -1,
    };
    let provider_str = match unsafe { CStr::from_ptr(provider) }.to_str() {
        Ok(s) => s.to_string(),
        Err(_) => return -1,
    };
    let api_key_str = if api_key.is_null() {
        None
    } else {
        unsafe { CStr::from_ptr(api_key) }
            .to_str()
            .ok()
            .map(String::from)
    };

    let gateway_port = if port == 0 { 3000 } else { port };
    let gateway_url = format!("http://127.0.0.1:{gateway_port}");

    // Build a ZeroClaw config for in-process gateway mode.
    let mut config = zeroclaw::config::Config::default();

    // Set workspace to the iOS app sandbox data directory.
    config.workspace_dir = std::path::PathBuf::from(&data_dir_str);

    // Configure the provider with the given API key.
    config.default_provider = Some(provider_str.clone());

    // Set the API key via environment (providers read from env).
    if let Some(ref key) = api_key_str {
        let env_var = match provider_str.as_str() {
            "openai" => "OPENAI_API_KEY",
            "anthropic" => "ANTHROPIC_API_KEY",
            "gemini" | "google" => "GEMINI_API_KEY",
            _ => "OPENAI_API_KEY",
        };
        std::env::set_var(env_var, key);
    }

    // Create shutdown signal.
    let (shutdown_tx, mut shutdown_rx) = watch::channel(false);

    // Store state.
    {
        let mut s = state().lock().unwrap();
        if s.running {
            // Already running — return success.
            return 0;
        }
        s.gateway_url = gateway_url;
        s.running = true;
        s.shutdown_tx = Some(shutdown_tx);
    }

    // Spawn the gateway on the Tokio runtime (non-blocking).
    let host = "127.0.0.1".to_string();
    runtime().spawn(async move {
        tokio::select! {
            result = zeroclaw::gateway::run_gateway(&host, gateway_port, config) => {
                if let Err(e) = result {
                    tracing::error!("Gateway exited with error: {e}");
                }
            }
            _ = async {
                while shutdown_rx.changed().await.is_ok() {
                    if *shutdown_rx.borrow() {
                        break;
                    }
                }
            } => {
                tracing::info!("Gateway shutdown signal received");
            }
        }

        // Mark as stopped.
        if let Ok(mut s) = state().lock() {
            s.running = false;
            s.shutdown_tx = None;
        }
    });

    0
}

/// Send a message to the ZeroClaw agent and get a response.
///
/// # Parameters
/// - `message`: The user message (C string).
///
/// # Returns
/// A newly allocated C string with the response. Caller must free with `zeroclaw_free_string`.
/// Returns NULL on error.
///
/// # Safety
/// Caller must ensure `message` is a valid C string.
#[no_mangle]
pub unsafe extern "C" fn zeroclaw_send_message(message: *const c_char) -> *mut c_char {
    if message.is_null() {
        return std::ptr::null_mut();
    }

    let msg = match unsafe { CStr::from_ptr(message) }.to_str() {
        Ok(s) => s.to_string(),
        Err(_) => return std::ptr::null_mut(),
    };

    let (gateway_url, token) = {
        let s = state().lock().unwrap();
        if !s.running {
            return std::ptr::null_mut();
        }
        (s.gateway_url.clone(), s.token.clone())
    };

    let response = runtime().block_on(async {
        let client = reqwest::Client::new();
        let mut req = client
            .post(format!("{gateway_url}/webhook"))
            .header("Content-Type", "application/json")
            .json(&serde_json::json!({"message": msg}));

        if let Some(ref tok) = token {
            req = req.header("Authorization", format!("Bearer {tok}"));
        }

        match req.send().await {
            Ok(res) => {
                if let Ok(body) = res.json::<serde_json::Value>().await {
                    body["response"].as_str().unwrap_or("").to_string()
                } else {
                    String::new()
                }
            }
            Err(e) => format!("Error: {e}"),
        }
    });

    match CString::new(response) {
        Ok(cs) => cs.into_raw(),
        Err(_) => std::ptr::null_mut(),
    }
}

/// Get the current status of the ZeroClaw agent.
///
/// # Returns
/// 1 if running, 0 if stopped, -1 on error.
#[no_mangle]
pub extern "C" fn zeroclaw_get_status() -> i32 {
    let s = state().lock().unwrap();
    if s.running {
        // Health check the gateway
        let url = s.gateway_url.clone();
        drop(s);
        let healthy = runtime().block_on(async {
            reqwest::get(format!("{url}/health"))
                .await
                .is_ok_and(|r| r.status().is_success())
        });
        if healthy {
            1
        } else {
            0
        }
    } else {
        0
    }
}

/// Stop the ZeroClaw gateway.
#[no_mangle]
pub extern "C" fn zeroclaw_stop() {
    let mut s = state().lock().unwrap();
    s.running = false;
    // Send shutdown signal to the gateway task.
    if let Some(tx) = s.shutdown_tx.take() {
        let _ = tx.send(true);
    }
}

/// Free a string returned by zeroclaw_send_message.
///
/// # Safety
/// Caller must only pass pointers returned by `zeroclaw_send_message`.
#[no_mangle]
pub unsafe extern "C" fn zeroclaw_free_string(ptr: *mut c_char) {
    if !ptr.is_null() {
        let _ = unsafe { CString::from_raw(ptr) };
    }
}

/// Set the authentication token for gateway communication.
///
/// # Safety
/// Caller must ensure `token` is a valid C string.
#[no_mangle]
pub unsafe extern "C" fn zeroclaw_set_token(token: *const c_char) {
    if token.is_null() {
        let mut s = state().lock().unwrap();
        s.token = None;
        return;
    }

    if let Ok(tok) = unsafe { CStr::from_ptr(token) }.to_str() {
        let mut s = state().lock().unwrap();
        s.token = Some(tok.to_string());
    }
}
