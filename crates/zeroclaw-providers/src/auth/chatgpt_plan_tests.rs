use super::*;
use base64::Engine;
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use ring::signature::{ECDSA_P256_SHA256_ASN1_SIGNING, EcdsaKeyPair, KeyPair};
use tempfile::TempDir;

struct Signer {
    key: EncodingKey,
    jwks: jsonwebtoken::jwk::JwkSet,
}
impl Signer {
    fn new() -> Self {
        let rng = ring::rand::SystemRandom::new();
        let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &rng).unwrap();
        let pair = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, pkcs8.as_ref(), &rng)
            .unwrap();
        let public = pair.public_key().as_ref();
        let jwks = serde_json::from_value(serde_json::json!({"keys":[{
            "kid":"fixture", "kty":"EC", "crv":"P-256", "alg":"ES256", "use":"sig",
            "x":base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&public[1..33]),
            "y":base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&public[33..65])
        }]}))
        .unwrap();
        Self {
            key: EncodingKey::from_ec_der(pkcs8.as_ref()),
            jwks,
        }
    }
    fn sign(&self, claims: &serde_json::Value) -> String {
        let mut header = Header::new(Algorithm::ES256);
        header.kid = Some("fixture".into());
        jsonwebtoken::encode(&header, claims, &self.key).unwrap()
    }
}
fn claims() -> serde_json::Value {
    serde_json::json!({"iss":ISSUER,"aud":"oaiapp_fixture","sub":"subject-fixture",
        "nonce":"nonce-fixture","iat":Utc::now().timestamp(),"exp":Utc::now().timestamp()+3600})
}

#[test]
fn signed_identity_rejects_each_invalid_claim_and_signature() {
    let signer = Signer::new();
    let valid = claims();
    assert_eq!(
        verify_identity(
            &signer.sign(&valid),
            &signer.jwks,
            "oaiapp_fixture",
            Some("nonce-fixture")
        )
        .unwrap(),
        "subject-fixture"
    );
    for (field, bad) in [
        ("iss", serde_json::json!("https://attacker.test")),
        ("aud", serde_json::json!("oaiapp_other")),
        ("sub", serde_json::json!("")),
        ("nonce", serde_json::json!("wrong-nonce")),
        ("exp", serde_json::json!(Utc::now().timestamp() - 60)),
        ("iat", serde_json::json!(Utc::now().timestamp() + 3600)),
    ] {
        let mut changed = valid.clone();
        changed[field] = bad;
        assert!(
            verify_identity(
                &signer.sign(&changed),
                &signer.jwks,
                "oaiapp_fixture",
                Some("nonce-fixture")
            )
            .is_err(),
            "{field}"
        );
    }
    for field in ["iss", "aud", "sub", "nonce", "exp", "iat"] {
        let mut changed = valid.clone();
        changed.as_object_mut().unwrap().remove(field);
        assert!(
            verify_identity(
                &signer.sign(&changed),
                &signer.jwks,
                "oaiapp_fixture",
                Some("nonce-fixture")
            )
            .is_err(),
            "missing {field}"
        );
    }
    let wrong_signer = Signer::new();
    assert!(
        verify_identity(
            &wrong_signer.sign(&valid),
            &signer.jwks,
            "oaiapp_fixture",
            Some("nonce-fixture")
        )
        .is_err()
    );
    let secret = b"synthetic-public-jwks-secret";
    let mut header = Header::new(Algorithm::HS256);
    header.kid = Some("fixture".into());
    let token = jsonwebtoken::encode(&header, &valid, &EncodingKey::from_secret(secret)).unwrap();
    let symmetric = serde_json::from_value(serde_json::json!({"keys":[{"kid":"fixture","kty":"oct","alg":"HS256","k":base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(secret)}]})).unwrap();
    assert!(verify_identity(&token, &symmetric, "oaiapp_fixture", Some("nonce-fixture")).is_err());
}

#[tokio::test]
async fn authorization_callback_is_bound_to_attempt_client_and_loopback_uri() {
    let root = TempDir::new().unwrap();
    let service = AuthService::new(root.path(), true);
    let oauth = PlanOAuth::default();
    let attempt = oauth
        .begin(
            &service,
            "subscriber",
            "http://127.0.0.1:54321/auth/callback",
        )
        .await
        .unwrap();
    let url = reqwest::Url::parse(&attempt.authorize_url).unwrap();
    let query: std::collections::HashMap<_, _> = url.query_pairs().collect();
    assert_eq!(query["client_id"], "dynamic_agent_client");
    assert_eq!(query["resource"], RESOURCE);
    assert_eq!(query["agent_name_hint"], "ZeroClaw");
    assert_eq!(query["code_challenge_method"], "S256");
    assert_eq!(
        query["code_challenge"],
        oauth_common::code_challenge_for_verifier(&attempt.pkce.code_verifier)
    );
    assert!(query["scope"].split_whitespace().any(|s| s == PLAN_SCOPE));
    assert_eq!(
        query["ext_agent_host_id"],
        service.store.ensure_plan_host_id().await.unwrap()
    );
    for callback in [
        "http://127.0.0.1:54321/auth/callback?state=wrong&code=code&client_id=oaiapp_fixture"
            .into(),
        format!(
            "http://127.0.0.1:54321/auth/callback?state={}&code=code",
            attempt.pkce.state
        ),
        format!(
            "http://127.0.0.1:54321/auth/callback?state={}&code=code&client_id=dynamic_agent_client",
            attempt.pkce.state
        ),
        format!(
            "http://localhost:54321/auth/callback?state={}&code=code&client_id=oaiapp_fixture",
            attempt.pkce.state
        ),
        format!(
            "http://127.0.0.1:54321/auth/callback?state={}&error=access_denied",
            attempt.pkce.state
        ),
        format!(
            "http://127.0.0.1:54321/auth/callback?state={}&state=other&code=code&client_id=oaiapp_fixture",
            attempt.pkce.state
        ),
    ] {
        assert!(attempt.callback(&callback).is_err());
    }
    for uri in [
        "http://localhost:1455/auth/callback",
        "https://127.0.0.1:1455/auth/callback",
        "http://127.0.0.1:1455/callback",
    ] {
        assert!(oauth.begin(&service, "subscriber", uri).await.is_err());
    }
    let callback = format!(
        "{}?state={}&code=fixture-code&client_id=oaiapp_fixture",
        attempt.redirect_uri, attempt.pkce.state
    );
    assert_eq!(
        attempt.callback(&callback).unwrap().client_id,
        "oaiapp_fixture"
    );
    for invalid in ["", "dynamic_agent_client", " ", "\n"] {
        let callback = format!(
            "{}?state={}&code=fixture&client_id={}",
            attempt.redirect_uri,
            attempt.pkce.state,
            oauth_common::url_encode(invalid)
        );
        assert!(attempt.callback(&callback).is_err());
    }
    let mut expired = attempt;
    expired.created = std::time::Instant::now() - CALLBACK_TIMEOUT - Duration::from_secs(1);
    assert!(expired.callback(&callback).is_err());
}

fn profile(label: &str, client: &str, subject: &str, scope: &str) -> AuthProfile {
    let mut profile = AuthProfile::new_oauth(
        PROVIDER,
        label,
        TokenSet {
            access_token: "synthetic-access".into(),
            refresh_token: Some("synthetic-refresh".into()),
            id_token: Some("synthetic-id".into()),
            expires_at: Some(Utc::now() + chrono::Duration::hours(1)),
            token_type: Some("Bearer".into()),
            scope: Some(scope.into()),
        },
    );
    profile.plan_registration = Some(ChatGptPlanRegistration {
        client_id: client.into(),
        subject: subject.into(),
        earliest_refresh_at: None,
        refresh_started_at: None,
    });
    profile
}

#[tokio::test]
async fn registration_substitution_never_replaces_old_credentials_or_active_profile() {
    let root = TempDir::new().unwrap();
    let service = AuthService::new(root.path(), true);
    let original = profile("subscriber", "oaiapp_fixture", "original", PLAN_SCOPE);
    service
        .store
        .upsert_profile(original.clone(), true)
        .await
        .unwrap();
    let before = std::fs::read(service.store.path()).unwrap();
    for replacement in [
        profile("subscriber", "oaiapp_other", "original", PLAN_SCOPE),
        profile("subscriber", "oaiapp_fixture", "another", PLAN_SCOPE),
    ] {
        assert!(
            service
                .store
                .upsert_profile(replacement, true)
                .await
                .is_err()
        );
        assert_eq!(std::fs::read(service.store.path()).unwrap(), before);
    }
    let attempt = PlanOAuth::default()
        .begin(
            &service,
            "subscriber",
            "http://127.0.0.1:1455/auth/callback",
        )
        .await
        .unwrap();
    let url = reqwest::Url::parse(&attempt.authorize_url).unwrap();
    assert!(
        url.query_pairs()
            .any(|(k, v)| k == "client_id" && v == "oaiapp_fixture")
    );
    assert!(
        !url.query_pairs()
            .any(|(k, _)| k == "agent_name_hint" || k == "id_token_hint")
    );
    let wrong_client = format!(
        "{}?state={}&code=fixture&client_id=oaiapp_other",
        attempt.redirect_uri, attempt.pkce.state
    );
    assert!(attempt.callback(&wrong_client).is_err());
    let absent_client = format!(
        "{}?state={}&code=fixture",
        attempt.redirect_uri, attempt.pkce.state
    );
    assert_eq!(
        attempt.callback(&absent_client).unwrap().client_id,
        "oaiapp_fixture"
    );
}

#[tokio::test]
async fn explicit_binding_ignores_global_active_and_identity_only_grants() {
    let root = TempDir::new().unwrap();
    let service = AuthService::new(root.path(), true);
    let original = profile("first", "oaiapp_first", "first", PLAN_SCOPE);
    service
        .store
        .upsert_profile(original.clone(), true)
        .await
        .unwrap();
    let mut second = profile("second", "oaiapp_second", "second", PLAN_SCOPE);
    second.token_set.as_mut().unwrap().access_token = "synthetic-second".into();
    service.store.upsert_profile(second, true).await.unwrap();
    let restarted = AuthService::new(root.path(), false);
    assert_eq!(
        restarted
            .get_valid_chatgpt_plan_access_token(&original.id)
            .await
            .unwrap(),
        "synthetic-access"
    );
    let identity_only = profile(
        "identity",
        "oaiapp_identity",
        "identity",
        "openid profile email",
    );
    service
        .store
        .upsert_profile(identity_only.clone(), false)
        .await
        .unwrap();
    assert!(
        service
            .get_valid_chatgpt_plan_access_token(&identity_only.id)
            .await
            .is_err()
    );
    for id in ["first", "chatgpt-plan:missing", "openai-codex:default", ""] {
        assert!(
            service
                .get_valid_chatgpt_plan_access_token(id)
                .await
                .is_err()
        );
    }
    let raw = std::fs::read_to_string(service.store.path()).unwrap();
    for secret in [
        "synthetic-access",
        "synthetic-refresh",
        "synthetic-id",
        "synthetic-second",
    ] {
        assert!(!raw.contains(secret));
    }
}

#[tokio::test]
async fn host_identity_persists_and_separate_roots_are_independent() {
    let first = TempDir::new().unwrap();
    let second = TempDir::new().unwrap();
    let a = AuthService::new(first.path(), true);
    let b = AuthService::new(second.path(), true);
    let id = a.store.ensure_plan_host_id().await.unwrap();
    assert!(id.starts_with("urn:uuid:"));
    assert_eq!(
        id,
        AuthService::new(first.path(), true)
            .store
            .ensure_plan_host_id()
            .await
            .unwrap()
    );
    assert_ne!(id, b.store.ensure_plan_host_id().await.unwrap());
    let lock = a
        .store
        .acquire_plan_refresh_lock("chatgpt-plan:same")
        .await
        .unwrap();
    let other = tokio::time::timeout(
        Duration::from_millis(250),
        b.store.acquire_plan_refresh_lock("chatgpt-plan:same"),
    )
    .await
    .unwrap()
    .unwrap();
    drop(other);
    drop(lock);
}

#[tokio::test]
async fn synthetic_oauth_restart_factory_completion_and_rotating_refresh() {
    use crate::plan_test_transport::{SyntheticIdentity, scope};
    use axum::{
        Json, Router,
        routing::{get, post},
    };
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    };
    let identity = Arc::new(SyntheticIdentity::new());
    let nonce = Arc::new(Mutex::new(String::new()));
    let refreshes = Arc::new(AtomicUsize::new(0));
    let forms = Arc::new(Mutex::new(Vec::<String>::new()));
    let app = Router::new()
        .route("/jwks",get({ let identity = identity.clone(); move || { let identity = identity.clone(); async move { Json(identity.jwks()) } } }))
        .route("/token",post({ let identity = identity.clone(); let nonce = nonce.clone(); let refreshes = refreshes.clone(); let forms = forms.clone();
            move |body: String| { let identity = identity.clone(); let nonce = nonce.clone(); let refreshes = refreshes.clone(); let forms = forms.clone(); async move {
                forms.lock().unwrap().push(body.clone());
                let form = oauth_common::parse_query_params(&body);
                assert_eq!(form["resource"],RESOURCE); assert_eq!(form["client_id"],"oaiapp_fixture");
                let refresh = form["grant_type"] == "refresh_token";
                if refresh { refreshes.fetch_add(1,Ordering::SeqCst); tokio::time::sleep(Duration::from_millis(100)).await; assert_eq!(form["refresh_token"],"synthetic-refresh"); assert!(!form.contains_key("scope")); }
                else { assert!(form.contains_key("code_verifier")); assert!(form["redirect_uri"].starts_with("http://127.0.0.1:")); }
                Json(serde_json::json!({"access_token":if refresh { "synthetic-replacement-access" } else { "synthetic-access" }, "refresh_token":if refresh { "synthetic-replacement-refresh" } else { "synthetic-refresh" },
                    "id_token":identity.id_token("oaiapp_fixture","subject-fixture",&nonce.lock().unwrap()),"token_type":"Bearer","expires_in":3600,"scope":PLAN_SCOPE}))
            } }
        }))
        .route("/models",get(|headers: axum::http::HeaderMap| async move { assert_eq!(headers["authorization"],"Bearer synthetic-access"); Json(serde_json::json!({"models":[{"slug":"model-fixture","visibility":"list"},{"slug":"hidden","visibility":"hidden"}]})) }))
        .route("/responses",post(|headers: axum::http::HeaderMap,Json(body):Json<serde_json::Value>| async move {
            assert_eq!(headers["authorization"],"Bearer synthetic-access"); assert_eq!(body["store"],false); assert_eq!(body["stream"],true);
            assert!(body.get("temperature").is_none() && body.get("tools").is_none() && body.get("max_output_tokens").is_none());
            ([("content-type","text/event-stream")],"data: {\"type\":\"response.output_text.delta\",\"delta\":\"READY\"}\n\ndata: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\"}}\n\n")
        }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = ::zeroclaw_spawn::spawn!(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let root = TempDir::new().unwrap();
    scope(&base, async {
        let service = AuthService::new(root.path(), true);
        let oauth = PlanOAuth::default();
        let attempt = oauth
            .begin(
                &service,
                "subscriber",
                "http://127.0.0.1:54321/auth/callback",
            )
            .await
            .unwrap();
        *nonce.lock().unwrap() = attempt.nonce.clone();
        let callback = format!(
            "{}?state={}&code=synthetic-code&client_id=oaiapp_fixture",
            attempt.redirect_uri, attempt.pkce.state
        );
        oauth.complete(&service, attempt, &callback).await.unwrap();
        let restarted = AuthService::new(root.path(), true);
        let original = restarted.store.load().await.unwrap();
        let registration = original.profiles["chatgpt-plan:subscriber"]
            .plan_registration
            .as_ref()
            .unwrap();
        assert_eq!(registration.client_id, "oaiapp_fixture");
        assert_eq!(registration.subject, "subject-fixture");
        let mut config = zeroclaw_config::schema::Config {
            config_path: root.path().join("config.toml"),
            ..Default::default()
        };
        let base = zeroclaw_config::schema::ModelProviderConfig {
            kind: Some("chatgpt-plan".into()),
            chatgpt_plan_auth: Some(zeroclaw_config::schema::ChatGptPlanAuthConfig {
                registration: "chatgpt-plan:subscriber".into(),
            }),
            ..Default::default()
        };
        config.providers.models.openai.insert(
            "subscriber".into(),
            zeroclaw_config::schema::OpenAIModelProviderConfig { base },
        );
        let opts = crate::model_provider_runtime_options_from_model_provider_entry(
            &config,
            config.providers.models.find("openai", "subscriber"),
        );
        let provider =
            crate::create_model_provider_for_alias(&config, "openai", "subscriber", None, &opts)
                .unwrap();
        assert_eq!(provider.list_models().await.unwrap(), ["model-fixture"]);
        assert_eq!(
            provider
                .simple_chat("Reply READY", "model-fixture", None)
                .await
                .unwrap(),
            "READY"
        );
        let binding = "chatgpt-plan:subscriber";
        restarted
            .store
            .update_profile(binding, |profile| {
                profile.token_set.as_mut().unwrap().expires_at = Some(Utc::now());
                Ok(())
            })
            .await
            .unwrap();
        let (one, two) = tokio::join!(
            oauth.access_token(&restarted, binding),
            oauth.access_token(&service, binding)
        );
        assert_eq!(one.unwrap(), "synthetic-replacement-access");
        assert_eq!(two.unwrap(), "synthetic-replacement-access");
        assert_eq!(refreshes.load(Ordering::SeqCst), 1);
        assert_eq!(
            restarted.store.load().await.unwrap().profiles[binding]
                .token_set
                .as_ref()
                .unwrap()
                .refresh_token
                .as_deref(),
            Some("synthetic-replacement-refresh")
        );
        assert_eq!(forms.lock().unwrap().len(), 2);
    })
    .await;
    server.abort();
}

#[tokio::test]
async fn cross_process_plan_refresh_child() {
    let Ok(root) = std::env::var("ZEROCLAW_PLAN_FIXTURE_ROOT") else {
        return;
    };
    let base = std::env::var("ZEROCLAW_PLAN_FIXTURE_BASE").unwrap();
    crate::plan_test_transport::scope(&base, async {
        let service = AuthService::new(std::path::Path::new(&root), true);
        let token = service
            .get_valid_chatgpt_plan_access_token("chatgpt-plan:subscriber")
            .await
            .unwrap();
        assert_eq!(token, "synthetic-rotated-access");
    })
    .await;
}

#[tokio::test]
async fn cross_process_refresh_serializes_rotation_and_preserves_replacement() {
    use axum::{Json, Router, routing::post};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let refreshes = Arc::new(AtomicUsize::new(0));
    let app = Router::new().route("/token",post({ let refreshes = refreshes.clone(); move |body: String| { let refreshes = refreshes.clone(); async move {
        let form = oauth_common::parse_query_params(&body); assert_eq!(form["refresh_token"],"synthetic-refresh");
        refreshes.fetch_add(1,Ordering::SeqCst); tokio::time::sleep(Duration::from_millis(150)).await;
        Json(serde_json::json!({"access_token":"synthetic-rotated-access","refresh_token":"synthetic-rotated-refresh","token_type":"Bearer","expires_in":3600,"scope":PLAN_SCOPE}))
    } } }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = ::zeroclaw_spawn::spawn!(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let root = TempDir::new().unwrap();
    let service = AuthService::new(root.path(), true);
    let mut expired = profile(
        "subscriber",
        "oaiapp_fixture",
        "subject-fixture",
        PLAN_SCOPE,
    );
    expired.token_set.as_mut().unwrap().expires_at = Some(Utc::now());
    service.store.upsert_profile(expired, false).await.unwrap();
    let executable = std::env::current_exe().unwrap();
    let spawn = || {
        let mut cmd = tokio::process::Command::new(&executable);
        cmd.args([
            "auth::chatgpt_plan::tests::cross_process_plan_refresh_child",
            "--exact",
        ])
        .env("ZEROCLAW_PLAN_FIXTURE_ROOT", root.path())
        .env("ZEROCLAW_PLAN_FIXTURE_BASE", &base)
        .kill_on_drop(true);
        cmd.spawn().unwrap()
    };
    let mut first = spawn();
    let mut second = spawn();
    let (first, second) = tokio::join!(first.wait(), second.wait());
    assert!(first.unwrap().success());
    assert!(second.unwrap().success());
    assert_eq!(refreshes.load(Ordering::SeqCst), 1);
    let saved = service.store.load().await.unwrap();
    assert_eq!(
        saved.profiles["chatgpt-plan:subscriber"]
            .token_set
            .as_ref()
            .unwrap()
            .refresh_token
            .as_deref(),
        Some("synthetic-rotated-refresh")
    );
    server.abort();
}

#[tokio::test]
async fn ambient_api_key_cannot_authorize_an_unbound_plan_provider() {
    let root = TempDir::new().unwrap();
    let config = zeroclaw_config::schema::Config {
        config_path: root.path().join("config.toml"),
        ..Default::default()
    };
    // This seam fails before any credential/env lookup or network request.
    for alias in ["openai", "openai.missing", "custom.missing"] {
        assert!(
            check_bound_provider(&config, alias, "fixture")
                .await
                .is_err()
        );
    }
}

#[test]
fn token_response_rejects_identity_only_or_incomplete_grants() {
    let valid = serde_json::json!({"access_token":"synthetic-access","refresh_token":"synthetic-refresh","id_token":"synthetic-id","token_type":"Bearer","expires_in":3600,"scope":PLAN_SCOPE});
    for (field, value) in [
        ("access_token", serde_json::json!("")),
        ("refresh_token", serde_json::json!("")),
        ("token_type", serde_json::json!("Other")),
        ("expires_in", serde_json::json!(0)),
        ("scope", serde_json::json!("openid profile email")),
    ] {
        let mut changed = valid.clone();
        changed[field] = value;
        let response: TokenResponse = serde_json::from_value(changed).unwrap();
        assert!(response.into_tokens(None).is_err(), "{field}");
    }
    let mut missing = valid.clone();
    missing.as_object_mut().unwrap().remove("scope");
    let response: TokenResponse = serde_json::from_value(missing).unwrap();
    assert!(response.into_tokens(None).is_err());
    let response: TokenResponse = serde_json::from_value(valid).unwrap();
    assert!(response.into_tokens(None).is_ok());
}

#[tokio::test]
async fn refresh_rejects_signed_identity_substitution_without_persisting_tokens() {
    use crate::plan_test_transport::{SyntheticIdentity, scope};
    use axum::{
        Json, Router,
        routing::{get, post},
    };
    use std::sync::Arc;
    let identity = Arc::new(SyntheticIdentity::new());
    let app = Router::new().route("/jwks",get({ let identity = identity.clone(); move || { let identity = identity.clone(); async move { Json(identity.jwks()) } } }))
        .route("/token",post({ let identity = identity.clone(); move || { let identity = identity.clone(); async move {
            Json(serde_json::json!({"access_token":"synthetic-substitute","refresh_token":"synthetic-substitute-refresh","id_token":identity.id_token("oaiapp_fixture","another-subject","not-required-for-refresh"),"token_type":"Bearer","expires_in":3600,"scope":PLAN_SCOPE}))
        } } }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = ::zeroclaw_spawn::spawn!(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let root = TempDir::new().unwrap();
    let service = AuthService::new(root.path(), true);
    let mut expired = profile(
        "subscriber",
        "oaiapp_fixture",
        "subject-fixture",
        PLAN_SCOPE,
    );
    expired.token_set.as_mut().unwrap().expires_at = Some(Utc::now());
    service.store.upsert_profile(expired, false).await.unwrap();
    let before = std::fs::read(service.store.path()).unwrap();
    scope(&base, async {
        assert!(
            service
                .get_valid_chatgpt_plan_access_token("chatgpt-plan:subscriber")
                .await
                .is_err()
        );
    })
    .await;
    let saved = service.store.load().await.unwrap();
    let saved = &saved.profiles["chatgpt-plan:subscriber"];
    assert_eq!(
        saved.token_set.as_ref().unwrap().access_token,
        "synthetic-access"
    );
    assert_eq!(
        saved.token_set.as_ref().unwrap().refresh_token.as_deref(),
        Some("synthetic-refresh")
    );
    assert!(
        saved
            .plan_registration
            .as_ref()
            .unwrap()
            .refresh_started_at
            .is_some()
    );
    assert_ne!(std::fs::read(service.store.path()).unwrap(), before);
    server.abort();
}

#[cfg(unix)]
#[tokio::test]
async fn root_aliases_share_refresh_lock_and_leaf_lock_symlinks_are_rejected() {
    use sha2::{Digest, Sha256};
    use std::os::unix::fs::symlink;
    let parent = TempDir::new().unwrap();
    let root = parent.path().join("root");
    std::fs::create_dir(&root).unwrap();
    let alias = parent.path().join("alias");
    symlink(&root, &alias).unwrap();
    let a = AuthService::new(&root, true);
    let b = AuthService::new(&alias, true);
    let lock = a
        .store
        .acquire_plan_refresh_lock("chatgpt-plan:subscriber")
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(
            Duration::from_millis(150),
            b.store.acquire_plan_refresh_lock("chatgpt-plan:subscriber")
        )
        .await
        .is_err()
    );
    drop(lock);
    let path = root.join(format!(
        "auth-plan-refresh-{}.lock",
        hex::encode(Sha256::digest(b"chatgpt-plan:other"))
    ));
    symlink(root.join("victim"), path).unwrap();
    assert!(
        a.store
            .acquire_plan_refresh_lock("chatgpt-plan:other")
            .await
            .is_err()
    );
}

#[tokio::test]
async fn plan_profile_use_and_logout_are_explicitly_unsupported() {
    let root = TempDir::new().unwrap();
    let service = AuthService::new(root.path(), true);
    let saved = profile(
        "subscriber",
        "oaiapp_fixture",
        "subject-fixture",
        PLAN_SCOPE,
    );
    service.store.upsert_profile(saved, false).await.unwrap();
    let before = std::fs::read(service.store.path()).unwrap();
    assert!(
        service
            .set_active_profile(PROVIDER, "subscriber")
            .await
            .is_err()
    );
    assert!(
        service
            .remove_profile(PROVIDER, "subscriber")
            .await
            .is_err()
    );
    assert_eq!(std::fs::read(service.store.path()).unwrap(), before);
}

#[tokio::test]
async fn login_ignores_unrelated_callbacks_and_accumulates_fragmented_headers() {
    use crate::plan_test_transport::{SyntheticIdentity, scope_with_observer};
    use axum::{
        Json, Router,
        routing::{get, post},
    };
    use std::sync::{Arc, Mutex};
    let identity = Arc::new(SyntheticIdentity::new());
    let nonce = Arc::new(Mutex::new(String::new()));
    let app = Router::new().route("/jwks",get({ let identity = identity.clone(); move || { let identity = identity.clone(); async move { Json(identity.jwks()) } } }))
        .route("/token",post({ let identity = identity.clone(); let nonce = nonce.clone(); move || { let identity = identity.clone(); let nonce = nonce.clone(); async move {
            Json(serde_json::json!({"access_token":"synthetic-access","refresh_token":"synthetic-refresh","id_token":identity.id_token("opaque-client-without-prefix","subject-fixture",&nonce.lock().unwrap()),"token_type":"Bearer","expires_in":3600,"scope":PLAN_SCOPE}))
        } } }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = ::zeroclaw_spawn::spawn!(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let root = TempDir::new().unwrap();
    let service = AuthService::new(root.path(), true);
    let config = zeroclaw_config::schema::Config {
        config_path: root.path().join("config.toml"),
        ..Default::default()
    };
    let observer = Arc::new({
        let nonce = nonce.clone();
        move |authorization: &str| {
            let url = reqwest::Url::parse(authorization).unwrap();
            let query: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
            *nonce.lock().unwrap() = query["nonce"].clone();
            ::zeroclaw_spawn::spawn!(async move {
                let uri = reqwest::Url::parse(&query["redirect_uri"]).unwrap();
                let client = reqwest::Client::new();
                let unrelated = format!("http://127.0.0.1:{}/favicon.ico", uri.port().unwrap());
                assert_eq!(
                    client.get(unrelated).send().await.unwrap().status(),
                    reqwest::StatusCode::BAD_REQUEST
                );
                let wrong = format!(
                    "{}?state=wrong&code=synthetic&client_id=opaque-client-without-prefix",
                    query["redirect_uri"]
                );
                assert_eq!(
                    client.get(wrong).send().await.unwrap().status(),
                    reqwest::StatusCode::BAD_REQUEST
                );
                let target = format!(
                    "/auth/callback?state={}&code=synthetic&client_id=opaque-client-without-prefix",
                    query["state"]
                );
                let request = format!("GET {target} HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n");
                let mut socket = tokio::net::TcpStream::connect(("127.0.0.1", uri.port().unwrap()))
                    .await
                    .unwrap();
                socket.write_all(&request.as_bytes()[..16]).await.unwrap();
                tokio::time::sleep(Duration::from_millis(25)).await;
                socket.write_all(&request.as_bytes()[16..]).await.unwrap();
                let mut response = Vec::new();
                socket.read_to_end(&mut response).await.unwrap();
                assert!(response.starts_with(b"HTTP/1.1 200 OK"));
            });
        }
    });
    scope_with_observer(&base, observer, async {
        let client = client().unwrap();
        let formatter = |_: &str, _: &[(&str, &str)], _: &str| String::new();
        let ctx = AuthFlowContext {
            config: &config,
            auth_service: &service,
            client: &client,
            format_cli: &formatter,
        };
        tokio::time::timeout(
            Duration::from_secs(5),
            ChatGptPlanFlow::default().login(&ctx, "subscriber", false, None),
        )
        .await
        .unwrap()
        .unwrap();
    })
    .await;
    assert!(
        service
            .store
            .load()
            .await
            .unwrap()
            .profiles
            .contains_key("chatgpt-plan:subscriber")
    );
    server.abort();
}

#[tokio::test]
async fn ambiguous_refresh_response_is_never_replayed_on_second_call() {
    use axum::{Router, routing::post};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let requests = Arc::new(AtomicUsize::new(0));
    let app = Router::new().route(
        "/token",
        post({
            let requests = requests.clone();
            move || {
                let requests = requests.clone();
                async move {
                    requests.fetch_add(1, Ordering::SeqCst);
                    (axum::http::StatusCode::OK, "malformed synthetic body")
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = ::zeroclaw_spawn::spawn!(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let root = TempDir::new().unwrap();
    let service = AuthService::new(root.path(), true);
    let mut expired = profile("subscriber", "opaque-client", "subject-fixture", PLAN_SCOPE);
    expired.token_set.as_mut().unwrap().expires_at = Some(Utc::now());
    service.store.upsert_profile(expired, false).await.unwrap();
    crate::plan_test_transport::scope(&base, async {
        for _ in 0..2 {
            assert!(
                service
                    .get_valid_chatgpt_plan_access_token("chatgpt-plan:subscriber")
                    .await
                    .is_err()
            );
        }
    })
    .await;
    assert_eq!(requests.load(Ordering::SeqCst), 1);
    assert!(
        service.store.load().await.unwrap().profiles["chatgpt-plan:subscriber"]
            .plan_registration
            .as_ref()
            .unwrap()
            .refresh_started_at
            .is_some()
    );
    server.abort();
}

#[tokio::test]
async fn failed_rotated_token_commit_is_never_replayed_on_second_call() {
    use axum::{Json, Router, routing::post};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let requests = Arc::new(AtomicUsize::new(0));
    let app = Router::new().route("/token",post({ let requests = requests.clone(); move || { let requests = requests.clone(); async move {
        requests.fetch_add(1,Ordering::SeqCst);
        Json(serde_json::json!({"access_token":"synthetic-rotated-access","refresh_token":"synthetic-rotated-refresh","token_type":"Bearer","expires_in":3600,"scope":PLAN_SCOPE}))
    } } }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = ::zeroclaw_spawn::spawn!(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let root = TempDir::new().unwrap();
    let service = AuthService::new(root.path(), true);
    let mut expired = profile("subscriber", "opaque-client", "subject-fixture", PLAN_SCOPE);
    expired.token_set.as_mut().unwrap().expires_at = Some(Utc::now());
    service.store.upsert_profile(expired, false).await.unwrap();
    crate::plan_test_transport::scope(&base, async {
        super::super::protected_file::FAIL_REPLACEMENT_NUMBER
            .scope(std::cell::Cell::new(2), async {
                for _ in 0..2 {
                    assert!(
                        service
                            .get_valid_chatgpt_plan_access_token("chatgpt-plan:subscriber")
                            .await
                            .is_err()
                    );
                }
            })
            .await;
    })
    .await;
    assert_eq!(requests.load(Ordering::SeqCst), 1);
    let data = service.store.load().await.unwrap();
    let saved = &data.profiles["chatgpt-plan:subscriber"];
    assert!(
        saved
            .plan_registration
            .as_ref()
            .unwrap()
            .refresh_started_at
            .is_some()
    );
    assert_eq!(
        saved.token_set.as_ref().unwrap().refresh_token.as_deref(),
        Some("synthetic-refresh")
    );
    assert!(!std::fs::read_dir(root.path()).unwrap().any(|entry| {
        entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .contains(".tmp.")
    }));
    server.abort();
}

#[tokio::test]
async fn process_crash_after_refresh_egress_blocks_replay() {
    use axum::{Json, Router, routing::post};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let requests = Arc::new(AtomicUsize::new(0));
    let started = Arc::new(tokio::sync::Notify::new());
    let app = Router::new().route(
        "/token",
        post({
            let requests = requests.clone();
            let started = started.clone();
            move || {
                let requests = requests.clone();
                let started = started.clone();
                async move {
                    requests.fetch_add(1, Ordering::SeqCst);
                    started.notify_one();
                    std::future::pending::<Json<serde_json::Value>>().await
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = ::zeroclaw_spawn::spawn!(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let root = TempDir::new().unwrap();
    let service = AuthService::new(root.path(), true);
    let mut expired = profile("subscriber", "opaque-client", "subject-fixture", PLAN_SCOPE);
    expired.token_set.as_mut().unwrap().expires_at = Some(Utc::now());
    service.store.upsert_profile(expired, false).await.unwrap();
    let mut child = tokio::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "auth::chatgpt_plan::tests::cross_process_plan_refresh_child",
            "--exact",
        ])
        .env("ZEROCLAW_PLAN_FIXTURE_ROOT", root.path())
        .env("ZEROCLAW_PLAN_FIXTURE_BASE", &base)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), started.notified())
        .await
        .unwrap();
    child.kill().await.unwrap();
    child.wait().await.unwrap();
    crate::plan_test_transport::scope(&base, async {
        assert!(
            tokio::time::timeout(
                Duration::from_secs(1),
                service.get_valid_chatgpt_plan_access_token("chatgpt-plan:subscriber")
            )
            .await
            .unwrap()
            .is_err()
        );
    })
    .await;
    assert_eq!(requests.load(Ordering::SeqCst), 1);
    server.abort();
}

#[cfg(unix)]
#[tokio::test]
async fn canonical_persistence_kill_child() {
    let Ok(root) = std::env::var("ZEROCLAW_PLAN_PERSISTENCE_ROOT") else {
        return;
    };
    let base = std::env::var("ZEROCLAW_PLAN_PERSISTENCE_BASE").unwrap();
    crate::plan_test_transport::scope(&base, async {
        let service = AuthService::new(std::path::Path::new(&root), true);
        let binding = "chatgpt-plan:subscriber";
        if let Ok(stage) = std::env::var("ZEROCLAW_PLAN_PERSISTENCE_STAGE") {
            let pause = super::super::protected_file::PersistencePause {
                remaining: std::cell::Cell::new(
                    std::env::var("ZEROCLAW_PLAN_PERSISTENCE_NUMBER")
                        .unwrap()
                        .parse()
                        .unwrap(),
                ),
                stage,
                ready: std::env::var_os("ZEROCLAW_PLAN_PERSISTENCE_READY")
                    .unwrap()
                    .into(),
            };
            super::super::protected_file::PERSISTENCE_PAUSE
                .scope(pause, async {
                    service
                        .get_valid_chatgpt_plan_access_token(binding)
                        .await
                        .unwrap();
                })
                .await;
            panic!("synthetic persistence child should be killed while holding the store guard");
        }
        let data = tokio::time::timeout(Duration::from_secs(1), service.store.load())
            .await
            .expect("fresh process must regain store access after persistence death")
            .unwrap();
        let saved = &data.profiles[binding];
        let uncertain = std::env::var("ZEROCLAW_PLAN_PERSISTENCE_UNCERTAIN").unwrap() == "true";
        assert_eq!(
            saved
                .plan_registration
                .as_ref()
                .unwrap()
                .refresh_started_at
                .is_some(),
            uncertain
        );
        assert_eq!(
            saved.token_set.as_ref().unwrap().refresh_token.as_deref(),
            Some(
                std::env::var("ZEROCLAW_PLAN_PERSISTENCE_REFRESH")
                    .unwrap()
                    .as_str()
            )
        );
        if uncertain {
            let error = service
                .get_valid_chatgpt_plan_access_token(binding)
                .await
                .unwrap_err();
            assert!(error.to_string().contains("uncertain"), "{error}");
            // Repair crosses the actual signed-identity validation and commit path.
            let oauth = PlanOAuth::default();
            let attempt = oauth
                .begin(
                    &service,
                    "subscriber",
                    "http://127.0.0.1:54321/auth/callback",
                )
                .await
                .unwrap();
            let callback = format!(
                "{}?state={}&code={}&client_id=opaque-client",
                attempt.redirect_uri, attempt.pkce.state, attempt.nonce
            );
            oauth.complete(&service, attempt, &callback).await.unwrap();
            assert_eq!(
                service
                    .get_valid_chatgpt_plan_access_token(binding)
                    .await
                    .unwrap(),
                "synthetic-repaired-access"
            );
            let repaired = service.store.load().await.unwrap();
            assert!(
                repaired.profiles[binding]
                    .plan_registration
                    .as_ref()
                    .unwrap()
                    .refresh_started_at
                    .is_none()
            );
            assert_eq!(
                repaired.profiles[binding]
                    .token_set
                    .as_ref()
                    .unwrap()
                    .refresh_token
                    .as_deref(),
                Some("synthetic-repaired-refresh")
            );
        } else {
            assert_eq!(
                service
                    .get_valid_chatgpt_plan_access_token(binding)
                    .await
                    .unwrap(),
                "synthetic-rotated-access"
            );
        }
    })
    .await;
}

#[cfg(unix)]
#[tokio::test]
async fn canonical_persistence_kill_covers_marker_and_rotated_commit_windows() {
    use crate::plan_test_transport::SyntheticIdentity;
    use axum::{
        Json, Router,
        routing::{get, post},
    };
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let identity = Arc::new(SyntheticIdentity::new());
    let refreshes = Arc::new(AtomicUsize::new(0));
    let signins = Arc::new(AtomicUsize::new(0));
    let app = Router::new()
        .route("/jwks", get({ let identity = identity.clone(); move || { let identity = identity.clone(); async move { Json(identity.jwks()) } } }))
        .route("/token", post({ let identity = identity.clone(); let refreshes = refreshes.clone(); let signins = signins.clone();
            move |body: String| { let identity = identity.clone(); let refreshes = refreshes.clone(); let signins = signins.clone(); async move {
                let form = oauth_common::parse_query_params(&body);
                assert_eq!(form["client_id"], "opaque-client");
                if form["grant_type"] == "refresh_token" {
                    assert_eq!(form["refresh_token"], "synthetic-refresh", "old rotating token must never be replayed");
                    refreshes.fetch_add(1, Ordering::SeqCst);
                    Json(serde_json::json!({"access_token":"synthetic-rotated-access","refresh_token":"synthetic-rotated-refresh","token_type":"Bearer","expires_in":3600,"scope":PLAN_SCOPE}))
                } else {
                    signins.fetch_add(1, Ordering::SeqCst);
                    Json(serde_json::json!({"access_token":"synthetic-repaired-access","refresh_token":"synthetic-repaired-refresh","id_token":identity.id_token("opaque-client","subject-fixture",&form["code"]),"token_type":"Bearer","expires_in":3600,"scope":PLAN_SCOPE}))
                }
            } }
        }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = ::zeroclaw_spawn::spawn!(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let mut failed_windows = Vec::new();
    for (number, stage, uncertain, refresh, egress) in [
        (1, "before-replace", false, "synthetic-refresh", 0),
        (1, "after-replace", true, "synthetic-refresh", 0),
        (2, "before-replace", true, "synthetic-refresh", 1),
        (2, "after-replace", false, "synthetic-rotated-refresh", 1),
    ] {
        refreshes.store(0, Ordering::SeqCst);
        signins.store(0, Ordering::SeqCst);
        let root = TempDir::new().unwrap();
        let alias_root = TempDir::new().unwrap();
        let alias = alias_root.path().join("instance");
        std::os::unix::fs::symlink(root.path(), &alias).unwrap();
        let service = AuthService::new(root.path(), true);
        let mut expired = profile("subscriber", "opaque-client", "subject-fixture", PLAN_SCOPE);
        expired.token_set.as_mut().unwrap().expires_at = Some(Utc::now());
        service.store.upsert_profile(expired, false).await.unwrap();
        let ready = root.path().join("persistence-ready");
        let mut command = tokio::process::Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "auth::chatgpt_plan::tests::canonical_persistence_kill_child",
                "--exact",
                "--nocapture",
            ])
            .env("ZEROCLAW_PLAN_PERSISTENCE_ROOT", &alias)
            .env("ZEROCLAW_PLAN_PERSISTENCE_BASE", &base)
            .kill_on_drop(true);
        let mut child = command
            .env("ZEROCLAW_PLAN_PERSISTENCE_STAGE", stage)
            .env("ZEROCLAW_PLAN_PERSISTENCE_NUMBER", number.to_string())
            .env("ZEROCLAW_PLAN_PERSISTENCE_READY", &ready)
            .spawn()
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            while !ready.exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(std::fs::read_to_string(&ready).unwrap(), stage);
        assert!(
            tokio::time::timeout(Duration::from_millis(150), service.store.load())
                .await
                .is_err(),
            "live persistence writer must retain exclusion"
        );
        assert_eq!(refreshes.load(Ordering::SeqCst), egress);
        child.kill().await.unwrap();
        child.wait().await.unwrap();
        command
            .env_remove("ZEROCLAW_PLAN_PERSISTENCE_STAGE")
            .env_remove("ZEROCLAW_PLAN_PERSISTENCE_NUMBER")
            .env_remove("ZEROCLAW_PLAN_PERSISTENCE_READY")
            .env("ZEROCLAW_PLAN_PERSISTENCE_ROOT", root.path())
            .env("ZEROCLAW_PLAN_PERSISTENCE_UNCERTAIN", uncertain.to_string())
            .env("ZEROCLAW_PLAN_PERSISTENCE_REFRESH", refresh);
        let status = tokio::time::timeout(Duration::from_secs(5), command.spawn().unwrap().wait())
            .await
            .unwrap()
            .unwrap();
        if !status.success() {
            failed_windows.push(format!("transaction {number} {stage}"));
            continue;
        }
        assert_eq!(
            refreshes.load(Ordering::SeqCst),
            egress + usize::from(number == 1 && !uncertain),
            "ambiguous refresh must not replay the old token"
        );
        assert_eq!(signins.load(Ordering::SeqCst), usize::from(uncertain));
        assert!(
            service
                .store
                .load()
                .await
                .unwrap()
                .profiles
                .contains_key("chatgpt-plan:subscriber")
        );
    }
    server.abort();
    assert!(
        failed_windows.is_empty(),
        "fresh process recovery failed: {failed_windows:?}"
    );
}

#[tokio::test]
async fn ambient_key_billing_isolation_child() {
    let Ok(root) = std::env::var("ZEROCLAW_PLAN_BILLING_FIXTURE_ROOT") else {
        return;
    };
    let base = std::env::var("ZEROCLAW_PLAN_BILLING_FIXTURE_BASE").unwrap();
    assert_eq!(
        std::env::var("OPENAI_API_KEY").unwrap(),
        "synthetic-metered-key"
    );
    let mut config = zeroclaw_config::schema::Config {
        config_path: std::path::Path::new(&root).join("config.toml"),
        ..Default::default()
    };
    let plan = zeroclaw_config::schema::ModelProviderConfig {
        kind: Some("chatgpt-plan".into()),
        model: Some("model-fixture".into()),
        chatgpt_plan_auth: Some(zeroclaw_config::schema::ChatGptPlanAuthConfig {
            registration: "chatgpt-plan:subscriber".into(),
        }),
        ..Default::default()
    };
    config.providers.models.openai.insert(
        "subscriber".into(),
        zeroclaw_config::schema::OpenAIModelProviderConfig { base: plan },
    );
    crate::plan_test_transport::scope(&base, async {
        assert_eq!(
            check_bound_provider(&config, "openai.subscriber", "Reply READY")
                .await
                .unwrap(),
            "READY"
        );
        assert!(
            check_bound_provider(&config, "openai.missing", "fixture")
                .await
                .is_err()
        );
        let entry = config
            .providers
            .models
            .openai
            .get_mut("subscriber")
            .unwrap();
        entry.base.chatgpt_plan_auth = None;
        let opts = crate::model_provider_runtime_options_from_model_provider_entry(
            &config,
            config.providers.models.find("openai", "subscriber"),
        );
        // The legacy dispatch (no binding) rejects the marker even with a
        // custom URI and explicit/ambient keys, which protects downgrades.
        assert!(
            crate::create_model_provider_for_alias_with_url(
                &config,
                "openai",
                "subscriber",
                Some("synthetic-metered-key"),
                Some("https://api.openai.com/v1"),
                &opts
            )
            .is_err()
        );
    })
    .await;
}

#[tokio::test]
async fn ambient_api_key_cannot_redirect_bound_plan_to_metered_endpoint() {
    use axum::{Router, routing::post};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let requests = Arc::new(AtomicUsize::new(0));
    let app = Router::new().route("/responses",post({ let requests = requests.clone(); move |headers:axum::http::HeaderMap| { let requests = requests.clone(); async move {
        assert_eq!(headers["authorization"],"Bearer synthetic-access"); requests.fetch_add(1,Ordering::SeqCst);
        ([("content-type","text/event-stream")],"data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"output_text\":\"READY\"}}\n\n")
    } } }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = ::zeroclaw_spawn::spawn!(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let root = TempDir::new().unwrap();
    let service = AuthService::new(root.path(), true);
    service
        .store
        .upsert_profile(
            profile("subscriber", "opaque-client", "subject-fixture", PLAN_SCOPE),
            false,
        )
        .await
        .unwrap();
    let status = tokio::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "auth::chatgpt_plan::tests::ambient_key_billing_isolation_child",
            "--exact",
        ])
        .env("ZEROCLAW_PLAN_BILLING_FIXTURE_ROOT", root.path())
        .env("ZEROCLAW_PLAN_BILLING_FIXTURE_BASE", &base)
        .env("OPENAI_API_KEY", "synthetic-metered-key")
        .kill_on_drop(true)
        .status()
        .await
        .unwrap();
    assert!(status.success());
    assert_eq!(requests.load(Ordering::SeqCst), 1);
    server.abort();
}

#[tokio::test]
async fn oauth_refresh_redirect_cannot_forward_rotating_token() {
    use axum::{Json, Router, routing::post};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let captures = Arc::new(AtomicUsize::new(0));
    let app = Router::new().route("/token",post(|| async { (axum::http::StatusCode::TEMPORARY_REDIRECT,[("location","/capture")]) }))
        .route("/capture",post({ let captures = captures.clone(); move || { let captures = captures.clone(); async move {
            captures.fetch_add(1,Ordering::SeqCst); Json(serde_json::json!({"access_token":"synthetic-rotated-access","refresh_token":"synthetic-rotated-refresh","token_type":"Bearer","expires_in":3600,"scope":PLAN_SCOPE}))
        } } }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = ::zeroclaw_spawn::spawn!(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let root = TempDir::new().unwrap();
    let service = AuthService::new(root.path(), true);
    let mut expired = profile("subscriber", "opaque-client", "subject-fixture", PLAN_SCOPE);
    expired.token_set.as_mut().unwrap().expires_at = Some(Utc::now());
    service.store.upsert_profile(expired, false).await.unwrap();
    crate::plan_test_transport::scope(&base, async {
        assert!(
            service
                .get_valid_chatgpt_plan_access_token("chatgpt-plan:subscriber")
                .await
                .is_err()
        );
    })
    .await;
    assert_eq!(captures.load(Ordering::SeqCst), 0);
    server.abort();
}

#[tokio::test]
async fn concurrent_refresh_waits_for_live_lease_before_interpreting_uncertainty() {
    use axum::{Json, Router, routing::post};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let requests = Arc::new(AtomicUsize::new(0));
    let started = Arc::new(tokio::sync::Notify::new());
    let app = Router::new().route("/token",post({ let requests = requests.clone(); let started = started.clone(); move || { let requests = requests.clone(); let started = started.clone(); async move {
        requests.fetch_add(1,Ordering::SeqCst); started.notify_one(); tokio::time::sleep(Duration::from_millis(250)).await;
        Json(serde_json::json!({"access_token":"synthetic-rotated-access","refresh_token":"synthetic-rotated-refresh","token_type":"Bearer","expires_in":3600,"scope":PLAN_SCOPE}))
    } } }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = ::zeroclaw_spawn::spawn!(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let root = TempDir::new().unwrap();
    let service = AuthService::new(root.path(), true);
    let mut expired = profile("subscriber", "opaque-client", "subject-fixture", PLAN_SCOPE);
    expired.token_set.as_mut().unwrap().expires_at = Some(Utc::now());
    service.store.upsert_profile(expired, false).await.unwrap();
    let first_service = service.clone();
    let first_base = base.clone();
    let first = ::zeroclaw_spawn::spawn!(async move {
        crate::plan_test_transport::scope(&first_base, async {
            first_service
                .get_valid_chatgpt_plan_access_token("chatgpt-plan:subscriber")
                .await
        })
        .await
    });
    tokio::time::timeout(Duration::from_secs(3), started.notified())
        .await
        .unwrap();
    let second = crate::plan_test_transport::scope(&base, async {
        service
            .get_valid_chatgpt_plan_access_token("chatgpt-plan:subscriber")
            .await
    })
    .await;
    assert_eq!(first.await.unwrap().unwrap(), "synthetic-rotated-access");
    assert_eq!(second.unwrap(), "synthetic-rotated-access");
    assert_eq!(requests.load(Ordering::SeqCst), 1);
    server.abort();
}
