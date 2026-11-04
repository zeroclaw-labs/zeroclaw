//! Dashboard and splash window lifecycle.
//!
//! Closing the dashboard hides it, so the tray app keeps the page intact.
//! Every route back (tray click, tray menu, relaunch) goes through
//! [`show_main_window`], which always ends on a visible window.

use std::sync::{Mutex, PoisonError};

use tauri::{
    AppHandle, Manager, Runtime, WebviewWindow, WebviewWindowBuilder, Window, WindowEvent,
};

/// Label of the dashboard window that `open_dashboard` builds.
pub const MAIN_WINDOW: &str = "main";

/// Label of the startup splash declared in `tauri.conf.json`.
pub const SPLASH_WINDOW: &str = "splash";

/// Serializes splash rebuilds: Tauri checks that a label is free before it
/// registers the new window, so two concurrent builds could both pass.
static SPLASH_REBUILD: Mutex<()> = Mutex::new(());

/// Whether an exit request should be declined so the app keeps running in the
/// tray. Closing the last window carries no exit code; an explicit exit such
/// as tray Quit carries one and proceeds.
pub fn keep_running_on_exit_request(code: Option<i32>) -> bool {
    code.is_none()
}

/// Only the dashboard hides on close. The splash stays closable because
/// `open_dashboard` closes it to hand off to the dashboard.
fn hides_on_close(label: &str) -> bool {
    label == MAIN_WINDOW
}

/// Window event handler that hides the dashboard instead of destroying it.
pub fn on_window_event<R: Runtime>(window: &Window<R>, event: &WindowEvent) {
    if let WindowEvent::CloseRequested { api, .. } = event
        && hides_on_close(window.label())
    {
        api.prevent_close();
        let _ = window.hide();
    }
}

/// Show, restore and focus a window. Focusing alone leaves a minimized
/// window minimized on Windows.
pub fn reveal<R: Runtime>(window: &WebviewWindow<R>) {
    let _ = window.show();
    if window.is_minimized().unwrap_or(false) {
        let _ = window.unminimize();
    }
    let _ = window.set_focus();
}

/// Bring back the dashboard, or the splash while startup is still running.
/// When neither window exists, rebuild the splash: it hands off to the
/// dashboard once the gateway is healthy, as it does at launch.
pub fn show_main_window<R: Runtime>(app: &AppHandle<R>, navigate_to: Option<&str>) {
    if let Some(window) = app.get_webview_window(MAIN_WINDOW) {
        reveal(&window);
        if let Some(path) = navigate_to {
            let script = format!("window.location.hash = '{path}'");
            let _ = window.eval(&script);
        }
    } else if let Some(splash) = app.get_webview_window(SPLASH_WINDOW) {
        reveal(&splash);
    } else {
        // Building a webview inside a tray, menu or single-instance handler
        // deadlocks on Windows, so the rebuild runs on its own thread.
        let app = app.clone();
        std::thread::spawn(move || {
            if let Err(error) = rebuild_splash(&app) {
                eprintln!("ZeroClaw Desktop: reopening the splash window failed: {error}");
            }
        });
    }
}

fn rebuild_splash<R: Runtime>(app: &AppHandle<R>) -> Result<(), String> {
    let _guard = SPLASH_REBUILD
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    // Another route may have brought a window back while this one waited.
    if app.get_webview_window(MAIN_WINDOW).is_some()
        || app.get_webview_window(SPLASH_WINDOW).is_some()
    {
        return Ok(());
    }
    let config = app
        .config()
        .app
        .windows
        .iter()
        .find(|config| config.label == SPLASH_WINDOW)
        .ok_or_else(|| format!("no `{SPLASH_WINDOW}` window in the app config"))?;
    let splash = WebviewWindowBuilder::from_config(app, config)
        .and_then(|builder| builder.build())
        .map_err(|error| error.to_string())?;
    let _ = splash.set_focus();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closing_the_last_window_keeps_running_but_an_explicit_exit_proceeds() {
        assert!(keep_running_on_exit_request(None));
        assert!(!keep_running_on_exit_request(Some(0)));
        assert!(!keep_running_on_exit_request(Some(1)));
    }

    #[test]
    fn only_the_dashboard_hides_on_close() {
        assert!(hides_on_close(MAIN_WINDOW));
        assert!(!hides_on_close(SPLASH_WINDOW));
    }

    #[test]
    fn the_splash_can_be_rebuilt_from_the_app_config() {
        let config: serde_json::Value = serde_json::from_str(include_str!("../tauri.conf.json"))
            .expect("tauri.conf.json must be valid JSON");
        let windows = config["app"]["windows"]
            .as_array()
            .expect("tauri.conf.json must declare app.windows");
        assert!(
            windows
                .iter()
                .any(|window| window["label"].as_str() == Some(SPLASH_WINDOW)),
            "rebuilding the splash needs its `{SPLASH_WINDOW}` window config"
        );
    }
}
