//! Microsoft Teams bot channel (Azure Bot Service / Bot Framework).
//!
//! Inbound: Teams POSTs Bot Framework activities to a channel-hosted axum
//! listener (the operator registers its public URL as the Azure Bot
//! messaging endpoint); every request is JWT-validated against the Bot
//! Framework JWKS before the body is touched. Outbound: proactive POSTs to
//! the Bot Connector API at the `service_url` carried by each inbound
//! activity, authenticated with a cached Entra client-credentials token.
//!
//! Design: `docs/msteams-channel-design.md`. Streaming draft updates land
//! in a follow-up PR.

pub mod activity;
pub mod auth;
pub mod conversation;

use activity::Activity;
use anyhow::{Context, Result};
use async_trait::async_trait;
use axum::{
    Router,
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode, header},
    routing::post,
};
use conversation::{ConversationReference, ConversationStore};
use portable_atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use zeroclaw_api::channel::{Channel, ChannelMessage, SendMessage};
use zeroclaw_config::schema::MSTeamsConfig;

/// Resolves this alias's `MSTeamsConfig` from canonical config state at
/// use-time. No snapshot is stored on the channel (see AGENTS.md
/// "ABSOLUTE RULE — SINGLE SOURCE OF TRUTH"): credentials, `allow_dms`,
/// and `mention_only` are all read through this resolver so a config
/// reload is observed on the next message.
pub type ConfigResolver = Arc<dyn Fn() -> Option<MSTeamsConfig> + Send + Sync>;

/// Resolves inbound external peers from canonical `peer_groups` state at
/// message-time.
pub type PeerResolver = Arc<dyn Fn() -> Vec<String> + Send + Sync>;

/// The bot's own identity on Teams, learned from `activity.recipient` on
/// the first inbound activity (the platform is its source of truth; it
/// exists nowhere in config).
#[derive(Debug, Clone)]
struct BotIdentity {
    id: String,
    name: Option<String>,
}

/// Connector token provider bound to the tenant it was built for.
/// Rebuilt when the canonical `tenant_id` changes on config reload — a
/// materialized view keyed on config state, not a cached copy of it.
struct ConnectorHandle {
    tenant_id: String,
    provider: Arc<auth::ConnectorTokenProvider>,
}

/// Microsoft Teams channel handle.
pub struct MsTeamsChannel {
    /// The alias key under `[channels.msteams.<alias>]` this handle is
    /// bound to.
    alias: String,
    /// Resolves the alias's config block from canonical state at use-time.
    config_resolver: ConfigResolver,
    /// Resolves inbound external peers from canonical state at message-time.
    peer_resolver: PeerResolver,
    validator: Arc<auth::JwtValidator>,
    conversations: Arc<ConversationStore>,
    bot_identity: Arc<OnceLock<BotIdentity>>,
    listener_ready: Arc<AtomicBool>,
    connector: tokio::sync::RwLock<Option<ConnectorHandle>>,
    #[cfg(test)]
    token_url_override: Option<String>,
}

impl MsTeamsChannel {
    pub fn new(
        alias: impl Into<String>,
        config_resolver: ConfigResolver,
        peer_resolver: PeerResolver,
    ) -> Self {
        Self {
            alias: alias.into(),
            config_resolver,
            peer_resolver,
            validator: Arc::new(auth::JwtValidator::new(
                auth::BOT_FRAMEWORK_OPENID_METADATA_URL,
            )),
            conversations: Arc::new(ConversationStore::default()),
            bot_identity: Arc::new(OnceLock::new()),
            listener_ready: Arc::new(AtomicBool::new(false)),
            connector: tokio::sync::RwLock::new(None),
            #[cfg(test)]
            token_url_override: None,
        }
    }

    /// Test hook: validate inbound JWTs against a mock OpenID/JWKS server.
    #[cfg(test)]
    fn with_openid_metadata_url(mut self, url: impl Into<String>) -> Self {
        self.validator = Arc::new(auth::JwtValidator::new(url.into()));
        self
    }

    /// Test hook: acquire connector tokens from a mock Entra endpoint.
    #[cfg(test)]
    fn with_token_url(mut self, url: impl Into<String>) -> Self {
        self.token_url_override = Some(url.into());
        self
    }

    /// Current config for this alias, resolved from canonical state.
    fn config(&self) -> Option<MSTeamsConfig> {
        (self.config_resolver)()
    }

    fn http_client(&self, proxy_url: Option<&str>) -> reqwest::Client {
        zeroclaw_config::schema::build_channel_proxy_client_with_timeouts(
            "channel.msteams",
            proxy_url,
            30,
            10,
        )
    }

    /// Token provider for the current tenant, rebuilt if `tenant_id`
    /// changed since the last send.
    async fn connector_provider(&self, tenant_id: &str) -> Arc<auth::ConnectorTokenProvider> {
        {
            let guard = self.connector.read().await;
            if let Some(handle) = guard.as_ref()
                && handle.tenant_id == tenant_id
            {
                return handle.provider.clone();
            }
        }
        let mut guard = self.connector.write().await;
        if let Some(handle) = guard.as_ref()
            && handle.tenant_id == tenant_id
        {
            return handle.provider.clone();
        }
        #[cfg(test)]
        let token_url = self
            .token_url_override
            .clone()
            .unwrap_or_else(|| auth::connector_token_url(tenant_id));
        #[cfg(not(test))]
        let token_url = auth::connector_token_url(tenant_id);
        let provider = Arc::new(auth::ConnectorTokenProvider::new(token_url));
        *guard = Some(ConnectorHandle {
            tenant_id: tenant_id.to_string(),
            provider: provider.clone(),
        });
        provider
    }

    /// Build the inbound activity router. Split from `listen()` so tests
    /// can bind an ephemeral port around the same handler.
    fn router(&self, path: &str, tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> Router {
        let state = Arc::new(ListenerState {
            alias: self.alias.clone(),
            tx,
            config_resolver: self.config_resolver.clone(),
            peer_resolver: self.peer_resolver.clone(),
            validator: self.validator.clone(),
            conversations: self.conversations.clone(),
            bot_identity: self.bot_identity.clone(),
            counter: AtomicU64::new(0),
        });
        Router::new()
            .route(path, post(handle_activity))
            .with_state(state)
    }
}

struct ListenerState {
    alias: String,
    tx: tokio::sync::mpsc::Sender<ChannelMessage>,
    config_resolver: ConfigResolver,
    peer_resolver: PeerResolver,
    validator: Arc<auth::JwtValidator>,
    conversations: Arc<ConversationStore>,
    bot_identity: Arc<OnceLock<BotIdentity>>,
    counter: AtomicU64,
}

async fn handle_activity(
    State(state): State<Arc<ListenerState>>,
    headers: HeaderMap,
    body: Bytes,
) -> StatusCode {
    let Some(cfg) = (state.config_resolver)() else {
        return StatusCode::SERVICE_UNAVAILABLE;
    };

    // Authenticate before touching the body.
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(auth::bearer_token);
    let Some(token) = token else {
        return StatusCode::UNAUTHORIZED;
    };
    let issuers = auth::allowed_issuers(&cfg.tenant_id);
    if let Err(err) = state.validator.validate(token, &cfg.app_id, &issuers).await {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                .with_attrs(::serde_json::json!({"error": format!("{err}")})),
            "rejecting inbound Teams activity: JWT validation failed"
        );
        return StatusCode::UNAUTHORIZED;
    }

    let activity: Activity = match serde_json::from_slice(&body) {
        Ok(a) => a,
        Err(err) => {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({"error": format!("{err}")})),
                "invalid Teams activity payload"
            );
            return StatusCode::BAD_REQUEST;
        }
    };

    process_activity(&state, &cfg, activity).await
}

/// Everything after authentication: reference recording, gating, and
/// `ChannelMessage` construction. All drops return 200 so Teams does not
/// retry delivery.
async fn process_activity(
    state: &ListenerState,
    cfg: &MSTeamsConfig,
    activity: Activity,
) -> StatusCode {
    // Record the conversation reference on every activity type; proactive
    // sends need it even if this particular activity is gated below.
    if let (Some(service_url), Some(conversation)) = (&activity.service_url, &activity.conversation)
    {
        let (base_id, _) = activity::split_conversation_id(&conversation.id);
        state.conversations.record(ConversationReference {
            service_url: service_url.clone(),
            conversation_id: base_id.to_string(),
            conversation_type: conversation.conversation_type.clone(),
        });
    }
    if let Some(recipient) = &activity.recipient {
        let _ = state.bot_identity.set(BotIdentity {
            id: recipient.id.clone(),
            name: recipient.name.clone(),
        });
    }

    if activity.activity_type != "message" {
        return StatusCode::OK;
    }
    let Some(from) = &activity.from else {
        return StatusCode::OK;
    };

    // Self-loop guard: never react to the bot's own activities.
    if activity
        .recipient
        .as_ref()
        .is_some_and(|recipient| recipient.id == from.id)
    {
        return StatusCode::OK;
    }

    let personal = activity.is_personal();
    if personal && !cfg.allow_dms {
        return StatusCode::OK;
    }
    if !personal
        && cfg.mention_only.unwrap_or(true)
        && !activity
            .recipient
            .as_ref()
            .is_some_and(|recipient| activity.mentions(&recipient.id))
    {
        return StatusCode::OK;
    }

    // Sender allowlist: match the stable Entra object id when Teams
    // provides it, else the channel-scoped `29:` id. Empty list denies
    // everyone, `"*"` allows everyone (shared allowlist semantics).
    let peers = (state.peer_resolver)();
    let candidates = [from.aad_object_id.as_deref(), Some(from.id.as_str())];
    let allowed = candidates.into_iter().flatten().any(|candidate| {
        crate::allowlist::is_user_allowed(
            &peers,
            candidate,
            crate::allowlist::Match::CaseInsensitive,
        )
    });
    if !allowed {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                .with_attrs(::serde_json::json!({"sender": from.id})),
            "dropping Teams message from sender outside peer allowlist"
        );
        return StatusCode::OK;
    }

    let text = activity
        .text
        .as_deref()
        .map(activity::clean_message_text)
        .unwrap_or_default();
    if text.is_empty() {
        return StatusCode::OK;
    }

    let Some(conversation) = &activity.conversation else {
        return StatusCode::OK;
    };
    let (base_id, thread_suffix) = activity::split_conversation_id(&conversation.id);
    let is_team_channel = conversation.conversation_type.as_deref() == Some("channel");
    // In team channels, reply in-thread: on the existing thread root when
    // the message came from one, else on the triggering message itself.
    let thread_ts = thread_suffix
        .map(str::to_string)
        .or_else(|| is_team_channel.then(|| activity.id.clone()).flatten());

    let seq = state.counter.fetch_add(1, Ordering::Relaxed);
    let explicitly_addressed = personal
        || activity
            .recipient
            .as_ref()
            .is_some_and(|recipient| activity.mentions(&recipient.id));

    let msg = ChannelMessage {
        channel_alias: Some(state.alias.clone()),
        thread_ts,
        interruption_scope_id: thread_suffix.map(str::to_string),
        explicitly_addressed,
        ..ChannelMessage::new(
            activity
                .id
                .clone()
                .unwrap_or_else(|| format!("msteams_{seq}")),
            from.aad_object_id
                .clone()
                .unwrap_or_else(|| from.id.clone()),
            base_id,
            text,
            "msteams",
            activity.timestamp_secs(),
        )
    };

    if state.tx.send(msg).await.is_err() {
        return StatusCode::SERVICE_UNAVAILABLE;
    }
    StatusCode::OK
}

impl ::zeroclaw_api::attribution::Attributable for MsTeamsChannel {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Channel(
            ::zeroclaw_api::attribution::ChannelKind::MsTeams,
        )
    }
    fn alias(&self) -> &str {
        &self.alias
    }
}

#[async_trait]
impl Channel for MsTeamsChannel {
    fn name(&self) -> &str {
        "msteams"
    }

    async fn send(&self, message: &SendMessage) -> Result<()> {
        let cfg = self.config().with_context(|| {
            format!(
                "Microsoft Teams channel '{}' has no [channels.msteams.{}] config block",
                self.alias, self.alias
            )
        })?;

        let (base_id, _) = activity::split_conversation_id(&message.recipient);
        let reference = self.conversations.get(base_id).with_context(|| {
            format!(
                "no conversation reference for '{base_id}': references are in-memory only, \
                 so the peer must message the bot (again) after a daemon restart before \
                 proactive sends can reach them"
            )
        })?;

        let provider = self.connector_provider(&cfg.tenant_id).await;
        let token = provider.token(&cfg.app_id, &cfg.app_password).await?;

        let conversation_id = match message.thread_ts.as_deref() {
            Some(ts) if !ts.is_empty() => format!("{base_id};messageid={ts}"),
            _ => base_id.to_string(),
        };
        let mut url = url::Url::parse(&reference.service_url)
            .with_context(|| format!("invalid service_url '{}'", reference.service_url))?;
        url.path_segments_mut()
            .map_err(|()| {
                anyhow::Error::msg(format!(
                    "service_url '{}' cannot be a base",
                    reference.service_url
                ))
            })?
            .pop_if_empty()
            .extend(["v3", "conversations", &conversation_id, "activities"]);

        let response = self
            .http_client(cfg.proxy_url.as_deref())
            .post(url)
            .bearer_auth(token)
            .json(&serde_json::json!({ "type": "message", "text": message.content }))
            .send()
            .await
            .context("Teams Connector send request failed")?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("Teams Connector send failed ({status}): {body}");
        }
        Ok(())
    }

    async fn listen(&self, tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> Result<()> {
        let cfg = self.config().with_context(|| {
            format!(
                "Microsoft Teams channel '{}' has no [channels.msteams.{}] config block",
                self.alias, self.alias
            )
        })?;
        if cfg.app_id.trim().is_empty() || cfg.tenant_id.trim().is_empty() {
            anyhow::bail!(
                "Microsoft Teams channel '{}' requires `app_id` and `tenant_id`: without \
                 them inbound activities cannot be authenticated; set them under \
                 [channels.msteams.{}]",
                self.alias,
                self.alias,
            );
        }

        let path = if cfg.path.starts_with('/') {
            cfg.path.clone()
        } else {
            format!("/{}", cfg.path)
        };
        let app = self.router(&path, tx);

        let addr = std::net::SocketAddr::from(([0, 0, 0, 0], cfg.port));
        let listener = tokio::net::TcpListener::bind(addr).await?;
        self.listener_ready.store(true, Ordering::Release);
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
            &format!(
                "Microsoft Teams channel listening on http://0.0.0.0:{}{path} ...",
                cfg.port
            )
        );

        axum::serve(listener, app)
            .await
            .map_err(|e| anyhow::Error::msg(format!("Teams activity listener error: {e}")))?;
        Ok(())
    }

    async fn health_check(&self) -> bool {
        self.listener_ready.load(Ordering::Acquire)
    }

    fn self_handle(&self) -> Option<String> {
        self.bot_identity.get().map(|identity| identity.id.clone())
    }

    fn self_addressed_mention(&self) -> Option<String> {
        self.bot_identity
            .get()
            .and_then(|identity| identity.name.clone())
            .map(|name| format!("<at>{name}</at>"))
    }

    fn is_direct_message(&self, msg: &ChannelMessage) -> bool {
        let (base_id, _) = activity::split_conversation_id(&msg.reply_target);
        self.conversations
            .get(base_id)
            .is_some_and(|reference| reference.is_personal())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jsonwebtoken::{Algorithm, EncodingKey, Header};
    use serde::Serialize;
    use wiremock::matchers::{body_partial_json, header as header_matcher, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    use zeroclaw_api::attribution::Attributable;

    const APP_ID: &str = "00000000-aaaa-bbbb-cccc-000000000000";
    const TENANT_ID: &str = "00000000-1111-2222-3333-000000000000";
    const TEST_KID: &str = "listener-test-key";
    /// Base64url RSA modulus of `auth::TEST_KEY_PEM`'s public half.
    const TEST_KEY_N: &str = "xX2UGrUUorIz6usPOp1zydsNMyL9Uy93wWSwLpJUY6HkZFW17wGqGVsZB2Sp6oUt\
                              ESOKHdCpSYeujymfj-EHVuClStkXdzKx2HcRa4R4yT87qG5BUIxt3p6fWd_7exYe\
                              H4YOKf-LwUwJU4TPMxU-ephQY9CfTVB1bQZG3TmIiqSEgR7NHCEawaZOC2e-eUXw\
                              Nt27IC36dYun2NX89NN7O3Rr_oAsQKWIf3GtSNdtFLdKSa4LDeXu_sl0uhR7zMyv\
                              ncuYW7nTso4MmLosar3qCDKgsA-MjKVyQDEq0Qb22WIMjVmF68NSah6IilXmjoIL\
                              G2OCDnwGMmWFll6E9WYuAQ";

    #[derive(Serialize)]
    struct TestClaims {
        iss: String,
        aud: String,
        exp: i64,
    }

    fn mint_service_token() -> String {
        let mut header = Header::new(Algorithm::RS256);
        header.kid = Some(TEST_KID.to_string());
        let claims = TestClaims {
            iss: auth::BOT_FRAMEWORK_ISSUER.to_string(),
            aud: APP_ID.to_string(),
            exp: chrono::Utc::now().timestamp() + 3600,
        };
        let key = EncodingKey::from_rsa_pem(auth::TEST_KEY_PEM.as_bytes()).unwrap();
        jsonwebtoken::encode(&header, &claims, &key).unwrap()
    }

    async fn mock_jwks(server: &MockServer) {
        Mock::given(method("GET"))
            .and(path("/metadata"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "issuer": auth::BOT_FRAMEWORK_ISSUER,
                "jwks_uri": format!("{}/keys", server.uri()),
            })))
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .and(path("/keys"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "keys": [{ "kty": "RSA", "use": "sig", "kid": TEST_KID, "n": TEST_KEY_N, "e": "AQAB" }]
            })))
            .mount(server)
            .await;
    }

    fn test_config() -> MSTeamsConfig {
        MSTeamsConfig {
            enabled: true,
            app_id: APP_ID.to_string(),
            app_password: "test-secret".to_string(),
            tenant_id: TENANT_ID.to_string(),
            ..MSTeamsConfig::default()
        }
    }

    fn channel_with(
        config: MSTeamsConfig,
        peers: Vec<String>,
        auth_server: &MockServer,
    ) -> MsTeamsChannel {
        MsTeamsChannel::new(
            "default",
            Arc::new(move || Some(config.clone())),
            Arc::new(move || peers.clone()),
        )
        .with_openid_metadata_url(format!("{}/metadata", auth_server.uri()))
    }

    /// Bind the channel's router on an ephemeral port; returns the base
    /// URL and the inbound message receiver.
    async fn spawn_listener(
        channel: &MsTeamsChannel,
    ) -> (String, tokio::sync::mpsc::Receiver<ChannelMessage>) {
        let (tx, rx) = tokio::sync::mpsc::channel(16);
        let app = channel.router("/api/messages", tx);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        zeroclaw_spawn::spawn!(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (format!("http://{addr}/api/messages"), rx)
    }

    fn personal_activity(text: &str) -> serde_json::Value {
        serde_json::json!({
            "type": "message",
            "id": "1712345",
            "timestamp": "2026-07-18T02:00:00.000Z",
            "serviceUrl": "https://smba.trafficmanager.net/teams/",
            "from": { "id": "29:user-x", "name": "User X", "aadObjectId": "00000000-0000-0000-0000-00000000feed" },
            "recipient": { "id": "28:bot", "name": "ZeroClaw" },
            "conversation": { "id": "a:1conv", "conversationType": "personal" },
            "text": text,
        })
    }

    async fn post_activity(
        url: &str,
        token: &str,
        activity: &serde_json::Value,
    ) -> reqwest::StatusCode {
        reqwest::Client::new()
            .post(url)
            .bearer_auth(token)
            .json(activity)
            .send()
            .await
            .unwrap()
            .status()
    }

    #[test]
    fn name_and_attribution() {
        let ch = MsTeamsChannel::new(
            "default",
            Arc::new(|| Some(MSTeamsConfig::default())),
            Arc::new(Vec::new),
        );
        assert_eq!(ch.name(), "msteams");
        assert_eq!(Attributable::alias(&ch), "default");
        assert!(matches!(
            ch.role(),
            zeroclaw_api::attribution::Role::Channel(
                zeroclaw_api::attribution::ChannelKind::MsTeams
            )
        ));
    }

    #[tokio::test]
    async fn listen_requires_app_id_and_tenant() {
        let ch = MsTeamsChannel::new(
            "default",
            Arc::new(|| Some(MSTeamsConfig::default())),
            Arc::new(Vec::new),
        );
        let (tx, _rx) = tokio::sync::mpsc::channel(1);
        let err = ch.listen(tx).await.unwrap_err();
        assert!(
            err.to_string()
                .contains("requires `app_id` and `tenant_id`")
        );
        assert!(!ch.health_check().await);
    }

    #[tokio::test]
    async fn valid_personal_message_produces_channel_message() {
        let auth_server = MockServer::start().await;
        mock_jwks(&auth_server).await;
        let ch = channel_with(test_config(), vec!["*".to_string()], &auth_server);
        let (url, mut rx) = spawn_listener(&ch).await;

        let token = mint_service_token();
        let activity = personal_activity("<at>ZeroClaw</at> 1 &lt; 2");
        assert_eq!(post_activity(&url, &token, &activity).await, 200);

        let msg = rx.recv().await.unwrap();
        assert_eq!(msg.channel, "msteams");
        assert_eq!(msg.channel_alias.as_deref(), Some("default"));
        assert_eq!(msg.sender, "00000000-0000-0000-0000-00000000feed");
        assert_eq!(msg.reply_target, "a:1conv");
        assert_eq!(msg.content, "1 < 2");
        assert!(msg.explicitly_addressed);
        assert!(msg.thread_ts.is_none());

        // The activity recorded the conversation reference and identity.
        assert!(ch.is_direct_message(&msg));
        assert_eq!(ch.self_handle().as_deref(), Some("28:bot"));
        assert_eq!(
            ch.self_addressed_mention().as_deref(),
            Some("<at>ZeroClaw</at>")
        );
    }

    #[tokio::test]
    async fn missing_or_invalid_token_is_rejected() {
        let auth_server = MockServer::start().await;
        mock_jwks(&auth_server).await;
        let ch = channel_with(test_config(), vec!["*".to_string()], &auth_server);
        let (url, mut rx) = spawn_listener(&ch).await;
        let activity = personal_activity("hi");

        let no_auth = reqwest::Client::new()
            .post(&url)
            .json(&activity)
            .send()
            .await
            .unwrap()
            .status();
        assert_eq!(no_auth, 401);
        assert_eq!(post_activity(&url, "garbage-token", &activity).await, 401);
        assert!(
            rx.try_recv().is_err(),
            "rejected requests must not produce messages"
        );
    }

    #[tokio::test]
    async fn dm_gate_drops_personal_chats_when_disabled() {
        let auth_server = MockServer::start().await;
        mock_jwks(&auth_server).await;
        let cfg = MSTeamsConfig {
            allow_dms: false,
            ..test_config()
        };
        let ch = channel_with(cfg, vec!["*".to_string()], &auth_server);
        let (url, mut rx) = spawn_listener(&ch).await;

        let token = mint_service_token();
        assert_eq!(
            post_activity(&url, &token, &personal_activity("hi")).await,
            200
        );
        assert!(rx.try_recv().is_err());
    }

    fn channel_activity(text: &str, mention_bot: bool) -> serde_json::Value {
        let entities = if mention_bot {
            serde_json::json!([{ "type": "mention", "mentioned": { "id": "28:bot", "name": "ZeroClaw" } }])
        } else {
            serde_json::json!([])
        };
        serde_json::json!({
            "type": "message",
            "id": "1800",
            "serviceUrl": "https://smba.trafficmanager.net/teams/",
            "from": { "id": "29:user-x" },
            "recipient": { "id": "28:bot", "name": "ZeroClaw" },
            "conversation": {
                "id": "19:general@thread.tacv2;messageid=1700",
                "conversationType": "channel"
            },
            "text": text,
            "entities": entities,
        })
    }

    #[tokio::test]
    async fn mention_gate_applies_to_team_channels_only() {
        let auth_server = MockServer::start().await;
        mock_jwks(&auth_server).await;
        let ch = channel_with(test_config(), vec!["*".to_string()], &auth_server);
        let (url, mut rx) = spawn_listener(&ch).await;
        let token = mint_service_token();

        // Unmentioned channel message: dropped (mention_only defaults on).
        assert_eq!(
            post_activity(&url, &token, &channel_activity("status?", false)).await,
            200
        );
        assert!(rx.try_recv().is_err());

        // Mentioned channel message: delivered, threaded on the thread root.
        assert_eq!(
            post_activity(
                &url,
                &token,
                &channel_activity("<at>ZeroClaw</at> status?", true)
            )
            .await,
            200
        );
        let msg = rx.recv().await.unwrap();
        assert_eq!(msg.reply_target, "19:general@thread.tacv2");
        assert_eq!(msg.thread_ts.as_deref(), Some("1700"));
        assert_eq!(msg.interruption_scope_id.as_deref(), Some("1700"));
        assert_eq!(msg.content, "status?");
        assert_eq!(msg.sender, "29:user-x");
        assert!(!ch.is_direct_message(&msg));
    }

    #[tokio::test]
    async fn empty_peer_list_denies_everyone() {
        let auth_server = MockServer::start().await;
        mock_jwks(&auth_server).await;
        let ch = channel_with(test_config(), Vec::new(), &auth_server);
        let (url, mut rx) = spawn_listener(&ch).await;

        let token = mint_service_token();
        assert_eq!(
            post_activity(&url, &token, &personal_activity("hi")).await,
            200
        );
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn allowlist_matches_aad_object_id() {
        let auth_server = MockServer::start().await;
        mock_jwks(&auth_server).await;
        let ch = channel_with(
            test_config(),
            vec!["00000000-0000-0000-0000-00000000FEED".to_string()],
            &auth_server,
        );
        let (url, mut rx) = spawn_listener(&ch).await;

        let token = mint_service_token();
        assert_eq!(
            post_activity(&url, &token, &personal_activity("hi")).await,
            200
        );
        assert!(rx.recv().await.is_some());
    }

    #[tokio::test]
    async fn self_authored_activity_is_dropped() {
        let auth_server = MockServer::start().await;
        mock_jwks(&auth_server).await;
        let ch = channel_with(test_config(), vec!["*".to_string()], &auth_server);
        let (url, mut rx) = spawn_listener(&ch).await;

        let mut activity = personal_activity("echo");
        activity["from"] = serde_json::json!({ "id": "28:bot", "name": "ZeroClaw" });
        let token = mint_service_token();
        assert_eq!(post_activity(&url, &token, &activity).await, 200);
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn non_message_activities_are_acknowledged_without_output() {
        let auth_server = MockServer::start().await;
        mock_jwks(&auth_server).await;
        let ch = channel_with(test_config(), vec!["*".to_string()], &auth_server);
        let (url, mut rx) = spawn_listener(&ch).await;

        let token = mint_service_token();
        let update = serde_json::json!({
            "type": "conversationUpdate",
            "serviceUrl": "https://smba.trafficmanager.net/teams/",
            "conversation": { "id": "a:1conv", "conversationType": "personal" },
            "recipient": { "id": "28:bot", "name": "ZeroClaw" },
        });
        assert_eq!(post_activity(&url, &token, &update).await, 200);
        assert!(rx.try_recv().is_err());
        // But it still recorded the reference and bot identity.
        assert_eq!(ch.self_handle().as_deref(), Some("28:bot"));
        assert!(ch.conversations.get("a:1conv").is_some());
    }

    #[tokio::test]
    async fn send_posts_to_connector_with_bearer_token() {
        let connector = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "connector-tok",
                "expires_in": 3600,
            })))
            .expect(1)
            .mount(&connector)
            .await;
        Mock::given(method("POST"))
            .and(path("/teams/v3/conversations/a:1conv/activities"))
            .and(header_matcher("authorization", "Bearer connector-tok"))
            .and(body_partial_json(
                serde_json::json!({ "type": "message", "text": "hello from zeroclaw" }),
            ))
            .respond_with(
                ResponseTemplate::new(201).set_body_json(serde_json::json!({ "id": "act-1" })),
            )
            .expect(2)
            .mount(&connector)
            .await;

        let ch = MsTeamsChannel::new(
            "default",
            Arc::new(|| Some(test_config())),
            Arc::new(Vec::new),
        )
        .with_token_url(format!("{}/token", connector.uri()));
        ch.conversations.record(ConversationReference {
            service_url: format!("{}/teams/", connector.uri()),
            conversation_id: "a:1conv".to_string(),
            conversation_type: Some("personal".to_string()),
        });

        ch.send(&SendMessage::new("hello from zeroclaw", "a:1conv"))
            .await
            .unwrap();
        // Second send reuses the cached connector token (token mock allows
        // exactly one hit).
        ch.send(&SendMessage::new("hello from zeroclaw", "a:1conv"))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn send_threads_via_messageid_suffix() {
        let connector = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "connector-tok",
                "expires_in": 3600,
            })))
            .mount(&connector)
            .await;
        Mock::given(method("POST"))
            .and(path(
                "/teams/v3/conversations/19:general@thread.tacv2;messageid=1700/activities",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&connector)
            .await;

        let ch = MsTeamsChannel::new(
            "default",
            Arc::new(|| Some(test_config())),
            Arc::new(Vec::new),
        )
        .with_token_url(format!("{}/token", connector.uri()));
        ch.conversations.record(ConversationReference {
            service_url: format!("{}/teams/", connector.uri()),
            conversation_id: "19:general@thread.tacv2".to_string(),
            conversation_type: Some("channel".to_string()),
        });

        let message = SendMessage::new("threaded reply", "19:general@thread.tacv2")
            .in_thread(Some("1700".to_string()));
        ch.send(&message).await.unwrap();
    }

    #[tokio::test]
    async fn send_without_reference_fails_with_clear_error() {
        let ch = MsTeamsChannel::new(
            "default",
            Arc::new(|| Some(test_config())),
            Arc::new(Vec::new),
        );
        let err = ch
            .send(&SendMessage::new("hi", "a:unknown"))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("no conversation reference"));
    }

    #[tokio::test]
    async fn send_surfaces_connector_error_body() {
        let connector = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "connector-tok",
                "expires_in": 3600,
            })))
            .mount(&connector)
            .await;
        Mock::given(method("POST"))
            .and(path("/teams/v3/conversations/a:1conv/activities"))
            .respond_with(
                ResponseTemplate::new(403)
                    .set_body_string(r#"{"error":"BotNotInConversationRoster"}"#),
            )
            .mount(&connector)
            .await;

        let ch = MsTeamsChannel::new(
            "default",
            Arc::new(|| Some(test_config())),
            Arc::new(Vec::new),
        )
        .with_token_url(format!("{}/token", connector.uri()));
        ch.conversations.record(ConversationReference {
            service_url: format!("{}/teams/", connector.uri()),
            conversation_id: "a:1conv".to_string(),
            conversation_type: Some("personal".to_string()),
        });

        let err = ch
            .send(&SendMessage::new("hi", "a:1conv"))
            .await
            .unwrap_err();
        let text = err.to_string();
        assert!(text.contains("403"), "missing status in: {text}");
        assert!(text.contains("BotNotInConversationRoster"));
    }
}
