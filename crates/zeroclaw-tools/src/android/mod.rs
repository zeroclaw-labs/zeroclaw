//! Android UI-control tools.
//!
//! ZeroClaw running as an app on an Android phone cannot drive the screen
//! itself — an ordinary app UID is denied `screencap` and input injection. A
//! Android APK holds the `AccessibilityService` and exposes UI control over a
//! Unix-domain socket; these tools are the client half.
//!
//! The whole family is off unless `[android] enabled = true` **and** the
//! process is actually running on Android (`zeroclaw_api::platform::is_android`).
//! Registration lives in `zeroclaw-runtime`'s tool registry.
//!
//! Production builds compile this module only for `target_os = "android"`;
//! Unix-hosted crate tests retain it for protocol and rendering coverage.

pub mod action;
pub mod client;
pub mod device;
pub mod dialog;
pub mod launch;
pub mod screenshot;
pub mod ui_read;

pub use action::AndroidActionTool;
pub use client::AndroidBridgeClient;
pub use device::AndroidDeviceTool;
pub use dialog::AndroidDialogTool;
pub use launch::AndroidLaunchTool;
pub use screenshot::AndroidScreenshotTool;
pub use ui_read::AndroidUiReadTool;

pub(crate) fn tool_msg(key: &str) -> String {
    crate::i18n::get_required_tool_string(key)
}

pub(crate) fn tool_msg_with_args(key: &str, args: &[(&str, &str)]) -> String {
    crate::i18n::get_required_tool_string_with_args(key, args)
}
