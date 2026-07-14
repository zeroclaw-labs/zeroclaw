use super::enriched::{
    CleanupSupport, EnrichedMemory, EnricherCapabilities, EnrichmentCleanupRequest,
    EnrichmentRecallRequest, EnrichmentStoreRequest, MemoryEnricher, RecallScope, RecallSupport,
    ResultKind,
};
#[cfg(test)]
use super::sqlite::SqliteMemory;
use super::traits::{MemoryCategory, MemoryEntry};
use async_trait::async_trait;
use reqwest::header::HeaderValue;
use reqwest::{Client, Url};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::cmp::Ordering;
use std::time::Duration;
use tokio::time::{Instant, timeout};
use zeroclaw_api::attribution::MemoryKind;
use zeroclaw_config::schema::ShodhEnrichmentConfig;

const API_KEY_HEADER: &str = "X-API-Key";
const UNSCOPED_USER_ID: &str = "zeroclaw-unscoped";
const MARKER_TAG: &str = "zeroclaw";
const KEY_TAG_PREFIX: &str = "zc-key:";
const CATEGORY_TAG_PREFIX: &str = "zc-category:";
const NAMESPACE_TAG_PREFIX: &str = "zc-namespace:";
const SESSION_TAG_PREFIX: &str = "zc-session:";

/// REST connector for an independently supervised local Shodh binary or remote
/// Shodh deployment.
pub struct ShodhConnector {
    client: Client,
    base_url: Url,
    api_key: HeaderValue,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    session_id: Option<&'a str>,
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

impl ShodhConnector {
    pub(crate) fn new(config: &ShodhEnrichmentConfig) -> anyhow::Result<Self> {
        let base_url = Self::validated_base_url(&config.base_url)?;
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
        let api_key = HeaderValue::from_str(&api_key)
            .map_err(|error| anyhow::Error::msg(format!("invalid Shodh api_key: {error}")))?;
        if config.recall_timeout_ms == 0 || config.store_timeout_ms == 0 {
            anyhow::bail!("Shodh recall and store timeouts must be greater than zero");
        }

        Ok(Self {
            client: Client::new(),
            base_url,
            api_key,
            recall_timeout: Duration::from_millis(config.recall_timeout_ms),
            store_timeout: Duration::from_millis(config.store_timeout_ms),
        })
    }

    fn validated_base_url(raw: &str) -> anyhow::Result<Url> {
        let mut url = Url::parse(raw.trim())
            .map_err(|error| anyhow::Error::msg(format!("invalid Shodh base_url: {error}")))?;
        if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
            anyhow::bail!("Shodh base_url must be an HTTP(S) origin with a host");
        }
        if !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            anyhow::bail!(
                "Shodh base_url must not contain credentials, a query string, or a fragment"
            );
        }
        let normalized_path = format!("{}/", url.path().trim_end_matches('/'));
        url.set_path(&normalized_path);
        Ok(url)
    }

    fn endpoint(&self, path: &str) -> anyhow::Result<Url> {
        self.base_url
            .join(path)
            .map_err(|error| anyhow::Error::msg(format!("invalid Shodh API endpoint: {error}")))
    }

    fn user_id(agent_id: Option<&str>) -> &str {
        agent_id.unwrap_or(UNSCOPED_USER_ID)
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
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        let response = self
            .client
            .post(self.endpoint("api/recall")?)
            .header(API_KEY_HEADER, self.api_key.clone())
            .timeout(self.recall_timeout)
            .json(&RecallRequest {
                user_id,
                query: request.query,
                limit: request.limit,
                mode: "hybrid",
                session_id: request.session_id,
                tags: vec![MARKER_TAG],
            })
            .send()
            .await?
            .error_for_status()?
            .json::<RecallResponse>()
            .await?;
        Ok(response
            .memories
            .into_iter()
            .filter_map(|memory| Self::into_entry(memory, agent_id))
            .collect())
    }

    async fn forget_by_tag(&self, user_id: &str, tag: String) -> anyhow::Result<()> {
        self.client
            .post(self.endpoint("api/forget/tags")?)
            .header(API_KEY_HEADER, self.api_key.clone())
            .timeout(self.store_timeout)
            .json(&ForgetByTagsRequest {
                user_id,
                tags: vec![tag],
            })
            .send()
            .await?
            .error_for_status()?;
        Ok(())
    }

    async fn delete_user(&self, agent_id: &str) -> anyhow::Result<()> {
        let endpoint = self.endpoint(&format!("api/users/{agent_id}"))?;
        self.client
            .delete(endpoint)
            .header(API_KEY_HEADER, self.api_key.clone())
            .timeout(self.store_timeout)
            .send()
            .await?
            .error_for_status()?;
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
        self.client
            .post(self.endpoint("api/upsert")?)
            .header(API_KEY_HEADER, self.api_key.clone())
            .timeout(self.store_timeout)
            .json(&UpsertRequest {
                user_id,
                external_id: Self::external_id(request.key),
                content: request.content,
                tags: Self::tags(request),
                memory_type: Self::category_to_memory_type(request.category),
                change_type: "content_updated",
                importance: request.importance.map(|value| value.clamp(0.0, 1.0)),
            })
            .send()
            .await?
            .error_for_status()?;
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
                        self.recall_user(agent_id, Some(agent_id), request),
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
                results.extend(self.recall_user(UNSCOPED_USER_ID, None, request).await?);
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
    use tempfile::TempDir;
    use wiremock::matchers::{body_partial_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn test_config(server: &MockServer) -> ShodhEnrichmentConfig {
        ShodhEnrichmentConfig {
            base_url: server.uri(),
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
    fn base_url_rejects_embedded_credentials() {
        let error = ShodhConnector::validated_base_url("https://user:pass@example.com")
            .expect_err("credentials must not be accepted");
        assert!(error.to_string().contains("must not contain credentials"));
    }

    #[test]
    fn constructor_requires_api_key() {
        let error = match ShodhConnector::new(&ShodhEnrichmentConfig::default()) {
            Ok(_) => panic!("missing API key must fail closed"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("requires `api_key`"));
    }

    #[tokio::test]
    async fn agent_scoped_store_and_recall_use_authenticated_http_api() {
        let server = MockServer::start().await;
        let temp = TempDir::new().unwrap();
        let local = SqliteMemory::new("sqlite", temp.path()).unwrap();
        let memory = crate::build_shodh_enriched_memory(local, &test_config(&server)).unwrap();
        let agent_id = memory.ensure_agent_uuid("alpha").await.unwrap();
        let agent_id = agent_id.as_str();
        let key = "local-key";
        let session_id = "session:alpha";

        Mock::given(method("POST"))
            .and(path("/api/upsert"))
            .and(header(API_KEY_HEADER, "test-api-key"))
            .and(body_partial_json(json!({
                "user_id": agent_id,
                "content": "canonical local payload",
                "memory_type": "learning",
                "importance": 0.7
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "remote-id",
                "success": true,
                "was_update": false,
                "version": 1
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/recall"))
            .and(header(API_KEY_HEADER, "test-api-key"))
            .and(body_partial_json(json!({
                "user_id": agent_id,
                "query": "semantic-only-query",
                "session_id": session_id
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(recall_body(
                key,
                "stale remote payload",
                agent_id,
                Some(session_id),
            )))
            .expect(1)
            .mount(&server)
            .await;

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
    }

    #[tokio::test]
    async fn stale_remote_row_cannot_resurrect_after_local_deletion() {
        let server = MockServer::start().await;
        let agent_id = "f879192b-5fa2-4dc0-97fc-63c049dc66ec";
        Mock::given(method("POST"))
            .and(path("/api/recall"))
            .respond_with(ResponseTemplate::new(200).set_body_json(recall_body(
                "deleted-key",
                "remote residue",
                agent_id,
                None,
            )))
            .expect(1)
            .mount(&server)
            .await;

        let temp = TempDir::new().unwrap();
        let local = SqliteMemory::new("sqlite", temp.path()).unwrap();
        let memory = crate::build_shodh_enriched_memory(local, &test_config(&server)).unwrap();
        let recalled = memory
            .recall_for_agents(&[agent_id], "residue", 5, None, None, None)
            .await
            .unwrap();
        assert!(recalled.is_empty());
    }

    #[tokio::test]
    async fn remote_recall_failure_preserves_local_results() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/upsert"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/recall"))
            .respond_with(ResponseTemplate::new(503))
            .expect(1)
            .mount(&server)
            .await;

        let temp = TempDir::new().unwrap();
        let local = SqliteMemory::new("sqlite", temp.path()).unwrap();
        let memory = crate::build_shodh_enriched_memory(local, &test_config(&server)).unwrap();
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
    }

    #[tokio::test]
    async fn cross_agent_recall_returns_successes_when_one_request_fails() {
        let server = MockServer::start().await;
        let successful_agent = "11111111-1111-4111-8111-111111111111";
        let failed_agent = "22222222-2222-4222-8222-222222222222";
        Mock::given(method("POST"))
            .and(path("/api/recall"))
            .and(body_partial_json(json!({ "user_id": successful_agent })))
            .respond_with(ResponseTemplate::new(200).set_body_json(recall_body(
                "remote-key",
                "remote payload",
                successful_agent,
                None,
            )))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/recall"))
            .and(body_partial_json(json!({ "user_id": failed_agent })))
            .respond_with(ResponseTemplate::new(503))
            .expect(1)
            .mount(&server)
            .await;

        let connector = ShodhConnector::new(&test_config(&server)).unwrap();
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
    }

    #[tokio::test]
    async fn cross_agent_recall_timeout_bounds_the_whole_operation() {
        let server = MockServer::start().await;
        let successful_agent = "11111111-1111-4111-8111-111111111111";
        let slow_agents = [
            "22222222-2222-4222-8222-222222222222",
            "33333333-3333-4333-8333-333333333333",
            "44444444-4444-4444-8444-444444444444",
        ];
        Mock::given(method("POST"))
            .and(path("/api/recall"))
            .and(body_partial_json(json!({ "user_id": successful_agent })))
            .respond_with(ResponseTemplate::new(200).set_body_json(recall_body(
                "remote-key",
                "remote payload",
                successful_agent,
                None,
            )))
            .expect(1)
            .mount(&server)
            .await;
        for agent_id in slow_agents {
            Mock::given(method("POST"))
                .and(path("/api/recall"))
                .and(body_partial_json(json!({ "user_id": agent_id })))
                .respond_with(
                    ResponseTemplate::new(200)
                        .set_delay(Duration::from_millis(400))
                        .set_body_json(recall_body("slow-key", "slow payload", agent_id, None)),
                )
                .mount(&server)
                .await;
        }

        let mut config = test_config(&server);
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
    }

    #[tokio::test]
    async fn scoped_forget_deletes_locally_and_propagates_by_tag() {
        let server = MockServer::start().await;
        let temp = TempDir::new().unwrap();
        let local = SqliteMemory::new("sqlite", temp.path()).unwrap();
        let memory = crate::build_shodh_enriched_memory(local, &test_config(&server)).unwrap();
        let agent_id = memory.ensure_agent_uuid("alpha").await.unwrap();
        let agent_id = agent_id.as_str();
        let key = "forget-me";
        Mock::given(method("POST"))
            .and(path("/api/upsert"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/forget/tags"))
            .and(header(API_KEY_HEADER, "test-api-key"))
            .and(body_partial_json(json!({
                "user_id": agent_id,
                "tags": [ShodhConnector::encoded_tag(KEY_TAG_PREFIX, key)]
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "success": true,
                "deleted_count": 1
            })))
            .expect(1)
            .mount(&server)
            .await;

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
    }
}
