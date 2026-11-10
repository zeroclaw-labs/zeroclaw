//! System tray integration for ZeroClaw Desktop.

pub mod events;
pub mod icon;
pub mod menu;

use tauri::{
    App, Runtime,
    tray::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent},
};

/// Set up the system tray icon and menu.
pub fn setup_tray<R: Runtime>(app: &App<R>) -> Result<TrayIcon<R>, tauri::Error> {
    let menu = menu::create_tray_menu(app)?;

    TrayIconBuilder::with_id("main")
        .tooltip("ZeroClaw — Disconnected")
        .icon(icon::icon_for_state(false, crate::state::AgentStatus::Idle))
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(events::handle_menu_event)
        .on_tray_icon_event(|tray, event| {
            // A left click arrives as a press and then a release; act once.
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                crate::window::show_main_window(tray.app_handle(), None);
            }
        })
        .build(app)
}
