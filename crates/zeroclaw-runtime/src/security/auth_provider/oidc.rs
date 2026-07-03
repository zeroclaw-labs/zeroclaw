use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use parking_lot::RwLock;
use serde::Deserialize;
use zeroclaw_api::grants::ResolvedGrants;
use zeroclaw_api::principal::{AuthMethod, AuthOutcome, DenyReason, Principal};
use zeroclaw_config::schema::{OidcConfig, OidcValidation};

use super::{AuthProvider, Credential};

const MFA_AMR_VALUES: &[&str] = &["mfa", "otp", "hwk"];

#[derive(Debug, Clone, Deserialize)]
struct Discovery {
    jwks_uri: Option<String>,
    introspection_endpoint: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct Jwk {
    #[serde(default)]
    kid: Option<String>,
    kty: String,
    #[serde(default)]
    alg: Option<String>,
    #[serde(default)]
    n: Option<String>,
    #[serde(default)]
    e: Option<String>,
    #[serde(default)]
    x: Option<String>,
    #[serde(default)]
    y: Option<String>,
    #[serde(default)]
    crv: Option<String>,
}

#[derive(Debug, Deserialize)]
struct JwkSet {
    keys: Vec<Jwk>,
}

#[derive(Debug, Deserialize)]
struct JwtHeader {
    alg: String,
    #[serde(default)]
    kid: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct Claims {
    #[serde(default)]
    active: Option<bool>,
    #[serde(default)]
    iss: Option<String>,
    #[serde(default)]
    sub: Option<String>,
    #[serde(default)]
    aud: Option<serde_json::Value>,
    #[serde(default)]
    exp: Option<u64>,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    amr: Option<Vec<String>>,
    #[serde(flatten)]
    rest: serde_json::Map<String, serde_json::Value>,
}

pub struct OidcAuthProvider {
    alias: String,
    config: OidcConfig,
    profiles: Arc<HashMap<String, ResolvedGrants>>,
    http: reqwest::Client,
    discovery: RwLock<Option<Discovery>>,
    jwks: RwLock<HashMap<String, Jwk>>,
}

impl OidcAuthProvider {
    pub fn new(
        alias: String,
        config: OidcConfig,
        profiles: Arc<HashMap<String, ResolvedGrants>>,
    ) -> anyhow::Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .build()?;
        Ok(Self {
            alias,
            config,
            profiles,
            http,
            discovery: RwLock::new(None),
            jwks: RwLock::new(HashMap::new()),
        })
    }

    fn split_jwt(token: &str) -> Option<(&str, &str, &str)> {
        let mut parts = token.splitn(3, '.');
        match (parts.next(), parts.next(), parts.next()) {
            (Some(h), Some(p), Some(s)) if !h.is_empty() && !p.is_empty() && !s.is_empty() => {
                Some((h, p, s))
            }
            _ => None,
        }
    }

    fn unverified_issuer(token: &str) -> Option<String> {
        let (_, payload, _) = Self::split_jwt(token)?;
        let bytes = URL_SAFE_NO_PAD.decode(payload).ok()?;
        let claims: Claims = serde_json::from_slice(&bytes).ok()?;
        claims.iss
    }

    async fn discovery(&self) -> anyhow::Result<Discovery> {
        if let Some(d) = self.discovery.read().clone() {
            return Ok(d);
        }
        let url = format!(
            "{}/.well-known/openid-configuration",
            self.config.issuer.trim_end_matches('/')
        );
        let d: Discovery = self
            .http
            .get(&url)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        *self.discovery.write() = Some(d.clone());
        Ok(d)
    }

    async fn refresh_jwks(&self) -> anyhow::Result<()> {
        let discovery = self.discovery().await?;
        let uri = discovery
            .jwks_uri
            .ok_or_else(|| anyhow::Error::msg("issuer discovery has no jwks_uri"))?;
        let set: JwkSet = self
            .http
            .get(&uri)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        let mut map = self.jwks.write();
        map.clear();
        for key in set.keys {
            if let Some(kid) = key.kid.clone() {
                map.insert(kid, key);
            }
        }
        Ok(())
    }

    fn cached_key(&self, kid: &str) -> Option<Jwk> {
        self.jwks.read().get(kid).cloned()
    }
}

fn verify_signature(header: &JwtHeader, jwk: &Jwk, signed: &str, sig: &[u8]) -> anyhow::Result<()> {
    use ring::signature;
    if let Some(alg) = &jwk.alg
        && alg != &header.alg
    {
        anyhow::bail!("token alg {} does not match key alg {alg}", header.alg);
    }
    match header.alg.as_str() {
        "RS256" => {
            if jwk.kty != "RSA" {
                anyhow::bail!("RS256 token but key kty is {}", jwk.kty);
            }
            let n = URL_SAFE_NO_PAD.decode(jwk.n.as_deref().unwrap_or_default())?;
            let e = URL_SAFE_NO_PAD.decode(jwk.e.as_deref().unwrap_or_default())?;
            let key = signature::RsaPublicKeyComponents { n, e };
            key.verify(
                &signature::RSA_PKCS1_2048_8192_SHA256,
                signed.as_bytes(),
                sig,
            )
            .map_err(|_| anyhow::Error::msg("RS256 signature verification failed"))
        }
        "ES256" => {
            if jwk.kty != "EC" || jwk.crv.as_deref() != Some("P-256") {
                anyhow::bail!("ES256 token but key is not an EC P-256 key");
            }
            let x = URL_SAFE_NO_PAD.decode(jwk.x.as_deref().unwrap_or_default())?;
            let y = URL_SAFE_NO_PAD.decode(jwk.y.as_deref().unwrap_or_default())?;
            let mut point = Vec::with_capacity(1 + x.len() + y.len());
            point.push(0x04);
            point.extend_from_slice(&x);
            point.extend_from_slice(&y);
            let key = signature::UnparsedPublicKey::new(&signature::ECDSA_P256_SHA256_FIXED, point);
            key.verify(signed.as_bytes(), sig)
                .map_err(|_| anyhow::Error::msg("ES256 signature verification failed"))
        }
        other => anyhow::bail!("unsupported JWT alg '{other}': expected RS256 or ES256"),
    }
}

fn claim_values(rest: &serde_json::Map<String, serde_json::Value>, path: &str) -> Vec<String> {
    let mut cursor = serde_json::Value::Object(rest.clone());
    for segment in path.split('.') {
        match cursor.get(segment) {
            Some(next) => cursor = next.clone(),
            None => return Vec::new(),
        }
    }
    match cursor {
        serde_json::Value::String(s) => vec![s],
        serde_json::Value::Array(items) => items
            .into_iter()
            .filter_map(|v| v.as_str().map(str::to_owned))
            .collect(),
        // Zitadel-style role claims are objects keyed by role name
        // (`urn:zitadel:iam:org:project:roles`); the keys are the values.
        serde_json::Value::Object(map) => map.keys().cloned().collect(),
        _ => Vec::new(),
    }
}

fn audience_matches(aud: Option<&serde_json::Value>, expected: &str) -> bool {
    match aud {
        Some(serde_json::Value::String(s)) => s == expected,
        Some(serde_json::Value::Array(items)) => items.iter().any(|v| v.as_str() == Some(expected)),
        _ => false,
    }
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

impl OidcAuthProvider {
    fn claims_to_outcome(&self, claims: &Claims) -> AuthOutcome {
        if claims.iss.as_deref() != Some(self.config.issuer.as_str()) {
            return AuthOutcome::Denied {
                reason: DenyReason::BadCredential,
            };
        }
        if !audience_matches(claims.aud.as_ref(), &self.config.audience) {
            return AuthOutcome::Denied {
                reason: DenyReason::BadCredential,
            };
        }
        match claims.exp {
            Some(exp) if exp > now_unix() => {}
            _ => {
                return AuthOutcome::Denied {
                    reason: DenyReason::TokenExpired,
                };
            }
        }
        let Some(sub) = claims.sub.as_deref().filter(|s| !s.trim().is_empty()) else {
            return AuthOutcome::Denied {
                reason: DenyReason::BadCredential,
            };
        };
        let mfa_verified = claims
            .amr
            .iter()
            .flatten()
            .any(|m| MFA_AMR_VALUES.contains(&m.as_str()));
        if self.config.require_mfa && !mfa_verified {
            return AuthOutcome::Denied {
                reason: DenyReason::MfaRequired,
            };
        }

        let roles = claim_values(&claims.rest, &self.config.claim_path);
        let mut grants = ResolvedGrants::none();
        let mut mapped = false;
        for role in &roles {
            if let Some(profile_alias) = self.config.role_map.get(role) {
                match self.profiles.get(profile_alias) {
                    Some(profile) => {
                        grants.merge(profile);
                        mapped = true;
                    }
                    None => {
                        return AuthOutcome::Denied {
                            reason: DenyReason::Misconfigured,
                        };
                    }
                }
            }
        }
        if !mapped {
            return AuthOutcome::Denied {
                reason: DenyReason::NotEntitled,
            };
        }

        let scopes = claims
            .scope
            .as_deref()
            .unwrap_or_default()
            .split_whitespace()
            .map(String::from)
            .collect();
        let mut principal = Principal::new(sub, sub, AuthMethod::Oidc)
            .with_roles(roles)
            .with_scopes(scopes)
            .with_mfa_verified(mfa_verified)
            .with_expires_at(claims.exp.unwrap_or(0))
            .with_auth_provider(self.alias.clone());
        principal.grants = grants;
        AuthOutcome::Authenticated(principal)
    }

    async fn verify_jwks(&self, token: &str) -> AuthOutcome {
        let Some((header_b64, payload_b64, sig_b64)) = Self::split_jwt(token) else {
            return AuthOutcome::Denied {
                reason: DenyReason::BadCredential,
            };
        };
        let header: JwtHeader = match URL_SAFE_NO_PAD
            .decode(header_b64)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
        {
            Some(h) => h,
            None => {
                return AuthOutcome::Denied {
                    reason: DenyReason::BadCredential,
                };
            }
        };
        let kid = header.kid.clone().unwrap_or_default();
        let key = match self.cached_key(&kid) {
            Some(k) => Some(k),
            None => {
                if self.refresh_jwks().await.is_err() {
                    return AuthOutcome::Denied {
                        reason: DenyReason::Misconfigured,
                    };
                }
                self.cached_key(&kid)
            }
        };
        let Some(key) = key else {
            return AuthOutcome::Denied {
                reason: DenyReason::BadCredential,
            };
        };
        let signed_len = header_b64.len() + 1 + payload_b64.len();
        let signed = &token[..signed_len];
        let Ok(sig) = URL_SAFE_NO_PAD.decode(sig_b64) else {
            return AuthOutcome::Denied {
                reason: DenyReason::BadCredential,
            };
        };
        if verify_signature(&header, &key, signed, &sig).is_err() {
            return AuthOutcome::Denied {
                reason: DenyReason::BadCredential,
            };
        }
        let claims: Claims = match URL_SAFE_NO_PAD
            .decode(payload_b64)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
        {
            Some(c) => c,
            None => {
                return AuthOutcome::Denied {
                    reason: DenyReason::BadCredential,
                };
            }
        };
        self.claims_to_outcome(&claims)
    }

    async fn verify_introspection(&self, token: &str) -> AuthOutcome {
        let discovery = match self.discovery().await {
            Ok(d) => d,
            Err(_) => {
                return AuthOutcome::Denied {
                    reason: DenyReason::Misconfigured,
                };
            }
        };
        let Some(endpoint) = discovery.introspection_endpoint else {
            return AuthOutcome::Denied {
                reason: DenyReason::Misconfigured,
            };
        };
        let Some(secret) = self.config.client_secret.as_deref() else {
            return AuthOutcome::Denied {
                reason: DenyReason::Misconfigured,
            };
        };
        let response = self
            .http
            .post(&endpoint)
            .basic_auth(self.config.enrollment_client_id(), Some(secret))
            .form(&[("token", token)])
            .send()
            .await;
        let claims: Claims = match response {
            Ok(resp) if resp.status().is_success() => match resp.json().await {
                Ok(c) => c,
                Err(_) => {
                    return AuthOutcome::Denied {
                        reason: DenyReason::BadCredential,
                    };
                }
            },
            _ => {
                return AuthOutcome::Denied {
                    reason: DenyReason::Misconfigured,
                };
            }
        };
        if claims.active != Some(true) {
            return AuthOutcome::Denied {
                reason: DenyReason::TokenExpired,
            };
        }
        self.claims_to_outcome(&claims)
    }
}

#[async_trait]
impl AuthProvider for OidcAuthProvider {
    fn name(&self) -> &str {
        &self.alias
    }

    fn method(&self) -> AuthMethod {
        AuthMethod::Oidc
    }

    fn accepts(&self, credential: &Credential) -> bool {
        let Credential::Bearer(token) = credential else {
            return false;
        };
        match Self::unverified_issuer(token) {
            Some(iss) => iss == self.config.issuer,
            None => self.config.validation == OidcValidation::Introspection,
        }
    }

    async fn verify(&self, credential: &Credential) -> AuthOutcome {
        let Credential::Bearer(token) = credential else {
            return AuthOutcome::Denied {
                reason: DenyReason::BadCredential,
            };
        };
        match self.config.validation {
            OidcValidation::Jwks => self.verify_jwks(token).await,
            OidcValidation::Introspection => self.verify_introspection(token).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::rand::SystemRandom;
    use ring::signature::{ECDSA_P256_SHA256_FIXED_SIGNING, EcdsaKeyPair, KeyPair};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    use zeroclaw_api::grants::{Resource, Verb};

    struct TestIdp {
        server: MockServer,
        key: EcdsaKeyPair,
        issuer: String,
    }

    async fn start_idp() -> TestIdp {
        let server = MockServer::start().await;
        let issuer = server.uri();
        let rng = SystemRandom::new();
        let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng).unwrap();
        let key = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, pkcs8.as_ref(), &rng)
            .unwrap();

        let public = key.public_key().as_ref();
        let x = URL_SAFE_NO_PAD.encode(&public[1..33]);
        let y = URL_SAFE_NO_PAD.encode(&public[33..65]);
        let jwks = serde_json::json!({
            "keys": [{
                "kid": "test-key",
                "kty": "EC",
                "crv": "P-256",
                "alg": "ES256",
                "x": x,
                "y": y,
            }]
        });
        Mock::given(method("GET"))
            .and(path("/.well-known/openid-configuration"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "jwks_uri": format!("{issuer}/jwks"),
                "introspection_endpoint": format!("{issuer}/introspect"),
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/jwks"))
            .respond_with(ResponseTemplate::new(200).set_body_json(jwks))
            .mount(&server)
            .await;
        TestIdp {
            server,
            key,
            issuer,
        }
    }

    impl TestIdp {
        fn mint(&self, claims: serde_json::Value) -> String {
            let header = URL_SAFE_NO_PAD.encode(r#"{"alg":"ES256","kid":"test-key"}"#);
            let payload = URL_SAFE_NO_PAD.encode(claims.to_string());
            let signed = format!("{header}.{payload}");
            let rng = SystemRandom::new();
            let sig = self.key.sign(&rng, signed.as_bytes()).unwrap();
            format!("{signed}.{}", URL_SAFE_NO_PAD.encode(sig.as_ref()))
        }

        fn provider(&self, validation: OidcValidation) -> OidcAuthProvider {
            let mut role_map = HashMap::new();
            role_map.insert("ops".to_string(), "operator".to_string());
            let config = OidcConfig {
                issuer: self.issuer.clone(),
                audience: "zeroclaw".into(),
                client_id: "zeroclaw".into(),
                client_secret: Some("s3cret".into()),
                validation,
                claim_path: "realm_access.roles".into(),
                role_map,
                require_mfa: false,
            };
            let mut grants = ResolvedGrants::none();
            grants.resources.insert(
                Resource::System,
                std::collections::BTreeSet::from([Verb::Read]),
            );
            let mut profiles = HashMap::new();
            profiles.insert("operator".to_string(), grants);
            OidcAuthProvider::new("oidc.test".into(), config, Arc::new(profiles)).unwrap()
        }

        fn good_claims(&self) -> serde_json::Value {
            serde_json::json!({
                "iss": self.issuer,
                "sub": "alice",
                "aud": "zeroclaw",
                "exp": now_unix() + 600,
                "scope": "openid profile",
                "realm_access": {"roles": ["ops"]},
            })
        }
    }

    #[tokio::test]
    async fn valid_jwt_authenticates_with_mapped_grants() {
        let idp = start_idp().await;
        let provider = idp.provider(OidcValidation::Jwks);
        let token = idp.mint(idp.good_claims());
        assert!(provider.accepts(&Credential::Bearer(token.clone())));
        let out = provider.verify(&Credential::Bearer(token)).await;
        let p = out.principal().expect("authenticated");
        assert_eq!(p.id.as_str(), "alice");
        assert_eq!(p.auth_method, AuthMethod::Oidc);
        assert!(p.grants.permits(Resource::System, Verb::Read));
        assert!(!p.grants.permits(Resource::Config, Verb::Update));
        assert!(p.is_authenticated());
        assert_eq!(p.auth_provider_label(), "oidc.test");
    }

    #[tokio::test]
    async fn tampered_signature_is_denied() {
        let idp = start_idp().await;
        let provider = idp.provider(OidcValidation::Jwks);
        let token = idp.mint(idp.good_claims());
        let mut parts: Vec<&str> = token.split('.').collect();
        let forged_payload = URL_SAFE_NO_PAD.encode(
            serde_json::json!({
                "iss": idp.issuer, "sub": "mallory", "aud": "zeroclaw",
                "exp": now_unix() + 600,
                "realm_access": {"roles": ["ops"]},
            })
            .to_string(),
        );
        parts[1] = &forged_payload;
        let forged = parts.join(".");
        let out = provider.verify(&Credential::Bearer(forged)).await;
        assert!(matches!(
            out,
            AuthOutcome::Denied {
                reason: DenyReason::BadCredential
            }
        ));
    }

    #[tokio::test]
    async fn expired_token_is_denied() {
        let idp = start_idp().await;
        let provider = idp.provider(OidcValidation::Jwks);
        let mut claims = idp.good_claims();
        claims["exp"] = serde_json::json!(now_unix() - 60);
        let out = provider.verify(&Credential::Bearer(idp.mint(claims))).await;
        assert!(matches!(
            out,
            AuthOutcome::Denied {
                reason: DenyReason::TokenExpired
            }
        ));
    }

    #[tokio::test]
    async fn wrong_audience_is_denied() {
        let idp = start_idp().await;
        let provider = idp.provider(OidcValidation::Jwks);
        let mut claims = idp.good_claims();
        claims["aud"] = serde_json::json!("someone-else");
        let out = provider.verify(&Credential::Bearer(idp.mint(claims))).await;
        assert!(!out.is_allowed());
    }

    #[tokio::test]
    async fn unmapped_role_is_not_entitled() {
        let idp = start_idp().await;
        let provider = idp.provider(OidcValidation::Jwks);
        let mut claims = idp.good_claims();
        claims["realm_access"] = serde_json::json!({"roles": ["guest"]});
        let out = provider.verify(&Credential::Bearer(idp.mint(claims))).await;
        assert!(matches!(
            out,
            AuthOutcome::Denied {
                reason: DenyReason::NotEntitled
            }
        ));
    }

    #[tokio::test]
    async fn mfa_required_without_amr_is_denied() {
        let idp = start_idp().await;
        let mut provider = idp.provider(OidcValidation::Jwks);
        provider.config.require_mfa = true;
        let out = provider
            .verify(&Credential::Bearer(idp.mint(idp.good_claims())))
            .await;
        assert!(matches!(
            out,
            AuthOutcome::Denied {
                reason: DenyReason::MfaRequired
            }
        ));

        let mut claims = idp.good_claims();
        claims["amr"] = serde_json::json!(["otp"]);
        let out = provider.verify(&Credential::Bearer(idp.mint(claims))).await;
        assert!(out.is_allowed());
        assert!(out.principal().unwrap().mfa_verified);
    }

    #[tokio::test]
    async fn foreign_issuer_is_not_accepted() {
        let idp = start_idp().await;
        let provider = idp.provider(OidcValidation::Jwks);
        let mut claims = idp.good_claims();
        claims["iss"] = serde_json::json!("https://other-idp.example.com");
        let token = idp.mint(claims);
        assert!(!provider.accepts(&Credential::Bearer(token)));
    }

    #[tokio::test]
    async fn opaque_token_rejected_in_jwks_mode_accepted_for_introspection() {
        let idp = start_idp().await;
        let jwks_provider = idp.provider(OidcValidation::Jwks);
        assert!(!jwks_provider.accepts(&Credential::Bearer("opaque-token".into())));
        let intro_provider = idp.provider(OidcValidation::Introspection);
        assert!(intro_provider.accepts(&Credential::Bearer("opaque-token".into())));
    }

    #[tokio::test]
    async fn introspection_active_token_authenticates() {
        let idp = start_idp().await;
        Mock::given(method("POST"))
            .and(path("/introspect"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "active": true,
                "iss": idp.issuer,
                "sub": "bob",
                "aud": "zeroclaw",
                "exp": now_unix() + 600,
                "scope": "openid",
                "realm_access": {"roles": ["ops"]},
            })))
            .mount(&idp.server)
            .await;
        let provider = idp.provider(OidcValidation::Introspection);
        let out = provider
            .verify(&Credential::Bearer("opaque-token".into()))
            .await;
        let p = out.principal().expect("authenticated via introspection");
        assert_eq!(p.id.as_str(), "bob");
        assert!(p.grants.permits(Resource::System, Verb::Read));
    }

    #[tokio::test]
    async fn introspection_inactive_token_is_denied() {
        let idp = start_idp().await;
        Mock::given(method("POST"))
            .and(path("/introspect"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"active": false})),
            )
            .mount(&idp.server)
            .await;
        let provider = idp.provider(OidcValidation::Introspection);
        let out = provider
            .verify(&Credential::Bearer("revoked-token".into()))
            .await;
        assert!(!out.is_allowed());
    }

    #[tokio::test]
    async fn unreachable_idp_fails_closed() {
        let idp = start_idp().await;
        let provider = idp.provider(OidcValidation::Introspection);
        drop(idp.server);
        let out = provider
            .verify(&Credential::Bearer("opaque-token".into()))
            .await;
        assert!(matches!(
            out,
            AuthOutcome::Denied {
                reason: DenyReason::Misconfigured
            }
        ));
    }

    #[test]
    fn claim_path_walks_nested_and_flat_shapes() {
        let claims: Claims = serde_json::from_value(serde_json::json!({
            "realm_access": {"roles": ["a", "b"]},
            "groups": ["g1"],
            "plan": "pro",
        }))
        .unwrap();
        assert_eq!(
            claim_values(&claims.rest, "realm_access.roles"),
            vec!["a", "b"]
        );
        assert_eq!(claim_values(&claims.rest, "groups"), vec!["g1"]);
        assert_eq!(claim_values(&claims.rest, "plan"), vec!["pro"]);
        assert!(claim_values(&claims.rest, "missing.path").is_empty());
    }

    #[test]
    fn claim_path_extracts_object_keys_for_zitadel_role_shape() {
        let claims: Claims = serde_json::from_value(serde_json::json!({
            "urn:zitadel:iam:org:project:roles": {
                "zeroclaw-admin": {"276...": "org.example.com"},
                "zeroclaw-operator": {"276...": "org.example.com"},
            },
        }))
        .unwrap();
        let mut roles = claim_values(&claims.rest, "urn:zitadel:iam:org:project:roles");
        roles.sort();
        assert_eq!(roles, vec!["zeroclaw-admin", "zeroclaw-operator"]);
    }
}
