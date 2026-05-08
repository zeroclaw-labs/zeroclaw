//! Tauri commands for macOS permission management.

use serde::Serialize;
use tauri::{AppHandle, Runtime};

#[derive(Debug, Serialize)]
pub struct PermissionInfo {
    pub name: String,
    pub label: String,
    pub status: String,
}

/// Return the status of all macOS permissions the app may need.
#[tauri::command]
pub fn get_permissions_status() -> Vec<PermissionInfo> {
    #[cfg(target_os = "macos")]
    {
        use crate::macos::permissions;

        vec![
            PermissionInfo {
                name: "accessibility".into(),
                label: "Accessibility".into(),
                status: permissions::check_accessibility().into(),
            },
            PermissionInfo {
                name: "screen_recording".into(),
                label: "Screen Recording".into(),
                status: permissions::check_screen_recording().into(),
            },
            PermissionInfo {
                name: "input_monitoring".into(),
                label: "Input Monitoring".into(),
                status: permissions::check_input_monitoring().into(),
            },
            PermissionInfo {
                name: "automation".into(),
                label: "Automation (AppleScript)".into(),
                status: permissions::check_automation().into(),
            },
            PermissionInfo {
                name: "microphone".into(),
                label: "Microphone".into(),
                status: permissions::check_microphone().into(),
            },
            PermissionInfo {
                name: "camera".into(),
                label: "Camera".into(),
                status: permissions::check_camera().into(),
            },
            PermissionInfo {
                name: "speech_recognition".into(),
                label: "Speech Recognition".into(),
                status: permissions::check_speech_recognition().into(),
            },
            PermissionInfo {
                name: "notifications".into(),
                label: "Notifications".into(),
                status: permissions::check_notifications().into(),
            },
            PermissionInfo {
                name: "full_disk_access".into(),
                label: "Full Disk Access".into(),
                status: permissions::check_full_disk_access().into(),
            },
            PermissionInfo {
                name: "local_network".into(),
                label: "Local Network".into(),
                status: permissions::check_local_network().into(),
            },
        ]
    }

    #[cfg(not(target_os = "macos"))]
    {
        // On non-macOS platforms, report all as granted (not applicable).
        let names = [
            ("accessibility", "Accessibility"),
            ("screen_recording", "Screen Recording"),
            ("input_monitoring", "Input Monitoring"),
            ("automation", "Automation (AppleScript)"),
            ("microphone", "Microphone"),
            ("camera", "Camera"),
            ("speech_recognition", "Speech Recognition"),
            ("notifications", "Notifications"),
            ("full_disk_access", "Full Disk Access"),
            ("local_network", "Local Network"),
        ];
        names
            .into_iter()
            .map(|(name, label)| PermissionInfo {
                name: name.into(),
                label: label.into(),
                status: "granted".into(),
            })
            .collect()
    }
}

/// Request a specific permission or open its System Settings pane.
#[tauri::command]
pub fn request_permission<R: Runtime>(app: AppHandle<R>, name: String) -> Result<String, String> {
    let result: Result<String, String>;

    #[cfg(target_os = "macos")]
    {
        use crate::macos::permissions;

        result = match name.as_str() {
            "camera" => Ok(permissions::request_camera().into()),
            "microphone" => Ok(permissions::request_microphone().into()),
            "screen_recording" => Ok(permissions::request_screen_recording().into()),
            "input_monitoring" => Ok(permissions::request_input_monitoring().into()),
            // These permissions cannot be requested programmatically — open Settings.
            "accessibility" | "automation" | "notifications" | "speech_recognition"
            | "full_disk_access" | "local_network" => {
                permissions::open_system_settings(&name).map(|_| "open_settings".into())
            }
            _ => Err(format!("Unknown permission: {name}")),
        };
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ = name;
        result = Ok("granted".into());
    }

    // Best-effort: keep the gateway's view of capabilities fresh after any successful
    // request. The 2s wizard poll will also catch async grants from System Settings.
    if result.is_ok() {
        crate::commands::onboarding::sync_capabilities_to_gateway(&app);
    }

    result
}

/// Open a specific macOS System Settings privacy pane.
#[tauri::command]
pub fn open_privacy_settings(pane: String) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        crate::macos::permissions::open_system_settings(&pane)
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ = pane;
        Err("System Settings is only available on macOS".into())
    }
}
