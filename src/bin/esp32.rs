//! Minimal Hrafn entrypoint for ESP32-S3.
//!
//! This binary provides a stripped-down agent runtime suitable for
//! microcontrollers with limited RAM and flash.
//!
//! Build: cargo build --bin hrafn-esp32 --features target-esp32 --no-default-features --target <xtensa-target>

fn main() {
    // TODO: Initialize ESP-IDF runtime
    // TODO: Connect WiFi
    // TODO: Load config from NVS or embedded defaults
    // TODO: Initialize provider (blocking HTTP to LLM API)
    // TODO: Initialize channel (serial UART or HTTP endpoint)
    // TODO: Initialize memory (SQLite on SPIFFS or in-memory)
    // TODO: Initialize runtime adapter (Esp32Runtime)
    // TODO: Run agent loop

    println!("Hrafn ESP32 runtime - not yet implemented");
    println!("This entrypoint will be completed when esp-idf-svc integration lands.");
}
