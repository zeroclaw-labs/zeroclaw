//! Mobile entry point for QuantClaw Desktop (iOS/Android).

#[tauri::mobile_entry_point]
fn main() {
    quantclaw_desktop::run();
}
