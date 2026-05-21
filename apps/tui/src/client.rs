//! JSON-RPC 2.0 client over a Unix socket (NDJSON framing).
//!
//! Wraps [`RpcOutbound`] from `zeroclaw-api` — the same request/response
//! plumbing the daemon uses for bidirectional calls.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::mpsc;

use zeroclaw_api::jsonrpc::{self, JsonRpcError, RpcOutbound, field};
use zeroclaw_config::traits::ConfigFieldEntry;

// ── Wire method names used by the TUI ────────────────────────────

pub mod method {
    pub const INITIALIZE: &str = "initialize";
    pub const CONFIG_LIST: &str = "config/list";
    pub const CONFIG_SET: &str = "config/set";
    pub const CONFIG_DELETE: &str = "config/delete";
    pub const CONFIG_MAP_KEYS: &str = "config/map-keys";
    pub const CONFIG_MAP_KEY_CREATE: &str = "config/map-key-create";
    pub const CONFIG_MAP_KEY_DELETE: &str = "config/map-key-delete";
    pub const CONFIG_MAP_KEY_RENAME: &str = "config/map-key-rename";
    pub const CONFIG_TEMPLATES: &str = "config/templates";
    pub const CONFIG_VALIDATE: &str = "config/validate";
    pub const ONBOARD_SECTIONS: &str = "onboard/sections";
    pub const ONBOARD_STATUS: &str = "onboard/status";
}

// ── Socket path resolution ───────────────────────────────────────

/// Resolve the daemon socket path.
/// `$ZEROCLAW_SOCKET` > `<config_dir>/data/daemon.sock`.
pub fn resolve_socket_path() -> Result<PathBuf> {
    if let Ok(p) = std::env::var("ZEROCLAW_SOCKET") {
        let p = p.trim();
        if !p.is_empty() {
            return Ok(PathBuf::from(p));
        }
    }
    Ok(resolve_config_dir()?.join("data").join("daemon.sock"))
}

/// Resolve config dir: `$ZEROCLAW_CONFIG_DIR` > `~/.zeroclaw`.
pub fn resolve_config_dir() -> Result<PathBuf> {
    if let Ok(d) = std::env::var("ZEROCLAW_CONFIG_DIR") {
        let d = d.trim();
        if !d.is_empty() {
            return Ok(PathBuf::from(d));
        }
    }
    let home = std::env::var("HOME").context("HOME not set")?;
    Ok(PathBuf::from(home).join(".zeroclaw"))
}

// ── Client ───────────────────────────────────────────────────────

pub struct RpcClient {
    rpc: Arc<RpcOutbound>,
    _read_task: tokio::task::JoinHandle<()>,
    pub server_version: String,
}

impl RpcClient {
    /// Connect to the daemon socket and complete the `initialize` handshake.
    pub async fn connect(socket: &Path) -> Result<Self> {
        let stream = UnixStream::connect(socket)
            .await
            .with_context(|| format!("connecting to {}", socket.display()))?;
        let (read_half, write_half) = stream.into_split();

        let (writer_tx, mut writer_rx) = mpsc::channel::<String>(64);
        tokio::spawn(async move {
            let mut w = write_half;
            while let Some(mut line) = writer_rx.recv().await {
                if !line.ends_with('\n') {
                    line.push('\n');
                }
                if w.write_all(line.as_bytes()).await.is_err() {
                    break;
                }
            }
        });

        let rpc = Arc::new(RpcOutbound::new(writer_tx));

        let rpc_for_reader = rpc.clone();
        let read_task = tokio::spawn(async move {
            let mut reader = BufReader::new(read_half);
            let mut buf = String::new();
            loop {
                buf.clear();
                match reader.read_line(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
                let frame: Value = match serde_json::from_str(buf.trim()) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                if let Some(id) = frame.get(field::ID).and_then(Value::as_str) {
                    let result = frame.get(field::RESULT).cloned();
                    let error: Option<JsonRpcError> = frame
                        .get(field::ERROR)
                        .and_then(|e| serde_json::from_value(e.clone()).ok());
                    rpc_for_reader.dispatch_response(id, result, error);
                }
            }
        });

        let init_params = serde_json::json!({
            "protocol_version": jsonrpc::ACP_PROTOCOL_VERSION
        });
        let resp = rpc
            .request(method::INITIALIZE, init_params)
            .await
            .map_err(|e| anyhow::anyhow!("initialize: {} ({})", e.message, e.code))?;

        let server_version = resp
            .get("server_version")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string();

        Ok(Self {
            rpc,
            _read_task: read_task,
            server_version,
        })
    }

    pub async fn call<T: DeserializeOwned>(&self, method: &str, params: Value) -> Result<T> {
        let result = self
            .rpc
            .request(method, params)
            .await
            .map_err(|e| anyhow::anyhow!("RPC {method}: {} ({})", e.message, e.code))?;
        serde_json::from_value(result).with_context(|| format!("deserializing {method} result"))
    }

    // ── Typed config helpers ─────────────────────────────────────

    pub async fn config_list(&self, prefix: Option<&str>) -> Result<Vec<ConfigFieldEntry>> {
        let result: ConfigListResult = self
            .call(method::CONFIG_LIST, serde_json::json!({ "prefix": prefix }))
            .await?;
        Ok(result.entries)
    }

    pub async fn config_set(&self, prop: &str, value: Value) -> Result<()> {
        let _: ConfigSetResult = self
            .call(
                method::CONFIG_SET,
                serde_json::json!({ "prop": prop, "value": value }),
            )
            .await?;
        Ok(())
    }

    pub async fn config_delete(&self, prop: &str) -> Result<()> {
        let _: ConfigDeleteResult = self
            .call(method::CONFIG_DELETE, serde_json::json!({ "prop": prop }))
            .await?;
        Ok(())
    }

    pub async fn onboard_sections(&self) -> Result<Vec<OnboardSectionEntry>> {
        let result: OnboardSectionsResult = self
            .call(method::ONBOARD_SECTIONS, serde_json::json!({}))
            .await?;
        Ok(result.sections)
    }

    pub async fn config_map_keys(&self, path: &str) -> Result<Vec<String>> {
        let result: ConfigMapKeysResult = self
            .call(method::CONFIG_MAP_KEYS, serde_json::json!({ "path": path }))
            .await?;
        Ok(result.keys)
    }

    pub async fn config_map_key_create(&self, path: &str, key: &str) -> Result<()> {
        let _: Value = self
            .call(
                method::CONFIG_MAP_KEY_CREATE,
                serde_json::json!({ "path": path, "key": key }),
            )
            .await?;
        Ok(())
    }

    pub async fn config_map_key_delete(&self, path: &str, key: &str) -> Result<()> {
        let _: Value = self
            .call(
                method::CONFIG_MAP_KEY_DELETE,
                serde_json::json!({ "path": path, "key": key }),
            )
            .await?;
        Ok(())
    }

    pub async fn config_templates(&self) -> Result<Vec<ConfigTemplateEntry>> {
        let result: ConfigTemplatesResult = self
            .call(method::CONFIG_TEMPLATES, serde_json::json!({}))
            .await?;
        Ok(result.templates)
    }
}

// ── Response types (client-side, minimal) ────────────────────────

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ConfigListResult {
    pub entries: Vec<ConfigFieldEntry>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ConfigSetResult {
    pub prop: String,
    pub set: bool,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ConfigDeleteResult {
    pub prop: String,
    pub deleted: bool,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ConfigMapKeysResult {
    pub path: String,
    pub keys: Vec<String>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct OnboardSectionsResult {
    pub sections: Vec<OnboardSectionEntry>,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct OnboardSectionEntry {
    pub key: String,
    pub label: String,
    pub help: String,
    pub has_picker: bool,
    pub completed: bool,
    #[serde(default)]
    pub shape: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ConfigTemplatesResult {
    pub templates: Vec<ConfigTemplateEntry>,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ConfigTemplateEntry {
    pub path: String,
    pub kind: String,
    pub value_type: String,
    pub description: String,
}
