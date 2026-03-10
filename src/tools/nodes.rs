//! Nodes tool and NodeRegistry trait for node-control.
//!
//! When `[gateway.node_control]` is enabled, the gateway injects this tool and
//! implements [`NodeRegistry`] with connected WebSocket nodes. The tool exposes
//! list, describe, invoke, and run actions to the agent.

use super::traits::{Tool, ToolResult};
use anyhow::Result;
use async_trait::async_trait;
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;
use std::sync::Arc;

/// Info for one connected node (list entry).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeInfo {
    pub node_id: String,
    pub status: String,
    pub capabilities: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub meta: Option<Value>,
}

/// Full description for a single node (describe).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeDescription {
    pub node_id: String,
    pub status: String,
    pub capabilities: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub meta: Option<Value>,
}

/// Result of an invoke or run command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeCommandResult {
    pub success: bool,
    pub output: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Trait for the connected-node registry. Implemented by the gateway when
/// node-control is enabled; used by [`NodesTool`] and HTTP node-control API.
#[async_trait]
pub trait NodeRegistry: Send + Sync {
    /// List all connected nodes (optionally filtered by allowlist elsewhere).
    fn list(&self) -> Vec<NodeInfo>;

    /// Describe one node by id; None if not connected.
    fn describe(&self, node_id: &str) -> Option<NodeDescription>;

    /// Send a structured invoke to the node; waits for response with timeout.
    async fn invoke(
        &self,
        node_id: &str,
        capability: &str,
        arguments: Value,
    ) -> Result<NodeCommandResult>;

    /// Send a raw command (e.g. shell) to the node; waits for response with timeout.
    async fn run(&self, node_id: &str, raw_command: &str) -> Result<NodeCommandResult>;
}

/// Tool that exposes node list, describe, invoke, and run to the agent.
/// Only registered when gateway runs with node_control.enabled and injects
/// a concrete NodeRegistry.
/// `workspace_dir` is used to save media (camera_snap, etc.) under workspace/media/.
pub struct NodesTool {
    registry: Arc<dyn NodeRegistry>,
    workspace_dir: PathBuf,
}

impl NodesTool {
    pub fn new(registry: Arc<dyn NodeRegistry>, workspace_dir: impl Into<PathBuf>) -> Self {
        Self {
            registry,
            workspace_dir: workspace_dir.into(),
        }
    }

    /// Returns a path for node media files under workspace/media/.
    fn temp_media_path(&self, kind: &str, label: &str, ext: &str) -> PathBuf {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let name = format!("node_{}_{}_{}.{}", kind, label, ts, ext);
        if self.workspace_dir.as_os_str().is_empty() {
            std::env::temp_dir().join("zeroclaw").join(name)
        } else {
            self.workspace_dir.join("media").join(name)
        }
    }

    /// Resolves "node" arg to node_id. Supports: exact nodeId, displayName (from meta),
    /// remoteIp (from meta), "current"/"default"/empty (first node), partial nodeId prefix (≥6 chars).
    fn resolve_node(&self, args: &Value) -> Result<String> {
        let ref_ = Self::read_optional_nonempty_string(args, "node").unwrap_or("");
        let sessions = self.registry.list();

        if ref_.is_empty() || ref_ == "current" || ref_ == "default" {
            return sessions
                .first()
                .map(|n| n.node_id.clone())
                .ok_or_else(|| anyhow::anyhow!("no nodes are currently connected"));
        }

        let q_norm = Self::normalize_node_key(ref_);
        let mut matches: Vec<&NodeInfo> = Vec::new();

        for n in &sessions {
            if n.node_id == ref_ {
                return Ok(n.node_id.clone());
            }
            if let Some(remote_ip) = n
                .meta
                .as_ref()
                .and_then(|m| m.get("remoteIp"))
                .and_then(Value::as_str)
            {
                if remote_ip == ref_ {
                    matches.push(n);
                    continue;
                }
            }
            let display_name = n
                .meta
                .as_ref()
                .and_then(|m| m.get("client"))
                .and_then(|c| c.get("displayName"))
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty());
            if let Some(dn) = display_name {
                if Self::normalize_node_key(dn) == q_norm {
                    matches.push(n);
                    continue;
                }
            }
            if ref_.len() >= 6 && n.node_id.starts_with(ref_) {
                matches.push(n);
            }
        }

        if matches.len() == 1 {
            return Ok(matches[0].node_id.clone());
        }
        if matches.len() > 1 {
            let names: Vec<String> = matches
                .iter()
                .map(|n| {
                    n.meta
                        .as_ref()
                        .and_then(|m| m.get("client"))
                        .and_then(|c| c.get("displayName"))
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                        .map(String::from)
                        .unwrap_or_else(|| n.node_id.clone())
                })
                .collect();
            return Err(anyhow::anyhow!(
                "ambiguous node {:?} (matches: {})",
                ref_,
                names.join(", ")
            ));
        }

        let known: Vec<String> = sessions
            .iter()
            .map(|n| {
                n.meta
                    .as_ref()
                    .and_then(|m| m.get("client"))
                    .and_then(|c| c.get("displayName"))
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map(String::from)
                    .unwrap_or_else(|| n.node_id.clone())
            })
            .collect();
        let hint = if known.is_empty() {
            String::new()
        } else {
            format!(" (connected: {})", known.join(", "))
        };
        Err(anyhow::anyhow!("no connected node matches {:?}{}", ref_, hint))
    }

    /// Converts string to lowercase slug (non-alnum → "-") for display-name comparison.
    fn normalize_node_key(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        let mut prev_dash = true;
        for c in s.to_lowercase().chars() {
            if c.is_ascii_alphanumeric() {
                out.push(c);
                prev_dash = false;
            } else if !prev_dash {
                out.push('-');
                prev_dash = true;
            }
        }
        out.trim_end_matches('-').to_string()
    }

    fn read_optional_nonempty_string<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
        args.get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
    }

    fn read_required_string(args: &Value, key: &str) -> Result<String> {
        Self::read_optional_nonempty_string(args, key)
            .map(str::to_string)
            .ok_or_else(|| anyhow::anyhow!("Missing required '{key}' parameter"))
    }

    fn parse_result_output(output: &str) -> Value {
        if output.trim().is_empty() {
            Value::Null
        } else if let Ok(parsed) = serde_json::from_str::<Value>(output) {
            parsed
        } else {
            Value::String(output.to_string())
        }
    }

    fn format_json_output(value: &Value) -> String {
        serde_json::to_string_pretty(value)
            .unwrap_or_else(|_| serde_json::to_string(value).unwrap_or_default())
    }

    fn parse_env_pairs(args: &Value) -> Option<Value> {
        let entries = args.get("env").and_then(Value::as_array)?;
        let mut env = serde_json::Map::new();
        for item in entries {
            let Some(raw) = item.as_str() else {
                continue;
            };
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                continue;
            }
            let Some(eq_pos) = trimmed.find('=') else {
                continue;
            };
            if eq_pos == 0 {
                continue;
            }
            let key = trimmed[..eq_pos].trim();
            if key.is_empty() {
                continue;
            }
            let value = &trimmed[(eq_pos + 1)..];
            env.insert(key.to_string(), Value::String(value.to_string()));
        }
        (!env.is_empty()).then_some(Value::Object(env))
    }

    fn parse_timeout_ms(args: &Value, key: &str) -> Option<u64> {
        args.get(key)
            .and_then(Value::as_f64)
            .filter(|value| value.is_finite() && *value > 0.0)
            .map(|value| value.round() as u64)
    }

    fn parse_duration_expr(input: &str) -> Result<u64> {
        let raw = input.trim();
        if raw.is_empty() {
            return Err(anyhow::anyhow!("duration must not be empty"));
        }

        let (number, unit) = if let Some(value) = raw.strip_suffix("ms") {
            (value.trim(), "ms")
        } else if let Some(value) = raw.strip_suffix('s') {
            (value.trim(), "s")
        } else if let Some(value) = raw.strip_suffix('m') {
            (value.trim(), "m")
        } else if let Some(value) = raw.strip_suffix('h') {
            (value.trim(), "h")
        } else {
            (raw, "ms")
        };

        let base = number
            .parse::<f64>()
            .map_err(|error| anyhow::anyhow!("invalid duration '{raw}': {error}"))?;
        if !base.is_finite() || base <= 0.0 {
            return Err(anyhow::anyhow!("duration must be > 0"));
        }

        let multiplier = match unit {
            "ms" => 1.0,
            "s" => 1000.0,
            "m" => 60_000.0,
            "h" => 3_600_000.0,
            _ => 1.0,
        };
        Ok((base * multiplier).round() as u64)
    }

    fn read_duration_ms(args: &Value, default_ms: u64) -> Result<u64> {
        if let Some(value) = Self::parse_timeout_ms(args, "durationMs") {
            return Ok(value);
        }
        if let Some(duration) = Self::read_optional_nonempty_string(args, "duration") {
            return Self::parse_duration_expr(duration);
        }
        Ok(default_ms)
    }

    async fn build_media_save_image_params(args: &Value) -> Result<Value> {
        let local_path = Self::read_required_string(args, "path")?;
        let expanded = shellexpand::tilde(&local_path).into_owned();
        let mut path_buf = PathBuf::from(expanded);
        if path_buf.is_relative() {
            path_buf = std::env::current_dir()
                .map_err(|error| anyhow::anyhow!("media_saveImage: cannot resolve current dir: {error}"))?
                .join(path_buf);
        }

        let data = tokio::fs::read(&path_buf)
            .await
            .map_err(|error| anyhow::anyhow!("media_saveImage: read file: {error}"))?;

        const MAX_FILE_SAVE_BYTES: usize = 10 * 1024 * 1024;
        if data.len() > MAX_FILE_SAVE_BYTES {
            return Err(anyhow::anyhow!(
                "media_saveImage: file too large (max {} MB)",
                MAX_FILE_SAVE_BYTES / (1024 * 1024)
            ));
        }

        let ext = path_buf
            .extension()
            .and_then(|value| value.to_str())
            .map(|value| value.trim().to_ascii_lowercase())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "bin".to_string());

        let mime_type = match ext.as_str() {
            "jpg" | "jpeg" => "image/jpeg",
            "png" => "image/png",
            "gif" => "image/gif",
            "webp" => "image/webp",
            _ => {
                return Err(anyhow::anyhow!(
                    "media_saveImage: unsupported format '{}' (allowed: jpg, jpeg, png, gif, webp)",
                    ext
                ));
            }
        };

        let file_name = Self::read_optional_nonempty_string(args, "filename")
            .map(str::to_string)
            .or_else(|| {
                path_buf
                    .file_name()
                    .and_then(|value| value.to_str())
                    .map(str::to_string)
            })
            .ok_or_else(|| anyhow::anyhow!("media_saveImage: cannot determine fileName"))?;

        Ok(serde_json::json!({
            "base64": base64::engine::general_purpose::STANDARD.encode(&data),
            "mimeType": mime_type,
            "fileName": file_name,
        }))
    }

    async fn execute_invoke_action(
        &self,
        node_id: &str,
        capability: &str,
        params: Value,
    ) -> Result<ToolResult> {
        let res = self
            .registry
            .invoke(node_id, capability, params)
            .await
            .map_err(|e| anyhow::anyhow!("invoke failed: {e}"))?;

        let payload = Self::parse_result_output(&res.output);
        Ok(ToolResult {
            success: res.success,
            output: Self::format_json_output(&payload),
            error: res.error,
        })
    }
}

#[async_trait]
impl Tool for NodesTool {
    fn name(&self) -> &str {
        "nodes"
    }

    fn description(&self) -> &str {
        "Discover and control paired nodes (status/describe/pairing/notify/camera/screen/location/run/media/invoke)."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": [
                        "status",
                        "describe",
                        "pending",
                        "approve",
                        "reject",
                        "notify",
                        "camera_snap",
                        "camera_list",
                        "camera_clip",
                        "screen_record",
                        "location_get",
                        "run",
                        "media_saveImage",
                        "invoke"
                    ],
                    "description": "Node action selector"
                },
                "gatewayUrl": {
                    "type": "string",
                    "description": "Reserved gateway URL override (compatibility field)"
                },
                "gatewayToken": {
                    "type": "string",
                    "description": "Reserved gateway token override (compatibility field)"
                },
                "timeoutMs": {
                    "type": "number",
                    "description": "Reserved gateway timeout override (compatibility field)"
                },
                "node": {
                    "type": "string",
                    "description": "Node id or Node Display Name/selector"
                },
                "requestId": {
                    "type": "string",
                    "description": "Pairing request id (approve/reject)"
                },
                "title": {
                    "type": "string",
                    "description": "Notification title for notify"
                },
                "body": {
                    "type": "string",
                    "description": "Notification body for notify"
                },
                "sound": {
                    "type": "string",
                    "description": "Optional sound hint for notify"
                },
                "priority": {
                    "type": "string",
                    "enum": ["passive", "active", "timeSensitive"],
                    "description": "Optional notify priority"
                },
                "delivery": {
                    "type": "string",
                    "enum": ["system", "overlay", "auto"],
                    "description": "Optional notify delivery mode"
                },
                "facing": {
                    "type": "string",
                    "enum": ["front", "back", "both"],
                    "description": "Camera facing for camera_snap/camera_clip"
                },
                "maxWidth": {
                    "type": "number",
                    "description": "camera_snap max width"
                },
                "quality": {
                    "type": "number",
                    "description": "camera_snap quality hint"
                },
                "delayMs": {
                    "type": "number",
                    "description": "camera_snap delay before capture"
                },
                "deviceId": {
                    "type": "string",
                    "description": "Optional camera device id"
                },
                "duration": {
                    "type": "string",
                    "description": "Duration string, e.g. 3s"
                },
                "durationMs": {
                    "type": "number",
                    "description": "Duration in milliseconds (camera_clip/screen_record)"
                },
                "includeAudio": {
                    "type": "boolean",
                    "description": "Whether to include audio for clip/record"
                },
                "fps": {
                    "type": "number",
                    "description": "screen_record frames per second"
                },
                "screenIndex": {
                    "type": "number",
                    "description": "screen_record display index"
                },
                "outPath": {
                    "type": "string",
                    "description": "Reserved output path for binary payload actions"
                },
                "maxAgeMs": {
                    "type": "number",
                    "description": "location_get cache age limit"
                },
                "locationTimeoutMs": {
                    "type": "number",
                    "description": "location_get timeout"
                },
                "desiredAccuracy": {
                    "type": "string",
                    "enum": ["coarse", "balanced", "precise"],
                    "description": "location_get accuracy preference"
                },
                "command": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "run action argv array"
                },
                "cwd": {
                    "type": "string",
                    "description": "run action cwd"
                },
                "env": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "run action env list, KEY=VALUE"
                },
                "commandTimeoutMs": {
                    "type": "number",
                    "description": "run action command timeout"
                },
                "invokeTimeoutMs": {
                    "type": "number",
                    "description": "Reserved invoke transport timeout (compatibility field)"
                },
                "needsScreenRecording": {
                    "type": "boolean",
                    "description": "run action screen recording hint"
                },
                "invokeCommand": {
                    "type": "string",
                    "description": "invoke action command name"
                },
                "invokeParamsJson": {
                    "type": "string",
                    "description": "invoke action JSON params string"
                },
                "path": {
                    "type": "string",
                    "description": "Local file path for media_saveImage (jpg/jpeg/png/gif/webp)"
                },
                "filename": {
                    "type": "string",
                    "description": "Optional target filename for media_saveImage"
                }
            },
            "required": ["action"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> Result<ToolResult> {
        let action = args
            .get("action")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("Missing 'action' parameter"))?;

        match action {
            "status" => {
                let nodes = self.registry.list();
                let payload = serde_json::json!({
                    "nodes": nodes,
                });
                Ok(ToolResult {
                    success: true,
                    output: Self::format_json_output(&payload),
                    error: None,
                })
            }
            "describe" => {
                let node_id = self.resolve_node(&args)?;
                match self.registry.describe(&node_id) {
                    Some(desc) => Ok(ToolResult {
                        success: true,
                        output: serde_json::to_string_pretty(&desc).unwrap_or_default(),
                        error: None,
                    }),
                    None => Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!("Node '{node_id}' not found or not connected")),
                    }),
                }
            }
            "pending" | "approve" | "reject" => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Action '{action}' is not supported yet in zeroclaw gateway node-control backend"
                )),
            }),
            "notify" => {
                let node_id = self.resolve_node(&args)?;
                let title = Self::read_optional_nonempty_string(&args, "title");
                let body = Self::read_optional_nonempty_string(&args, "body");
                if title.is_none() && body.is_none() {
                    return Err(anyhow::anyhow!(
                        "notify requires at least one of 'title' or 'body'"
                    ));
                }
                let arguments = serde_json::json!({
                    "title": title,
                    "body": body,
                    "sound": Self::read_optional_nonempty_string(&args, "sound"),
                    "priority": Self::read_optional_nonempty_string(&args, "priority"),
                    "delivery": Self::read_optional_nonempty_string(&args, "delivery"),
                });
                self.execute_invoke_action(&node_id, "system.notify", arguments)
                    .await
            }
            "camera_snap" => {
                let node_id = self.resolve_node(&args)?;
                let facing = Self::read_optional_nonempty_string(&args, "facing").unwrap_or("both");
                if !matches!(facing, "front" | "back" | "both") {
                    return Err(anyhow::anyhow!("invalid facing (front|back|both)"));
                }
                let facings: Vec<&str> = match facing {
                    "front" | "back" => vec![facing],
                    _ => vec!["front", "back"],
                };

                let mut results: Vec<serde_json::Value> = Vec::new();
                let mut files_out = String::new();

                for f in &facings {
                    let arguments = serde_json::json!({
                        "facing": f,
                        "maxWidth": args.get("maxWidth").cloned(),
                        "quality": args.get("quality").cloned(),
                        "format": "jpg",
                        "delayMs": args.get("delayMs").cloned(),
                        "deviceId": Self::read_optional_nonempty_string(&args, "deviceId"),
                    });
                    let res = self
                        .registry
                        .invoke(&node_id, "camera.snap", arguments)
                        .await
                        .map_err(|e| anyhow::anyhow!("camera.snap facing={}: {e}", f))?;
                    if !res.success {
                        return Ok(ToolResult {
                            success: false,
                            output: String::new(),
                            error: res.error.or_else(|| Some("invoke failed".to_string())),
                        });
                    }
                    let payload = Self::parse_result_output(&res.output);
                    let payload_obj = payload.as_object().ok_or_else(|| {
                        anyhow::anyhow!("camera.snap: expected object in response")
                    })?;
                    let b64 = payload_obj
                        .get("base64")
                        .and_then(Value::as_str)
                        .ok_or_else(|| anyhow::anyhow!("camera.snap: empty base64 in response"))?;
                    let b64_clean = b64.replace(['\n', '\r'], "");
                    let img_bytes = base64::engine::general_purpose::STANDARD
                        .decode(&b64_clean)
                        .or_else(|_| {
                            base64::engine::general_purpose::STANDARD_NO_PAD.decode(&b64_clean)
                        })
                        .map_err(|e| anyhow::anyhow!("camera.snap: base64 decode failed: {e}"))?;

                    let path = self.temp_media_path("snap", f, "jpg");
                    if let Some(parent) = path.parent() {
                        tokio::fs::create_dir_all(parent).await.map_err(|e| {
                            anyhow::anyhow!("camera.snap: mkdir: {e}")
                        })?;
                    }
                    tokio::fs::write(&path, &img_bytes)
                        .await
                        .map_err(|e| anyhow::anyhow!("camera.snap: write file: {e}"))?;

                    files_out.push_str(&format!("MEDIA:{}\n", path.display()));
                    results.push(serde_json::json!({
                        "facing": f,
                        "path": path.to_string_lossy(),
                        "width": payload_obj.get("width"),
                        "height": payload_obj.get("height"),
                    }));
                }

                let output = format!(
                    "{}{}",
                    files_out,
                    serde_json::to_string(&results).unwrap_or_default()
                );
                Ok(ToolResult {
                    success: true,
                    output,
                    error: None,
                })
            }
            "camera_list" => {
                let node_id = self.resolve_node(&args)?;
                self.execute_invoke_action(&node_id, "camera.list", serde_json::json!({}))
                    .await
            }
            "camera_clip" => {
                let node_id = self.resolve_node(&args)?;
                let facing = Self::read_optional_nonempty_string(&args, "facing").unwrap_or("front");
                if !matches!(facing, "front" | "back") {
                    return Err(anyhow::anyhow!("invalid facing (front|back)"));
                }
                let duration_ms = Self::read_duration_ms(&args, 3000)?;
                let arguments = serde_json::json!({
                    "facing": facing,
                    "durationMs": duration_ms,
                    "includeAudio": args.get("includeAudio").cloned().unwrap_or(serde_json::json!(true)),
                    "format": "mp4",
                    "deviceId": Self::read_optional_nonempty_string(&args, "deviceId"),
                });
                self.execute_invoke_action(&node_id, "camera.clip", arguments)
                    .await
            }
            "screen_record" => {
                let node_id = self.resolve_node(&args)?;
                let duration_ms = Self::read_duration_ms(&args, 10_000)?;
                let arguments = serde_json::json!({
                    "durationMs": duration_ms,
                    "fps": args.get("fps").cloned().unwrap_or(serde_json::json!(10)),
                    "screenIndex": args.get("screenIndex").cloned().unwrap_or(serde_json::json!(0)),
                    "format": "mp4",
                    "includeAudio": args.get("includeAudio").cloned().unwrap_or(serde_json::json!(true)),
                });
                self.execute_invoke_action(&node_id, "screen.record", arguments)
                    .await
            }
            "location_get" => {
                let node_id = self.resolve_node(&args)?;
                let desired_accuracy =
                    match Self::read_optional_nonempty_string(&args, "desiredAccuracy") {
                        Some(value) if matches!(value, "coarse" | "balanced" | "precise") => {
                            Some(value)
                        }
                        Some(_) => return Err(anyhow::anyhow!(
                            "invalid desiredAccuracy (coarse|balanced|precise)"
                        )),
                        None => None,
                    };
                let arguments = serde_json::json!({
                    "maxAgeMs": args.get("maxAgeMs").cloned(),
                    "desiredAccuracy": desired_accuracy,
                    "timeoutMs": args.get("locationTimeoutMs").cloned(),
                });
                self.execute_invoke_action(&node_id, "location.get", arguments)
                    .await
            }
            "invoke" => {
                let node_id = self.resolve_node(&args)?;
                let invoke_command = Self::read_required_string(&args, "invokeCommand")?;
                let invoke_params = if let Some(raw_json) =
                    Self::read_optional_nonempty_string(&args, "invokeParamsJson")
                {
                    serde_json::from_str::<Value>(raw_json).map_err(|error| {
                        anyhow::anyhow!("invokeParamsJson must be valid JSON: {error}")
                    })?
                } else {
                    serde_json::json!({})
                };
                self.execute_invoke_action(&node_id, &invoke_command, invoke_params)
                    .await
            }
            "run" => {
                let node_id = self.resolve_node(&args)?;
                let raw_command = args
                    .get("command")
                    .and_then(Value::as_array)
                    .ok_or_else(|| anyhow::anyhow!("command required (argv array)"))?;
                let command: Vec<String> = raw_command
                    .iter()
                    .map(|value| match value {
                        Value::String(s) => s.trim().to_string(),
                        _ => value.to_string(),
                    })
                    .filter(|value| !value.is_empty())
                    .collect();
                if command.is_empty() {
                    return Err(anyhow::anyhow!("command must not be empty"));
                }
                let run_params = serde_json::json!({
                    "command": command,
                    "cwd": Self::read_optional_nonempty_string(&args, "cwd"),
                    "env": Self::parse_env_pairs(&args),
                    "timeoutMs": Self::parse_timeout_ms(&args, "commandTimeoutMs"),
                    "needsScreenRecording": args.get("needsScreenRecording").cloned(),
                });
                self.execute_invoke_action(&node_id, "system.run", run_params)
                    .await
            }
            "media_saveImage" => {
                let node_id = self.resolve_node(&args)?;
                let params = Self::build_media_save_image_params(&args).await?;
                self.execute_invoke_action(&node_id, "media.saveImage", params)
                    .await
            }
            _ => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Unknown action: {action}")),
            }),
        }
    }
}
