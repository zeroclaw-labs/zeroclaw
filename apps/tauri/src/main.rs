//! ZeroClaw Desktop — main entry point.
//!
//! Prevents an additional console window on Windows in release.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // Install default crypto provider for Rustls TLS.
    // The desktop app uses reqwest's `rustls-tls-webpki-roots-no-provider`
    // feature, which requires selecting a process-wide provider explicitly
    // before any HTTP client is constructed.
    if let Err(e) = rustls::crypto::ring::default_provider().install_default() {
        eprintln!("Warning: Failed to install default crypto provider: {e:?}");
    }
    zeroclaw_desktop::run();
}
