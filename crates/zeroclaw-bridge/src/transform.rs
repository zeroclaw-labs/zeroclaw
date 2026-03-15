//! Message transformation between MQTT and WebSocket formats.
//!
//! This module provides stateless conversion functions for translating
//! MQTT JSON payloads to WebSocket message formats and vice versa.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// MQTT message from node (register or result)
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MqttNodeMessage {
    Register {
        node_id: String,
        capabilities: Vec<NodeCapability>,
    },
    Result {
        call_id: String,
        success: bool,
        output: String,
        #[serde(default)]
        error: Option<String>,
    },
}

/// MQTT message to node (invoke)
#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MqttGatewayMessage {
    Invoke {
        call_id: String,
        capability: String,
        args: Value,
    },
}

/// Node capability definition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeCapability {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

/// WebSocket message from node
#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WsNodeMessage {
    Register {
        node_id: String,
        capabilities: Vec<NodeCapability>,
    },
    Result {
        call_id: String,
        success: bool,
        output: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
}

/// WebSocket message to node
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WsGatewayMessage {
    Invoke {
        call_id: String,
        capability: String,
        args: Value,
    },
}

/// Convert MQTT JSON payload to WebSocket JSON string
pub fn mqtt_to_ws(mqtt_json: &str) -> Result<String> {
    let mqtt_msg: MqttNodeMessage = serde_json::from_str(mqtt_json)?;

    let ws_msg = match mqtt_msg {
        MqttNodeMessage::Register {
            node_id,
            capabilities,
        } => WsNodeMessage::Register {
            node_id,
            capabilities,
        },
        MqttNodeMessage::Result {
            call_id,
            success,
            output,
            error,
        } => WsNodeMessage::Result {
            call_id,
            success,
            output,
            error,
        },
    };

    Ok(serde_json::to_string(&ws_msg)?)
}

/// Convert WebSocket JSON payload to MQTT JSON string
pub fn ws_to_mqtt(ws_json: &str) -> Result<String> {
    let ws_msg: WsGatewayMessage = serde_json::from_str(ws_json)?;

    let mqtt_msg = match ws_msg {
        WsGatewayMessage::Invoke {
            call_id,
            capability,
            args,
        } => MqttGatewayMessage::Invoke {
            call_id,
            capability,
            args,
        },
    };

    Ok(serde_json::to_string(&mqtt_msg)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mqtt_to_ws_register() {
        let mqtt = r#"{
            "type": "register",
            "node_id": "test-node-01",
            "capabilities": [
                {
                    "name": "read_temperature",
                    "description": "Read temperature sensor",
                    "parameters": {"type": "object", "properties": {}}
                }
            ]
        }"#;

        let ws = mqtt_to_ws(mqtt).unwrap();
        let parsed: Value = serde_json::from_str(&ws).unwrap();

        assert_eq!(parsed["type"], "register");
        assert_eq!(parsed["node_id"], "test-node-01");
        assert_eq!(parsed["capabilities"][0]["name"], "read_temperature");
    }

    #[test]
    fn test_mqtt_to_ws_result_success() {
        let mqtt = r#"{
            "type": "result",
            "call_id": "call_123",
            "success": true,
            "output": "22.5°C",
            "error": null
        }"#;

        let ws = mqtt_to_ws(mqtt).unwrap();
        let parsed: Value = serde_json::from_str(&ws).unwrap();

        assert_eq!(parsed["type"], "result");
        assert_eq!(parsed["call_id"], "call_123");
        assert_eq!(parsed["success"], true);
        assert_eq!(parsed["output"], "22.5°C");
    }

    #[test]
    fn test_mqtt_to_ws_result_error() {
        let mqtt = r#"{
            "type": "result",
            "call_id": "call_456",
            "success": false,
            "output": "",
            "error": "Sensor timeout"
        }"#;

        let ws = mqtt_to_ws(mqtt).unwrap();
        let parsed: Value = serde_json::from_str(&ws).unwrap();

        assert_eq!(parsed["type"], "result");
        assert_eq!(parsed["success"], false);
        assert_eq!(parsed["error"], "Sensor timeout");
    }

    #[test]
    fn test_ws_to_mqtt_invoke() {
        let ws = r#"{
            "type": "invoke",
            "call_id": "call_789",
            "capability": "toggle_led",
            "args": {"state": true}
        }"#;

        let mqtt = ws_to_mqtt(ws).unwrap();
        let parsed: Value = serde_json::from_str(&mqtt).unwrap();

        assert_eq!(parsed["type"], "invoke");
        assert_eq!(parsed["call_id"], "call_789");
        assert_eq!(parsed["capability"], "toggle_led");
        assert_eq!(parsed["args"]["state"], true);
    }

    #[test]
    fn test_mqtt_to_ws_invalid_json() {
        let result = mqtt_to_ws("not json");
        assert!(result.is_err());
    }

    #[test]
    fn test_ws_to_mqtt_invalid_json() {
        let result = ws_to_mqtt("not json");
        assert!(result.is_err());
    }
}
