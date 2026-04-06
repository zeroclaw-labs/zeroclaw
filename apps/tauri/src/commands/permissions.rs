//! Tauri commands for macOS permission management.

use serde::Serialize;

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
                name: "camera".into(),
                label: "Camera".into(),
                status: permissions::check_camera().into(),
            },
            PermissionInfo {
                name: "microphone".into(),
                label: "Microphone".into(),
                status: permissions::check_microphone().into(),
            },
            PermissionInfo {
                name: "automation".into(),
                label: "Automation (AppleScript)".into(),
                status: permissions::check_automation().into(),
            },
            PermissionInfo {
                name: "notifications".into(),
                label: "Notifications".into(),
                status: permissions::check_notifications().into(),
            },
            PermissionInfo {
                name: "speech_recognition".into(),
                label: "Speech Recognition".into(),
                status: permissions::check_speech_recognition().into(),
            },
        ]
    }

    #[cfg(not(target_os = "macos"))]
    {
        // On non-macOS platforms, report all as granted (not applicable).
        let names = [
            ("accessibility", "Accessibility"),
            ("screen_recording", "Screen Recording"),
            ("camera", "Camera"),
            ("microphone", "Microphone"),
            ("automation", "Automation (AppleScript)"),
            ("notifications", "Notifications"),
            ("speech_recognition", "Speech Recognition"),
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
pub fn request_permission(name: String) -> Result<String, String> {
    #[cfg(target_os = "macos")]
    {
        use crate::macos::permissions;

        match name.as_str() {
            "camera" => Ok(permissions::request_camera().into()),
            "microphone" => Ok(permissions::request_microphone().into()),
            "screen_recording" => Ok(permissions::request_screen_recording().into()),
            // These permissions cannot be requested programmatically — open Settings.
            "accessibility" | "automation" | "notifications" | "speech_recognition" => {
                permissions::open_system_settings(&name)?;
                Ok("open_settings".into())
            }
            _ => Err(format!("Unknown permission: {name}")),
        }
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ = name;
        Ok("granted".into())
    }
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
