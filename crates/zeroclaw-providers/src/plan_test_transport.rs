//! Test-only dependency injection. Absent from production builds, has no
//! environment/config switch, and accepts only a synthetic loopback server.
use std::future::Future;

#[derive(Clone)]
pub(crate) struct Endpoints {
    pub authorize: String,
    pub token: String,
    pub jwks: String,
    pub responses: String,
    pub models: String,
}
tokio::task_local! { pub(crate) static ENDPOINTS: Endpoints; }
type AuthorizationObserver = std::sync::Arc<dyn Fn(&str) + Send + Sync>;
tokio::task_local! { static AUTHORIZATION_OBSERVER: AuthorizationObserver; }

pub(crate) fn observe_authorization(url: &str) {
    let _ = AUTHORIZATION_OBSERVER.try_with(|observer| observer(url));
}

/// Observe a synthetic authorization URL to emulate browser consent in tests.
pub async fn scope_with_observer<T>(
    base: &str,
    observer: AuthorizationObserver,
    operation: impl Future<Output = T>,
) -> T {
    scope(base, AUTHORIZATION_OBSERVER.scope(observer, operation)).await
}

/// Execute a fixture against a local synthetic OAuth/JWKS/Responses server.
pub async fn scope<T>(base: &str, operation: impl Future<Output = T>) -> T {
    let url = reqwest::Url::parse(base).expect("test base URL");
    assert_eq!(url.scheme(), "http");
    assert_eq!(url.host_str(), Some("127.0.0.1"));
    assert!(url.port().is_some());
    assert_eq!(url.path(), "/");
    assert!(
        url.query().is_none()
            && url.fragment().is_none()
            && url.username().is_empty()
            && url.password().is_none()
    );
    let base = base.trim_end_matches('/');
    ENDPOINTS
        .scope(
            Endpoints {
                authorize: format!("{base}/authorize"),
                token: format!("{base}/token"),
                jwks: format!("{base}/jwks"),
                responses: format!("{base}/responses"),
                models: format!("{base}/models"),
            },
            operation,
        )
        .await
}

/// Generates a fresh synthetic signing identity. No saved account material.
pub struct SyntheticIdentity {
    key: jsonwebtoken::EncodingKey,
    jwks: serde_json::Value,
}
impl Default for SyntheticIdentity {
    fn default() -> Self {
        Self::new()
    }
}
impl SyntheticIdentity {
    pub fn new() -> Self {
        use base64::Engine;
        use ring::signature::{ECDSA_P256_SHA256_ASN1_SIGNING, EcdsaKeyPair, KeyPair};
        let rng = ring::rand::SystemRandom::new();
        let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &rng)
            .expect("fixture signing key");
        let pair = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, pkcs8.as_ref(), &rng)
            .expect("fixture signing key");
        let public = pair.public_key().as_ref();
        Self {
            key: jsonwebtoken::EncodingKey::from_ec_der(pkcs8.as_ref()),
            jwks: serde_json::json!({"keys":[{
                "kid":"fixture","kty":"EC","crv":"P-256","alg":"ES256","use":"sig",
                "x":base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&public[1..33]),
                "y":base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&public[33..65])
            }]}),
        }
    }
    pub fn jwks(&self) -> serde_json::Value {
        self.jwks.clone()
    }
    pub fn id_token(&self, client: &str, subject: &str, nonce: &str) -> String {
        let mut header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::ES256);
        header.kid = Some("fixture".into());
        jsonwebtoken::encode(&header,&serde_json::json!({"iss":"https://auth.openai.com","aud":client,"sub":subject,"nonce":nonce,"iat":chrono::Utc::now().timestamp(),"exp":chrono::Utc::now().timestamp()+3600}),&self.key).expect("fixture ID token")
    }
}
