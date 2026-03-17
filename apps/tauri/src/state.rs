//! Shared application state for Tauri.

use std::sync::Mutex;

/// Shared application state.
pub struct AppState {
    /// Gateway URL
    pub gateway_url: Mutex<String>,
    /// Bearer token for authentication
    pub token: Mutex<Option<String>>,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            gateway_url: Mutex::new("http://127.0.0.1:42617".to_string()),
            token: Mutex::new(None),
        }
    }
}
