//! Mobile entry point for DaemonClaw Desktop (iOS/Android).

#[tauri::mobile_entry_point]
fn main() {
    daemonclaw_desktop::run();
}
