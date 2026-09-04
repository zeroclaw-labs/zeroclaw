//! The `oidc.<alias>` auth provider: verifies a presented bearer token
//! against ONE configured issuer — offline (JWKS signature validation) or
//! online (RFC 7662 introspection) — and emits the verified
//! [`AuthenticatedIdentity`] for the shared principal resolver.
//!
//! Contract boundaries (RFC 7141 Rev 8):
//! - The provider verifies credentials into identities. It never touches
//!   permission profiles or grants — claim-to-profile mapping belongs to
//!   the resolver.
//! - Only access tokens authenticate the bearer path. A token carrying the
//!   ID-token `nonce` marker is rejected even when its signature, issuer,
//!   and audience are valid; `require_at_jwt` additionally demands the
//!   RFC 9068 `at+jwt` type.
//! - Offline (JWKS) validation cannot see revocation, so the identity's
//!   expiry is capped at the earlier of the token `exp` and
//!   `max_auth_lifetime_secs`. Introspection identities carry a
//!   `revalidate_by` deadline instead; once it passes, the next privileged
//!   operation must re-verify or fail closed.
//! - JWKS refresh is bounded: an unknown `kid` may trigger at most one
//!   fetch per cooldown window, so a stream of bad tokens cannot hammer
//!   the IdP.
//! - Every ambiguity — unreachable issuer, malformed token, missing
//!   claims, unmatched audience — is a denial, never a fallback.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use parking_lot::{Mutex, RwLock};
use serde::Deserialize;
use zeroclaw_api::principal::{
    AuthMethod, AuthOutcome, AuthenticatedIdentity, DenyReason, IdentitySubject,
};
use zeroclaw_config::schema::{OidcConfig, OidcValidation};

use super::{AuthProvider, Credential};

/// `amr` values accepted as evidence of a completed second factor.
const MFA_AMR_VALUES: &[&str] = &["mfa", "otp", "hwk"];

/// Allowed skew between our clock and the IdP's when checking `exp`/`nbf`.
const CLOCK_LEEWAY_SECS: u64 = 30;

/// Minimum interval between JWKS refreshes triggered by unknown key ids.
const JWKS_REFRESH_COOLDOWN: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Deserialize)]
struct Discovery {
    /// The discovery document's own issuer assertion. OIDC Discovery
    /// requires it; it must equal the configured issuer or the document is
    /// not ours to trust.
    issuer: Option<String>,
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
    #[serde(default)]
    typ: Option<String>,
}

/// The standard claims this provider checks. The full verified claim map
/// is carried separately on the emitted identity for the resolver's
/// claim-path mapping.
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
    nbf: Option<u64>,
    #[serde(default)]
    azp: Option<String>,
    #[serde(default)]
    client_id: Option<String>,
    #[serde(default)]
    acr: Option<String>,
    #[serde(default)]
    amr: Option<Vec<String>>,
    /// ID-token marker (OIDC Core echoes the auth-request nonce into ID
    /// tokens, never into access tokens). Presence ⇒ wrong token purpose.
    #[serde(default)]
    nonce: Option<serde_json::Value>,
}

/// Which validation path produced the claims — it changes which absent
/// claims are acceptable and how the identity's lifetime is bounded.
#[derive(Clone, Copy, PartialEq)]
enum VerifiedVia {
    /// Offline JWKS signature validation: the token itself is the only
    /// evidence, so `iss`/`aud`/`exp` are all mandatory.
    Jwks,
    /// A positive verdict from the configured issuer's authenticated
    /// introspection endpoint: the endpoint is the authority, so RFC 7662
    /// optional response fields are enforced only when present.
    Introspection,
}

fn deny(reason: DenyReason) -> AuthOutcome {
    AuthOutcome::Denied { reason }
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn audience_matches(aud: Option<&serde_json::Value>, expected: &str) -> bool {
    match aud {
        Some(serde_json::Value::String(s)) => s == expected,
        Some(serde_json::Value::Array(items)) => items.iter().any(|v| v.as_str() == Some(expected)),
        _ => false,
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
        // No HS* (shared-secret) and no "none": asymmetric only, so a leaked
        // verification input can never mint tokens.
        other => anyhow::bail!("unsupported JWT alg '{other}': expected RS256 or ES256"),
    }
}

pub struct OidcAuthProvider {
    /// Registry selection key: `oidc.<alias>`.
    name: String,
    alias: String,
    config: OidcConfig,
    http: reqwest::Client,
    discovery: RwLock<Option<Discovery>>,
    jwks: RwLock<HashMap<String, Jwk>>,
    /// Earliest moment the next unknown-`kid` JWKS refresh may run.
    jwks_refresh_after: Mutex<Option<Instant>>,
}

impl OidcAuthProvider {
    pub fn new(alias: impl Into<String>, config: OidcConfig) -> anyhow::Result<Self> {
        let alias = alias.into();
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .build()?;
        Ok(Self {
            name: format!("oidc.{alias}"),
            alias,
            config,
            http,
            discovery: RwLock::new(None),
            jwks: RwLock::new(HashMap::new()),
            jwks_refresh_after: Mutex::new(None),
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
        // The document must assert the issuer we configured, or we fetched
        // somebody else's metadata (or a spoofed/misrouted endpoint).
        if d.issuer.as_deref() != Some(self.config.issuer.as_str()) {
            anyhow::bail!(
                "discovery document issuer does not match the configured issuer for oidc.{}",
                self.alias
            );
        }
        *self.discovery.write() = Some(d.clone());
        Ok(d)
    }

    /// Refresh the JWKS cache, rate-limited to one fetch per cooldown
    /// window. Returns whether a refresh actually ran.
    async fn refresh_jwks_bounded(&self) -> anyhow::Result<bool> {
        {
            let mut after = self.jwks_refresh_after.lock();
            if let Some(after) = *after
                && Instant::now() < after
            {
                return Ok(false);
            }
            // Claim the slot before the network call so concurrent bad
            // tokens cannot trigger parallel fetch storms.
            *after = Some(Instant::now() + JWKS_REFRESH_COOLDOWN);
        }
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
        Ok(true)
    }

    fn cached_key(&self, kid: &str) -> Option<Jwk> {
        self.jwks.read().get(kid).cloned()
    }

    async fn verify_jwks(&self, token: &str) -> AuthOutcome {
        let Some((header_b64, payload_b64, sig_b64)) = Self::split_jwt(token) else {
            return deny(DenyReason::BadCredential);
        };
        let header: JwtHeader = match URL_SAFE_NO_PAD
            .decode(header_b64)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
        {
            Some(h) => h,
            None => return deny(DenyReason::BadCredential),
        };
        // RFC 9068 typed access tokens, when the deployment demands them.
        if self.config.require_at_jwt
            && !header
                .typ
                .as_deref()
                .is_some_and(|t| t.eq_ignore_ascii_case("at+jwt"))
        {
            return deny(DenyReason::BadCredential);
        }
        let kid = header.kid.clone().unwrap_or_default();
        let key = match self.cached_key(&kid) {
            Some(k) => Some(k),
            None => match self.refresh_jwks_bounded().await {
                Ok(_) => self.cached_key(&kid),
                Err(_) => return deny(DenyReason::Misconfigured),
            },
        };
        let Some(key) = key else {
            return deny(DenyReason::BadCredential);
        };
        let signed_len = header_b64.len() + 1 + payload_b64.len();
        let signed = &token[..signed_len];
        let Ok(sig) = URL_SAFE_NO_PAD.decode(sig_b64) else {
            return deny(DenyReason::BadCredential);
        };
        if verify_signature(&header, &key, signed, &sig).is_err() {
            return deny(DenyReason::BadCredential);
        }
        let Some(payload) = URL_SAFE_NO_PAD.decode(payload_b64).ok() else {
            return deny(DenyReason::BadCredential);
        };
        let (Ok(claims), Ok(raw)) = (
            serde_json::from_slice::<Claims>(&payload),
            serde_json::from_slice::<serde_json::Map<String, serde_json::Value>>(&payload),
        ) else {
            return deny(DenyReason::BadCredential);
        };
        self.claims_to_identity(&claims, raw, VerifiedVia::Jwks)
    }

    async fn verify_introspection(&self, token: &str) -> AuthOutcome {
        let discovery = match self.discovery().await {
            Ok(d) => d,
            Err(_) => return deny(DenyReason::Misconfigured),
        };
        let Some(endpoint) = discovery.introspection_endpoint else {
            return deny(DenyReason::Misconfigured);
        };
        let Some(secret) = self.config.client_secret.as_deref() else {
            return deny(DenyReason::Misconfigured);
        };
        let response = self
            .http
            .post(&endpoint)
            .basic_auth(self.config.effective_client_id(), Some(secret))
            .form(&[("token", token)])
            .send()
            .await;
        let body = match response {
            Ok(resp) if resp.status().is_success() => match resp.bytes().await {
                Ok(b) => b,
                Err(_) => return deny(DenyReason::Misconfigured),
            },
            // Unreachable or refusing authority = cannot verify = deny.
            _ => return deny(DenyReason::Misconfigured),
        };
        let (Ok(claims), Ok(raw)) = (
            serde_json::from_slice::<Claims>(&body),
            serde_json::from_slice::<serde_json::Map<String, serde_json::Value>>(&body),
        ) else {
            return deny(DenyReason::BadCredential);
        };
        if claims.active != Some(true) {
            return deny(DenyReason::BadCredential);
        }
        self.claims_to_identity(&claims, raw, VerifiedVia::Introspection)
    }

    /// Shared claim checks + identity assembly. `via` decides which absent
    /// claims are tolerable: a bare JWT must prove everything itself,
    /// while an authenticated introspection verdict comes from the
    /// configured authority and RFC 7662 leaves most fields optional.
    fn claims_to_identity(
        &self,
        claims: &Claims,
        raw: serde_json::Map<String, serde_json::Value>,
        via: VerifiedVia,
    ) -> AuthOutcome {
        let now = now_unix();

        match (&claims.iss, via) {
            (Some(iss), _) if iss == &self.config.issuer => {}
            (None, VerifiedVia::Introspection) => {}
            _ => return deny(DenyReason::BadCredential),
        }
        match (&claims.aud, via) {
            (Some(aud), _) if audience_matches(Some(aud), &self.config.audience) => {}
            (None, VerifiedVia::Introspection) => {}
            _ => return deny(DenyReason::BadCredential),
        }
        match (claims.exp, via) {
            (Some(exp), _) if exp + CLOCK_LEEWAY_SECS > now => {}
            (None, VerifiedVia::Introspection) => {}
            _ => return deny(DenyReason::TokenExpired),
        }
        if let Some(nbf) = claims.nbf
            && nbf > now + CLOCK_LEEWAY_SECS
        {
            return deny(DenyReason::BadCredential);
        }
        // Token purpose: an ID token is authentication evidence for the
        // browser flow, never an API/RPC bearer.
        if claims.nonce.is_some() {
            return deny(DenyReason::BadCredential);
        }
        if !self.config.allowed_authorized_parties.is_empty()
            && !claims.azp.as_deref().is_some_and(|azp| {
                self.config
                    .allowed_authorized_parties
                    .iter()
                    .any(|allowed| allowed == azp)
            })
        {
            return deny(DenyReason::BadCredential);
        }
        if !self.config.required_acr.is_empty()
            && !claims
                .acr
                .as_deref()
                .is_some_and(|acr| self.config.required_acr.iter().any(|r| r == acr))
        {
            return deny(DenyReason::MfaRequired);
        }
        let mfa_verified = claims
            .amr
            .iter()
            .flatten()
            .any(|m| MFA_AMR_VALUES.contains(&m.as_str()));
        if self.config.require_mfa && !mfa_verified {
            return deny(DenyReason::MfaRequired);
        }

        // Service principals are keyed by the stable verified client
        // identity; humans by `sub`. A caller with neither fails closed.
        let client_identity = claims.client_id.as_deref().or(claims.azp.as_deref());
        let subject = match client_identity {
            Some(cid) if self.config.service_clients.iter().any(|s| s == cid) => {
                IdentitySubject::Service {
                    issuer: self.config.issuer.clone(),
                    client_id: cid.to_owned(),
                }
            }
            _ => match claims.sub.as_deref().filter(|s| !s.trim().is_empty()) {
                Some(sub) => IdentitySubject::Oidc {
                    issuer: self.config.issuer.clone(),
                    subject: sub.to_owned(),
                },
                None => return deny(DenyReason::BadCredential),
            },
        };

        let mut identity = AuthenticatedIdentity::new(subject, AuthMethod::Oidc)
            .with_provider_alias(self.alias.clone())
            .with_claims(raw)
            .with_mfa_verified(mfa_verified);
        match via {
            VerifiedVia::Jwks => {
                // Offline validation cannot see revocation: cap the
                // authentication lifetime.
                let cap = now.saturating_add(self.config.max_auth_lifetime_secs);
                let exp = claims.exp.unwrap_or(cap);
                identity = identity.with_expires_at(exp.min(cap));
            }
            VerifiedVia::Introspection => {
                if let Some(exp) = claims.exp {
                    identity = identity.with_expires_at(exp);
                }
                identity =
                    identity.with_revalidate_by(now.saturating_add(self.config.revalidation_secs));
            }
        }
        AuthOutcome::Verified(identity)
    }
}

#[async_trait]
impl AuthProvider for OidcAuthProvider {
    fn name(&self) -> &str {
        &self.name
    }

    fn method(&self) -> AuthMethod {
        AuthMethod::Oidc
    }

    fn accepts(&self, credential: &Credential) -> bool {
        matches!(credential, Credential::Bearer(_))
    }

    async fn verify(&self, credential: &Credential) -> AuthOutcome {
        let Credential::Bearer(token) = credential else {
            return deny(DenyReason::BadCredential);
        };
        match self.config.validation {
            OidcValidation::Jwks => self.verify_jwks(token).await,
            OidcValidation::Introspection => self.verify_introspection(token).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::ProviderRegistry;
    use super::*;
    use ring::rand::SystemRandom;
    use ring::signature::{ECDSA_P256_SHA256_FIXED_SIGNING, EcdsaKeyPair, KeyPair};
    use std::sync::Arc;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    use zeroclaw_api::grants::{Resource, Verb};
    use zeroclaw_api::principal::ActorKind;

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
                "issuer": issuer,
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
        fn mint_with_header(&self, header: &str, claims: &serde_json::Value) -> String {
            let header = URL_SAFE_NO_PAD.encode(header);
            let payload = URL_SAFE_NO_PAD.encode(claims.to_string());
            let signed = format!("{header}.{payload}");
            let rng = SystemRandom::new();
            let sig = self.key.sign(&rng, signed.as_bytes()).unwrap();
            format!("{signed}.{}", URL_SAFE_NO_PAD.encode(sig.as_ref()))
        }

        fn mint(&self, claims: serde_json::Value) -> String {
            self.mint_with_header(r#"{"alg":"ES256","kid":"test-key"}"#, &claims)
        }

        fn config(&self, validation: OidcValidation) -> OidcConfig {
            OidcConfig {
                issuer: self.issuer.clone(),
                audience: "zeroclaw".into(),
                client_secret: Some("s3cret".into()),
                validation,
                claim_path: "realm_access.roles".into(),
                profile_map: HashMap::from([("ops".to_string(), "operator".to_string())]),
                ..OidcConfig::default()
            }
        }

        fn provider(&self, validation: OidcValidation) -> OidcAuthProvider {
            OidcAuthProvider::new("test", self.config(validation)).unwrap()
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

    fn bearer(token: impl Into<String>) -> Credential {
        Credential::Bearer(token.into())
    }

    #[tokio::test]
    async fn valid_jwt_verifies_into_an_identity_without_grants() {
        let idp = start_idp().await;
        let provider = idp.provider(OidcValidation::Jwks);
        let token = idp.mint(idp.good_claims());
        assert_eq!(provider.name(), "oidc.test");
        assert!(provider.accepts(&bearer(token.clone())));
        let out = provider.verify(&bearer(token)).await;
        let identity = out.identity().expect("verified");
        assert_eq!(
            identity.subject,
            IdentitySubject::Oidc {
                issuer: idp.issuer.clone(),
                subject: "alice".into(),
            }
        );
        assert_eq!(identity.provider_label(), "oidc.test");
        assert!(
            identity.claims.contains_key("realm_access"),
            "verified claims are carried for the resolver's mapping"
        );
        assert!(identity.expires_at.is_some());
        assert!(
            identity.revalidate_by.is_none(),
            "offline validation is bounded by expiry, not revalidation"
        );
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
        let out = provider.verify(&bearer(forged)).await;
        assert!(matches!(
            out,
            AuthOutcome::Denied {
                reason: DenyReason::BadCredential
            }
        ));
    }

    #[tokio::test]
    async fn expired_token_is_denied_beyond_leeway() {
        let idp = start_idp().await;
        let provider = idp.provider(OidcValidation::Jwks);
        let mut claims = idp.good_claims();
        claims["exp"] = serde_json::json!(now_unix() - CLOCK_LEEWAY_SECS - 10);
        let out = provider.verify(&bearer(idp.mint(claims))).await;
        assert!(matches!(
            out,
            AuthOutcome::Denied {
                reason: DenyReason::TokenExpired
            }
        ));
    }

    #[tokio::test]
    async fn not_yet_valid_token_is_denied() {
        let idp = start_idp().await;
        let provider = idp.provider(OidcValidation::Jwks);
        let mut claims = idp.good_claims();
        claims["nbf"] = serde_json::json!(now_unix() + CLOCK_LEEWAY_SECS + 300);
        let out = provider.verify(&bearer(idp.mint(claims))).await;
        assert!(!out.is_allowed());
    }

    #[tokio::test]
    async fn wrong_audience_and_foreign_issuer_are_denied() {
        let idp = start_idp().await;
        let provider = idp.provider(OidcValidation::Jwks);

        let mut claims = idp.good_claims();
        claims["aud"] = serde_json::json!("someone-else");
        assert!(
            !provider
                .verify(&bearer(idp.mint(claims)))
                .await
                .is_allowed()
        );

        let mut claims = idp.good_claims();
        claims["iss"] = serde_json::json!("https://other-idp.example.com");
        assert!(
            !provider
                .verify(&bearer(idp.mint(claims)))
                .await
                .is_allowed()
        );
    }

    #[tokio::test]
    async fn id_token_nonce_marker_is_rejected_despite_valid_signature() {
        // Rev 8 token purpose: an ID token presented as an API bearer is
        // rejected even when signature, issuer, and audience all check out.
        let idp = start_idp().await;
        let provider = idp.provider(OidcValidation::Jwks);
        let mut claims = idp.good_claims();
        claims["nonce"] = serde_json::json!("browser-login-nonce");
        let out = provider.verify(&bearer(idp.mint(claims))).await;
        assert!(matches!(
            out,
            AuthOutcome::Denied {
                reason: DenyReason::BadCredential
            }
        ));
    }

    #[tokio::test]
    async fn require_at_jwt_demands_the_rfc_9068_type() {
        let idp = start_idp().await;
        let mut config = idp.config(OidcValidation::Jwks);
        config.require_at_jwt = true;
        let provider = OidcAuthProvider::new("test", config).unwrap();

        let untyped = idp.mint(idp.good_claims());
        assert!(!provider.verify(&bearer(untyped)).await.is_allowed());

        let typed = idp.mint_with_header(
            r#"{"alg":"ES256","kid":"test-key","typ":"at+jwt"}"#,
            &idp.good_claims(),
        );
        assert!(provider.verify(&bearer(typed)).await.is_allowed());
    }

    #[tokio::test]
    async fn offline_lifetime_is_capped_by_max_auth_lifetime() {
        let idp = start_idp().await;
        let mut config = idp.config(OidcValidation::Jwks);
        config.max_auth_lifetime_secs = 300;
        let provider = OidcAuthProvider::new("test", config).unwrap();
        let mut claims = idp.good_claims();
        claims["exp"] = serde_json::json!(now_unix() + 999_999);
        let out = provider.verify(&bearer(idp.mint(claims))).await;
        let identity = out.identity().expect("verified");
        let expires_at = identity.expires_at.expect("bounded");
        assert!(
            expires_at <= now_unix() + 301,
            "offline authentication must not outlive the configured cap"
        );
    }

    #[tokio::test]
    async fn mfa_and_acr_requirements_fail_closed() {
        let idp = start_idp().await;
        let mut config = idp.config(OidcValidation::Jwks);
        config.require_mfa = true;
        let provider = OidcAuthProvider::new("test", config).unwrap();
        let out = provider.verify(&bearer(idp.mint(idp.good_claims()))).await;
        assert!(matches!(
            out,
            AuthOutcome::Denied {
                reason: DenyReason::MfaRequired
            }
        ));

        let mut claims = idp.good_claims();
        claims["amr"] = serde_json::json!(["otp"]);
        let out = provider.verify(&bearer(idp.mint(claims))).await;
        assert!(out.is_allowed());
        assert!(out.identity().unwrap().mfa_verified);

        let mut config = idp.config(OidcValidation::Jwks);
        config.required_acr = vec!["urn:mace:incommon:iap:silver".into()];
        let provider = OidcAuthProvider::new("test", config).unwrap();
        assert!(
            !provider
                .verify(&bearer(idp.mint(idp.good_claims())))
                .await
                .is_allowed()
        );
        let mut claims = idp.good_claims();
        claims["acr"] = serde_json::json!("urn:mace:incommon:iap:silver");
        assert!(
            provider
                .verify(&bearer(idp.mint(claims)))
                .await
                .is_allowed()
        );
    }

    #[tokio::test]
    async fn azp_allowlist_is_enforced_when_configured() {
        let idp = start_idp().await;
        let mut config = idp.config(OidcValidation::Jwks);
        config.allowed_authorized_parties = vec!["zerocode-cli".into()];
        let provider = OidcAuthProvider::new("test", config).unwrap();

        assert!(
            !provider
                .verify(&bearer(idp.mint(idp.good_claims())))
                .await
                .is_allowed(),
            "a missing azp fails closed when a party allowlist is configured"
        );
        let mut claims = idp.good_claims();
        claims["azp"] = serde_json::json!("zerocode-cli");
        assert!(
            provider
                .verify(&bearer(idp.mint(claims.clone())))
                .await
                .is_allowed()
        );
        claims["azp"] = serde_json::json!("rogue-client");
        assert!(
            !provider
                .verify(&bearer(idp.mint(claims)))
                .await
                .is_allowed()
        );
    }

    #[tokio::test]
    async fn service_clients_resolve_to_service_identities() {
        let idp = start_idp().await;
        let mut config = idp.config(OidcValidation::Jwks);
        config.service_clients = vec!["reporting-batch".into()];
        let provider = OidcAuthProvider::new("test", config).unwrap();

        let mut claims = idp.good_claims();
        claims["client_id"] = serde_json::json!("reporting-batch");
        let out = provider.verify(&bearer(idp.mint(claims))).await;
        let identity = out.identity().expect("verified");
        assert_eq!(
            identity.subject,
            IdentitySubject::Service {
                issuer: idp.issuer.clone(),
                client_id: "reporting-batch".into(),
            }
        );

        // A sub-less token whose client is NOT declared a service fails
        // closed: there is no stable identity to bind.
        let mut claims = idp.good_claims();
        claims.as_object_mut().unwrap().remove("sub");
        claims["client_id"] = serde_json::json!("undeclared-client");
        assert!(
            !provider
                .verify(&bearer(idp.mint(claims)))
                .await
                .is_allowed()
        );
    }

    #[tokio::test]
    async fn unknown_kid_refreshes_jwks_at_most_once_per_cooldown() {
        let idp = start_idp().await;
        let provider = idp.provider(OidcValidation::Jwks);
        // Prime the caches (discovery + jwks fetch #1 via the good token).
        assert!(
            provider
                .verify(&bearer(idp.mint(idp.good_claims())))
                .await
                .is_allowed()
        );
        // Two bad-kid tokens inside the cooldown: at most ONE extra fetch.
        let bad = idp.mint_with_header(
            r#"{"alg":"ES256","kid":"rotated-away"}"#,
            &idp.good_claims(),
        );
        assert!(!provider.verify(&bearer(bad.clone())).await.is_allowed());
        assert!(!provider.verify(&bearer(bad)).await.is_allowed());
        let jwks_fetches = idp
            .server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|r| r.url.path() == "/jwks")
            .count();
        assert!(
            jwks_fetches <= 2,
            "bad tokens must not hammer the JWKS endpoint (saw {jwks_fetches} fetches)"
        );
    }

    #[tokio::test]
    async fn unsupported_algorithms_are_denied() {
        let idp = start_idp().await;
        let provider = idp.provider(OidcValidation::Jwks);
        for header in [
            r#"{"alg":"HS256","kid":"test-key"}"#,
            r#"{"alg":"none","kid":"test-key"}"#,
        ] {
            let token = idp.mint_with_header(header, &idp.good_claims());
            assert!(
                !provider.verify(&bearer(token)).await.is_allowed(),
                "alg from {header} must be rejected"
            );
        }
    }

    #[tokio::test]
    async fn discovery_issuer_mismatch_fails_closed() {
        let server = MockServer::start().await;
        let issuer = server.uri();
        Mock::given(method("GET"))
            .and(path("/.well-known/openid-configuration"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "issuer": "https://evil.example.com",
                "jwks_uri": format!("{issuer}/jwks"),
            })))
            .mount(&server)
            .await;
        let config = OidcConfig {
            issuer: issuer.clone(),
            audience: "zeroclaw".into(),
            claim_path: "groups".into(),
            profile_map: HashMap::from([("ops".to_string(), "operator".to_string())]),
            ..OidcConfig::default()
        };
        let provider = OidcAuthProvider::new("test", config).unwrap();
        // Any token needing a JWKS fetch hits the mismatched discovery.
        let out = provider.verify(&bearer("aaaa.bbbb.cccc")).await;
        assert!(!out.is_allowed());
    }

    #[tokio::test]
    async fn introspection_active_token_verifies_with_revalidation_deadline() {
        let idp = start_idp().await;
        Mock::given(method("POST"))
            .and(path("/introspect"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "active": true,
                "iss": idp.issuer,
                "sub": "bob",
                "aud": "zeroclaw",
                "exp": now_unix() + 600,
                "realm_access": {"roles": ["ops"]},
            })))
            .mount(&idp.server)
            .await;
        let provider = idp.provider(OidcValidation::Introspection);
        let out = provider.verify(&bearer("opaque-token")).await;
        let identity = out.identity().expect("verified via introspection");
        assert_eq!(
            identity.subject,
            IdentitySubject::Oidc {
                issuer: idp.issuer.clone(),
                subject: "bob".into(),
            }
        );
        let deadline = identity.revalidate_by.expect("bounded revalidation");
        assert!(
            deadline <= now_unix() + provider.config.revalidation_secs + 1,
            "revalidation deadline must honor revalidation_secs"
        );
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
        assert!(!provider.verify(&bearer("revoked-token")).await.is_allowed());
    }

    #[tokio::test]
    async fn unreachable_idp_fails_closed() {
        let idp = start_idp().await;
        let provider = idp.provider(OidcValidation::Introspection);
        drop(idp.server);
        let out = provider.verify(&bearer("opaque-token")).await;
        assert!(matches!(
            out,
            AuthOutcome::Denied {
                reason: DenyReason::Misconfigured
            }
        ));
    }

    #[tokio::test]
    async fn introspection_without_client_secret_fails_closed() {
        let idp = start_idp().await;
        let mut config = idp.config(OidcValidation::Introspection);
        config.client_secret = None;
        let provider = OidcAuthProvider::new("test", config).unwrap();
        let out = provider.verify(&bearer("opaque-token")).await;
        assert!(matches!(
            out,
            AuthOutcome::Denied {
                reason: DenyReason::Misconfigured
            }
        ));
    }

    #[tokio::test]
    async fn registry_selection_is_authoritative_for_oidc_denials() {
        // The registry never retries a credential the selected oidc
        // provider denied — even with another bearer provider registered.
        struct TrustAnyBearer;
        #[async_trait]
        impl AuthProvider for TrustAnyBearer {
            fn name(&self) -> &str {
                "native"
            }
            fn method(&self) -> AuthMethod {
                AuthMethod::Native
            }
            fn accepts(&self, credential: &Credential) -> bool {
                matches!(credential, Credential::Bearer(_))
            }
            async fn verify(&self, _credential: &Credential) -> AuthOutcome {
                AuthOutcome::Verified(AuthenticatedIdentity::shared_operator(AuthMethod::Native))
            }
        }

        let idp = start_idp().await;
        let mut config = idp.config(OidcValidation::Jwks);
        config.require_mfa = true;
        let mut registry = ProviderRegistry::new();
        registry
            .register(Arc::new(OidcAuthProvider::new("corp", config).unwrap()))
            .unwrap();
        registry.register(Arc::new(TrustAnyBearer)).unwrap();

        let no_mfa = idp.mint(idp.good_claims());
        let out = registry.resolve_named("oidc.corp", &bearer(no_mfa)).await;
        assert!(
            matches!(
                out,
                AuthOutcome::Denied {
                    reason: DenyReason::MfaRequired
                }
            ),
            "the native provider must never see a credential oidc.corp denied"
        );
    }

    #[tokio::test]
    async fn verified_identity_resolves_through_the_shared_resolver() {
        // End to end across the contract: provider verifies the token into
        // an identity, the shared resolver maps its claims to grants.
        use crate::security::principal_resolver::{OidcMapping, PrincipalResolver, ResolverPolicy};
        use zeroclaw_api::grants::ResolvedGrants;

        let idp = start_idp().await;
        let provider = idp.provider(OidcValidation::Jwks);
        let out = provider.verify(&bearer(idp.mint(idp.good_claims()))).await;
        let identity = out.identity().expect("verified").clone();

        let mut grants = ResolvedGrants::none();
        grants
            .resources
            .insert(Resource::Sessions, [Verb::Read].into());
        let policy = ResolverPolicy {
            profiles: HashMap::from([("operator".to_string(), grants)]),
            oidc: HashMap::from([(
                "test".to_string(),
                OidcMapping {
                    issuer: idp.issuer.clone(),
                    claim_path: "realm_access.roles".into(),
                    profile_map: HashMap::from([("ops".to_string(), "operator".to_string())]),
                    service_profile_map: HashMap::new(),
                },
            )]),
            roster: HashMap::new(),
            roster_conflict: false,
        };
        let resolver = PrincipalResolver::new(policy);
        let resolved = resolver.resolve(&identity).expect("resolves");
        assert_eq!(resolved.principal.actor, ActorKind::Human);
        assert!(resolved.grants.permits(Resource::Sessions, Verb::Read));
        assert!(!resolved.grants.admin);
        assert!(
            resolved.principal.id.as_str().starts_with("oidc:"),
            "issuer-keyed canonical principal"
        );
    }
}
