//! Windows permission probes.
//!
//! We surface privacy state from CapabilityAccessManager and admin status for
//! input monitoring style hooks.

use std::process::Command;

fn powershell(script: &str) -> Option<String> {
    let out = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn read_consent_store(capability: &str) -> &'static str {
    let script = format!(
        r#"
$paths = @(
  "HKCU:\Software\Microsoft\Windows\CurrentVersion\CapabilityAccessManager\ConsentStore\{cap}",
  "HKLM:\Software\Microsoft\Windows\CurrentVersion\CapabilityAccessManager\ConsentStore\{cap}"
)
foreach ($p in $paths) {{
  if (Test-Path $p) {{
    $v = (Get-ItemProperty -Path $p -Name Value -ErrorAction SilentlyContinue).Value
    if ($v) {{ Write-Output $v; break }}
  }}
}}
"#,
        cap = capability
    );

    match powershell(&script) {
        Some(value) if value.eq_ignore_ascii_case("allow") => "granted",
        Some(value) if value.eq_ignore_ascii_case("deny") => "denied",
        _ => "not_determined",
    }
}

fn is_admin() -> bool {
    let script = r#"
$principal = New-Object Security.Principal.WindowsPrincipal([Security.Principal.WindowsIdentity]::GetCurrent())
if ($principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) { "true" } else { "false" }
"#;
    matches!(powershell(script).as_deref(), Some("true"))
}

fn open_settings(uri: &str) -> Result<(), String> {
    Command::new("cmd")
        .args(["/C", "start", "", uri])
        .status()
        .map_err(|e| format!("failed to open settings URI {uri}: {e}"))?;
    Ok(())
}

pub fn check_microphone() -> &'static str {
    read_consent_store("microphone")
}

pub fn check_camera() -> &'static str {
    read_consent_store("webcam")
}

pub fn check_input_monitoring() -> &'static str {
    if is_admin() { "granted" } else { "denied" }
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
    if is_admin() {
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
