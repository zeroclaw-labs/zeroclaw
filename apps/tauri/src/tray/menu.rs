//! Tray menu construction.

use tauri::{
    menu::{Menu, MenuItem},
    App, Runtime,
};

pub fn create_tray_menu<R: Runtime>(app: &App<R>) -> Result<Menu<R>, tauri::Error> {
    let show = MenuItem::with_id(app, "show", "Show Dashboard", true, None::<&str>)?;
    let status = MenuItem::with_id(app, "status", "Status: Checking...", false, None::<&str>)?;
    let separator1 = MenuItem::with_id(app, "sep1", "---", false, None::<&str>)?;
    let gateway = MenuItem::with_id(app, "gateway", "Open Gateway", true, None::<&str>)?;
    let separator2 = MenuItem::with_id(app, "sep2", "---", false, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit ZeroClaw", true, None::<&str>)?;

    Menu::with_items(app, &[&show, &status, &separator1, &gateway, &separator2, &quit])
}
