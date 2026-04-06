//! Native macOS permission checks via FFI.
//!
//! Each function returns one of: `"granted"`, `"denied"`, or `"not_determined"`.
//! These map directly to the macOS authorization status enums.

use std::process::Command;

// ── Accessibility ──────────────────────────────────────────────────────────

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrusted() -> bool;
}

/// Check if the app has Accessibility permission.
pub fn check_accessibility() -> &'static str {
    // Safety: AXIsProcessTrusted is a simple boolean query with no side effects.
    let trusted = unsafe { AXIsProcessTrusted() };
    if trusted { "granted" } else { "denied" }
}

// ── Screen Recording ───────────────────────────────────────────────────────

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGPreflightScreenCaptureAccess() -> bool;
    fn CGRequestScreenCaptureAccess() -> bool;
}

/// Check if the app has Screen Recording permission (macOS 10.15+).
pub fn check_screen_recording() -> &'static str {
    // Safety: CGPreflightScreenCaptureAccess is a read-only permission query.
    let granted = unsafe { CGPreflightScreenCaptureAccess() };
    if granted { "granted" } else { "denied" }
}

/// Request Screen Recording permission. Returns the new status.
pub fn request_screen_recording() -> &'static str {
    // Safety: CGRequestScreenCaptureAccess triggers a system dialog if not yet determined.
    let granted = unsafe { CGRequestScreenCaptureAccess() };
    if granted { "granted" } else { "denied" }
}

// ── Camera & Microphone (AVFoundation) ─────────────────────────────────────
//
// AVCaptureDevice.authorizationStatus(for:) returns an enum:
//   0 = notDetermined, 1 = restricted, 2 = denied, 3 = authorized
//
// We use a small inline AppleScript/ObjC bridge via osascript because linking
// AVFoundation from pure Rust FFI requires significant boilerplate.  The
// osascript approach is reliable for permission *checks* and keeps the binary
// lean.

fn check_av_permission(media_type: &str) -> &'static str {
    // Use swift CLI to check AVCaptureDevice authorization status.
    let script = format!(
        r#"import AVFoundation; print(AVCaptureDevice.authorizationStatus(for: .{}).rawValue)"#,
        media_type
    );
    let output = Command::new("swift").args(["-e", &script]).output();

    match output {
        Ok(out) => {
            let status = String::from_utf8_lossy(&out.stdout).trim().to_string();
            match status.as_str() {
                "3" => "granted",
                "0" => "not_determined",
                _ => "denied", // 1 (restricted) and 2 (denied)
            }
        }
        Err(_) => "not_determined",
    }
}

/// Check Camera permission status.
pub fn check_camera() -> &'static str {
    check_av_permission("video")
}

/// Check Microphone permission status.
pub fn check_microphone() -> &'static str {
    check_av_permission("audio")
}

/// Request Camera permission. This triggers the system dialog.
pub fn request_camera() -> &'static str {
    let script = r#"
import AVFoundation
import Darwin
let sem = DispatchSemaphore(value: 0)
var result = 0
AVCaptureDevice.requestAccess(for: .video) { granted in
    result = granted ? 3 : 2
    sem.signal()
}
sem.wait()
print(result)
"#;
    let output = Command::new("swift").args(["-e", script]).output();
    match output {
        Ok(out) => {
            let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if s == "3" { "granted" } else { "denied" }
        }
        Err(_) => "denied",
    }
}

/// Request Microphone permission. This triggers the system dialog.
pub fn request_microphone() -> &'static str {
    let script = r#"
import AVFoundation
import Darwin
let sem = DispatchSemaphore(value: 0)
var result = 0
AVCaptureDevice.requestAccess(for: .audio) { granted in
    result = granted ? 3 : 2
    sem.signal()
}
sem.wait()
print(result)
"#;
    let output = Command::new("swift").args(["-e", script]).output();
    match output {
        Ok(out) => {
            let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if s == "3" { "granted" } else { "denied" }
        }
        Err(_) => "denied",
    }
}

// ── Notifications ──────────────────────────────────────────────────────────
//
// UNUserNotificationCenter authorization status:
//   0 = notDetermined, 1 = denied, 2 = authorized, 3 = provisional, 4 = ephemeral

pub fn check_notifications() -> &'static str {
    let script = r#"
import UserNotifications
import Darwin
let sem = DispatchSemaphore(value: 0)
var result = 0
UNUserNotificationCenter.current().getNotificationSettings { settings in
    result = settings.authorizationStatus.rawValue
    sem.signal()
}
sem.wait()
print(result)
"#;
    let output = Command::new("swift").args(["-e", script]).output();
    match output {
        Ok(out) => {
            let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
            match s.as_str() {
                "2" | "3" | "4" => "granted",
                "0" => "not_determined",
                _ => "denied",
            }
        }
        Err(_) => "not_determined",
    }
}

// ── Speech Recognition ─────────────────────────────────────────────────────
//
// SFSpeechRecognizer.authorizationStatus():
//   0 = notDetermined, 1 = denied, 2 = restricted, 3 = authorized

pub fn check_speech_recognition() -> &'static str {
    let script = r#"import Speech; print(SFSpeechRecognizer.authorizationStatus().rawValue)"#;
    let output = Command::new("swift").args(["-e", &script]).output();
    match output {
        Ok(out) => {
            let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
            match s.as_str() {
                "3" => "granted",
                "0" => "not_determined",
                _ => "denied",
            }
        }
        Err(_) => "not_determined",
    }
}

// ── Automation (AppleScript) ───────────────────────────────────────────────
//
// Automation permission is checked per-target-app.  A general check tests
// against System Events (the most common automation target).

pub fn check_automation() -> &'static str {
    let output = Command::new("osascript")
        .args([
            "-e",
            r#"tell application "System Events" to return name of first process"#,
        ])
        .output();

    match output {
        Ok(out) if out.status.success() => "granted",
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            if stderr.contains("not allowed") || stderr.contains("-1743") {
                "denied"
            } else {
                // Could be first run — macOS may prompt
                "not_determined"
            }
        }
        Err(_) => "not_determined",
    }
}

// ── System Settings Launcher ───────────────────────────────────────────────

/// Open a specific pane in macOS System Settings (Ventura+) or System Preferences.
pub fn open_system_settings(pane: &str) -> Result<(), String> {
    let url = match pane {
        "accessibility" => {
            "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility"
        }
        "screen_recording" => {
            "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture"
        }
        "camera" => "x-apple.systempreferences:com.apple.preference.security?Privacy_Camera",
        "microphone" => {
            "x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone"
        }
        "automation" => {
            "x-apple.systempreferences:com.apple.preference.security?Privacy_Automation"
        }
        "notifications" => {
            "x-apple.systempreferences:com.apple.preference.security?Privacy_Notifications"
        }
        "speech_recognition" => {
            "x-apple.systempreferences:com.apple.preference.security?Privacy_SpeechRecognition"
        }
        "full_disk_access" => {
            "x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles"
        }
        _ => return Err(format!("Unknown settings pane: {pane}")),
    };

    Command::new("open")
        .arg(url)
        .status()
        .map(|_| ())
        .map_err(|e| format!("Failed to open System Settings: {e}"))
}
