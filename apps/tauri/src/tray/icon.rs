//! Tray icon management — swap icon based on connection/agent status.

use crate::state::AgentStatus;
use tauri::image::Image;

/// Embedded tray icon PNGs (22x22, RGBA).
const ICON_IDLE: &[u8] = include_bytes!("../../icons/tray-idle.png");
const ICON_WORKING: &[u8] = include_bytes!("../../icons/tray-working.png");
const ICON_ERROR: &[u8] = include_bytes!("../../icons/tray-error.png");
const ICON_DISCONNECTED: &[u8] = include_bytes!("../../icons/tray-disconnected.png");

/// Select the appropriate tray icon for the current state.
pub fn icon_for_state(connected: bool, status: AgentStatus) -> Image<'static> {
    let bytes: &[u8] = if !connected {
        ICON_DISCONNECTED
    } else {
        match status {
            AgentStatus::Idle => ICON_IDLE,
            AgentStatus::Working => ICON_WORKING,
            AgentStatus::Error => ICON_ERROR,
        }
    };
    Image::from_bytes(bytes).expect("embedded tray icon is a valid PNG")
}

/// Tooltip text for the current state.
pub fn tooltip_for_state(connected: bool, status: AgentStatus) -> &'static str {
    if !connected {
        return "ZeroClaw — Disconnected";
    }
    match status {
        AgentStatus::Idle => "ZeroClaw — Idle",
        AgentStatus::Working => "ZeroClaw — Working",
        AgentStatus::Error => "ZeroClaw — Error",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icon_disconnected_when_not_connected() {
        // Should not panic — icon bytes are valid PNGs.
        let _img = icon_for_state(false, AgentStatus::Idle);
        let _img = icon_for_state(false, AgentStatus::Working);
        let _img = icon_for_state(false, AgentStatus::Error);
    }

    #[test]
    fn icon_connected_variants() {
        let _idle = icon_for_state(true, AgentStatus::Idle);
        let _working = icon_for_state(true, AgentStatus::Working);
        let _error = icon_for_state(true, AgentStatus::Error);
    }

    #[test]
    fn tooltip_disconnected() {
        assert_eq!(
            tooltip_for_state(false, AgentStatus::Idle),
            "ZeroClaw — Disconnected"
        );
        // Agent status is irrelevant when disconnected.
        assert_eq!(
            tooltip_for_state(false, AgentStatus::Working),
            "ZeroClaw — Disconnected"
        );
        assert_eq!(
            tooltip_for_state(false, AgentStatus::Error),
            "ZeroClaw — Disconnected"
        );
    }

    #[test]
    fn tooltip_connected_variants() {
        assert_eq!(
            tooltip_for_state(true, AgentStatus::Idle),
            "ZeroClaw — Idle"
        );
        assert_eq!(
            tooltip_for_state(true, AgentStatus::Working),
            "ZeroClaw — Working"
        );
        assert_eq!(
            tooltip_for_state(true, AgentStatus::Error),
            "ZeroClaw — Error"
        );
    }

    #[test]
    fn embedded_icons_are_valid_png() {
        // Verify the PNG signature (first 8 bytes) of each embedded icon.
        let png_sig: &[u8] = &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        assert!(ICON_IDLE.starts_with(png_sig), "idle icon not valid PNG");
        assert!(
            ICON_WORKING.starts_with(png_sig),
            "working icon not valid PNG"
        );
        assert!(ICON_ERROR.starts_with(png_sig), "error icon not valid PNG");
        assert!(
            ICON_DISCONNECTED.starts_with(png_sig),
            "disconnected icon not valid PNG"
        );
    }
}
