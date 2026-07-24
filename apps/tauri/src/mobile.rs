//! Mobile entry point for ZeroClaw Desktop (iOS/Android).

#[tauri::mobile_entry_point]
fn main() {
    zeroclaw_desktop::run();
}
