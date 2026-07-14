use super::enriched::{
    CleanupSupport, EnrichedMemory, EnricherCapabilities, EnrichmentCleanupRequest,
    EnrichmentRecallRequest, EnrichmentStoreRequest, MemoryEnricher, RecallScope, RecallSupport,
    ResultKind,
};
#[cfg(test)]
use super::sqlite::SqliteMemory;
use super::traits::{MemoryCategory, MemoryEntry};
use anyhow::Context;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::cmp::Ordering;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
#[cfg(windows)]
use tokio::time::sleep;
use tokio::time::{Instant, timeout};
use uuid::Uuid;
use zeroclaw_api::attribution::MemoryKind;
use zeroclaw_config::schema::ShodhEnrichmentConfig;

const UNSCOPED_USER_ID: &str = "zeroclaw-unscoped";
const MARKER_TAG: &str = "zeroclaw";
const KEY_TAG_PREFIX: &str = "zc-key:";
const CATEGORY_TAG_PREFIX: &str = "zc-category:";
const NAMESPACE_TAG_PREFIX: &str = "zc-namespace:";
const SESSION_TAG_PREFIX: &str = "zc-session:";
const IPC_PROTOCOL_VERSION: u8 = 1;
const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;

/// Local-IPC connector for an independently supervised Shodh server.
pub struct ShodhConnector {
    socket_path: PathBuf,
    api_key: String,
    recall_timeout: Duration,
    store_timeout: Duration,
}

/// SQLite-authoritative memory enriched by Shodh semantic recall.
pub type ShodhEnrichedMemory = EnrichedMemory<ShodhConnector>;

#[derive(Serialize)]
struct UpsertRequest<'a> {
    user_id: &'a str,
    external_id: String,
    content: &'a str,
    tags: Vec<String>,
    memory_type: &'static str,
    change_type: &'static str,
    importance: Option<f64>,
}

#[derive(Serialize)]
struct RecallRequest<'a> {
    user_id: &'a str,
    query: &'a str,
    limit: usize,
    mode: &'static str,
    tags: Vec<&'static str>,
}

#[derive(Deserialize)]
struct RecallResponse {
    #[serde(default)]
    memories: Vec<ShodhRecallMemory>,
}

#[derive(Deserialize)]
struct ShodhRecallMemory {
    id: String,
    experience: ShodhExperience,
    importance: f64,
    created_at: String,
    score: f64,
}

#[derive(Deserialize)]
struct ShodhExperience {
    content: String,
    #[serde(default)]
    memory_type: Option<String>,
    #[serde(default)]
    tags: Vec<String>,
}

#[derive(Serialize)]
struct ForgetByTagsRequest<'a> {
    user_id: &'a str,
    tags: Vec<String>,
}

#[derive(Serialize)]
struct IpcRequest<'a> {
    v: u8,
    id: &'a str,
    auth: &'a str,
    method: &'a str,
    path: &'a str,
    body: Value,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct IpcResponse {
    v: u8,
    id: String,
    status: u16,
    body: Value,
}

#[cfg(unix)]
type LocalStream = tokio::net::UnixStream;

#[cfg(windows)]
type LocalStream = tokio::net::windows::named_pipe::NamedPipeClient;

#[cfg(unix)]
async fn open_local_stream(path: &Path) -> anyhow::Result<LocalStream> {
    tokio::net::UnixStream::connect(path)
        .await
        .with_context(|| format!("connecting to Shodh socket {}", path.display()))
}

#[cfg(windows)]
async fn open_local_stream(path: &Path) -> anyhow::Result<LocalStream> {
    use tokio::net::windows::named_pipe::ClientOptions;

    let name = path.to_string_lossy().into_owned();
    loop {
        match ClientOptions::new().open(&name) {
            Ok(client) => return Ok(client),
            Err(error) if error.raw_os_error() == Some(231) => {
                // ERROR_PIPE_BUSY. The operation-wide deadline bounds retries.
                sleep(Duration::from_millis(20)).await;
            }
            Err(error) => {
                return Err(anyhow::Error::from(error))
                    .with_context(|| format!("connecting to Shodh pipe {name}"));
            }
        }
    }
}

impl ShodhConnector {
    pub(crate) fn new(config: &ShodhEnrichmentConfig) -> anyhow::Result<Self> {
        let socket_path = config.socket_path.trim();
        if socket_path.is_empty() {
            anyhow::bail!(
                "memory enricher 'shodh' requires `socket_path` in \
                 [memory_enrichment.shodh.<alias>]"
            );
        }
        let api_key = config
            .api_key
            .as_deref()
            .map(str::trim)
            .filter(|key| !key.is_empty())
            .ok_or_else(|| {
                anyhow::Error::msg(
                    "memory enricher 'shodh' requires `api_key` in \
                     [memory_enrichment.shodh.<alias>]",
                )
            })?
            .to_string();
        if config.recall_timeout_ms == 0 || config.store_timeout_ms == 0 {
            anyhow::bail!("Shodh recall and store timeouts must be greater than zero");
        }

        Ok(Self {
            socket_path: PathBuf::from(socket_path),
            api_key,
            recall_timeout: Duration::from_millis(config.recall_timeout_ms),
            store_timeout: Duration::from_millis(config.store_timeout_ms),
        })
    }

    async fn request<T: Serialize>(
        &self,
        method: &'static str,
        path: &str,
        body: &T,
        timeout_window: Duration,
    ) -> anyhow::Result<Value> {
        let id = Uuid::new_v4().to_string();
        let body = serde_json::to_value(body).context("serializing Shodh IPC request body")?;
        let request = IpcRequest {
            v: IPC_PROTOCOL_VERSION,
            id: &id,
            auth: &self.api_key,
            method,
            path,
            body,
        };
        let mut frame = serde_json::to_vec(&request).context("serializing Shodh IPC request")?;
        if frame.len().saturating_add(1) > MAX_FRAME_BYTES {
            anyhow::bail!("Shodh IPC request exceeds {MAX_FRAME_BYTES} bytes");
        }
        frame.push(b'\n');

        let response = timeout(timeout_window, self.exchange(&frame))
            .await
            .map_err(|_| {
                anyhow::Error::msg(format!(
                    "Shodh IPC {method} {path} timed out after {}ms",
                    timeout_window.as_millis()
                ))
            })??;
        if !response.ends_with(b"\n") {
            anyhow::bail!("Shodh IPC response ended before its newline delimiter");
        }
        let response: IpcResponse = serde_json::from_slice(&response).map_err(|error| {
            anyhow::Error::msg(format!(
                "decoding Shodh IPC response: {}",
                self.redact(error)
            ))
        })?;
        if response.v != IPC_PROTOCOL_VERSION {
            anyhow::bail!(
                "Shodh IPC protocol version mismatch: expected {}, received {}",
                IPC_PROTOCOL_VERSION,
                response.v
            );
        }
        if response.id != id {
            anyhow::bail!("Shodh IPC response id did not match its request");
        }
        if !(100..=599).contains(&response.status) {
            anyhow::bail!("Shodh IPC response status was invalid");
        }
        if !(200..300).contains(&response.status) {
            let detail = response
                .body
                .get("error")
                .or_else(|| response.body.get("message"))
                .and_then(Value::as_str)
                .map(|message| message.replace(&self.api_key, "[redacted]"))
                .map(|message| message.chars().take(512).collect::<String>());
            if let Some(detail) = detail {
                anyhow::bail!(
                    "Shodh IPC {method} {path} returned status {}: {detail}",
                    response.status
                );
            }
            anyhow::bail!(
                "Shodh IPC {method} {path} returned status {}",
                response.status
            );
        }
        Ok(response.body)
    }

    #[cfg(any(unix, windows))]
    async fn exchange(&self, request: &[u8]) -> anyhow::Result<Vec<u8>> {
        let stream = open_local_stream(&self.socket_path).await?;
        let (read_half, mut write_half) = tokio::io::split(stream);
        write_half
            .write_all(request)
            .await
            .context("writing Shodh IPC request")?;
        write_half
            .flush()
            .await
            .context("flushing Shodh IPC request")?;

        let mut reader = BufReader::new(read_half);
        let mut limited = (&mut reader).take((MAX_FRAME_BYTES + 1) as u64);
        let mut response = Vec::new();
        let bytes_read = limited
            .read_until(b'\n', &mut response)
            .await
            .context("reading Shodh IPC response")?;
        if bytes_read == 0 {
            anyhow::bail!("Shodh IPC server closed without a response");
        }
        if response.len() > MAX_FRAME_BYTES {
            anyhow::bail!("Shodh IPC response exceeds {MAX_FRAME_BYTES} bytes");
        }
        let mut trailing = Vec::new();
        limited
            .read_to_end(&mut trailing)
            .await
            .context("finishing Shodh IPC response")?;
        if !trailing.is_empty() {
            anyhow::bail!("Shodh IPC response contained data after its newline delimiter");
        }
        Ok(response)
    }

    #[cfg(not(any(unix, windows)))]
    async fn exchange(&self, _request: &[u8]) -> anyhow::Result<Vec<u8>> {
        anyhow::bail!("Shodh local IPC is unsupported on this platform")
    }

    fn user_id(agent_id: Option<&str>) -> &str {
        agent_id.unwrap_or(UNSCOPED_USER_ID)
    }

    fn redact(&self, value: impl ToString) -> String {
        value.to_string().replace(&self.api_key, "[redacted]")
    }

    fn external_id(key: &str) -> String {
        format!("zeroclaw:{}", Self::digest(key))
    }

    fn digest(value: &str) -> String {
        let bytes = Sha256::digest(value.as_bytes());
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    fn encode_tag_value(value: &str) -> String {
        value
            .as_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    fn decode_tag_value(value: &str) -> Option<String> {
        if !value.len().is_multiple_of(2) {
            return None;
        }
        let bytes = value
            .as_bytes()
            .chunks_exact(2)
            .map(|pair| {
                let encoded = std::str::from_utf8(pair).ok()?;
                u8::from_str_radix(encoded, 16).ok()
            })
            .collect::<Option<Vec<_>>>()?;
        String::from_utf8(bytes).ok()
    }

    fn encoded_tag(prefix: &str, value: &str) -> String {
        format!("{prefix}{}", Self::encode_tag_value(value))
    }

    fn decoded_tag(tags: &[String], prefix: &str) -> Option<String> {
        tags.iter()
            .find_map(|tag| tag.strip_prefix(prefix).and_then(Self::decode_tag_value))
    }

    fn category_to_memory_type(category: &MemoryCategory) -> &'static str {
        match category {
            MemoryCategory::Core => "learning",
            MemoryCategory::Daily => "observation",
            MemoryCategory::Conversation => "context",
            MemoryCategory::Custom(_) => "observation",
        }
    }

    fn category_from_remote(memory_type: Option<&str>, tags: &[String]) -> MemoryCategory {
        if let Some(category) = Self::decoded_tag(tags, CATEGORY_TAG_PREFIX) {
            return match category.as_str() {
                "core" => MemoryCategory::Core,
                "daily" => MemoryCategory::Daily,
                "conversation" => MemoryCategory::Conversation,
                custom => MemoryCategory::Custom(custom.to_string()),
            };
        }

        match memory_type.unwrap_or_default() {
            "learning" | "decision" => MemoryCategory::Core,
            "context" => MemoryCategory::Conversation,
            _ => MemoryCategory::Daily,
        }
    }

    fn tags(request: EnrichmentStoreRequest<'_>) -> Vec<String> {
        let mut tags = vec![
            MARKER_TAG.to_string(),
            Self::encoded_tag(KEY_TAG_PREFIX, request.key),
            Self::encoded_tag(CATEGORY_TAG_PREFIX, &request.category.to_string()),
        ];
        if let Some(namespace) = request.namespace {
            tags.push(Self::encoded_tag(NAMESPACE_TAG_PREFIX, namespace));
        }
        if let Some(session_id) = request.session_id {
            tags.push(Self::encoded_tag(SESSION_TAG_PREFIX, session_id));
        }
        tags
    }

    fn into_entry(remote: ShodhRecallMemory, agent_id: Option<&str>) -> Option<MemoryEntry> {
        let key = Self::decoded_tag(&remote.experience.tags, KEY_TAG_PREFIX)?;
        let category = Self::category_from_remote(
            remote.experience.memory_type.as_deref(),
            &remote.experience.tags,
        );
        Some(MemoryEntry {
            id: format!("shodh:{}", remote.id),
            key,
            content: remote.experience.content,
            category,
            timestamp: remote.created_at,
            session_id: Self::decoded_tag(&remote.experience.tags, SESSION_TAG_PREFIX),
            score: Some(remote.score),
            namespace: Self::decoded_tag(&remote.experience.tags, NAMESPACE_TAG_PREFIX)
                .unwrap_or_else(|| "default".into()),
            importance: Some(remote.importance),
            superseded_by: None,
            kind: None,
            pinned: false,
            tenant_id: None,
            agent_alias: None,
            agent_id: agent_id.map(str::to_string),
        })
    }

    async fn recall_user(
        &self,
        user_id: &str,
        agent_id: Option<&str>,
        request: EnrichmentRecallRequest<'_>,
        timeout_window: Duration,
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        let response = self
            .request(
                "POST",
                "/api/recall",
                &RecallRequest {
                    user_id,
                    query: request.query,
                    limit: request.limit,
                    mode: "hybrid",
                    tags: vec![MARKER_TAG],
                },
                timeout_window,
            )
            .await?;
        let response: RecallResponse = serde_json::from_value(response).map_err(|error| {
            anyhow::Error::msg(format!(
                "decoding Shodh recall response body: {}",
                self.redact(error)
            ))
        })?;
        Ok(response
            .memories
            .into_iter()
            .filter_map(|memory| Self::into_entry(memory, agent_id))
            .collect())
    }

    async fn forget_by_tag(&self, user_id: &str, tag: String) -> anyhow::Result<()> {
        self.request(
            "POST",
            "/api/forget/tags",
            &ForgetByTagsRequest {
                user_id,
                tags: vec![tag],
            },
            self.store_timeout,
        )
        .await?;
        Ok(())
    }

    async fn delete_user(&self, agent_id: &str) -> anyhow::Result<()> {
        self.request(
            "DELETE",
            &format!("/api/users/{agent_id}"),
            &serde_json::json!({}),
            self.store_timeout,
        )
        .await?;
        Ok(())
    }
}

#[async_trait]
impl MemoryEnricher for ShodhConnector {
    fn name(&self) -> &'static str {
        "shodh"
    }

    fn attribution_kind(&self) -> MemoryKind {
        MemoryKind::Shodh
    }

    fn capabilities(&self) -> EnricherCapabilities {
        EnricherCapabilities {
            result_kind: ResultKind::CanonicalRowReference,
            recall_scope: RecallScope::AgentAllowlist,
            recall_support: RecallSupport::SemanticOnly,
            cleanup_support: CleanupSupport::AgentScoped,
        }
    }

    async fn store(&self, request: EnrichmentStoreRequest<'_>) -> anyhow::Result<()> {
        let user_id = Self::user_id(request.agent_id);
        self.request(
            "POST",
            "/api/upsert",
            &UpsertRequest {
                user_id,
                external_id: Self::external_id(request.key),
                content: request.content,
                tags: Self::tags(request),
                memory_type: Self::category_to_memory_type(request.category),
                change_type: "content_updated",
                importance: request.importance.map(|value| value.clamp(0.0, 1.0)),
            },
            self.store_timeout,
        )
        .await?;
        Ok(())
    }

    async fn recall(
        &self,
        request: EnrichmentRecallRequest<'_>,
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        if request.query.trim().is_empty() || request.limit == 0 {
            return Ok(Vec::new());
        }

        let mut results = Vec::new();
        match request.allowed_agent_ids {
            Some(agent_ids) => {
                let mut first_error = None;
                let mut successful_requests = 0_usize;
                let mut failed_requests = 0_usize;
                let deadline = Instant::now() + self.recall_timeout;
                for (index, agent_id) in agent_ids.iter().copied().enumerate() {
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    if remaining.is_zero() {
                        failed_requests += agent_ids.len() - index;
                        first_error.get_or_insert_with(|| {
                            anyhow::Error::msg("Shodh aggregate recall deadline exceeded")
                        });
                        break;
                    }
                    match timeout(
                        remaining,
                        self.recall_user(agent_id, Some(agent_id), request, remaining),
                    )
                    .await
                    {
                        Ok(Ok(agent_results)) => {
                            successful_requests += 1;
                            results.extend(agent_results);
                        }
                        Ok(Err(error)) => {
                            failed_requests += 1;
                            first_error.get_or_insert(error);
                        }
                        Err(_) => {
                            failed_requests += agent_ids.len() - index;
                            first_error.get_or_insert_with(|| {
                                anyhow::Error::msg("Shodh aggregate recall deadline exceeded")
                            });
                            break;
                        }
                    }
                }
                if successful_requests > 0 && failed_requests > 0 {
                    ::zeroclaw_log::record!(
                        DEBUG,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note,)
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                            .with_attrs(::serde_json::json!({
                                "requested_agents": agent_ids.len(),
                                "successful_requests": successful_requests,
                                "failed_requests": failed_requests,
                                "first_error": first_error.as_ref().map(ToString::to_string),
                            })),
                        "Shodh cross-agent recall partially failed"
                    );
                }
                if successful_requests == 0
                    && let Some(error) = first_error
                {
                    return Err(error);
                }
            }
            None => {
                results.extend(
                    self.recall_user(UNSCOPED_USER_ID, None, request, self.recall_timeout)
                        .await?,
                );
            }
        }
        results.sort_by(|left, right| {
            right
                .score
                .partial_cmp(&left.score)
                .unwrap_or(Ordering::Equal)
                .then_with(|| right.timestamp.cmp(&left.timestamp))
                .then_with(|| left.key.cmp(&right.key))
        });
        results.truncate(request.limit);
        Ok(results)
    }

    async fn cleanup(&self, request: EnrichmentCleanupRequest<'_>) -> anyhow::Result<()> {
        match request {
            EnrichmentCleanupRequest::Entry { key, agent_id } => {
                self.forget_by_tag(agent_id, Self::encoded_tag(KEY_TAG_PREFIX, key))
                    .await
            }
            EnrichmentCleanupRequest::Session {
                session_id,
                agent_id,
            } => {
                self.forget_by_tag(agent_id, Self::encoded_tag(SESSION_TAG_PREFIX, session_id))
                    .await
            }
            EnrichmentCleanupRequest::Agent { agent_id } => self.delete_user(agent_id).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::enriched::RecallKind;
    use crate::traits::Memory;
    use serde_json::json;
    use std::sync::{Arc, Mutex};
    use tempfile::TempDir;
    use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};

    struct FakeResponse {
        version: u8,
        id: Option<String>,
        status: u16,
        body: Value,
        delay: Duration,
        trailing: Vec<u8>,
        extra_field: Option<(String, Value)>,
        raw: Option<Vec<u8>>,
        omit_newline: bool,
    }

    impl FakeResponse {
        fn ok(body: Value) -> Self {
            Self {
                version: IPC_PROTOCOL_VERSION,
                id: None,
                status: 200,
                body,
                delay: Duration::ZERO,
                trailing: Vec::new(),
                extra_field: None,
                raw: None,
                omit_newline: false,
            }
        }

        fn error(status: u16, message: &str) -> Self {
            Self {
                version: IPC_PROTOCOL_VERSION,
                id: None,
                status,
                body: json!({ "error": message }),
                delay: Duration::ZERO,
                trailing: Vec::new(),
                extra_field: None,
                raw: None,
                omit_newline: false,
            }
        }

        fn delayed(mut self, delay: Duration) -> Self {
            self.delay = delay;
            self
        }

        fn with_trailing_data(mut self, trailing: &[u8]) -> Self {
            self.trailing = trailing.to_vec();
            self
        }

        fn with_extra_field(mut self, name: &str, value: Value) -> Self {
            self.extra_field = Some((name.to_string(), value));
            self
        }

        fn raw(bytes: &[u8]) -> Self {
            let mut response = Self::ok(Value::Null);
            response.raw = Some(bytes.to_vec());
            response
        }

        fn without_newline(mut self) -> Self {
            self.omit_newline = true;
            self
        }
    }

    type FakeResponder = Arc<dyn Fn(&Value) -> FakeResponse + Send + Sync>;
    type CapturedRequests = Arc<Mutex<Vec<Value>>>;

    fn responder(
        callback: impl Fn(&Value) -> FakeResponse + Send + Sync + 'static,
    ) -> FakeResponder {
        Arc::new(callback)
    }

    async fn serve_fake_connection<S>(
        stream: S,
        captured: &CapturedRequests,
        responder: &FakeResponder,
    ) where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        let (read_half, mut write_half) = tokio::io::split(stream);
        let mut reader = BufReader::new(read_half);
        let mut request_line = Vec::new();
        reader.read_until(b'\n', &mut request_line).await.unwrap();
        assert!(request_line.len() <= MAX_FRAME_BYTES);
        let request: Value = serde_json::from_slice(&request_line).unwrap();
        captured.lock().unwrap().push(request.clone());

        let response = responder(&request);
        if !response.delay.is_zero() {
            tokio::time::sleep(response.delay).await;
        }
        if let Some(raw) = response.raw.as_ref() {
            let _ = write_half.write_all(raw).await;
            return;
        }
        let id = response
            .id
            .unwrap_or_else(|| request["id"].as_str().unwrap().to_string());
        let mut envelope = json!({
            "v": response.version,
            "id": id,
            "status": response.status,
            "body": response.body,
        });
        if let Some((name, value)) = response.extra_field {
            envelope.as_object_mut().unwrap().insert(name, value);
        }
        let mut response_line = serde_json::to_vec(&envelope).unwrap();
        if !response.omit_newline {
            response_line.push(b'\n');
        }
        response_line.extend_from_slice(&response.trailing);
        let _ = write_half.write_all(&response_line).await;
    }

    #[cfg(unix)]
    fn spawn_fake_server(
        temp: &TempDir,
        connection_count: usize,
        responder: FakeResponder,
    ) -> (PathBuf, CapturedRequests, tokio::task::JoinHandle<()>) {
        let path = temp.path().join("shodh.sock");
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        let captured = Arc::new(Mutex::new(Vec::new()));
        let server_captured = captured.clone();
        let cleanup_path = path.clone();
        let handle = zeroclaw_spawn::spawn!(async move {
            for _ in 0..connection_count {
                let (stream, _) = listener.accept().await.unwrap();
                serve_fake_connection(stream, &server_captured, &responder).await;
            }
            let _ = tokio::fs::remove_file(cleanup_path).await;
        });
        (path, captured, handle)
    }

    #[cfg(windows)]
    fn spawn_fake_server(
        _temp: &TempDir,
        connection_count: usize,
        responder: FakeResponder,
    ) -> (PathBuf, CapturedRequests, tokio::task::JoinHandle<()>) {
        use tokio::net::windows::named_pipe::ServerOptions;

        let path = PathBuf::from(format!(r"\\.\pipe\zeroclaw-shodh-test-{}", Uuid::new_v4()));
        let name = path.to_string_lossy().into_owned();
        let mut listener = ServerOptions::new()
            .first_pipe_instance(true)
            .create(&name)
            .unwrap();
        let captured = Arc::new(Mutex::new(Vec::new()));
        let server_captured = captured.clone();
        let handle = zeroclaw_spawn::spawn!(async move {
            for _ in 0..connection_count {
                listener.connect().await.unwrap();
                let next = ServerOptions::new().create(&name).unwrap();
                let connected = std::mem::replace(&mut listener, next);
                serve_fake_connection(connected, &server_captured, &responder).await;
            }
        });
        (path, captured, handle)
    }

    async fn finish_fake_server(handle: tokio::task::JoinHandle<()>) {
        tokio::time::timeout(Duration::from_secs(3), handle)
            .await
            .expect("fake Shodh server did not receive its expected connections")
            .unwrap();
    }

    fn test_config(socket_path: &Path) -> ShodhEnrichmentConfig {
        ShodhEnrichmentConfig {
            socket_path: socket_path.to_string_lossy().into_owned(),
            api_key: Some("test-api-key".into()),
            local_hit_threshold: 10,
            ..ShodhEnrichmentConfig::default()
        }
    }

    fn recall_body(
        key: &str,
        content: &str,
        agent_id: &str,
        session_id: Option<&str>,
    ) -> serde_json::Value {
        let mut tags = vec![
            MARKER_TAG.to_string(),
            ShodhConnector::encoded_tag(KEY_TAG_PREFIX, key),
            ShodhConnector::encoded_tag(CATEGORY_TAG_PREFIX, "core"),
        ];
        if let Some(session_id) = session_id {
            tags.push(ShodhConnector::encoded_tag(SESSION_TAG_PREFIX, session_id));
        }
        json!({
            "memories": [{
                "id": format!("remote-{agent_id}"),
                "experience": {
                    "content": content,
                    "memory_type": "learning",
                    "tags": tags
                },
                "importance": 0.8,
                "created_at": "2026-01-02T03:04:05Z",
                "score": 0.91,
                "tier": "hot"
            }],
            "count": 1
        })
    }

    #[test]
    fn tag_values_round_trip_unicode_without_delimiter_ambiguity() {
        let input = "session:α/with spaces";
        let encoded = ShodhConnector::encode_tag_value(input);
        assert_eq!(
            ShodhConnector::decode_tag_value(&encoded).as_deref(),
            Some(input)
        );
    }

    #[test]
    fn constructor_requires_explicit_socket_path() {
        let error = match ShodhConnector::new(&ShodhEnrichmentConfig::default()) {
            Ok(_) => panic!("missing socket path must fail closed"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("requires `socket_path`"));
    }

    #[test]
    fn constructor_requires_api_key() {
        let config = ShodhEnrichmentConfig {
            socket_path: "shodh-test.sock".to_string(),
            ..ShodhEnrichmentConfig::default()
        };
        let error = match ShodhConnector::new(&config) {
            Ok(_) => panic!("missing API key must fail closed"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("requires `api_key`"));
    }

    #[tokio::test]
    async fn agent_scoped_store_and_recall_use_authenticated_local_ipc() {
        let temp = TempDir::new().unwrap();
        let (socket_path, captured, server) = spawn_fake_server(
            &temp,
            2,
            responder(|request| match request["path"].as_str().unwrap() {
                "/api/upsert" => FakeResponse::ok(json!({
                    "id": "remote-id",
                    "success": true,
                    "was_update": false,
                    "version": 1
                })),
                "/api/recall" => {
                    let agent_id = request["body"]["user_id"].as_str().unwrap();
                    FakeResponse::ok(recall_body(
                        "local-key",
                        "stale remote payload",
                        agent_id,
                        Some("session:alpha"),
                    ))
                }
                path => panic!("unexpected path {path}"),
            }),
        );
        let local = SqliteMemory::new("sqlite", temp.path()).unwrap();
        let memory = crate::build_shodh_enriched_memory(local, &test_config(&socket_path)).unwrap();
        let agent_id = memory.ensure_agent_uuid("alpha").await.unwrap();
        let agent_id = agent_id.as_str();
        let key = "local-key";
        let session_id = "session:alpha";

        memory
            .store_with_agent(
                key,
                "canonical local payload",
                MemoryCategory::Core,
                Some(session_id),
                Some("project"),
                Some(0.7),
                Some(agent_id),
            )
            .await
            .unwrap();

        let recalled = memory
            .recall_for_agents(
                &[agent_id],
                "semantic-only-query",
                5,
                Some(session_id),
                None,
                None,
            )
            .await
            .unwrap();
        assert_eq!(recalled.len(), 1);
        assert_eq!(recalled[0].key, key);
        assert_eq!(recalled[0].content, "canonical local payload");
        assert_eq!(recalled[0].agent_id.as_deref(), Some(agent_id));
        assert_eq!(recalled[0].score, Some(0.91));

        finish_fake_server(server).await;
        let requests = captured.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert!(requests.iter().all(|request| request["v"] == 1));
        assert!(
            requests
                .iter()
                .all(|request| request["auth"] == "test-api-key")
        );
        assert_eq!(requests[0]["method"], "POST");
        assert_eq!(requests[0]["path"], "/api/upsert");
        assert_eq!(requests[0]["body"]["user_id"], agent_id);
        assert_eq!(requests[0]["body"]["content"], "canonical local payload");
        assert_eq!(requests[1]["path"], "/api/recall");
        assert!(requests[1]["body"].get("session_id").is_none());
    }

    #[tokio::test]
    async fn stale_remote_row_cannot_resurrect_after_local_deletion() {
        let agent_id = "f879192b-5fa2-4dc0-97fc-63c049dc66ec";
        let temp = TempDir::new().unwrap();
        let (socket_path, _, server) = spawn_fake_server(
            &temp,
            1,
            responder(move |_| {
                FakeResponse::ok(recall_body("deleted-key", "remote residue", agent_id, None))
            }),
        );

        let local = SqliteMemory::new("sqlite", temp.path()).unwrap();
        let memory = crate::build_shodh_enriched_memory(local, &test_config(&socket_path)).unwrap();
        let recalled = memory
            .recall_for_agents(&[agent_id], "residue", 5, None, None, None)
            .await
            .unwrap();
        assert!(recalled.is_empty());
        finish_fake_server(server).await;
    }

    #[tokio::test]
    async fn remote_recall_failure_preserves_local_results() {
        let temp = TempDir::new().unwrap();
        let (socket_path, _, server) = spawn_fake_server(
            &temp,
            2,
            responder(|request| match request["path"].as_str().unwrap() {
                "/api/upsert" => FakeResponse::ok(json!({})),
                "/api/recall" => FakeResponse::error(503, "temporarily unavailable"),
                path => panic!("unexpected path {path}"),
            }),
        );
        let local = SqliteMemory::new("sqlite", temp.path()).unwrap();
        let memory = crate::build_shodh_enriched_memory(local, &test_config(&socket_path)).unwrap();
        let agent_id = memory.ensure_agent_uuid("alpha").await.unwrap();
        memory
            .store_with_agent(
                "local-key",
                "local anchor survives",
                MemoryCategory::Core,
                None,
                None,
                None,
                Some(&agent_id),
            )
            .await
            .unwrap();

        let recalled = memory
            .recall_for_agents(&[&agent_id], "anchor", 5, None, None, None)
            .await
            .unwrap();
        assert_eq!(recalled.len(), 1);
        assert_eq!(recalled[0].content, "local anchor survives");
        finish_fake_server(server).await;
    }

    #[tokio::test]
    async fn cross_agent_recall_returns_successes_when_one_request_fails() {
        let successful_agent = "11111111-1111-4111-8111-111111111111";
        let failed_agent = "22222222-2222-4222-8222-222222222222";
        let temp = TempDir::new().unwrap();
        let (socket_path, _, server) = spawn_fake_server(
            &temp,
            2,
            responder(move |request| {
                let agent_id = request["body"]["user_id"].as_str().unwrap();
                if agent_id == successful_agent {
                    FakeResponse::ok(recall_body(
                        "remote-key",
                        "remote payload",
                        successful_agent,
                        None,
                    ))
                } else {
                    assert_eq!(agent_id, failed_agent);
                    FakeResponse::error(503, "temporarily unavailable")
                }
            }),
        );

        let connector = ShodhConnector::new(&test_config(&socket_path)).unwrap();
        let allowed_agents = [successful_agent, failed_agent];
        let recalled = connector
            .recall(EnrichmentRecallRequest {
                query: "remote",
                limit: 5,
                session_id: None,
                allowed_agent_ids: Some(&allowed_agents),
                kind: RecallKind::Semantic,
            })
            .await
            .unwrap();

        assert_eq!(recalled.len(), 1);
        assert_eq!(recalled[0].key, "remote-key");
        assert_eq!(recalled[0].agent_id.as_deref(), Some(successful_agent));
        finish_fake_server(server).await;
    }

    #[tokio::test]
    async fn cross_agent_recall_timeout_bounds_the_whole_operation() {
        let successful_agent = "11111111-1111-4111-8111-111111111111";
        let slow_agents = [
            "22222222-2222-4222-8222-222222222222",
            "33333333-3333-4333-8333-333333333333",
            "44444444-4444-4444-8444-444444444444",
        ];
        let temp = TempDir::new().unwrap();
        let (socket_path, captured, server) = spawn_fake_server(
            &temp,
            2,
            responder(move |request| {
                let agent_id = request["body"]["user_id"].as_str().unwrap();
                if agent_id == successful_agent {
                    FakeResponse::ok(recall_body(
                        "remote-key",
                        "remote payload",
                        successful_agent,
                        None,
                    ))
                } else {
                    assert!(slow_agents.contains(&agent_id));
                    FakeResponse::ok(recall_body("slow-key", "slow payload", agent_id, None))
                        .delayed(Duration::from_millis(400))
                }
            }),
        );

        let mut config = test_config(&socket_path);
        config.recall_timeout_ms = 100;
        let connector = ShodhConnector::new(&config).unwrap();
        let allowed_agents = [
            successful_agent,
            slow_agents[0],
            slow_agents[1],
            slow_agents[2],
        ];
        let recalled = tokio::time::timeout(
            Duration::from_millis(250),
            connector.recall(EnrichmentRecallRequest {
                query: "remote",
                limit: 5,
                session_id: None,
                allowed_agent_ids: Some(&allowed_agents),
                kind: RecallKind::Semantic,
            }),
        )
        .await
        .expect("aggregate recall deadline must bound all agent requests")
        .unwrap();

        assert_eq!(recalled.len(), 1);
        assert_eq!(recalled[0].agent_id.as_deref(), Some(successful_agent));
        finish_fake_server(server).await;
        assert_eq!(captured.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn scoped_forget_deletes_locally_and_propagates_by_tag() {
        let temp = TempDir::new().unwrap();
        let (socket_path, captured, server) = spawn_fake_server(
            &temp,
            2,
            responder(|request| match request["path"].as_str().unwrap() {
                "/api/upsert" => FakeResponse::ok(json!({})),
                "/api/forget/tags" => FakeResponse::ok(json!({
                    "success": true,
                    "deleted_count": 1
                })),
                path => panic!("unexpected path {path}"),
            }),
        );
        let local = SqliteMemory::new("sqlite", temp.path()).unwrap();
        let memory = crate::build_shodh_enriched_memory(local, &test_config(&socket_path)).unwrap();
        let agent_id = memory.ensure_agent_uuid("alpha").await.unwrap();
        let agent_id = agent_id.as_str();
        let key = "forget-me";

        memory
            .store_with_agent(
                key,
                "payload",
                MemoryCategory::Daily,
                None,
                None,
                None,
                Some(agent_id),
            )
            .await
            .unwrap();
        assert!(memory.forget_for_agent(key, agent_id).await.unwrap());
        assert!(memory.get_for_agent(key, agent_id).await.unwrap().is_none());
        finish_fake_server(server).await;

        let requests = captured.lock().unwrap();
        assert_eq!(requests[1]["auth"], "test-api-key");
        assert_eq!(requests[1]["method"], "POST");
        assert_eq!(requests[1]["path"], "/api/forget/tags");
        assert_eq!(requests[1]["body"]["user_id"], agent_id);
        assert_eq!(
            requests[1]["body"]["tags"],
            json!([ShodhConnector::encoded_tag(KEY_TAG_PREFIX, key)])
        );
    }

    #[tokio::test]
    async fn response_protocol_version_must_match() {
        let temp = TempDir::new().unwrap();
        let (socket_path, _, server) = spawn_fake_server(
            &temp,
            1,
            responder(|_| FakeResponse {
                version: IPC_PROTOCOL_VERSION + 1,
                id: None,
                status: 200,
                body: json!({}),
                delay: Duration::ZERO,
                trailing: Vec::new(),
                extra_field: None,
                raw: None,
                omit_newline: false,
            }),
        );
        let connector = ShodhConnector::new(&test_config(&socket_path)).unwrap();

        let error = connector
            .request("POST", "/api/recall", &json!({}), Duration::from_secs(1))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("protocol version mismatch"));
        finish_fake_server(server).await;
    }

    #[tokio::test]
    async fn response_id_must_match_request() {
        let temp = TempDir::new().unwrap();
        let (socket_path, _, server) = spawn_fake_server(
            &temp,
            1,
            responder(|_| FakeResponse {
                version: IPC_PROTOCOL_VERSION,
                id: Some("wrong-request-id".into()),
                status: 200,
                body: json!({}),
                delay: Duration::ZERO,
                trailing: Vec::new(),
                extra_field: None,
                raw: None,
                omit_newline: false,
            }),
        );
        let connector = ShodhConnector::new(&test_config(&socket_path)).unwrap();

        let error = connector
            .request("POST", "/api/recall", &json!({}), Duration::from_secs(1))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("response id did not match"));
        finish_fake_server(server).await;
    }

    #[tokio::test]
    async fn non_success_response_does_not_expose_auth_secret() {
        let temp = TempDir::new().unwrap();
        let (socket_path, _, server) = spawn_fake_server(
            &temp,
            1,
            responder(|_| FakeResponse::error(401, "authentication failed for test-api-key")),
        );
        let connector = ShodhConnector::new(&test_config(&socket_path)).unwrap();

        let error = connector
            .request("POST", "/api/recall", &json!({}), Duration::from_secs(1))
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("status 401"));
        assert!(!error.contains("test-api-key"));
        assert!(error.contains("[redacted]"));
        finish_fake_server(server).await;
    }

    #[tokio::test]
    async fn malformed_envelope_does_not_expose_auth_secret() {
        let temp = TempDir::new().unwrap();
        let (socket_path, _, server) = spawn_fake_server(
            &temp,
            1,
            responder(|_| {
                FakeResponse::ok(json!({})).with_extra_field("test-api-key", json!(true))
            }),
        );
        let connector = ShodhConnector::new(&test_config(&socket_path)).unwrap();

        let error = connector
            .request("POST", "/api/recall", &json!({}), Duration::from_secs(1))
            .await
            .expect_err("an unknown envelope field must fail closed")
            .to_string();
        assert!(!error.contains("test-api-key"));
        assert!(error.contains("[redacted]"));
        finish_fake_server(server).await;
    }

    #[tokio::test]
    async fn oversized_request_is_rejected_before_connecting() {
        let config = ShodhEnrichmentConfig {
            socket_path: "definitely-missing-shodh.sock".into(),
            api_key: Some("test-api-key".into()),
            ..ShodhEnrichmentConfig::default()
        };
        let connector = ShodhConnector::new(&config).unwrap();
        let body = json!({ "payload": "x".repeat(MAX_FRAME_BYTES) });

        let error = connector
            .request("POST", "/api/upsert", &body, Duration::from_secs(1))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("request exceeds"));
    }

    #[tokio::test]
    async fn oversized_response_is_rejected() {
        let temp = TempDir::new().unwrap();
        let (socket_path, _, server) = spawn_fake_server(
            &temp,
            1,
            responder(|_| {
                FakeResponse::ok(json!({
                    "payload": "x".repeat(MAX_FRAME_BYTES)
                }))
            }),
        );
        let connector = ShodhConnector::new(&test_config(&socket_path)).unwrap();

        let error = connector
            .request("POST", "/api/recall", &json!({}), Duration::from_secs(2))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("response exceeds"));
        finish_fake_server(server).await;
    }

    #[tokio::test]
    async fn response_rejects_data_after_newline_delimiter() {
        let temp = TempDir::new().unwrap();
        let (socket_path, _, server) = spawn_fake_server(
            &temp,
            1,
            responder(|_| FakeResponse::ok(json!({})).with_trailing_data(b"{}\n")),
        );
        let connector = ShodhConnector::new(&test_config(&socket_path)).unwrap();

        let error = connector
            .request("GET", "/health", &json!({}), Duration::from_secs(1))
            .await
            .expect_err("a second response frame must fail closed");
        assert!(error.to_string().contains("after its newline delimiter"));
        finish_fake_server(server).await;
    }

    #[tokio::test]
    async fn response_must_be_valid_utf8_json() {
        let temp = TempDir::new().unwrap();
        let (socket_path, _, server) =
            spawn_fake_server(&temp, 1, responder(|_| FakeResponse::raw(&[0xff, b'\n'])));
        let connector = ShodhConnector::new(&test_config(&socket_path)).unwrap();

        let error = connector
            .request("GET", "/health", &json!({}), Duration::from_secs(1))
            .await
            .expect_err("invalid UTF-8 JSON must fail closed");
        assert!(error.to_string().contains("decoding Shodh IPC response"));
        finish_fake_server(server).await;
    }

    #[tokio::test]
    async fn response_requires_newline_delimiter() {
        let temp = TempDir::new().unwrap();
        let (socket_path, _, server) = spawn_fake_server(
            &temp,
            1,
            responder(|_| FakeResponse::ok(json!({})).without_newline()),
        );
        let connector = ShodhConnector::new(&test_config(&socket_path)).unwrap();

        let error = connector
            .request("GET", "/health", &json!({}), Duration::from_secs(1))
            .await
            .expect_err("a response without LF must fail closed");
        assert!(error.to_string().contains("before its newline delimiter"));
        finish_fake_server(server).await;
    }

    #[tokio::test]
    async fn response_status_must_be_an_http_status() {
        let temp = TempDir::new().unwrap();
        let (socket_path, _, server) = spawn_fake_server(
            &temp,
            1,
            responder(|_| FakeResponse::error(600, "invalid status")),
        );
        let connector = ShodhConnector::new(&test_config(&socket_path)).unwrap();

        let error = connector
            .request("GET", "/health", &json!({}), Duration::from_secs(1))
            .await
            .expect_err("an out-of-range status must fail closed");
        assert!(error.to_string().contains("response status was invalid"));
        finish_fake_server(server).await;
    }
}
