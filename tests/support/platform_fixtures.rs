//! Pre-built capability sets for simulating different device platforms in tests.

use zeroclaw::gateway::nodes::NodeCapability;

fn cap(name: &str, desc: &str) -> NodeCapability {
    NodeCapability {
        name: name.to_string(),
        description: desc.to_string(),
        parameters: serde_json::json!({"type": "object", "properties": {}}),
    }
}

/// macOS desktop capabilities (mirrors Tauri desktop_capabilities).
pub fn macos_capabilities() -> Vec<NodeCapability> {
    vec![
        cap(
            "desktop.applescript",
            "Execute a predefined AppleScript action",
        ),
        cap("desktop.screenshot", "Capture a screenshot"),
        cap("desktop.camera.snap", "Capture a photo from the camera"),
        cap("desktop.notify", "Send a native macOS notification"),
        cap("desktop.accessibility.inspect", "Inspect UI elements"),
        cap("desktop.accessibility.click", "Click a UI element"),
        cap("desktop.accessibility.type", "Type text into an element"),
        cap("desktop.audio.record", "Record audio from microphone"),
    ]
}

/// Windows desktop capabilities.
pub fn windows_capabilities() -> Vec<NodeCapability> {
    vec![
        cap("desktop.screenshot", "Capture a screenshot"),
        cap("desktop.camera.snap", "Capture a photo from the camera"),
        cap("desktop.notify", "Send a Windows notification"),
        cap("desktop.clipboard", "Read/write clipboard contents"),
    ]
}

/// Android mobile capabilities.
pub fn android_capabilities() -> Vec<NodeCapability> {
    vec![
        cap("camera.snap", "Take a photo"),
        cap("location.get", "Get GPS location"),
        cap("system.notify", "Send a notification"),
        cap("sensors.accelerometer", "Read accelerometer data"),
    ]
}

/// Linux desktop capabilities.
pub fn linux_capabilities() -> Vec<NodeCapability> {
    vec![
        cap("desktop.screenshot", "Capture a screenshot"),
        cap("desktop.notify", "Send a desktop notification"),
    ]
}
