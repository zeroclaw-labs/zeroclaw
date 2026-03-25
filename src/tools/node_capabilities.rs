//! Standard node capability definitions for device nodes.
//!
//! These define the expected schemas that camera, screen, location, and
//! notification nodes should advertise when they connect via WebSocket.

use serde_json::json;

/// A standard node capability definition.
pub struct NodeCapabilityDef {
    pub name: &'static str,
    pub description: &'static str,
    pub parameters: serde_json::Value,
    pub risk_level: RiskLevel,
}

/// Risk classification for a node capability.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RiskLevel {
    Low,
    Medium,
    High, // Requires approval
}

/// Camera-related capabilities.
pub fn camera_capabilities() -> Vec<NodeCapabilityDef> {
    vec![
        NodeCapabilityDef {
            name: "camera.snap",
            description: "Capture a photo from the device camera",
            parameters: json!({
                "type": "object",
                "properties": {
                    "camera": { "type": "string", "enum": ["front", "back"], "default": "back" },
                    "quality": { "type": "string", "enum": ["low", "medium", "high"], "default": "medium" },
                    "approved": { "type": "boolean", "description": "Set to true to approve camera access" }
                },
                "required": ["approved"]
            }),
            risk_level: RiskLevel::High,
        },
        NodeCapabilityDef {
            name: "camera.clip",
            description: "Record a short video clip from the device camera",
            parameters: json!({
                "type": "object",
                "properties": {
                    "camera": { "type": "string", "enum": ["front", "back"], "default": "back" },
                    "duration_secs": { "type": "integer", "minimum": 1, "maximum": 30, "default": 5 },
                    "quality": { "type": "string", "enum": ["low", "medium", "high"], "default": "medium" },
                    "approved": { "type": "boolean", "description": "Set to true to approve camera access" }
                },
                "required": ["approved"]
            }),
            risk_level: RiskLevel::High,
        },
    ]
}

/// Screen-related capabilities.
pub fn screen_capabilities() -> Vec<NodeCapabilityDef> {
    vec![
        NodeCapabilityDef {
            name: "screen.capture",
            description: "Capture a screenshot of the device screen",
            parameters: json!({
                "type": "object",
                "properties": {
                    "display": { "type": "integer", "default": 0, "description": "Display index for multi-monitor setups" },
                    "approved": { "type": "boolean", "description": "Set to true to approve screen capture" }
                },
                "required": ["approved"]
            }),
            risk_level: RiskLevel::High,
        },
        NodeCapabilityDef {
            name: "screen.record",
            description: "Record the device screen for a specified duration",
            parameters: json!({
                "type": "object",
                "properties": {
                    "duration_secs": { "type": "integer", "minimum": 1, "maximum": 60, "default": 10 },
                    "display": { "type": "integer", "default": 0 },
                    "approved": { "type": "boolean", "description": "Set to true to approve screen recording" }
                },
                "required": ["approved"]
            }),
            risk_level: RiskLevel::High,
        },
    ]
}

/// Location-related capabilities.
pub fn location_capabilities() -> Vec<NodeCapabilityDef> {
    vec![NodeCapabilityDef {
        name: "location.get",
        description: "Get the current GPS location of the device",
        parameters: json!({
            "type": "object",
            "properties": {
                "accuracy": { "type": "string", "enum": ["coarse", "fine"], "default": "coarse" },
                "approved": { "type": "boolean", "description": "Set to true to approve location access" }
            },
            "required": ["approved"]
        }),
        risk_level: RiskLevel::High,
    }]
}

/// Notification capabilities.
pub fn notification_capabilities() -> Vec<NodeCapabilityDef> {
    vec![NodeCapabilityDef {
        name: "system.notify",
        description: "Send a system notification to the device",
        parameters: json!({
            "type": "object",
            "properties": {
                "title": { "type": "string", "description": "Notification title" },
                "body": { "type": "string", "description": "Notification body text" },
                "priority": { "type": "string", "enum": ["low", "normal", "high"], "default": "normal" }
            },
            "required": ["title", "body"]
        }),
        risk_level: RiskLevel::Low,
    }]
}

/// All standard node capabilities.
pub fn all_standard_capabilities() -> Vec<NodeCapabilityDef> {
    let mut caps = Vec::new();
    caps.extend(camera_capabilities());
    caps.extend(screen_capabilities());
    caps.extend(location_capabilities());
    caps.extend(notification_capabilities());
    caps
}

/// Check if a capability name is a sensitive operation requiring approval.
pub fn requires_approval(capability_name: &str) -> bool {
    let sensitive_prefixes = ["camera.", "screen.", "location."];
    sensitive_prefixes
        .iter()
        .any(|p| capability_name.starts_with(p))
}

/// Detect the current platform.
pub fn detect_platform() -> &'static str {
    #[cfg(target_os = "macos")]
    {
        "macos"
    }
    #[cfg(target_os = "linux")]
    {
        "linux"
    }
    #[cfg(target_os = "android")]
    {
        "android"
    }
    #[cfg(target_os = "ios")]
    {
        "ios"
    }
    #[cfg(target_os = "windows")]
    {
        "windows"
    }
    #[cfg(not(any(
        target_os = "macos",
        target_os = "linux",
        target_os = "android",
        target_os = "ios",
        target_os = "windows"
    )))]
    {
        "unknown"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_capabilities_have_names() {
        for cap in all_standard_capabilities() {
            assert!(!cap.name.is_empty(), "Capability name must not be empty");
        }
    }

    #[test]
    fn all_capabilities_have_descriptions() {
        for cap in all_standard_capabilities() {
            assert!(
                !cap.description.is_empty(),
                "Capability '{}' must have a description",
                cap.name
            );
        }
    }

    #[test]
    fn all_capabilities_have_valid_schemas() {
        for cap in all_standard_capabilities() {
            assert_eq!(
                cap.parameters["type"], "object",
                "Capability '{}' schema must be an object",
                cap.name
            );
            assert!(
                cap.parameters["properties"].is_object(),
                "Capability '{}' schema must have properties",
                cap.name
            );
        }
    }

    #[test]
    fn sensitive_capabilities_require_approval() {
        assert!(requires_approval("camera.snap"));
        assert!(requires_approval("camera.clip"));
        assert!(requires_approval("screen.capture"));
        assert!(requires_approval("screen.record"));
        assert!(requires_approval("location.get"));
    }

    #[test]
    fn notification_does_not_require_approval() {
        assert!(!requires_approval("system.notify"));
    }

    #[test]
    fn detect_platform_returns_known_value() {
        let platform = detect_platform();
        let known = ["macos", "linux", "android", "ios", "windows", "unknown"];
        assert!(
            known.contains(&platform),
            "Platform '{}' is not in the known set",
            platform
        );
    }

    #[test]
    fn camera_snap_schema_has_required_fields() {
        let caps = camera_capabilities();
        let snap = caps.iter().find(|c| c.name == "camera.snap").unwrap();
        let props = &snap.parameters["properties"];
        assert!(props["camera"].is_object());
        assert!(props["quality"].is_object());
        assert!(props["approved"].is_object());
        let required = snap.parameters["required"].as_array().unwrap();
        assert!(required.contains(&serde_json::Value::String("approved".to_string())));
    }

    #[test]
    fn all_high_risk_have_approved_field() {
        for cap in all_standard_capabilities() {
            if cap.risk_level == RiskLevel::High {
                assert!(
                    cap.parameters["properties"]["approved"].is_object(),
                    "High-risk capability '{}' must have an 'approved' parameter",
                    cap.name
                );
            }
        }
    }
}
