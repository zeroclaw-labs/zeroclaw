use std::collections::HashMap;
use std::sync::OnceLock;

use serde::{Deserialize, Deserializer, Serialize};
use thiserror::Error;
use tokio::sync::Mutex;

// ── Error type ──────────────────────────────────────────────────────

#[derive(Debug, Error)]
#[error("Quilt API error {status}: [{error_code}] {message}")]
pub struct QuiltError {
    pub status: u16,
    pub error_code: String,
    pub message: String,
    pub hint: Option<String>,
}

#[derive(Debug, Deserialize)]
struct QuiltErrorBody {
    #[serde(default)]
    error_code: String,
    #[serde(default)]
    message: String,
    #[serde(default)]
    hint: Option<String>,
}

// ── Container state ─────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuiltContainerState {
    Created,
    Pending,
    Starting,
    Running,
    Exited,
    Error,
}

impl std::fmt::Display for QuiltContainerState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Created => write!(f, "created"),
            Self::Pending => write!(f, "pending"),
            Self::Starting => write!(f, "starting"),
            Self::Running => write!(f, "running"),
            Self::Exited => write!(f, "exited"),
            Self::Error => write!(f, "error"),
        }
    }
}

// ── Data types ──────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuiltContainerStatus {
    #[serde(alias = "container_id")]
    pub id: String,
    #[serde(default)]
    pub tenant_id: Option<String>,
    #[serde(default, deserialize_with = "deserialize_string_or_default")]
    pub name: String,
    pub state: QuiltContainerState,
    pub pid: Option<u32>,
    pub exit_code: Option<i32>,
    pub ip_address: Option<String>,
    pub memory_limit_mb: Option<u32>,
    pub cpu_limit_percent: Option<f64>,
    pub labels: Option<HashMap<String, String>>,
    #[serde(default, alias = "started_at", deserialize_with = "deserialize_opt_epoch_ms")]
    pub started_at_ms: Option<i64>,
    #[serde(default, alias = "exited_at", deserialize_with = "deserialize_opt_epoch_ms")]
    pub exited_at_ms: Option<i64>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum ContainersListResponse {
    Wrapped { containers: Vec<QuiltContainerStatus> },
    Direct(Vec<QuiltContainerStatus>),
}

#[derive(Debug, Deserialize)]
struct GetContainerByNameResponse {
    container_id: Option<String>,
    found: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuiltExecResult {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuiltCreateResult {
    pub container_id: String,
    pub name: String,
    pub ip_address: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VolumeMount {
    pub host_path: String,
    pub container_path: String,
    pub read_only: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortMapping {
    pub host_port: u16,
    pub container_port: u16,
    pub protocol: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuiltCreateParams {
    pub name: String,
    pub image: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<Vec<String>>,
    pub environment: HashMap<String, String>,
    pub volumes: Vec<VolumeMount>,
    pub ports: Vec<PortMapping>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory_limit_mb: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpu_limit_percent: Option<u32>,
    pub labels: HashMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub network: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub restart_policy: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuiltExecParams {
    pub command: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub working_dir: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub environment: Option<HashMap<String, String>>,
}

// ── Client ──────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct QuiltClient {
    http: reqwest::Client,
    api_url: String,
    api_key: String,
}

/// Global singleton for the Quilt client.
static QUILT_CLIENT: OnceLock<Mutex<Option<QuiltClient>>> = OnceLock::new();

fn client_slot() -> &'static Mutex<Option<QuiltClient>> {
    QUILT_CLIENT.get_or_init(|| Mutex::new(None))
}

/// Returns the global `QuiltClient`, creating it from the given URL and key
/// on first call. Subsequent calls ignore the arguments and return the
/// existing instance.
pub async fn get_client(api_url: &str, api_key: &str) -> Result<QuiltClient, anyhow::Error> {
    let mut slot = client_slot().lock().await;
    if let Some(ref c) = *slot {
        return Ok(c.clone());
    }
    let client = QuiltClient::new(api_url, api_key)?;
    *slot = Some(client.clone());
    Ok(client)
}

/// Resets the global singleton so the next `get_client` call creates a fresh
/// instance. Primarily useful for tests.
pub async fn reset_client() {
    let mut slot = client_slot().lock().await;
    *slot = None;
}

impl QuiltClient {
    /// Create a `QuiltClient` from environment variables.
    ///
    /// Reads `QUILT_API_URL` and `QUILT_API_KEY` from the environment.
    pub fn from_env() -> Result<Self, anyhow::Error> {
        let api_url =
            std::env::var("QUILT_API_URL").map_err(|_| anyhow::anyhow!("QUILT_API_URL not set"))?;
        let api_key =
            std::env::var("QUILT_API_KEY").map_err(|_| anyhow::anyhow!("QUILT_API_KEY not set"))?;
        Self::new(&api_url, &api_key)
    }

    /// Build a new `QuiltClient`.
    ///
    /// Requires the current Quilt key format: `quilt_sk_...`.
    pub fn new(api_url: &str, api_key: &str) -> Result<Self, anyhow::Error> {
        if !api_key.starts_with("quilt_sk_") {
            anyhow::bail!("Quilt API key must start with 'quilt_sk_' prefix");
        }

        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .build()?;

        Ok(Self {
            http,
            api_url: api_url.trim_end_matches('/').to_string(),
            api_key: api_key.to_string(),
        })
    }

    // ── Helpers ─────────────────────────────────────────────────

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.api_url)
    }

    fn auth_header(&self) -> (&str, &str) {
        ("X-Api-Key", &self.api_key)
    }

    async fn handle_error(&self, resp: reqwest::Response) -> QuiltError {
        let status = resp.status().as_u16();
        match resp.json::<QuiltErrorBody>().await {
            Ok(body) => QuiltError {
                status,
                error_code: body.error_code,
                message: body.message,
                hint: body.hint,
            },
            Err(_) => QuiltError {
                status,
                error_code: "UNKNOWN".into(),
                message: format!("HTTP {status}"),
                hint: None,
            },
        }
    }

    async fn parse_status_response(
        &self,
        resp: reqwest::Response,
    ) -> Result<QuiltContainerStatus, anyhow::Error> {
        if resp.status().is_success() {
            Ok(resp.json().await?)
        } else {
            Err(self.handle_error(resp).await.into())
        }
    }

    // ── Container CRUD ──────────────────────────────────────────

    /// Create a new container.
    /// POST /api/containers
    pub async fn create_container(
        &self,
        params: QuiltCreateParams,
    ) -> Result<QuiltCreateResult, anyhow::Error> {
        let (hdr_name, hdr_val) = self.auth_header();
        let resp = self
            .http
            .post(self.url("/api/containers"))
            .header(hdr_name, hdr_val)
            .json(&params)
            .send()
            .await?;

        if resp.status().is_success() {
            Ok(resp.json().await?)
        } else {
            Err(self.handle_error(resp).await.into())
        }
    }

    /// Get container status by ID.
    /// GET /api/containers/:id
    pub async fn get_container(&self, id: &str) -> Result<QuiltContainerStatus, anyhow::Error> {
        let (hdr_name, hdr_val) = self.auth_header();
        let resp = self
            .http
            .get(self.url(&format!("/api/containers/{id}")))
            .header(hdr_name, hdr_val)
            .send()
            .await?;

        self.parse_status_response(resp).await
    }

    /// Get container status by name.
    /// GET /api/containers/by-name/:name
    pub async fn get_container_by_name(
        &self,
        name: &str,
    ) -> Result<QuiltContainerStatus, anyhow::Error> {
        let (hdr_name, hdr_val) = self.auth_header();
        let resp = self
            .http
            .get(self.url(&format!("/api/containers/by-name/{name}")))
            .header(hdr_name, hdr_val)
            .send()
            .await?;

        if !resp.status().is_success() {
            return Err(self.handle_error(resp).await.into());
        }

        let payload: GetContainerByNameResponse = resp.json().await?;
        if !payload.found.unwrap_or(false) {
            anyhow::bail!("Container '{name}' not found");
        }
        let id = payload
            .container_id
            .ok_or_else(|| anyhow::anyhow!("Container '{name}' lookup returned no container_id"))?;
        self.get_container(&id).await
    }

    /// Start a container.
    /// POST /api/containers/:id/start
    pub async fn start_container(&self, id: &str) -> Result<(), anyhow::Error> {
        let (hdr_name, hdr_val) = self.auth_header();
        let resp = self
            .http
            .post(self.url(&format!("/api/containers/{id}/start")))
            .header(hdr_name, hdr_val)
            .send()
            .await?;

        if resp.status().is_success() {
            Ok(())
        } else {
            Err(self.handle_error(resp).await.into())
        }
    }

    /// Stop a container gracefully.
    /// POST /api/containers/:id/stop
    pub async fn stop_container(&self, id: &str) -> Result<(), anyhow::Error> {
        let (hdr_name, hdr_val) = self.auth_header();
        let resp = self
            .http
            .post(self.url(&format!("/api/containers/{id}/stop")))
            .header(hdr_name, hdr_val)
            .send()
            .await?;

        if resp.status().is_success() {
            Ok(())
        } else {
            Err(self.handle_error(resp).await.into())
        }
    }

    /// Kill a container immediately.
    /// POST /api/containers/:id/kill
    pub async fn kill_container(&self, id: &str) -> Result<(), anyhow::Error> {
        let (hdr_name, hdr_val) = self.auth_header();
        let resp = self
            .http
            .post(self.url(&format!("/api/containers/{id}/kill")))
            .header(hdr_name, hdr_val)
            .send()
            .await?;

        if resp.status().is_success() {
            Ok(())
        } else {
            Err(self.handle_error(resp).await.into())
        }
    }

    /// Delete a container.
    /// DELETE /api/containers/:id
    pub async fn delete_container(&self, id: &str) -> Result<(), anyhow::Error> {
        let (hdr_name, hdr_val) = self.auth_header();
        let resp = self
            .http
            .delete(self.url(&format!("/api/containers/{id}")))
            .header(hdr_name, hdr_val)
            .send()
            .await?;

        if resp.status().is_success() {
            Ok(())
        } else {
            Err(self.handle_error(resp).await.into())
        }
    }

    /// Execute a command inside a running container.
    /// POST /api/containers/:id/exec
    pub async fn exec(
        &self,
        id: &str,
        params: QuiltExecParams,
    ) -> Result<QuiltExecResult, anyhow::Error> {
        let (hdr_name, hdr_val) = self.auth_header();
        let resp = self
            .http
            .post(self.url(&format!("/api/containers/{id}/exec")))
            .header(hdr_name, hdr_val)
            .json(&params)
            .send()
            .await?;

        if resp.status().is_success() {
            Ok(resp.json().await?)
        } else {
            Err(self.handle_error(resp).await.into())
        }
    }

    /// List all containers.
    /// GET /api/containers
    pub async fn list_containers(&self) -> Result<Vec<QuiltContainerStatus>, anyhow::Error> {
        let (hdr_name, hdr_val) = self.auth_header();
        let resp = self
            .http
            .get(self.url("/api/containers"))
            .header(hdr_name, hdr_val)
            .send()
            .await?;

        if resp.status().is_success() {
            let payload: ContainersListResponse = resp.json().await?;
            Ok(match payload {
                ContainersListResponse::Wrapped { containers } => containers,
                ContainersListResponse::Direct(containers) => containers,
            })
        } else {
            Err(self.handle_error(resp).await.into())
        }
    }

    /// Accessor: base API URL (without trailing slash).
    pub fn api_url(&self) -> &str {
        &self.api_url
    }

    /// Accessor: API key.
    pub fn api_key(&self) -> &str {
        &self.api_key
    }
}

fn deserialize_string_or_default<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(Option::<String>::deserialize(deserializer)?.unwrap_or_default())
}

fn deserialize_opt_epoch_ms<'de, D>(deserializer: D) -> Result<Option<i64>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum EpochValue {
        I64(i64),
        U64(u64),
        F64(f64),
        Str(String),
    }

    let raw = Option::<EpochValue>::deserialize(deserializer)?;
    let value = match raw {
        Some(EpochValue::I64(v)) => v,
        Some(EpochValue::U64(v)) => v as i64,
        Some(EpochValue::F64(v)) => v as i64,
        Some(EpochValue::Str(v)) => v.parse::<i64>().map_err(serde::de::Error::custom)?,
        None => return Ok(None),
    };

    // Quilt timestamps are currently seconds. Normalize to milliseconds for internal logic.
    if value.abs() < 1_000_000_000_000 {
        Ok(Some(value * 1000))
    } else {
        Ok(Some(value))
    }
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Key validation ──────────────────────────────────────────

    #[test]
    fn client_rejects_key_without_prefix() {
        let err = QuiltClient::new("https://backend.quilt.sh", "bad-key-no-prefix");
        assert!(err.is_err());
        assert!(
            err.unwrap_err().to_string().contains("quilt_sk_"),
            "error should mention the required prefix"
        );
    }

    #[test]
    fn client_accepts_valid_key() {
        let client = QuiltClient::new("https://backend.quilt.sh", "quilt_sk_test_key_12345");
        assert!(client.is_ok());
    }

    #[test]
    fn client_strips_trailing_slash_from_url() {
        let client = QuiltClient::new("https://backend.quilt.sh/", "quilt_sk_test_key").unwrap();
        assert_eq!(client.api_url(), "https://backend.quilt.sh");
    }

    #[test]
    fn client_url_builder() {
        let client = QuiltClient::new("https://backend.quilt.sh", "quilt_sk_test_key").unwrap();
        assert_eq!(
            client.url("/api/containers"),
            "https://backend.quilt.sh/api/containers"
        );
        assert_eq!(
            client.url("/api/containers/abc-123/start"),
            "https://backend.quilt.sh/api/containers/abc-123/start"
        );
    }

    // ── Serialization ───────────────────────────────────────────

    #[test]
    fn create_params_serializes_correctly() {
        let params = QuiltCreateParams {
            name: "sandbox-001".into(),
            image: "ubuntu:22.04".into(),
            command: Some(vec!["bash".into(), "-c".into(), "sleep infinity".into()]),
            environment: HashMap::from([("FOO".into(), "bar".into())]),
            volumes: vec![VolumeMount {
                host_path: "/host/data".into(),
                container_path: "/data".into(),
                read_only: true,
            }],
            ports: vec![PortMapping {
                host_port: 8080,
                container_port: 80,
                protocol: "tcp".into(),
            }],
            memory_limit_mb: Some(4096),
            cpu_limit_percent: Some(100),
            labels: HashMap::from([("aria.sandbox".into(), "true".into())]),
            network: Some("bridge".into()),
            restart_policy: None,
        };

        let json = serde_json::to_value(&params).unwrap();
        assert_eq!(json["name"], "sandbox-001");
        assert_eq!(json["image"], "ubuntu:22.04");
        assert_eq!(json["command"][0], "bash");
        assert_eq!(json["environment"]["FOO"], "bar");
        assert_eq!(json["volumes"][0]["host_path"], "/host/data");
        assert!(json["volumes"][0]["read_only"].as_bool().unwrap());
        assert_eq!(json["ports"][0]["host_port"], 8080);
        assert_eq!(json["memory_limit_mb"], 4096);
        assert_eq!(json["labels"]["aria.sandbox"], "true");
        assert_eq!(json["network"], "bridge");
        // restart_policy is None, should be omitted
        assert!(json.get("restart_policy").is_none());
    }

    #[test]
    fn create_params_omits_none_fields() {
        let params = QuiltCreateParams {
            name: "test".into(),
            image: "alpine:latest".into(),
            command: None,
            environment: HashMap::new(),
            volumes: vec![],
            ports: vec![],
            memory_limit_mb: None,
            cpu_limit_percent: None,
            labels: HashMap::new(),
            network: None,
            restart_policy: None,
        };

        let json = serde_json::to_value(&params).unwrap();
        assert!(json.get("command").is_none());
        assert!(json.get("memory_limit_mb").is_none());
        assert!(json.get("cpu_limit_percent").is_none());
        assert!(json.get("network").is_none());
        assert!(json.get("restart_policy").is_none());
    }

    #[test]
    fn exec_params_serializes_correctly() {
        let params = QuiltExecParams {
            command: vec!["ls".into(), "-la".into()],
            timeout_ms: Some(30_000),
            working_dir: Some("/workspace".into()),
            environment: Some(HashMap::from([("PATH".into(), "/usr/bin".into())])),
        };

        let json = serde_json::to_value(&params).unwrap();
        assert_eq!(json["command"][0], "ls");
        assert_eq!(json["command"][1], "-la");
        assert_eq!(json["timeout_ms"], 30_000);
        assert_eq!(json["working_dir"], "/workspace");
        assert_eq!(json["environment"]["PATH"], "/usr/bin");
    }

    #[test]
    fn exec_params_omits_none_fields() {
        let params = QuiltExecParams {
            command: vec!["echo".into(), "hello".into()],
            timeout_ms: None,
            working_dir: None,
            environment: None,
        };

        let json = serde_json::to_value(&params).unwrap();
        assert!(json.get("timeout_ms").is_none());
        assert!(json.get("working_dir").is_none());
        assert!(json.get("environment").is_none());
    }

    // ── Response deserialization ─────────────────────────────────

    #[test]
    fn container_status_deserializes() {
        let json = serde_json::json!({
            "id": "ctr-abc123",
            "tenant_id": "tenant-1",
            "name": "sandbox-session-xyz",
            "state": "running",
            "pid": 12345,
            "exit_code": null,
            "ip_address": "10.0.0.5",
            "memory_limit_mb": 4096,
            "cpu_limit_percent": 100,
            "labels": {"aria.sandbox": "true", "aria.session_key": "xyz"},
            "started_at_ms": 1700000000000_i64,
            "exited_at_ms": null
        });

        let status: QuiltContainerStatus = serde_json::from_value(json).unwrap();
        assert_eq!(status.id, "ctr-abc123");
        assert_eq!(status.tenant_id.as_deref(), Some("tenant-1"));
        assert_eq!(status.name, "sandbox-session-xyz");
        assert_eq!(status.state, QuiltContainerState::Running);
        assert_eq!(status.pid, Some(12345));
        assert!(status.exit_code.is_none());
        assert_eq!(status.ip_address.as_deref(), Some("10.0.0.5"));
        assert_eq!(status.memory_limit_mb, Some(4096));
        assert_eq!(status.cpu_limit_percent, Some(100));
        let labels = status.labels.unwrap();
        assert_eq!(labels.get("aria.sandbox").unwrap(), "true");
        assert_eq!(status.started_at_ms, Some(1_700_000_000_000));
        assert!(status.exited_at_ms.is_none());
    }

    #[test]
    fn container_status_deserializes_minimal() {
        let json = serde_json::json!({
            "id": "ctr-min",
            "tenant_id": null,
            "name": "minimal",
            "state": "pending",
            "pid": null,
            "exit_code": null,
            "ip_address": null,
            "memory_limit_mb": null,
            "cpu_limit_percent": null,
            "labels": null,
            "started_at_ms": null,
            "exited_at_ms": null
        });

        let status: QuiltContainerStatus = serde_json::from_value(json).unwrap();
        assert_eq!(status.state, QuiltContainerState::Pending);
        assert!(status.tenant_id.is_none());
        assert!(status.pid.is_none());
        assert!(status.labels.is_none());
    }

    #[test]
    fn container_status_exited_state() {
        let json = serde_json::json!({
            "id": "ctr-exited",
            "tenant_id": null,
            "name": "done",
            "state": "exited",
            "pid": null,
            "exit_code": 0,
            "ip_address": null,
            "memory_limit_mb": null,
            "cpu_limit_percent": null,
            "labels": null,
            "started_at_ms": 1700000000000_i64,
            "exited_at_ms": 1700000060000_i64
        });

        let status: QuiltContainerStatus = serde_json::from_value(json).unwrap();
        assert_eq!(status.state, QuiltContainerState::Exited);
        assert_eq!(status.exit_code, Some(0));
        assert_eq!(status.exited_at_ms, Some(1_700_000_060_000));
    }

    #[test]
    fn container_status_error_state() {
        let json = serde_json::json!({
            "id": "ctr-err",
            "tenant_id": null,
            "name": "broken",
            "state": "error",
            "pid": null,
            "exit_code": 137,
            "ip_address": null,
            "memory_limit_mb": null,
            "cpu_limit_percent": null,
            "labels": null,
            "started_at_ms": null,
            "exited_at_ms": null
        });

        let status: QuiltContainerStatus = serde_json::from_value(json).unwrap();
        assert_eq!(status.state, QuiltContainerState::Error);
        assert_eq!(status.exit_code, Some(137));
    }

    #[test]
    fn exec_result_deserializes() {
        let json = serde_json::json!({
            "exit_code": 0,
            "stdout": "hello world\n",
            "stderr": ""
        });

        let result: QuiltExecResult = serde_json::from_value(json).unwrap();
        assert_eq!(result.exit_code, 0);
        assert_eq!(result.stdout, "hello world\n");
        assert!(result.stderr.is_empty());
    }

    #[test]
    fn exec_result_with_stderr() {
        let json = serde_json::json!({
            "exit_code": 1,
            "stdout": "",
            "stderr": "command not found\n"
        });

        let result: QuiltExecResult = serde_json::from_value(json).unwrap();
        assert_eq!(result.exit_code, 1);
        assert!(result.stdout.is_empty());
        assert_eq!(result.stderr, "command not found\n");
    }

    #[test]
    fn create_result_deserializes() {
        let json = serde_json::json!({
            "container_id": "ctr-new-abc",
            "name": "sandbox-session-1",
            "ip_address": "10.0.0.10"
        });

        let result: QuiltCreateResult = serde_json::from_value(json).unwrap();
        assert_eq!(result.container_id, "ctr-new-abc");
        assert_eq!(result.name, "sandbox-session-1");
        assert_eq!(result.ip_address.as_deref(), Some("10.0.0.10"));
    }

    #[test]
    fn create_result_without_ip() {
        let json = serde_json::json!({
            "container_id": "ctr-no-ip",
            "name": "sandbox-no-ip",
            "ip_address": null
        });

        let result: QuiltCreateResult = serde_json::from_value(json).unwrap();
        assert!(result.ip_address.is_none());
    }

    // ── State enum ──────────────────────────────────────────────

    #[test]
    fn state_display() {
        assert_eq!(QuiltContainerState::Pending.to_string(), "pending");
        assert_eq!(QuiltContainerState::Starting.to_string(), "starting");
        assert_eq!(QuiltContainerState::Running.to_string(), "running");
        assert_eq!(QuiltContainerState::Exited.to_string(), "exited");
        assert_eq!(QuiltContainerState::Error.to_string(), "error");
    }

    #[test]
    fn state_serde_roundtrip() {
        let states = vec![
            QuiltContainerState::Pending,
            QuiltContainerState::Starting,
            QuiltContainerState::Running,
            QuiltContainerState::Exited,
            QuiltContainerState::Error,
        ];
        for state in states {
            let json = serde_json::to_string(&state).unwrap();
            let parsed: QuiltContainerState = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, state);
        }
    }

    // ── Error type ──────────────────────────────────────────────

    #[test]
    fn quilt_error_display() {
        let err = QuiltError {
            status: 404,
            error_code: "CONTAINER_NOT_FOUND".into(),
            message: "Container ctr-abc not found".into(),
            hint: Some("Check the container ID".into()),
        };
        let display = err.to_string();
        assert!(display.contains("404"));
        assert!(display.contains("CONTAINER_NOT_FOUND"));
        assert!(display.contains("Container ctr-abc not found"));
    }

    #[test]
    fn quilt_error_body_deserializes() {
        let json = serde_json::json!({
            "error_code": "RATE_LIMITED",
            "message": "Too many requests",
            "hint": "Retry after 60 seconds"
        });

        let body: QuiltErrorBody = serde_json::from_value(json).unwrap();
        assert_eq!(body.error_code, "RATE_LIMITED");
        assert_eq!(body.message, "Too many requests");
        assert_eq!(body.hint.as_deref(), Some("Retry after 60 seconds"));
    }

    #[test]
    fn quilt_error_body_minimal() {
        let json = serde_json::json!({});
        let body: QuiltErrorBody = serde_json::from_value(json).unwrap();
        assert!(body.error_code.is_empty());
        assert!(body.message.is_empty());
        assert!(body.hint.is_none());
    }

    // ── Volume and port types ───────────────────────────────────

    #[test]
    fn volume_mount_serde() {
        let vol = VolumeMount {
            host_path: "/tmp/data".into(),
            container_path: "/mnt/data".into(),
            read_only: false,
        };
        let json = serde_json::to_value(&vol).unwrap();
        assert_eq!(json["host_path"], "/tmp/data");
        assert_eq!(json["container_path"], "/mnt/data");
        assert!(!json["read_only"].as_bool().unwrap());

        let parsed: VolumeMount = serde_json::from_value(json).unwrap();
        assert_eq!(parsed.host_path, "/tmp/data");
        assert!(!parsed.read_only);
    }

    #[test]
    fn port_mapping_serde() {
        let port = PortMapping {
            host_port: 3000,
            container_port: 80,
            protocol: "tcp".into(),
        };
        let json = serde_json::to_value(&port).unwrap();
        assert_eq!(json["host_port"], 3000);
        assert_eq!(json["container_port"], 80);
        assert_eq!(json["protocol"], "tcp");

        let parsed: PortMapping = serde_json::from_value(json).unwrap();
        assert_eq!(parsed.host_port, 3000);
    }

    // ── Singleton ───────────────────────────────────────────────

    #[tokio::test]
    async fn singleton_reset_clears_client() {
        reset_client().await;
        let c = get_client("https://example.com", "quilt_sk_test_key")
            .await
            .unwrap();
        assert_eq!(c.api_url(), "https://example.com");

        reset_client().await;
        let c2 = get_client("https://other.com", "quilt_sk_other_key")
            .await
            .unwrap();
        assert_eq!(c2.api_url(), "https://other.com");

        // Clean up
        reset_client().await;
    }

    // ── List deserialization ────────────────────────────────────

    #[test]
    fn list_containers_deserializes_array() {
        let json = serde_json::json!([
            {
                "id": "ctr-1",
                "tenant_id": null,
                "name": "sandbox-a",
                "state": "running",
                "pid": 100,
                "exit_code": null,
                "ip_address": "10.0.0.1",
                "memory_limit_mb": 2048,
                "cpu_limit_percent": 50,
                "labels": {"aria.sandbox": "true"},
                "started_at_ms": 1700000000000_i64,
                "exited_at_ms": null
            },
            {
                "id": "ctr-2",
                "tenant_id": null,
                "name": "sandbox-b",
                "state": "exited",
                "pid": null,
                "exit_code": 0,
                "ip_address": null,
                "memory_limit_mb": null,
                "cpu_limit_percent": null,
                "labels": null,
                "started_at_ms": 1700000000000_i64,
                "exited_at_ms": 1700000001000_i64
            }
        ]);

        let list: Vec<QuiltContainerStatus> = serde_json::from_value(json).unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].id, "ctr-1");
        assert_eq!(list[0].state, QuiltContainerState::Running);
        assert_eq!(list[1].id, "ctr-2");
        assert_eq!(list[1].state, QuiltContainerState::Exited);
    }
}
