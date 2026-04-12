//! Plugin manifest — `~/.zeroclaw/tools/<name>/tool.toml` schema.
//!
//! Each user plugin lives in its own subdirectory and carries a `tool.toml`
//! that describes the tool, how to invoke it, and what parameters it accepts.
//!
//! Example `tool.toml`:
//! ```toml
//! [tool]
//! name        = "i2c_scan"
//! version     = "1.0.0"
//! description = "Scan the I2C bus for connected devices"
//!
//! [exec]
//! binary = "i2c_scan.py"
//!
//! [transport]
//! preferred       = "serial"
//! device_required = true
//!
//! [[parameters]]
//! name        = "device"
//! type        = "string"
//! description = "Device alias e.g. pico0"
//! required    = true
//!
//! [[parameters]]
//! name        = "bus"
//! type        = "integer"
//! description = "I2C bus number (default 0)"
//! required    = false
//! default     = 0
//! ```

use serde::Deserialize;

/// Full plugin manifest — parsed from `tool.toml`.
#[derive(Debug, Deserialize)]
pub struct ToolManifest {
    /// Tool identity and human-readable metadata.
    pub tool: ToolMeta,
    /// How to invoke the tool binary.
    pub exec: ExecConfig,
    /// Optional transport preference and device requirement.
    pub transport: Option<TransportConfig>,
    /// Parameter definitions used to build the JSON Schema for the LLM.
    #[serde(default)]
    pub parameters: Vec<ParameterDef>,
}

/// Tool identity metadata.
#[derive(Debug, Deserialize)]
pub struct ToolMeta {
    /// Unique tool name, used as the function-call key by the LLM.
    pub name: String,
    /// Semantic version string (e.g. `"1.0.0"`).
    pub version: String,
    /// Human-readable description injected into the LLM system prompt.
    pub description: String,
}

/// Execution configuration — how ZeroClaw spawns the tool.
#[derive(Debug, Deserialize)]
pub struct ExecConfig {
    /// Path to the binary, relative to the plugin directory.
    ///
    /// Can be a Python script (`"tool.py"`), a shell script (`"run.sh"`),
    /// a compiled binary (`"i2c_scan"`), or any executable.
    pub binary: String,
}

/// Optional transport hint for the tool.
///
/// When present, ZeroClaw will prefer the named transport kind
/// and can enforce device presence before calling the tool.
#[derive(Debug, Deserialize)]
pub struct TransportConfig {
    /// Preferred transport kind: `"serial"` | `"swd"` | `"native"` | `"any"`.
    pub preferred: String,
    /// Whether the tool requires a hardware device to be connected.
    pub device_required: bool,
}

/// A single parameter definition for a plugin tool.
#[derive(Debug, Deserialize)]
pub struct ParameterDef {
    /// Parameter name (matches the JSON key passed to the tool via stdin).
    pub name: String,
    /// JSON Schema primitive type: `"string"` | `"integer"` | `"boolean"`.
    #[serde(rename = "type")]
    pub r#type: String,
    /// Human-readable description shown to the LLM.
    pub description: String,
    /// Whether the LLM must supply this parameter.
    pub required: bool,
    /// Optional default value serialized as a JSON Value.
    pub default: Option<serde_json::Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL_TOML: &str = r#"
[tool]
name        = "i2c_scan"
version     = "1.0.0"
description = "Scan the I2C bus"

[exec]
binary = "i2c_scan.py"

[[parameters]]
name        = "device"
type        = "string"
description = "Device alias"
required    = true
"#;

    #[test]
    fn manifest_parses_minimal_toml() {
        let m: ToolManifest = toml::from_str(MINIMAL_TOML).expect("parse failed");
        assert_eq!(m.tool.name, "i2c_scan");
        assert_eq!(m.tool.version, "1.0.0");
        assert_eq!(m.exec.binary, "i2c_scan.py");
        assert!(m.transport.is_none());
        assert_eq!(m.parameters.len(), 1);
        assert_eq!(m.parameters[0].name, "device");
        assert!(m.parameters[0].required);
    }

    const FULL_TOML: &str = r#"
[tool]
name        = "pwm_set"
version     = "1.0.0"
description = "Set PWM duty cycle on a pin"

[exec]
binary = "pwm_set"

[transport]
preferred       = "serial"
device_required = true

[[parameters]]
name        = "device"
type        = "string"
description = "Device alias"
required    = true

[[parameters]]
name        = "pin"
type        = "integer"
description = "PWM pin number"
required    = true

[[parameters]]
name        = "duty"
type        = "integer"
description = "Duty cycle 0–100"
required    = false
default     = 50
"#;

    #[test]
    fn manifest_parses_full_toml_with_transport_and_defaults() {
        let m: ToolManifest = toml::from_str(FULL_TOML).expect("parse failed");
        assert_eq!(m.tool.name, "pwm_set");
        let transport = m.transport.as_ref().expect("transport missing");
        assert_eq!(transport.preferred, "serial");
        assert!(transport.device_required);
        let duty = m
            .parameters
            .iter()
            .find(|p| p.name == "duty")
            .expect("duty param missing");
        assert!(!duty.required);
        assert_eq!(duty.default, Some(serde_json::json!(50)));
    }

    #[test]
    fn manifest_empty_parameters_default_to_empty_vec() {
        let raw = r#"
[tool]
name        = "noop"
version     = "0.1.0"
description = "No-op tool"

[exec]
binary = "noop"
"#;
        let m: ToolManifest = toml::from_str(raw).expect("parse failed");
        assert!(m.parameters.is_empty());
    }
}
