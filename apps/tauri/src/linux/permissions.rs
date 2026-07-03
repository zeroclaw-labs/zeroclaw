//! Linux permission probes.
//!
//! Linux screenshot capture is not yet implemented in the Tauri capability
//! layer, so we must not advertise `screen_recording` as granted.

pub fn check_screen_recording() -> &'static str {
    "denied"
}

pub fn request_screen_recording() -> &'static str {
    "denied"
}

pub fn check_notifications() -> &'static str {
    // Linux desktop notifications do not use a centralized user privacy gate.
    "granted"
}

pub fn request_notifications() -> &'static str {
    "granted"
}
