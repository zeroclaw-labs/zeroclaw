//! Windows permission probes.
//!
//! We surface privacy state from CapabilityAccessManager and admin status for
//! input monitoring style hooks.

use std::process::Command;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

#[derive(Clone)]
struct WindowsPrivacySnapshot {
    microphone: String,
    camera: String,
    admin: bool,
}

const SNAPSHOT_TTL: Duration = Duration::from_secs(1);
static SNAPSHOT_CACHE: OnceLock<Mutex<Option<(Instant, WindowsPrivacySnapshot)>>> = OnceLock::new();

fn execute_powershell_script(script: &str) -> Option<String> {
    let out = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn map_consent(value: &str) -> &'static str {
    if value.eq_ignore_ascii_case("allow") {
        "granted"
    } else if value.eq_ignore_ascii_case("deny") {
        "denied"
    } else {
        "not_determined"
    }
}

fn query_snapshot() -> WindowsPrivacySnapshot {
    let script = r#"
$probe = {
  param([string]$cap)
  $paths = @(
    "HKCU:\Software\Microsoft\Windows\CurrentVersion\CapabilityAccessManager\ConsentStore\$cap",
    "HKLM:\Software\Microsoft\Windows\CurrentVersion\CapabilityAccessManager\ConsentStore\$cap"
  )
  foreach ($p in $paths) {
    if (Test-Path $p) {
      $v = (Get-ItemProperty -Path $p -Name Value -ErrorAction SilentlyContinue).Value
      if ($v) { return $v }
    }
  }
  return ""
}
$principal = New-Object Security.Principal.WindowsPrincipal([Security.Principal.WindowsIdentity]::GetCurrent())
$admin = $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
[PSCustomObject]@{
  microphone = (& $probe "microphone")
  camera = (& $probe "webcam")
  admin = $admin
} | ConvertTo-Json -Compress
"#;
    if let Some(json) = execute_powershell_script(script)
        && let Ok(value) = serde_json::from_str::<serde_json::Value>(&json)
    {
        return WindowsPrivacySnapshot {
            microphone: value
                .get("microphone")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            camera: value
                .get("camera")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            admin: value
                .get("admin")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
        };
    }

    WindowsPrivacySnapshot {
        microphone: String::new(),
        camera: String::new(),
        admin: false,
    }
}

fn snapshot() -> WindowsPrivacySnapshot {
    let cache = SNAPSHOT_CACHE.get_or_init(|| Mutex::new(None));
    let mut guard = match cache.lock() {
        Ok(g) => g,
        Err(_) => return query_snapshot(),
    };

    if let Some((ts, snap)) = guard.as_ref()
        && ts.elapsed() < SNAPSHOT_TTL
    {
        return snap.clone();
    }

    let fresh = query_snapshot();
    *guard = Some((Instant::now(), fresh.clone()));
    fresh
}

fn open_settings(uri: &str) -> Result<(), String> {
    // Empty string is the console-window title parameter required by `start`
    // before URL arguments.
    let status = Command::new("cmd")
        // `start` syntax: start "<title>" <target>; we pass an empty title.
        .args(["/C", "start", "", uri])
        .status()
        .map_err(|e| format!("failed to open settings URI {uri}: {e}"))?;
    if !status.success() {
        return Err(format!(
            "settings command for {uri} exited with code {:?}",
            status.code()
        ));
    }
    Ok(())
}

pub fn check_microphone() -> &'static str {
    map_consent(&snapshot().microphone)
}

pub fn check_camera() -> &'static str {
    map_consent(&snapshot().camera)
}

pub fn check_input_monitoring() -> &'static str {
    // Desktop-wide input hooks on Windows typically require elevated privileges.
    if snapshot().admin {
        "granted"
    } else {
        "denied"
    }
}

pub fn check_notifications() -> &'static str {
    // Action Center availability is not privacy-gated per app for this desktop flow.
    "granted"
}

pub fn request_microphone() -> &'static str {
    let _ = open_settings("ms-settings:privacy-microphone");
    check_microphone()
}

pub fn request_camera() -> &'static str {
    let _ = open_settings("ms-settings:privacy-webcam");
    check_camera()
}

pub fn request_input_monitoring() -> &'static str {
    if snapshot().admin {
        "granted"
    } else {
        "denied"
    }
}

pub fn request_notifications() -> &'static str {
    "granted"
}

pub fn open_privacy_settings(pane: &str) -> Result<(), String> {
    match pane {
        "microphone" => open_settings("ms-settings:privacy-microphone"),
        "camera" => open_settings("ms-settings:privacy-webcam"),
        "notifications" => open_settings("ms-settings:notifications"),
        _ => Err(format!("Unknown Windows privacy pane: {pane}")),
    }
}
