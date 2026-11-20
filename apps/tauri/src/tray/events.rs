//! Tray menu event handling.

use crate::window::show_main_window;
use tauri::{AppHandle, Runtime, menu::MenuEvent};

pub fn handle_menu_event<R: Runtime>(app: &AppHandle<R>, event: MenuEvent) {
    match event.id().as_ref() {
        "show" => show_main_window(app, None),
        "chat" => show_main_window(app, Some("/agent")),
        "quit" => {
            app.exit(0);
        }
        _ => {}
    }
}
