//! Tray menu event handling.

use tauri::{
    menu::MenuEvent,
    tray::TrayIcon,
    AppHandle, Runtime,
};

pub fn handle_menu_event<R: Runtime>(_tray: &TrayIcon<R>, event: MenuEvent) {
    match event.id().as_ref() {
        "show" => {
            // TODO: Show/focus the main window
        }
        "gateway" => {
            // TODO: Open gateway URL in browser
        }
        "quit" => {
            std::process::exit(0);
        }
        _ => {}
    }
}
