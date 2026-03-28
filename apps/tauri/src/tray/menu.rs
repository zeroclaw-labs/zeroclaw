//! Tray menu construction.

use tauri::{
    App, Runtime,
    menu::{Menu, MenuItemBuilder, PredefinedMenuItem},
};

pub fn create_tray_menu<R: Runtime>(app: &App<R>) -> Result<Menu<R>, tauri::Error> {
    let show = MenuItemBuilder::with_id("show", "Show Dashboard").build(app)?;
    let chat = MenuItemBuilder::with_id("chat", "Agent Chat").build(app)?;
    let sep1 = PredefinedMenuItem::separator(app)?;
    let status = MenuItemBuilder::with_id("status", "Status: Checking...")
        .enabled(false)
        .build(app)?;
    let sep2 = PredefinedMenuItem::separator(app)?;
    let quit = MenuItemBuilder::with_id("quit", "Quit ZeroClaw").build(app)?;

    Menu::with_items(app, &[&show, &chat, &sep1, &status, &sep2, &quit])
}
