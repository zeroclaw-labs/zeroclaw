//! Linux permission probes.
//!
//! Linux has few OS-level permission gates compared to macOS/Windows.
//! Screen capture is the main special case on Wayland, where applications
//! need xdg-desktop-portal for user-mediated capture sessions.

use std::process::Command;

fn env_true(name: &str) -> bool {
    std::env::var(name).is_ok_and(|v| !v.trim().is_empty())
}

pub fn is_wayland() -> bool {
    env_true("WAYLAND_DISPLAY")
        || std::env::var("XDG_SESSION_TYPE")
            .map(|v| v.eq_ignore_ascii_case("wayland"))
            .unwrap_or(false)
}

fn portal_available() -> bool {
    // Keep this lightweight and best-effort. If the bus probe fails, treat it
    // as unavailable rather than crashing.
    if let Ok(out) = Command::new("gdbus")
        .args([
            "call",
            "--session",
            "--dest",
            "org.freedesktop.DBus",
            "--object-path",
            "/org/freedesktop/DBus",
            "--method",
            "org.freedesktop.DBus.NameHasOwner",
            "org.freedesktop.portal.Desktop",
        ])
        .output()
    {
        return out.status.success() && String::from_utf8_lossy(&out.stdout).contains("true");
    }
    false
}

pub fn check_screen_recording() -> &'static str {
    if !is_wayland() {
        // X11 desktop capture is generally unrestricted.
        return "granted";
    }

    if portal_available() {
        // Portal-mediated capture is available; user consent occurs at capture time.
        "not_determined"
    } else {
        "denied"
    }
}

pub fn request_screen_recording() -> &'static str {
    if !is_wayland() {
        return "granted";
    }

    if portal_available() {
        // ScreenCast consent is granted during an actual capture flow.
        // Request here only confirms readiness of the portal path.
        "not_determined"
    } else {
        "denied"
    }
}

pub fn check_notifications() -> &'static str {
    // Linux desktop notifications do not use a centralized user privacy gate.
    "granted"
}

pub fn request_notifications() -> &'static str {
    "granted"
}
