//! Tray menu event handling.

use tauri::{menu::MenuEvent, AppHandle, Manager, Runtime};

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

fn show_main_window<R: Runtime>(app: &AppHandle<R>, navigate_to: Option<&str>) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.set_focus();
        if let Some(path) = navigate_to {
            let script = format!("window.location.hash = '{path}'");
            let _ = window.eval(&script);
        }
    }
}
