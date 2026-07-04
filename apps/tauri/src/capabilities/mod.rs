//! Capability handlers — Tauri commands the agent (or the dashboard webview)
//! can invoke to act on the local machine. v1 minimal scope: a single
//! read-only capability (screenshot) and a single risky capability (AppleScript)
//! to prove the dispatch path end-to-end before the full WS NodeClient lands.

pub mod applescript;
pub mod screenshot;
