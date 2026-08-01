//! System tray integration for ZeroClaw Desktop.

pub mod events;
pub mod icon;
pub mod menu;

use tauri::{
    App, Manager, Runtime,
    tray::{TrayIcon, TrayIconBuilder, TrayIconEvent},
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
            if let TrayIconEvent::Click { button, .. } = event
                && button == tauri::tray::MouseButton::Left
            {
                let app = tray.app_handle();
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.show();
                    let _ = window.set_focus();
                }
            }
        })
        .build(app)
}
