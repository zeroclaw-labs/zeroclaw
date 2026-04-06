//! Registers the Tauri desktop app as a "local node" with the ZeroClaw gateway.
//!
//! This allows the AI agent to invoke desktop automation capabilities (AppleScript,
//! screen capture, camera, accessibility, etc.) through the existing node tool
//! dispatch system.

use crate::state::SharedState;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio_tungstenite::{connect_async, tungstenite::Message};

const NODE_ID: &str = "desktop-local";

#[derive(Serialize)]
struct RegisterMessage {
    r#type: &'static str,
    node_id: &'static str,
    capabilities: Vec<Capability>,
    device_type: &'static str,
}

#[derive(Serialize)]
struct Capability {
    name: String,
    description: String,
    parameters: serde_json::Value,
}

#[derive(Deserialize)]
struct InvokeMessage {
    call_id: String,
    capability: String,
    args: serde_json::Value,
}

#[derive(Serialize)]
struct ResultMessage {
    r#type: &'static str,
    call_id: String,
    success: bool,
    output: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

fn desktop_capabilities() -> Vec<Capability> {
    vec![
        Capability {
            name: "desktop.applescript".into(),
            description: "Execute a predefined AppleScript action (app control, clipboard, window management, system settings)".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "action": { "type": "string", "description": "Action name (e.g. app.launch, clipboard.get, system.dark_mode)" },
                    "params": { "type": "object", "description": "Action parameters" }
                },
                "required": ["action"]
            }),
        },
        Capability {
            name: "desktop.screenshot".into(),
            description: "Capture a screenshot of the screen or a specific region".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "region": {
                        "type": "object",
                        "properties": {
                            "x": { "type": "integer" },
                            "y": { "type": "integer" },
                            "width": { "type": "integer" },
                            "height": { "type": "integer" }
                        }
                    }
                }
            }),
        },
        Capability {
            name: "desktop.camera.snap".into(),
            description: "Capture a photo from the built-in camera".into(),
            parameters: serde_json::json!({ "type": "object", "properties": {} }),
        },
        Capability {
            name: "desktop.notify".into(),
            description: "Send a native macOS notification".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "title": { "type": "string" },
                    "body": { "type": "string" },
                    "subtitle": { "type": "string" }
                },
                "required": ["title", "body"]
            }),
        },
        Capability {
            name: "desktop.accessibility.inspect".into(),
            description: "Inspect UI elements of a running application".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "app_name": { "type": "string" },
                    "role_filter": { "type": "string" }
                },
                "required": ["app_name"]
            }),
        },
        Capability {
            name: "desktop.accessibility.click".into(),
            description: "Click a UI element in an application by name".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "app_name": { "type": "string" },
                    "element_name": { "type": "string" }
                },
                "required": ["app_name", "element_name"]
            }),
        },
        Capability {
            name: "desktop.accessibility.type".into(),
            description: "Type text into a focused element in an application".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "app_name": { "type": "string" },
                    "text": { "type": "string" }
                },
                "required": ["app_name", "text"]
            }),
        },
        Capability {
            name: "desktop.audio.record".into(),
            description: "Record audio from the microphone for a given duration".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "duration_secs": { "type": "integer", "default": 5 }
                }
            }),
        },
    ]
}

/// Handle an invocation from the gateway by dispatching to the appropriate
/// local automation command.
async fn handle_invocation(
    capability: &str,
    args: &serde_json::Value,
) -> (bool, String, Option<String>) {
    use crate::commands::automation;

    match capability {
        "desktop.applescript" => {
            let action = args
                .get("action")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let params: automation::applescript::AppleScriptParams = serde_json::from_value(
                args.get("params").cloned().unwrap_or_default(),
            )
            .unwrap_or(automation::applescript::AppleScriptParams {
                bundle_id: None,
                app_name: None,
                text: None,
                path: None,
                x: None,
                y: None,
                width: None,
                height: None,
                level: None,
                enabled: None,
            });
            match automation::applescript::run_applescript_action(action, params).await {
                Ok(out) => (true, out, None),
                Err(e) => (false, String::new(), Some(e)),
            }
        }
        "desktop.screenshot" => {
            let region = args
                .get("region")
                .and_then(|v| serde_json::from_value(v.clone()).ok());
            match automation::screen::capture_screen(region).await {
                Ok(result) => (true, result.base64, None),
                Err(e) => (false, String::new(), Some(e)),
            }
        }
        "desktop.camera.snap" => match automation::camera::capture_photo().await {
            Ok(b64) => (true, b64, None),
            Err(e) => (false, String::new(), Some(e)),
        },
        "desktop.notify" => {
            let title = args
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or("ZeroClaw")
                .to_string();
            let body = args
                .get("body")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let subtitle = args
                .get("subtitle")
                .and_then(|v| v.as_str())
                .map(String::from);
            match automation::notifications::send_desktop_notification(title, body, subtitle, None)
                .await
            {
                Ok(()) => (true, "Notification sent".into(), None),
                Err(e) => (false, String::new(), Some(e)),
            }
        }
        "desktop.accessibility.inspect" => {
            let app = args
                .get("app_name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let role = args
                .get("role_filter")
                .and_then(|v| v.as_str())
                .map(String::from);
            match automation::accessibility::inspect_ui_element(app, role).await {
                Ok(elements) => (
                    true,
                    serde_json::to_string(&elements).unwrap_or_default(),
                    None,
                ),
                Err(e) => (false, String::new(), Some(e)),
            }
        }
        "desktop.accessibility.click" => {
            let app = args
                .get("app_name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let elem = args
                .get("element_name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            match automation::accessibility::click_ui_element(app, elem).await {
                Ok(()) => (true, "Clicked".into(), None),
                Err(e) => (false, String::new(), Some(e)),
            }
        }
        "desktop.accessibility.type" => {
            let app = args
                .get("app_name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let text = args
                .get("text")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            match automation::accessibility::type_into_element(app, text).await {
                Ok(()) => (true, "Typed".into(), None),
                Err(e) => (false, String::new(), Some(e)),
            }
        }
        "desktop.audio.record" => {
            let dur = args
                .get("duration_secs")
                .and_then(|v| v.as_u64())
                .unwrap_or(5) as u32;
            match automation::microphone::record_audio(dur).await {
                Ok(b64) => (true, b64, None),
                Err(e) => (false, String::new(), Some(e)),
            }
        }
        _ => (
            false,
            String::new(),
            Some(format!("Unknown capability: {capability}")),
        ),
    }
}

/// Connect to the gateway's `/ws/nodes` endpoint and register desktop
/// capabilities. Automatically reconnects on disconnect.
pub fn spawn_local_node(state: SharedState) {
    tauri::async_runtime::spawn(async move {
        loop {
            if let Err(e) = run_node_connection(&state).await {
                tracing::warn!("Local node connection lost: {e}. Reconnecting in 5s...");
            }
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        }
    });
}

async fn run_node_connection(state: &SharedState) -> anyhow::Result<()> {
    let (gateway_url, token) = {
        let s = state.read().await;
        (s.gateway_url.clone(), s.token.clone())
    };

    // Build WebSocket URL.
    let ws_url = gateway_url
        .replace("http://", "ws://")
        .replace("https://", "wss://");
    let mut url = format!("{ws_url}/ws/nodes");
    if let Some(ref t) = token {
        url.push_str(&format!("?token={t}"));
    }

    let (ws_stream, _) = connect_async(&url).await?;
    let (mut write, mut read) = ws_stream.split();

    // Send registration.
    let register = RegisterMessage {
        r#type: "register",
        node_id: NODE_ID,
        capabilities: desktop_capabilities(),
        device_type: std::env::consts::OS,
    };
    let msg = serde_json::to_string(&register)?;
    write.send(Message::Text(msg.into())).await?;

    // Process incoming messages.
    while let Some(Ok(msg)) = read.next().await {
        let text = match msg {
            Message::Text(t) => t.to_string(),
            Message::Ping(p) => {
                let _ = write.send(Message::Pong(p)).await;
                continue;
            }
            Message::Close(_) => break,
            _ => continue,
        };

        // Try to parse as an invoke message.
        if let Ok(invoke) = serde_json::from_str::<InvokeMessage>(&text) {
            let (success, output, error) =
                handle_invocation(&invoke.capability, &invoke.args).await;
            let result = ResultMessage {
                r#type: "result",
                call_id: invoke.call_id,
                success,
                output,
                error,
            };
            let result_json = serde_json::to_string(&result)?;
            write.send(Message::Text(result_json.into())).await?;
        }
    }

    Ok(())
}
