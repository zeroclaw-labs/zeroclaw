//! ZeroClaw Desktop — Tauri application library.

pub mod state;
pub mod gateway_client;
pub mod commands;

use state::AppState;

/// Configure and run the Tauri application.
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![
            commands::gateway::get_status,
            commands::gateway::get_health,
            commands::channels::list_channels,
            commands::pairing::initiate_pairing,
            commands::pairing::get_devices,
            commands::agent::send_message,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
