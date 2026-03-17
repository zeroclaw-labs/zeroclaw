//! System tray integration for ZeroClaw Desktop.

pub mod menu;
pub mod events;

use tauri::{
    tray::{TrayIcon, TrayIconBuilder},
    App, Runtime,
};

/// Set up the system tray icon and menu.
pub fn setup_tray<R: Runtime>(app: &App<R>) -> Result<TrayIcon<R>, tauri::Error> {
    let menu = menu::create_tray_menu(app)?;

    TrayIconBuilder::new()
        .tooltip("ZeroClaw")
        .menu(&menu)
        .on_menu_event(events::handle_menu_event)
        .build(app)
}
