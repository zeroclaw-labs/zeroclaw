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
use tokio::sync::{broadcast, mpsc};

use zeroclaw_api::jsonrpc::{self, JsonRpcError, RpcOutbound, field};
use zeroclaw_config::sections::SectionShape;
use zeroclaw_config::traits::{ConfigFieldEntry, MapKeyKind};

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
    pub const CONFIG_SECTIONS: &str = "config/sections";
    pub const CONFIG_STATUS: &str = "config/status";
    pub const CONFIG_CATALOG_MODELS: &str = "config/catalog-models";
    // Personality
    pub const PERSONALITY_LIST: &str = "personality/list";
    pub const PERSONALITY_GET: &str = "personality/get";
    pub const PERSONALITY_PUT: &str = "personality/put";
    pub const PERSONALITY_TEMPLATES: &str = "personality/templates";
    // Skills
    pub const SKILLS_LIST: &str = "skills/list";
    pub const SKILLS_READ: &str = "skills/read";
    pub const SKILLS_WRITE: &str = "skills/write";
    pub const SKILLS_CREATE: &str = "skills/write";
    pub const SKILLS_DELETE: &str = "skills/delete";
}

// ── Socket path resolution ───────────────────────────────────────

/// Resolve the daemon socket path.
/// CLI flag > `$ZEROCLAW_SOCKET` > `<config_dir>/data/daemon.sock`.
pub fn resolve_socket_path(config_dir: &Path) -> Result<PathBuf> {
    if let Ok(p) = std::env::var("ZEROCLAW_SOCKET") {
        let p = p.trim();
        if !p.is_empty() {
            return Ok(PathBuf::from(p));
        }
    }
    Ok(config_dir.join("data").join("daemon.sock"))
}

/// Resolve config dir: CLI flag > `$ZEROCLAW_CONFIG_DIR` > `~/.zeroclaw`.
pub fn resolve_config_dir(cli_override: Option<&Path>) -> Result<PathBuf> {
    if let Some(dir) = cli_override {
        return Ok(dir.to_path_buf());
    }
    if let Ok(d) = std::env::var("ZEROCLAW_CONFIG_DIR") {
        let d = d.trim();
        if !d.is_empty() {
            return Ok(PathBuf::from(d));
        }
    }
    let home = std::env::var("HOME").context("HOME not set")?;
    Ok(PathBuf::from(home).join(".zeroclaw"))
}

// ── Notifications ────────────────────────────────────────────────

/// A server-initiated notification (no `id` field).
#[derive(Debug, Clone)]
pub struct RpcNotification {
    pub method: String,
    pub params: Value,
}

// ── Client ───────────────────────────────────────────────────────

pub struct RpcClient {
    rpc: Arc<RpcOutbound>,
    _read_task: tokio::task::JoinHandle<()>,
    pub server_version: String,
    notifications: broadcast::Sender<RpcNotification>,
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
        let (notif_tx, _) = broadcast::channel::<RpcNotification>(256);
        let notif_tx_for_reader = notif_tx.clone();

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
                } else if let Some(method) = frame.get(field::METHOD).and_then(Value::as_str) {
                    let params = frame.get("params").cloned().unwrap_or(Value::Null);
                    let _ = notif_tx_for_reader.send(RpcNotification {
                        method: method.to_string(),
                        params,
                    });
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
            notifications: notif_tx,
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

    // ── Notifications ─────────────────────────────────────────────

    /// Get a receiver for server-initiated notifications.
    pub fn subscribe_notifications(&self) -> broadcast::Receiver<RpcNotification> {
        self.notifications.subscribe()
    }

    /// Ask the daemon to start streaming log events as notifications.
    pub async fn logs_subscribe(&self) -> Result<()> {
        let _: Value = self.call("logs/subscribe", serde_json::json!({})).await?;
        Ok(())
    }

    /// Query persisted log events from the daemon.
    pub async fn logs_query(&self, params: LogsQueryParams) -> Result<LogsQueryResult> {
        self.call("logs/query", serde_json::to_value(params)?).await
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

    pub async fn config_sections(&self) -> Result<Vec<ConfigSectionEntry>> {
        let result: ConfigSectionsResult = self
            .call(method::CONFIG_SECTIONS, serde_json::json!({}))
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

    pub async fn catalog_models(&self, provider: &str) -> Result<Vec<String>> {
        let result: CatalogModelsResult = self
            .call(
                method::CONFIG_CATALOG_MODELS,
                serde_json::json!({ "model_provider": provider }),
            )
            .await?;
        Ok(result.models)
    }

    // ── Personality helpers ──────────────────────────────────────

    pub async fn personality_list(&self, agent: Option<&str>) -> Result<PersonalityListResult> {
        self.call(
            method::PERSONALITY_LIST,
            serde_json::json!({ "agent": agent }),
        )
        .await
    }

    pub async fn personality_get(
        &self,
        agent: &str,
        filename: &str,
    ) -> Result<PersonalityGetResult> {
        self.call(
            method::PERSONALITY_GET,
            serde_json::json!({ "agent": agent, "filename": filename }),
        )
        .await
    }

    pub async fn personality_put(
        &self,
        agent: &str,
        filename: &str,
        content: &str,
    ) -> Result<PersonalityPutResult> {
        self.call(
            method::PERSONALITY_PUT,
            serde_json::json!({ "agent": agent, "filename": filename, "content": content }),
        )
        .await
    }

    pub async fn personality_templates(
        &self,
        agent: Option<&str>,
    ) -> Result<PersonalityTemplatesResult> {
        self.call(
            method::PERSONALITY_TEMPLATES,
            serde_json::json!({ "agent": agent }),
        )
        .await
    }

    // ── Skills helpers ───────────────────────────────────────────

    pub async fn skills_list(&self, bundle: Option<&str>) -> Result<SkillsListResult> {
        self.call(method::SKILLS_LIST, serde_json::json!({ "bundle": bundle }))
            .await
    }

    pub async fn skills_read(&self, bundle: &str, name: &str) -> Result<SkillsReadResult> {
        self.call(
            method::SKILLS_READ,
            serde_json::json!({ "bundle": bundle, "name": name }),
        )
        .await
    }

    pub async fn skills_write(
        &self,
        bundle: &str,
        name: &str,
        frontmatter: &SkillFrontmatter,
        body: &str,
    ) -> Result<SkillsWriteResult> {
        self.call(
            method::SKILLS_WRITE,
            serde_json::json!({
                "bundle": bundle,
                "name": name,
                "frontmatter": frontmatter,
                "body": body,
            }),
        )
        .await
    }

    pub async fn skills_delete(&self, bundle: &str, name: &str) -> Result<SkillsDeleteResult> {
        self.call(
            method::SKILLS_DELETE,
            serde_json::json!({ "bundle": bundle, "name": name }),
        )
        .await
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
pub struct ConfigSectionsResult {
    pub sections: Vec<ConfigSectionEntry>,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ConfigSectionEntry {
    pub key: String,
    pub label: String,
    pub help: String,
    pub has_picker: bool,
    pub completed: bool,
    #[serde(default)]
    pub shape: Option<SectionShape>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ConfigTemplatesResult {
    pub templates: Vec<ConfigTemplateEntry>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct CatalogModelsResult {
    pub models: Vec<String>,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ConfigTemplateEntry {
    pub path: String,
    pub kind: MapKeyKind,
    pub value_type: String,
    pub description: String,
}

// ── Personality types ────────────────────────────────────────────

#[derive(Debug, Clone, serde::Deserialize)]
pub struct PersonalityFileEntry {
    pub filename: String,
    pub exists: bool,
    #[serde(default)]
    pub size: u64,
}

#[derive(Debug, serde::Deserialize)]
pub struct PersonalityListResult {
    pub files: Vec<PersonalityFileEntry>,
    pub max_chars: usize,
}

#[derive(Debug, serde::Deserialize)]
pub struct PersonalityGetResult {
    pub filename: String,
    #[serde(default)]
    pub content: Option<String>,
    pub exists: bool,
    #[serde(default)]
    pub truncated: bool,
}

#[derive(Debug, serde::Deserialize)]
pub struct PersonalityPutResult {
    pub bytes_written: u64,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct TemplateFileEntry {
    pub filename: String,
    pub content: String,
}

#[derive(Debug, serde::Deserialize)]
pub struct PersonalityTemplatesResult {
    pub files: Vec<TemplateFileEntry>,
}

// ── Skills types ─────────────────────────────────────────────────

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct SkillFrontmatter {
    pub name: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct SkillListEntry {
    pub bundle: String,
    pub name: String,
}

#[derive(Debug, serde::Deserialize)]
pub struct SkillsListResult {
    pub skills: Vec<SkillListEntry>,
}

#[derive(Debug, serde::Deserialize)]
pub struct SkillsReadResult {
    pub bundle: String,
    pub name: String,
    pub frontmatter: SkillFrontmatter,
    pub body: String,
}

#[derive(Debug, serde::Deserialize)]
pub struct SkillsWriteResult {
    pub bundle: String,
    pub name: String,
    pub written: bool,
}

#[derive(Debug, serde::Deserialize)]
pub struct SkillsDeleteResult {
    pub bundle: String,
    pub name: String,
    pub deleted: bool,
}

// ── Logs types ───────────────────────────────────────────────────

#[derive(Debug, Default, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub struct LogsQueryParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub since_ts: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub until_ts: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub until_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub severity_min: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub q: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
    #[serde(default)]
    pub hide_internal: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct LogsQueryResult {
    pub events: Vec<serde_json::Value>,
    pub next_cursor: Option<(String, String)>,
    pub at_end: bool,
}
