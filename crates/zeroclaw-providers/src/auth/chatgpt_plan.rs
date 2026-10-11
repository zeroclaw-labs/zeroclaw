//! OpenAI's OSS ChatGPT plan-usage grant, separate from legacy Codex auth.
//! Endpoints are pinned; test endpoint injection is private to this module.
use super::profiles::{AuthProfile, ChatGptPlanRegistration, TokenSet, profile_id};
use super::{AuthFlowContext, AuthProviderFlow, AuthService, RefreshStatus, oauth_common};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header, jwk::JwkSet};
use serde::Deserialize;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub const PROVIDER: &str = "chatgpt-plan";
pub const RESOURCE: &str = "https://api.openai.com/v1";
const ISSUER: &str = "https://auth.openai.com";
const PLAN_SCOPE: &str = "chatgpt.tokens.use.direct";
const CALLBACK_TIMEOUT: Duration = Duration::from_secs(300);

fn protocol_error(message: &'static str) -> anyhow::Error {
    ::zeroclaw_log::record!(
        WARN,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
            .with_attrs(serde_json::json!({"reason":message})),
        "ChatGPT authorization protocol failure"
    );
    anyhow::Error::msg(message)
}

#[derive(Clone)]
struct PlanOAuth {
    authorize: String,
    token: String,
    jwks: String,
}
impl Default for PlanOAuth {
    fn default() -> Self {
        #[cfg(any(test, feature = "test-helpers"))]
        if let Ok(endpoints) = crate::plan_test_transport::ENDPOINTS.try_with(Clone::clone) {
            return Self {
                authorize: endpoints.authorize,
                token: endpoints.token,
                jwks: endpoints.jwks,
            };
        }
        Self {
            authorize: format!("{ISSUER}/api/accounts/authorize"),
            token: format!("{ISSUER}/api/accounts/oauth/token"),
            jwks: format!("{ISSUER}/.well-known/jwks.json"),
        }
    }
}

fn client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(30))
        .build()
        .context("Unable to create ChatGPT plan HTTP client")
}

// No Debug/Serialize: contains the verifier, nonce and pending identity.
struct Attempt {
    authorize_url: String,
    profile: String,
    redirect_uri: String,
    pkce: oauth_common::PkceState,
    nonce: String,
    returning: Option<ChatGptPlanRegistration>,
    created: std::time::Instant,
}
struct Callback {
    code: String,
    client_id: String,
}

fn issued_client_id(value: &str) -> bool {
    !value.trim().is_empty()
        && value.len() <= 512
        && value != "dynamic_agent_client"
        && !value.chars().any(char::is_control)
}

fn loopback_uri(uri: &str) -> Result<reqwest::Url> {
    let url =
        reqwest::Url::parse(uri).map_err(|_| protocol_error("Invalid ChatGPT callback URI"))?;
    anyhow::ensure!(
        url.scheme() == "http"
            && url.host_str() == Some("127.0.0.1")
            && url.port().is_some()
            && url.path() == "/auth/callback"
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none(),
        "ChatGPT callback must use http://127.0.0.1:<port>/auth/callback"
    );
    Ok(url)
}

impl Attempt {
    fn validated_query(&self, value: &str) -> Result<std::collections::BTreeMap<String, String>> {
        anyhow::ensure!(
            self.created.elapsed() <= CALLBACK_TIMEOUT,
            "ChatGPT authorization attempt expired"
        );
        let url =
            reqwest::Url::parse(value).map_err(|_| protocol_error("Invalid ChatGPT callback"))?;
        let mut base = url.clone();
        base.set_query(None);
        anyhow::ensure!(
            base.as_str() == self.redirect_uri,
            "ChatGPT callback URI mismatch"
        );
        let mut query = std::collections::BTreeMap::new();
        for (key, value) in url.query_pairs() {
            anyhow::ensure!(
                query.insert(key.into_owned(), value.into_owned()).is_none(),
                "Duplicate ChatGPT callback parameter"
            );
        }
        anyhow::ensure!(
            query.get("state") == Some(&self.pkce.state),
            "ChatGPT callback state mismatch"
        );
        Ok(query)
    }

    fn callback(&self, value: &str) -> Result<Callback> {
        let query = self.validated_query(value)?;
        anyhow::ensure!(
            !query.contains_key("error"),
            "ChatGPT authorization was denied or failed"
        );
        let supplied = query.get("client_id");
        let client_id = match &self.returning {
            Some(registration) => {
                anyhow::ensure!(
                    supplied.is_none_or(|id| id == &registration.client_id),
                    "ChatGPT callback client changed"
                );
                registration.client_id.clone()
            }
            None => supplied
                .cloned()
                .context("ChatGPT registration callback missing issued client ID")?,
        };
        anyhow::ensure!(
            issued_client_id(&client_id),
            "Invalid issued ChatGPT client ID"
        );
        let code = query
            .get("code")
            .filter(|code| !code.is_empty())
            .cloned()
            .context("ChatGPT callback missing authorization code")?;
        Ok(Callback { code, client_id })
    }
}

#[derive(Deserialize)]
struct Identity {
    sub: String,
    iat: i64,
    nonce: Option<String>,
}

fn verify_identity(
    token: &str,
    jwks: &JwkSet,
    client_id: &str,
    nonce: Option<&str>,
) -> Result<String> {
    let header =
        decode_header(token).map_err(|_| protocol_error("Invalid ChatGPT ID token header"))?;
    anyhow::ensure!(
        matches!(header.alg, Algorithm::RS256 | Algorithm::ES256),
        "Unsupported ChatGPT ID token algorithm"
    );
    let key = header
        .kid
        .as_deref()
        .and_then(|kid| jwks.find(kid))
        .context("ChatGPT ID token signing key unavailable")?;
    let key =
        DecodingKey::from_jwk(key).map_err(|_| protocol_error("Invalid ChatGPT signing key"))?;
    let mut validation = Validation::new(header.alg);
    validation.leeway = 5;
    validation.validate_nbf = true;
    validation.set_issuer(&[ISSUER]);
    validation.set_audience(&[client_id]);
    validation.set_required_spec_claims(&["sub", "iss", "aud", "iat", "exp"]);
    let identity = decode::<Identity>(token, &key, &validation)
        .map_err(|_| protocol_error("ChatGPT ID token verification failed"))?
        .claims;
    anyhow::ensure!(
        !identity.sub.trim().is_empty() && identity.iat <= Utc::now().timestamp() + 5,
        "Invalid ChatGPT ID token identity or issuance time"
    );
    if let Some(expected) = nonce {
        anyhow::ensure!(
            identity.nonce.as_deref() == Some(expected),
            "ChatGPT ID token nonce mismatch"
        );
    }
    Ok(identity.sub)
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: String,
    #[serde(default)]
    id_token: Option<String>,
    token_type: String,
    expires_in: i64,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    earliest_refresh_at: Option<serde_json::Value>,
}

impl TokenResponse {
    fn into_tokens(self, old_scope: Option<&str>) -> Result<(TokenSet, Option<DateTime<Utc>>)> {
        anyhow::ensure!(
            !self.access_token.trim().is_empty()
                && !self.refresh_token.trim().is_empty()
                && self.token_type.eq_ignore_ascii_case("bearer")
                && self.expires_in > 0
                && self.expires_in <= 3600,
            "Incomplete ChatGPT token response"
        );
        let scope = self.scope.or_else(|| old_scope.map(str::to_owned));
        require_plan_scope(scope.as_deref())?;
        let earliest = match self.earliest_refresh_at {
            None | Some(serde_json::Value::Null) => None,
            Some(serde_json::Value::Number(value)) => Some(
                DateTime::from_timestamp(
                    value
                        .as_i64()
                        .context("Invalid ChatGPT refresh timestamp")?,
                    0,
                )
                .context("Invalid ChatGPT refresh timestamp")?,
            ),
            Some(serde_json::Value::String(value)) => Some(
                DateTime::parse_from_rfc3339(&value)
                    .map_err(|_| protocol_error("Invalid ChatGPT refresh timestamp"))?
                    .with_timezone(&Utc),
            ),
            Some(_) => anyhow::bail!("Invalid ChatGPT refresh timestamp"),
        };
        Ok((
            TokenSet {
                access_token: self.access_token,
                refresh_token: Some(self.refresh_token),
                id_token: self.id_token,
                expires_at: Some(Utc::now() + chrono::Duration::seconds(self.expires_in)),
                token_type: Some(self.token_type),
                scope,
            },
            earliest,
        ))
    }
}

fn require_plan_scope(scope: Option<&str>) -> Result<()> {
    anyhow::ensure!(
        scope.is_some_and(|scope| scope.split_whitespace().any(|s| s == PLAN_SCOPE)),
        "ChatGPT plan usage permission was not granted"
    );
    Ok(())
}

async fn json_response<T: serde::de::DeserializeOwned>(response: reqwest::Response) -> Result<T> {
    anyhow::ensure!(
        response.status() == reqwest::StatusCode::OK,
        "ChatGPT authorization endpoint rejected the request (HTTP {})",
        response.status().as_u16()
    );
    let bytes = crate::compatible::read_body_capped(response, 1024 * 1024)
        .await
        .map_err(|_| protocol_error("Unable to read ChatGPT authorization response"))?;
    serde_json::from_slice(&bytes)
        .map_err(|_| protocol_error("Invalid ChatGPT authorization response"))
}

impl PlanOAuth {
    async fn begin(
        &self,
        service: &AuthService,
        profile: &str,
        redirect_uri: &str,
    ) -> Result<Attempt> {
        anyhow::ensure!(
            cfg!(unix),
            "ChatGPT plan usage is unsupported on native Windows in this slice"
        );
        loopback_uri(redirect_uri)?;
        anyhow::ensure!(
            !profile.is_empty()
                && profile
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-'),
            "Invalid ChatGPT profile label"
        );
        let host = service.store.ensure_plan_host_id().await?;
        let data = service.store.load().await?;
        let returning = data
            .profiles
            .get(&profile_id(PROVIDER, profile))
            .map(|saved| {
                saved
                    .plan_registration
                    .clone()
                    .context("Selected profile is not a ChatGPT registration")
            })
            .transpose()?;
        let pkce = oauth_common::generate_pkce_state();
        let nonce = oauth_common::random_base64url(32);
        let mut url = reqwest::Url::parse(&self.authorize)?;
        let mut query = url.query_pairs_mut();
        query
            .append_pair(
                "client_id",
                returning
                    .as_ref()
                    .map_or("dynamic_agent_client", |r| &r.client_id),
            )
            .append_pair("ext_agent_host_id", &host)
            .append_pair("response_type", "code")
            .append_pair("redirect_uri", redirect_uri)
            .append_pair(
                "scope",
                "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct",
            )
            .append_pair("resource", RESOURCE)
            .append_pair("state", &pkce.state)
            .append_pair("nonce", &nonce)
            .append_pair("code_challenge_method", "S256")
            .append_pair("code_challenge", &pkce.code_challenge);
        if returning.is_none() {
            query.append_pair("agent_name_hint", "ZeroClaw");
        }
        // Omit optional ID-token/email hints from a URL displayed in a terminal.
        drop(query);
        Ok(Attempt {
            authorize_url: url.into(),
            profile: profile.into(),
            redirect_uri: redirect_uri.into(),
            pkce,
            nonce,
            returning,
            created: std::time::Instant::now(),
        })
    }

    async fn complete(
        &self,
        service: &AuthService,
        attempt: Attempt,
        callback_url: &str,
    ) -> Result<()> {
        let callback = attempt.callback(callback_url)?;
        let client = client()?;
        let response = client
            .post(&self.token)
            .form(&[
                ("grant_type", "authorization_code"),
                ("client_id", &callback.client_id),
                ("code", &callback.code),
                ("code_verifier", &attempt.pkce.code_verifier),
                ("redirect_uri", &attempt.redirect_uri),
                ("resource", RESOURCE),
            ])
            .send()
            .await
            .map_err(|_| protocol_error("ChatGPT token exchange failed"))?;
        let response: TokenResponse = json_response(response).await?;
        let (tokens, earliest_refresh_at) = response.into_tokens(None)?;
        let jwks: JwkSet = json_response(
            client
                .get(&self.jwks)
                .send()
                .await
                .map_err(|_| protocol_error("Unable to fetch ChatGPT signing keys"))?,
        )
        .await?;
        let subject = verify_identity(
            tokens
                .id_token
                .as_deref()
                .context("ChatGPT token response missing ID token")?,
            &jwks,
            &callback.client_id,
            Some(&attempt.nonce),
        )?;
        if let Some(old) = &attempt.returning {
            anyhow::ensure!(
                subject == old.subject,
                "Returning ChatGPT account identity changed"
            );
        }
        let mut profile = AuthProfile::new_oauth(PROVIDER, &attempt.profile, tokens);
        profile.plan_registration = Some(ChatGptPlanRegistration {
            client_id: callback.client_id,
            subject,
            earliest_refresh_at,
            refresh_started_at: None,
        });
        // Same kernel-held transaction lock used by refresh. Never overwrite a
        // rotated token concurrently with a returning sign-in.
        let _lock = service.store.acquire_plan_refresh_lock(&profile.id).await?;
        service.store.upsert_profile(profile, false).await
    }

    async fn access_token(&self, service: &AuthService, binding: &str) -> Result<String> {
        Ok(self.credential(service, binding).await?.access_token)
    }

    async fn credential(&self, service: &AuthService, binding: &str) -> Result<PlanCredential> {
        anyhow::ensure!(
            binding.starts_with("chatgpt-plan:") && binding.len() > "chatgpt-plan:".len(),
            "Explicit ChatGPT plan registration reference required"
        );
        let data = service.store.load().await?;
        let profile = data
            .profiles
            .get(binding)
            .context("Bound ChatGPT plan registration missing")?;
        let (registration, tokens) = registration_tokens(profile)?;
        if registration.refresh_started_at.is_none()
            && !tokens.is_expiring_within(Duration::from_secs(90))
        {
            return Ok(PlanCredential::from_validated(registration, tokens));
        }
        let _lock = service.store.acquire_plan_refresh_lock(binding).await?;
        // Another process may already have refreshed and rotated this session.
        let fresh = service.store.load().await?;
        let profile = fresh
            .profiles
            .get(binding)
            .context("Bound ChatGPT registration disappeared")?;
        let (current, tokens) = registration_tokens(profile)?;
        anyhow::ensure!(
            current.refresh_started_at.is_none(),
            "ChatGPT refresh outcome is uncertain; sign in again before using this registration"
        );
        anyhow::ensure!(
            current.subject == registration.subject && current.client_id == registration.client_id,
            "Bound ChatGPT registration changed during refresh"
        );
        if !tokens.is_expiring_within(Duration::from_secs(90)) {
            return Ok(PlanCredential::from_validated(current, tokens));
        }
        if let Some(earliest) = current.earliest_refresh_at {
            anyhow::ensure!(
                Utc::now() >= earliest,
                "ChatGPT token cannot be refreshed yet"
            );
        }
        let refresh = tokens
            .refresh_token
            .as_deref()
            .context("ChatGPT session requires sign-in")?;
        let client = client()?;
        // Do not replay a rotating refresh token after a transport ambiguity.
        service
            .store
            .update_profile(binding, |profile| {
                profile
                    .plan_registration
                    .as_mut()
                    .context("Missing ChatGPT registration")?
                    .refresh_started_at = Some(Utc::now());
                Ok(())
            })
            .await?;
        let response = client
            .post(&self.token)
            .form(&[
                ("grant_type", "refresh_token"),
                ("client_id", &current.client_id),
                ("refresh_token", refresh),
                ("resource", RESOURCE),
            ])
            .send()
            .await
            .map_err(|_| protocol_error("ChatGPT session refresh failed; sign in again"))?;
        let response: TokenResponse = json_response(response).await?;
        let (mut replacement, earliest) = response.into_tokens(tokens.scope.as_deref())?;
        if let Some(id_token) = &replacement.id_token {
            let jwks = json_response(
                client
                    .get(&self.jwks)
                    .send()
                    .await
                    .map_err(|_| protocol_error("Unable to fetch ChatGPT signing keys"))?,
            )
            .await?;
            anyhow::ensure!(
                verify_identity(id_token, &jwks, &current.client_id, None)? == current.subject,
                "ChatGPT refresh account identity changed"
            );
        } else {
            replacement.id_token = tokens.id_token.clone();
        }
        let mut updated = profile.clone();
        updated.token_set = Some(replacement);
        updated
            .plan_registration
            .as_mut()
            .context("Missing ChatGPT registration")?
            .earliest_refresh_at = earliest;
        updated
            .plan_registration
            .as_mut()
            .context("Missing ChatGPT registration")?
            .refresh_started_at = None;
        let (registration, tokens) = registration_tokens(&updated)?;
        let credential = PlanCredential::from_validated(registration, tokens);
        service.store.upsert_profile(updated, false).await?;
        Ok(credential)
    }
}

fn registration_tokens(profile: &AuthProfile) -> Result<(&ChatGptPlanRegistration, &TokenSet)> {
    anyhow::ensure!(
        profile.model_provider == PROVIDER
            && profile.kind == super::profiles::AuthProfileKind::OAuth,
        "Bound profile is not ChatGPT plan OAuth"
    );
    let registration = profile
        .plan_registration
        .as_ref()
        .context("ChatGPT registration identity missing")?;
    anyhow::ensure!(
        issued_client_id(&registration.client_id) && !registration.subject.is_empty(),
        "Invalid saved ChatGPT registration"
    );
    let tokens = profile
        .token_set
        .as_ref()
        .context("ChatGPT session is signed out")?;
    require_plan_scope(tokens.scope.as_deref())?;
    anyhow::ensure!(
        !tokens.access_token.trim().is_empty()
            && tokens
                .token_type
                .as_deref()
                .is_some_and(|s| s.eq_ignore_ascii_case("bearer"))
            && tokens.expires_at.is_some(),
        "Invalid ChatGPT access token metadata"
    );
    Ok((registration, tokens))
}

/// Ephemeral bearer and replay identity from one validated canonical profile
/// snapshot. Never persist or independently re-resolve either half.
pub(crate) struct PlanCredential {
    pub(crate) access_token: String,
    pub(crate) registration_provenance: String,
}

impl PlanCredential {
    fn from_validated(registration: &ChatGptPlanRegistration, tokens: &TokenSet) -> Self {
        use sha2::{Digest, Sha256};
        let mut digest = Sha256::new();
        digest.update(b"zeroclaw.chatgpt-plan.replay-registration.v1\0");
        // Length prefixes prevent ambiguous concatenations. Account identifiers
        // stay in the canonical auth store rather than tool/history metadata.
        for part in [&registration.client_id, &registration.subject] {
            digest.update((part.len() as u64).to_be_bytes());
            digest.update(part.as_bytes());
        }
        Self {
            access_token: tokens.access_token.clone(),
            registration_provenance: hex::encode(digest.finalize()),
        }
    }
}

impl AuthService {
    pub(crate) async fn resolve_chatgpt_plan_credential(
        &self,
        binding: &str,
    ) -> Result<PlanCredential> {
        PlanOAuth::default().credential(self, binding).await
    }

    /// Exact registration only; deliberately bypasses active/default selection.
    pub async fn get_valid_chatgpt_plan_access_token(&self, binding: &str) -> Result<String> {
        PlanOAuth::default().access_token(self, binding).await
    }
}

/// Thin CLI orchestration: require an explicit alias binding and exercise
/// the ordinary config/factory/provider seam with no tools or fallbacks.
pub async fn check_bound_provider(
    config: &zeroclaw_config::schema::Config,
    alias: &str,
    message: &str,
) -> Result<String> {
    anyhow::ensure!(
        cfg!(unix),
        "ChatGPT plan usage is unsupported on native Windows in this slice"
    );
    let (family, name) = alias
        .split_once('.')
        .context("ChatGPT plan check requires an explicit openai.<alias> provider")?;
    anyhow::ensure!(
        family == "openai",
        "ChatGPT plan check requires an OpenAI provider alias"
    );
    let entry = config
        .providers
        .models
        .find(family, name)
        .context("ChatGPT plan provider alias missing")?;
    anyhow::ensure!(
        entry.chatgpt_plan_auth.is_some(),
        "ChatGPT plan provider has no explicit registration binding"
    );
    anyhow::ensure!(
        config.reliability.api_keys.is_empty(),
        "ChatGPT plan check does not support reliability API-key rotation"
    );
    let model = entry
        .model
        .as_deref()
        .context("ChatGPT plan provider requires an account-visible model")?;
    let options =
        crate::model_provider_runtime_options_from_model_provider_entry(config, Some(entry));
    let provider = crate::create_model_provider_for_alias(
        config,
        family,
        name,
        entry.api_key.as_deref(),
        &options,
    )?;
    provider.simple_chat(message, model, None).await
}

#[derive(Default)]
pub struct ChatGptPlanFlow {
    oauth: PlanOAuth,
}

async fn callback_target(socket: &mut tokio::net::TcpStream) -> Result<String> {
    let mut header = Vec::new();
    loop {
        let mut chunk = [0; 1024];
        let count = socket.read(&mut chunk).await?;
        anyhow::ensure!(
            count > 0,
            "ChatGPT callback connection closed before headers"
        );
        header.extend_from_slice(&chunk[..count]);
        anyhow::ensure!(
            header.len() <= 8192,
            "ChatGPT callback headers exceed size limit"
        );
        if header.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }
    let header = std::str::from_utf8(&header)
        .map_err(|_| protocol_error("Invalid ChatGPT callback encoding"))?;
    let target = header
        .lines()
        .next()
        .and_then(|line| line.strip_prefix("GET "))
        .and_then(|line| line.strip_suffix(" HTTP/1.1"))
        .context("Invalid ChatGPT callback request")?;
    anyhow::ensure!(
        target.starts_with("/auth/callback?"),
        "Invalid ChatGPT callback path"
    );
    Ok(target.into())
}

async fn callback_reply(socket: &mut tokio::net::TcpStream, ok: bool) {
    let response = if ok {
        b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".as_slice()
    } else {
        b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".as_slice()
    };
    // A browser disconnect cannot undo a committed sign-in. Invalid callback
    // connections are untrusted and must not terminate the pending attempt.
    let _ = tokio::time::timeout(Duration::from_secs(2), socket.write_all(response)).await;
}

#[async_trait::async_trait]
impl AuthProviderFlow for ChatGptPlanFlow {
    async fn login(
        &self,
        ctx: &AuthFlowContext<'_>,
        profile: &str,
        device_code: bool,
        import: Option<&std::path::Path>,
    ) -> Result<()> {
        anyhow::ensure!(
            cfg!(unix),
            "ChatGPT plan usage is unsupported on native Windows in this slice"
        );
        anyhow::ensure!(
            !device_code && import.is_none(),
            "ChatGPT plan login supports browser authorization only; credential imports are unsupported"
        );
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .context("Unable to start ChatGPT callback listener")?;
        let uri = format!(
            "http://127.0.0.1:{}/auth/callback",
            listener.local_addr()?.port()
        );
        let attempt = self.oauth.begin(ctx.auth_service, profile, &uri).await?;
        println!(
            "{}",
            ctx.cli_text(
                "cli-auth-chatgpt-continue",
                &[("url", &attempt.authorize_url)],
                "Continue with ChatGPT: open {$url} in your browser. This request asks permission to use your ChatGPT plan."
            )
        );
        #[cfg(any(test, feature = "test-helpers"))]
        crate::plan_test_transport::observe_authorization(&attempt.authorize_url);
        let deadline = tokio::time::Instant::now() + CALLBACK_TIMEOUT;
        loop {
            let (mut socket, _) = tokio::time::timeout_at(deadline, listener.accept())
                .await
                .context("ChatGPT sign-in timed out")??;
            let read_deadline = deadline.min(tokio::time::Instant::now() + Duration::from_secs(10));
            let target =
                match tokio::time::timeout_at(read_deadline, callback_target(&mut socket)).await {
                    Ok(Ok(target)) => target,
                    _ => {
                        callback_reply(&mut socket, false).await;
                        continue;
                    }
                };
            let callback_url =
                format!("http://127.0.0.1:{}{target}", listener.local_addr()?.port());
            let query = match attempt.validated_query(&callback_url) {
                Ok(query) => query,
                Err(_) => {
                    callback_reply(&mut socket, false).await;
                    continue;
                }
            };
            if query.contains_key("error") {
                callback_reply(&mut socket, false).await;
                anyhow::bail!("ChatGPT authorization was denied or failed");
            }
            if attempt.callback(&callback_url).is_err() {
                callback_reply(&mut socket, false).await;
                continue;
            }
            let result = self
                .oauth
                .complete(ctx.auth_service, attempt, &callback_url)
                .await;
            callback_reply(&mut socket, result.is_ok()).await;
            result?;
            break;
        }
        println!(
            "{}",
            ctx.cli_text(
                "cli-auth-chatgpt-saved",
                &[("profile", profile)],
                "Saved ChatGPT registration chatgpt-plan:{$profile}. Bind an OpenAI provider alias explicitly before inference."
            )
        );
        Ok(())
    }

    async fn refresh_status(
        &self,
        ctx: &AuthFlowContext<'_>,
        profile_override: Option<&str>,
    ) -> Result<RefreshStatus> {
        let profile = profile_override.context("ChatGPT plan refresh requires --profile")?;
        let binding = if profile.starts_with("chatgpt-plan:") {
            profile.to_string()
        } else {
            profile_id(PROVIDER, profile)
        };
        self.oauth.access_token(ctx.auth_service, &binding).await?;
        Ok(RefreshStatus::Refreshed {
            profile: profile.into(),
        })
    }
}

#[cfg(all(test, unix))]
#[path = "chatgpt_plan_tests.rs"]
mod tests;
