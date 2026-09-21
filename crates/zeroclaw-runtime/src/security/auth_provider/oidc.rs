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
//!   and audience are valid; the bearer profile also requires an RFC 9068
//!   typed access token.
//! - Offline (JWKS) validation cannot see revocation, so the identity's
//!   expiry is capped at the earlier of the token `exp` and
//!   `max_auth_lifetime_secs`. Introspection identities carry a
//!   `revalidate_by` deadline instead; once it passes, the next privileged
//!   operation must re-verify or fail closed.
//! - JWKS refresh is bounded: an unknown `kid` may trigger at most one
//!   fetch per cooldown window, so a stream of bad tokens cannot hammer
//!   the IdP.
//! - Actor class (human vs. service) is declared, never inferred from the
//!   `sub`/`client_id` relationship: `service_clients` and
//!   `interactive_clients` classify by verified `client_id`, and
//!   `actor_claim` classifies a client that issues both kinds of token.
//!   A token with no declaration and no configured claim evidence, or with
//!   evidence contradicting its declaration, is denied.
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
use zeroclaw_config::schema::{OidcActorKind, OidcConfig, OidcValidation};

use super::{AuthProvider, Credential};

/// Allowed skew between our clock and the IdP's when checking `exp`/`nbf`.
const CLOCK_LEEWAY_SECS: u64 = 30;

/// Minimum interval between JWKS refreshes triggered by unknown key ids.
const JWKS_REFRESH_COOLDOWN: Duration = Duration::from_secs(30);

/// A known key is still refreshed periodically, so a same-`kid` rotation or
/// removal cannot remain trusted indefinitely.
const JWKS_CACHE_TTL: Duration = Duration::from_secs(300);

/// Bound all untrusted OIDC metadata and introspection payloads, even when a
/// peer omits Content-Length or uses chunked transfer encoding.
const MAX_OIDC_RESPONSE_BYTES: usize = 1024 * 1024;

/// A JWKS is an untrusted network document; cap its key cardinality before
/// materializing the selection map.
const MAX_JWKS_KEYS: usize = 64;

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
    #[serde(rename = "use", default)]
    key_use: Option<String>,
    #[serde(default)]
    key_ops: Vec<String>,
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
    jti: Option<String>,
    #[serde(default)]
    iat: Option<u64>,
    #[serde(default)]
    nbf: Option<u64>,
    #[serde(default)]
    azp: Option<String>,
    #[serde(default)]
    client_id: Option<String>,
    #[serde(default)]
    token_type: Option<String>,
    /// Issuer-specific purpose markers (Keycloak `typ`, Cognito
    /// `token_use`). Not standardized, so absence proves nothing; a present
    /// marker naming anything but an access token is a denial.
    #[serde(default)]
    typ: Option<String>,
    #[serde(default)]
    token_use: Option<String>,
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
    /// introspection endpoint. The endpoint is the authority, but its
    /// `active` flag alone binds nothing: the response must also carry
    /// `token_type: Bearer`, the configured audience, and a `client_id`
    /// (see [`OidcValidation::Introspection`]). Only `iss` and `exp` stay
    /// optional, since the authenticated endpoint speaks for the issuer.
    Introspection,
}

/// Which actor kind a verified token belongs to. Decided from the
/// operator's declarations and the configured actor claim, never from the
/// `sub`/`client_id` relationship.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ActorClass {
    Service,
    Human,
}

impl ActorClass {
    fn other(self) -> Self {
        match self {
            Self::Service => Self::Human,
            Self::Human => Self::Service,
        }
    }
}

impl From<OidcActorKind> for ActorClass {
    fn from(kind: OidcActorKind) -> Self {
        match kind {
            OidcActorKind::Service => Self::Service,
            OidcActorKind::Human => Self::Human,
        }
    }
}

/// Look up a dotted claim path (`realm_access.roles`) in the verified
/// claim map. A JSON `null` is treated as absent.
fn claim_at_path<'a>(
    claims: &'a serde_json::Map<String, serde_json::Value>,
    path: &str,
) -> Option<&'a serde_json::Value> {
    let mut cursor: Option<&serde_json::Value> = None;
    for segment in path.split('.') {
        cursor = match cursor {
            None => claims.get(segment),
            Some(value) => value.get(segment),
        };
        cursor?;
    }
    cursor.filter(|value| !value.is_null())
}

/// Explicit purpose markers are enforced wherever the issuer sends them.
/// Keycloak stamps `typ` (`Bearer`, `Refresh`, `ID`, `Offline`) and Cognito
/// stamps `token_use` (`access`, `id`) on tokens and introspection responses
/// alike; neither is standardized, so absence proves nothing, but a marker
/// naming anything other than an access token is a denial.
fn purpose_markers_permit_access(claims: &Claims) -> bool {
    let typ_permits = claims
        .typ
        .as_deref()
        .is_none_or(|typ| typ.eq_ignore_ascii_case("Bearer") || is_access_token_type(Some(typ)));
    let token_use_permits = claims
        .token_use
        .as_deref()
        .is_none_or(|token_use| token_use.eq_ignore_ascii_case("access"));
    typ_permits && token_use_permits
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

fn is_loopback_host(host: &str) -> bool {
    matches!(host, "localhost" | "127.0.0.1" | "[::1]" | "::1")
}

/// OIDC discovery endpoints are token-verification roots of trust. Apply the
/// same HTTPS/exact-loopback transport rule as the configured issuer before
/// a request can carry a bearer token or client credentials.
fn validate_discovered_endpoint(endpoint: &str, field: &str) -> anyhow::Result<()> {
    let url = reqwest::Url::parse(endpoint)
        .map_err(|e| anyhow::Error::msg(format!("invalid discovery {field}: {e}")))?;
    if !url.username().is_empty() || url.password().is_some() {
        anyhow::bail!("discovery {field} must not contain userinfo");
    }
    match url.scheme() {
        "https" => Ok(()),
        "http" if is_loopback_host(url.host_str().unwrap_or_default()) => Ok(()),
        "http" => anyhow::bail!(
            "discovery {field} must use https (http is allowed only for an exact loopback host)"
        ),
        other => anyhow::bail!("discovery {field} must be an http(s) URL, got scheme '{other}'"),
    }
}

async fn read_response_limited(mut response: reqwest::Response) -> anyhow::Result<Vec<u8>> {
    if response
        .content_length()
        .is_some_and(|len| len > MAX_OIDC_RESPONSE_BYTES as u64)
    {
        anyhow::bail!("OIDC response exceeds the configured size limit");
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        let new_len = body
            .len()
            .checked_add(chunk.len())
            .ok_or_else(|| anyhow::Error::msg("OIDC response length overflow"))?;
        if new_len > MAX_OIDC_RESPONSE_BYTES {
            anyhow::bail!("OIDC response exceeds the configured size limit");
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn is_verification_key(jwk: &Jwk) -> bool {
    jwk.key_use.as_deref().is_none_or(|use_| use_ == "sig")
        && (jwk.key_ops.is_empty() || jwk.key_ops.iter().any(|op| op == "verify"))
}

fn select_verification_keys(set: JwkSet) -> anyhow::Result<HashMap<String, Jwk>> {
    if set.keys.len() > MAX_JWKS_KEYS {
        anyhow::bail!("issuer JWKS exceeds the configured key count limit");
    }
    let mut next = HashMap::new();
    let mut seen_kids = std::collections::HashSet::new();
    for key in set.keys {
        let kid = key
            .kid
            .as_deref()
            .filter(|kid| !kid.trim().is_empty())
            .ok_or_else(|| anyhow::Error::msg("issuer JWKS contains a missing or empty kid"))?
            .to_owned();
        // Reject duplicate ids before filtering eligibility. Otherwise an
        // attacker can make the result depend on key order by placing an
        // ineligible copy before or after a verification key.
        if !seen_kids.insert(kid.clone()) {
            anyhow::bail!("issuer JWKS contains a duplicate kid");
        }
        if is_verification_key(&key) {
            next.insert(kid, key);
        }
    }
    if next.is_empty() {
        anyhow::bail!("issuer JWKS contains no eligible verification keys");
    }
    Ok(next)
}

fn is_access_token_type(typ: Option<&str>) -> bool {
    typ.is_some_and(|value| {
        value.eq_ignore_ascii_case("at+jwt") || value.eq_ignore_ascii_case("application/at+jwt")
    })
}

fn mfa_evidence_is_sufficient(amr: Option<&Vec<String>>, acr_accepted: bool) -> bool {
    // Raw AMR method labels identify methods, not independent factors. Trust
    // only an accepted configured ACR policy or the IdP aggregate MFA marker.
    acr_accepted || amr.is_some_and(|values| values.iter().any(|value| value == "mfa"))
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
    /// A cached key set becomes stale after this deadline even when its
    /// current token `kid` is known.
    jwks_fresh_until: Mutex<Option<Instant>>,
}

impl OidcAuthProvider {
    pub fn new(alias: impl Into<String>, config: OidcConfig) -> anyhow::Result<Self> {
        let alias = alias.into();
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        Ok(Self {
            name: format!("oidc.{alias}"),
            alias,
            config,
            http,
            discovery: RwLock::new(None),
            jwks: RwLock::new(HashMap::new()),
            jwks_refresh_after: Mutex::new(None),
            jwks_fresh_until: Mutex::new(None),
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
        let response = self.http.get(&url).send().await?.error_for_status()?;
        let body = read_response_limited(response).await?;
        let d: Discovery = serde_json::from_slice(&body)?;
        // The document must assert the issuer we configured, or we fetched
        // somebody else's metadata (or a spoofed/misrouted endpoint).
        if d.issuer.as_deref() != Some(self.config.issuer.as_str()) {
            anyhow::bail!(
                "discovery document issuer does not match the configured issuer for oidc.{}",
                self.alias
            );
        }
        if let Some(uri) = d.jwks_uri.as_deref() {
            validate_discovered_endpoint(uri, "jwks_uri")?;
        }
        if let Some(endpoint) = d.introspection_endpoint.as_deref() {
            validate_discovered_endpoint(endpoint, "introspection_endpoint")?;
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
        let response = self.http.get(&uri).send().await?.error_for_status()?;
        let body = read_response_limited(response).await?;
        let set: JwkSet = serde_json::from_slice(&body)?;
        let next = select_verification_keys(set)?;
        *self.jwks.write() = next;
        *self.jwks_fresh_until.lock() = Some(Instant::now() + JWKS_CACHE_TTL);
        Ok(true)
    }

    fn cached_key(&self, kid: &str) -> Option<Jwk> {
        self.jwks.read().get(kid).cloned()
    }

    fn jwks_is_fresh(&self) -> bool {
        self.jwks_fresh_until
            .lock()
            .is_some_and(|until| Instant::now() < until)
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
        // The bearer profile accepts RFC 9068 typed access tokens only. A
        // nonce-less ID token is not a safe discriminator.
        if !is_access_token_type(header.typ.as_deref()) {
            return deny(DenyReason::BadCredential);
        }
        let Some(kid) = header.kid.as_deref().filter(|kid| !kid.trim().is_empty()) else {
            return deny(DenyReason::BadCredential);
        };
        if !self.jwks_is_fresh() {
            match self.refresh_jwks_bounded().await {
                Ok(true) => {}
                Ok(false) => return deny(DenyReason::Misconfigured),
                Err(_) => return deny(DenyReason::Misconfigured),
            };
        }
        let key = match self.cached_key(kid) {
            Some(key) => Some(key),
            None => match self.refresh_jwks_bounded().await {
                Ok(_) => self.cached_key(kid),
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
            .form(&[("token", token), ("token_type_hint", "access_token")])
            .send()
            .await;
        let body = match response {
            Ok(resp) if resp.status().is_success() => match read_response_limited(resp).await {
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
        // RFC 7662's active flag alone does not bind the result to the API
        // bearer purpose or to this daemon. Require both from the authority.
        if !claims
            .token_type
            .as_deref()
            .is_some_and(|token_type| token_type.eq_ignore_ascii_case("Bearer"))
            || !audience_matches(claims.aud.as_ref(), &self.config.audience)
        {
            return deny(DenyReason::BadCredential);
        }
        self.claims_to_identity(&claims, raw, VerifiedVia::Introspection)
    }

    /// The operator's declaration for a verified client, if any.
    fn declared_actor(&self, client_id: &str) -> Option<ActorClass> {
        if self.config.service_clients.iter().any(|c| c == client_id) {
            Some(ActorClass::Service)
        } else if self
            .config
            .interactive_clients
            .iter()
            .any(|c| c == client_id)
        {
            Some(ActorClass::Human)
        } else {
            None
        }
    }

    /// What the configured `actor_claim` says about this token: presence
    /// proves the configured kind, absence proves the other. `None` when no
    /// claim is configured.
    fn actor_evidence(
        &self,
        raw: &serde_json::Map<String, serde_json::Value>,
    ) -> Option<ActorClass> {
        if self.config.actor_claim.is_empty() {
            return None;
        }
        let marked = ActorClass::from(self.config.actor_claim_marks);
        Some(if claim_at_path(raw, &self.config.actor_claim).is_some() {
            marked
        } else {
            marked.other()
        })
    }

    /// Shared claim checks + identity assembly. `via` decides which absent
    /// claims are tolerable: a bare JWT must prove everything itself,
    /// while an authenticated introspection verdict speaks for the issuer
    /// and RFC 7662 leaves `iss`/`exp` optional. Audience is mandatory on
    /// both paths.
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
        if !audience_matches(claims.aud.as_ref(), &self.config.audience) {
            return deny(DenyReason::BadCredential);
        }
        match (claims.exp, via) {
            (Some(exp), _) if exp.saturating_add(CLOCK_LEEWAY_SECS) > now => {}
            (None, VerifiedVia::Introspection) => {}
            _ => return deny(DenyReason::TokenExpired),
        }
        if let Some(nbf) = claims.nbf
            && nbf > now.saturating_add(CLOCK_LEEWAY_SECS)
        {
            return deny(DenyReason::BadCredential);
        }
        // Token purpose: an ID token is authentication evidence for the
        // browser flow, never an API/RPC bearer; an explicit issuer purpose
        // marker naming a refresh/ID token is denied wherever it appears.
        if claims.nonce.is_some() || !purpose_markers_permit_access(claims) {
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
        let acr_accepted = !self.config.required_acr.is_empty()
            && claims
                .acr
                .as_deref()
                .is_some_and(|acr| self.config.required_acr.iter().any(|r| r == acr));
        let mfa_verified = mfa_evidence_is_sufficient(claims.amr.as_ref(), acr_accepted);
        if self.config.require_mfa && !mfa_verified {
            return deny(DenyReason::MfaRequired);
        }

        // A verified client_id is mandatory (RFC 9068 §2.2; the introspection
        // contract). It keys the operator's actor declarations; the
        // `sub`/`client_id` relationship is never consulted because
        // service-token subjects are provider-specific (`<client>@clients`,
        // a service-account user id, or the client id itself).
        let Some(client_id) = claims
            .client_id
            .as_deref()
            .filter(|client_id| !client_id.trim().is_empty())
        else {
            return deny(DenyReason::BadCredential);
        };
        let actor = match (self.declared_actor(client_id), self.actor_evidence(&raw)) {
            // Evidence contradicting a declaration is ambiguity: deny rather
            // than let either side win.
            (Some(declared), Some(evidence)) if declared != evidence => {
                return deny(DenyReason::BadCredential);
            }
            (Some(declared), _) => declared,
            // A client declared nowhere is classified by the operator's
            // actor claim alone (a shared human+machine client).
            (None, Some(evidence)) => evidence,
            // No declaration and no claim evidence: nothing positively
            // establishes the actor class, so the token authenticates nobody.
            (None, None) => return deny(DenyReason::BadCredential),
        };
        let subject = match actor {
            ActorClass::Service => IdentitySubject::Service {
                issuer: self.config.issuer.clone(),
                client_id: client_id.to_owned(),
            },
            ActorClass::Human => {
                let Some(subject) = claims.sub.as_deref().filter(|sub| !sub.trim().is_empty())
                else {
                    return deny(DenyReason::BadCredential);
                };
                IdentitySubject::Oidc {
                    issuer: self.config.issuer.clone(),
                    subject: subject.to_owned(),
                }
            }
        };

        let mut identity = AuthenticatedIdentity::new(subject, AuthMethod::Oidc)
            .with_provider_alias(self.alias.clone())
            .with_claims(raw)
            .with_mfa_verified(mfa_verified);
        match via {
            VerifiedVia::Jwks => {
                // The RFC 9068 profile requires iat. Cap lifetime from token
                // issuance (not from every verification) so an old valid
                // token cannot be extended indefinitely.
                let Some(iat) = claims.iat else {
                    return deny(DenyReason::BadCredential);
                };
                if claims
                    .jti
                    .as_deref()
                    .is_none_or(|jti| jti.trim().is_empty())
                {
                    return deny(DenyReason::BadCredential);
                }
                if iat > now.saturating_add(CLOCK_LEEWAY_SECS) {
                    return deny(DenyReason::BadCredential);
                }
                let cap = iat.saturating_add(self.config.max_auth_lifetime_secs);
                if cap.saturating_add(CLOCK_LEEWAY_SECS) <= now {
                    return deny(DenyReason::TokenExpired);
                }
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
            self.mint_with_header(
                r#"{"alg":"ES256","kid":"test-key","typ":"application/at+jwt"}"#,
                &claims,
            )
        }

        fn config(&self, validation: OidcValidation) -> OidcConfig {
            OidcConfig {
                issuer: self.issuer.clone(),
                audience: "zeroclaw".into(),
                client_secret: Some("s3cret".into()),
                validation,
                claim_path: "realm_access.roles".into(),
                profile_map: HashMap::from([("ops".to_string(), "operator".to_string())]),
                // The fixture's human tokens come from a declared
                // interactive client; nothing is inferred from `sub`.
                interactive_clients: vec!["zerocode-cli".into()],
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
                "iat": now_unix(),
                "jti": "test-token-id",
                "client_id": "zerocode-cli",
                "scope": "openid profile",
                "realm_access": {"roles": ["ops"]},
            })
        }
    }

    fn bearer(token: impl Into<String>) -> Credential {
        Credential::Bearer(token.into())
    }

    fn public_jwk(key: &EcdsaKeyPair, kid: &str) -> serde_json::Value {
        let public = key.public_key().as_ref();
        serde_json::json!({
            "kid": kid, "kty": "EC", "crv": "P-256", "alg": "ES256",
            "use": "sig", "key_ops": ["verify"],
            "x": URL_SAFE_NO_PAD.encode(&public[1..33]),
            "y": URL_SAFE_NO_PAD.encode(&public[33..65]),
        })
    }

    fn expire_jwks(provider: &OidcAuthProvider) {
        // Advance just the cache deadlines, without wall-clock sleeps or
        // changing the authenticated claims or the cached key material.
        *provider.jwks_fresh_until.lock() = Some(Instant::now() - Duration::from_secs(1));
        *provider.jwks_refresh_after.lock() = None;
    }

    #[derive(Clone, Copy, Debug)]
    enum ResponseFraming {
        Advertised,
        Chunked,
        CloseDelimited,
    }

    // A raw HTTP peer is necessary here: a mock framework may insert
    // Content-Length and accidentally stop exercising the streaming limit.
    async fn serve_bounded_response(
        listener: tokio::net::TcpListener,
        body: String,
        framing: ResponseFraming,
    ) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            let count = socket.read(&mut buf).await.unwrap();
            assert!(count > 0, "request must arrive before the response");
            request.extend_from_slice(&buf[..count]);
            if let Some(end) = request.windows(4).position(|part| part == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&request[..end]);
                let length = headers
                    .lines()
                    .filter_map(|line| line.split_once(':'))
                    .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                    .map_or(0, |(_, value)| value.trim().parse::<usize>().unwrap());
                if request.len() >= end + 4 + length {
                    break;
                }
            }
        }
        let mut response =
            b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\n".to_vec();
        match framing {
            ResponseFraming::Advertised => {
                response.extend_from_slice(
                    format!("Content-Length: {}\r\n\r\n", body.len()).as_bytes(),
                );
                response.extend_from_slice(body.as_bytes());
            }
            ResponseFraming::Chunked => {
                response.extend_from_slice(b"Transfer-Encoding: chunked\r\n\r\n");
                for chunk in body.as_bytes().chunks(8192) {
                    response.extend_from_slice(format!("{:x}\r\n", chunk.len()).as_bytes());
                    response.extend_from_slice(chunk);
                    response.extend_from_slice(b"\r\n");
                }
                response.extend_from_slice(b"0\r\n\r\n");
            }
            ResponseFraming::CloseDelimited => {
                response.extend_from_slice(b"\r\n");
                response.extend_from_slice(body.as_bytes());
            }
        }
        // An oversized response can be rejected while the peer is writing;
        // a reset/broken pipe then means the verifier stopped consuming it.
        let _ = socket.write_all(&response).await;
        String::from_utf8(request).unwrap()
    }

    async fn assert_response_bound(surface: &str, framing: ResponseFraming) {
        // Both fixtures are valid JSON with otherwise valid authentication
        // evidence. Without the byte limit, the oversized case would verify.
        for oversized in [false, true] {
            let idp = start_idp().await;
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let endpoint = format!("http://{}", listener.local_addr().unwrap());
            let mut config = idp.config(if surface == "introspection" {
                OidcValidation::Introspection
            } else {
                OidcValidation::Jwks
            });
            let mut payload = match surface {
                "discovery" => {
                    config.issuer.clone_from(&endpoint);
                    serde_json::json!({
                        "issuer": endpoint,
                        "jwks_uri": format!("{}/jwks", idp.issuer),
                    })
                }
                "jwks" => serde_json::json!({"keys": [public_jwk(&idp.key, "test-key")]}),
                "introspection" => {
                    let mut claims = idp.good_claims();
                    claims["active"] = serde_json::json!(true);
                    claims["token_type"] = serde_json::json!("Bearer");
                    claims
                }
                _ => panic!("unknown test surface"),
            };
            payload["padding"] = serde_json::json!(" ".repeat(if oversized {
                MAX_OIDC_RESPONSE_BYTES
            } else {
                32
            }));
            let body = payload.to_string();
            assert_eq!(body.len() > MAX_OIDC_RESPONSE_BYTES, oversized);
            let peer = ::zeroclaw_spawn::spawn!(async move {
                tokio::time::timeout(
                    Duration::from_secs(5),
                    serve_bounded_response(listener, body, framing),
                )
                .await
                .expect("the verifier must reach the response peer")
            });
            if surface != "discovery" {
                let mut discovery = serde_json::json!({
                    "issuer": idp.issuer,
                    "jwks_uri": format!("{}/jwks", idp.issuer),
                    "introspection_endpoint": format!("{}/introspect", idp.issuer),
                });
                discovery[if surface == "jwks" {
                    "jwks_uri"
                } else {
                    "introspection_endpoint"
                }] = serde_json::json!(endpoint);
                Mock::given(method("GET"))
                    .and(path("/.well-known/openid-configuration"))
                    .respond_with(ResponseTemplate::new(200).set_body_json(discovery))
                    .with_priority(1)
                    .mount(&idp.server)
                    .await;
            }
            let mut claims = idp.good_claims();
            claims["iss"] = serde_json::json!(config.issuer);
            let token = if surface == "introspection" {
                "opaque-token".to_owned()
            } else {
                idp.mint(claims)
            };
            let provider = OidcAuthProvider::new("test", config).unwrap();
            let outcome =
                tokio::time::timeout(Duration::from_secs(5), provider.verify(&bearer(token)))
                    .await
                    .expect("response collection must remain bounded");
            assert_eq!(
                outcome.is_allowed(),
                !oversized,
                "{surface} {framing:?}: {outcome:?}"
            );
            if oversized {
                assert!(matches!(
                    outcome,
                    AuthOutcome::Denied {
                        reason: DenyReason::Misconfigured
                    }
                ));
            }
            let request = peer.await.unwrap();
            assert!(request.starts_with(if surface == "introspection" {
                "POST "
            } else {
                "GET "
            }));
        }
    }

    #[tokio::test]
    async fn advertised_response_bound_reaches_all_oidc_endpoints() {
        for surface in ["discovery", "jwks", "introspection"] {
            assert_response_bound(surface, ResponseFraming::Advertised).await;
        }
    }

    #[tokio::test]
    async fn chunked_response_bound_reaches_all_oidc_endpoints() {
        for surface in ["discovery", "jwks", "introspection"] {
            assert_response_bound(surface, ResponseFraming::Chunked).await;
        }
    }

    #[tokio::test]
    async fn no_length_response_bound_reaches_all_oidc_endpoints() {
        for surface in ["discovery", "jwks", "introspection"] {
            assert_response_bound(surface, ResponseFraming::CloseDelimited).await;
        }
    }

    #[tokio::test]
    async fn redirects_never_deliver_credentials_to_the_target() {
        for surface in ["discovery", "jwks", "introspection"] {
            for status in [302, 307, 308] {
                let idp = start_idp().await;
                let sink = MockServer::start().await;
                Mock::given(path("/capture"))
                    .respond_with(ResponseTemplate::new(200))
                    .expect(0)
                    .mount(&sink)
                    .await;
                let (verb, endpoint) = match surface {
                    "discovery" => ("GET", "/.well-known/openid-configuration"),
                    "jwks" => ("GET", "/jwks"),
                    _ => ("POST", "/introspect"),
                };
                Mock::given(method(verb))
                    .and(path(endpoint))
                    .respond_with(
                        ResponseTemplate::new(status)
                            .insert_header("Location", format!("{}/capture", sink.uri())),
                    )
                    .with_priority(1)
                    .expect(1)
                    .mount(&idp.server)
                    .await;
                let provider = idp.provider(if surface == "introspection" {
                    OidcValidation::Introspection
                } else {
                    OidcValidation::Jwks
                });
                let token = if surface == "introspection" {
                    "opaque-token".to_owned()
                } else {
                    idp.mint(idp.good_claims())
                };
                assert!(!provider.verify(&bearer(token)).await.is_allowed());
                let requests = idp.server.received_requests().await.unwrap();
                let request = requests
                    .iter()
                    .find(|request| request.url.path() == endpoint)
                    .expect("the redirecting endpoint must actually be reached");
                if surface == "introspection" {
                    assert_eq!(
                        request.headers["authorization"].to_str().unwrap(),
                        format!(
                            "Basic {}",
                            base64::engine::general_purpose::STANDARD.encode("zeroclaw:s3cret")
                        )
                    );
                    assert_eq!(
                        request.body,
                        b"token=opaque-token&token_type_hint=access_token"
                    );
                }
                assert!(
                    sink.received_requests().await.unwrap().is_empty(),
                    "{surface} status {status} must not deliver any request to the redirect target"
                );
            }
        }
    }

    #[tokio::test]
    async fn jwks_key_count_limit_is_independent_of_duplicate_ids() {
        for count in [64, 65] {
            let idp = start_idp().await;
            let keys: Vec<_> = (0..count)
                .map(|index| {
                    if index == 0 {
                        return public_jwk(&idp.key, "key-0");
                    }
                    let rng = SystemRandom::new();
                    let pkcs8 =
                        EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng)
                            .unwrap();
                    let key = EcdsaKeyPair::from_pkcs8(
                        &ECDSA_P256_SHA256_FIXED_SIGNING,
                        pkcs8.as_ref(),
                        &rng,
                    )
                    .unwrap();
                    public_jwk(&key, &format!("key-{index}"))
                })
                .collect();
            let body = serde_json::json!({"keys": keys});
            let set: JwkSet = serde_json::from_value(body.clone()).unwrap();
            if count == 65 {
                let error = select_verification_keys(set).unwrap_err();
                assert!(error.to_string().contains("key count limit"), "{error}");
            } else {
                assert_eq!(select_verification_keys(set).unwrap().len(), 64);
            }
            Mock::given(method("GET"))
                .and(path("/jwks"))
                .respond_with(ResponseTemplate::new(200).set_body_json(body))
                .with_priority(1)
                .expect(1)
                .mount(&idp.server)
                .await;
            let provider = idp.provider(OidcValidation::Jwks);
            let token = idp.mint_with_header(
                r#"{"alg":"ES256","kid":"key-0","typ":"at+jwt"}"#,
                &idp.good_claims(),
            );
            assert_eq!(
                provider.verify(&bearer(token)).await.is_allowed(),
                count == 64
            );
            idp.server.verify().await;
        }
    }

    #[tokio::test]
    async fn same_kid_replacement_and_removal_replace_cached_trust() {
        let idp = start_idp().await;
        let replacement = start_idp().await;
        let provider = idp.provider(OidcValidation::Jwks);
        let old_token = idp.mint(idp.good_claims());
        let new_token = replacement.mint(idp.good_claims());
        assert!(
            provider
                .verify(&bearer(old_token.clone()))
                .await
                .is_allowed()
        );
        Mock::given(method("GET"))
            .and(path("/jwks"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "keys": [public_jwk(&replacement.key, "test-key")]
            })))
            .with_priority(1)
            .expect(1)
            .mount(&idp.server)
            .await;
        assert!(
            !provider
                .verify(&bearer(new_token.clone()))
                .await
                .is_allowed(),
            "a fresh cache still trusts the old signing key"
        );
        expire_jwks(&provider);
        assert!(
            provider
                .verify(&bearer(new_token.clone()))
                .await
                .is_allowed()
        );
        assert!(
            !provider.verify(&bearer(old_token)).await.is_allowed(),
            "the replaced key must stop authenticating after refresh"
        );
        idp.server.verify().await;
        idp.server.reset().await;
        // Discovery is already cached; only the next JWKS response changes.
        Mock::given(method("GET"))
            .and(path("/jwks"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "keys": [public_jwk(&replacement.key, "remaining-key")]
            })))
            .with_priority(1)
            .expect(1)
            .mount(&idp.server)
            .await;
        expire_jwks(&provider);
        let outcome = provider.verify(&bearer(new_token)).await;
        assert!(matches!(
            outcome,
            AuthOutcome::Denied {
                reason: DenyReason::BadCredential
            }
        ));
        let remaining = replacement.mint_with_header(
            r#"{"alg":"ES256","kid":"remaining-key","typ":"at+jwt"}"#,
            &idp.good_claims(),
        );
        assert!(
            provider.verify(&bearer(remaining)).await.is_allowed(),
            "denial of the removed key must not be a broken JWKS fixture"
        );
        idp.server.verify().await;
    }

    #[tokio::test]
    async fn introspection_accepts_only_the_configured_acr_assurance() {
        for acr in ["urn:example:assurance:mfa", "urn:example:assurance:single"] {
            let idp = start_idp().await;
            let response = Mock::given(method("POST"))
                .and(path("/introspect"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "active": true, "token_type": "Bearer", "iss": idp.issuer,
                    "sub": "subject-1", "aud": "zeroclaw", "client_id": "zerocode-cli",
                    "amr": ["otp"], "acr": acr,
                })))
                .expect(1)
                .mount_as_scoped(&idp.server)
                .await;
            let mut config = idp.config(OidcValidation::Introspection);
            config.require_mfa = true;
            config.required_acr = vec!["urn:example:assurance:mfa".into()];
            config.require_at_jwt = false;
            config
                .validate("test")
                .expect("opaque introspection does not require JWT typing");
            let provider = OidcAuthProvider::new("test", config).unwrap();
            let outcome = provider.verify(&bearer("opaque-token")).await;
            if acr == "urn:example:assurance:mfa" {
                assert!(outcome.identity().expect("accepted ACR").mfa_verified);
            } else {
                assert!(matches!(
                    outcome,
                    AuthOutcome::Denied {
                        reason: DenyReason::MfaRequired
                    }
                ));
            }
            drop(response);
        }
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
    async fn bearer_profile_demands_an_rfc_9068_access_token_type() {
        let idp = start_idp().await;
        let config = idp.config(OidcValidation::Jwks);
        let provider = OidcAuthProvider::new("test", config).unwrap();

        let untyped =
            idp.mint_with_header(r#"{"alg":"ES256","kid":"test-key"}"#, &idp.good_claims());
        assert!(!provider.verify(&bearer(untyped)).await.is_allowed());

        let typed = idp.mint_with_header(
            r#"{"alg":"ES256","kid":"test-key","typ":"at+jwt"}"#,
            &idp.good_claims(),
        );
        assert!(provider.verify(&bearer(typed)).await.is_allowed());

        let media_typed = idp.mint_with_header(
            r#"{"alg":"ES256","kid":"test-key","typ":"application/at+jwt"}"#,
            &idp.good_claims(),
        );
        assert!(provider.verify(&bearer(media_typed)).await.is_allowed());
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
    async fn offline_lifetime_uses_iat_and_expiry_math_is_overflow_safe() {
        let idp = start_idp().await;
        let mut config = idp.config(OidcValidation::Jwks);
        config.max_auth_lifetime_secs = 300;
        let provider = OidcAuthProvider::new("test", config).unwrap();

        let mut too_old = idp.good_claims();
        too_old["iat"] = serde_json::json!(now_unix() - 331);
        too_old["exp"] = serde_json::json!(u64::MAX);
        assert!(
            !provider
                .verify(&bearer(idp.mint(too_old)))
                .await
                .is_allowed(),
            "an old token cannot restart its offline lifetime on each verification"
        );

        let mut extreme_expiry = idp.good_claims();
        extreme_expiry["exp"] = serde_json::json!(u64::MAX);
        assert!(
            provider
                .verify(&bearer(idp.mint(extreme_expiry)))
                .await
                .is_allowed(),
            "maximal exp must not overflow the leeway comparison"
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

        for factor in ["otp", "hwk"] {
            let mut claims = idp.good_claims();
            claims["amr"] = serde_json::json!([factor]);
            assert!(
                !provider
                    .verify(&bearer(idp.mint(claims)))
                    .await
                    .is_allowed(),
                "{factor}-only evidence is not sufficient MFA"
            );
        }
        let mut claims = idp.good_claims();
        claims["amr"] = serde_json::json!(["otp", "hwk"]);
        let out = provider.verify(&bearer(idp.mint(claims))).await;
        assert!(
            !out.is_allowed(),
            "method labels alone are not factor proof"
        );
        let mut claims = idp.good_claims();
        claims["amr"] = serde_json::json!(["mfa"]);
        assert!(
            provider
                .verify(&bearer(idp.mint(claims)))
                .await
                .is_allowed()
        );

        let mut config = idp.config(OidcValidation::Jwks);
        config.require_mfa = true;
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
        claims["amr"] = serde_json::json!(["otp"]);
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
        config.service_clients = vec!["zerocode-cli".into()];
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
        claims["sub"] = serde_json::json!("provider-service-subject");
        let out = provider.verify(&bearer(idp.mint(claims))).await;
        let identity = out.identity().expect("verified");
        assert_eq!(
            identity.subject,
            IdentitySubject::Service {
                issuer: idp.issuer.clone(),
                client_id: "reporting-batch".into(),
            }
        );

        // A client declared nowhere carries no positive actor signal, so it
        // is denied whatever its `sub` looks like: never a human fallback.
        for sub in ["undeclared-client", "some-distinct-subject"] {
            let mut claims = idp.good_claims();
            claims["client_id"] = serde_json::json!("undeclared-client");
            claims["sub"] = serde_json::json!(sub);
            assert!(
                !provider
                    .verify(&bearer(idp.mint(claims)))
                    .await
                    .is_allowed(),
                "undeclared client with sub {sub:?} must not authenticate"
            );
        }

        let mut claims = idp.good_claims();
        claims["client_id"] = serde_json::json!("reporting-batch");
        claims.as_object_mut().unwrap().remove("sub");
        assert!(
            matches!(
                provider.verify(&bearer(idp.mint(claims))).await.identity().map(|identity| &identity.subject),
                Some(IdentitySubject::Service { client_id, .. }) if client_id == "reporting-batch"
            ),
            "configured services do not rely on a provider-specific subject shape"
        );

        let mut claims = idp.good_claims();
        claims.as_object_mut().unwrap().remove("client_id");
        assert!(
            !provider
                .verify(&bearer(idp.mint(claims)))
                .await
                .is_allowed(),
            "a token without a stable client discriminator is denied"
        );
    }

    /// A resolver policy mirroring `TestIdp::config`: `ops` → `operator` for
    /// humans, `service_client` → `operator` for services.
    fn resolver_policy(
        idp: &TestIdp,
        service_client: &str,
    ) -> crate::security::principal_resolver::ResolverPolicy {
        use crate::security::principal_resolver::{OidcMapping, ResolverPolicy};
        use zeroclaw_api::grants::ResolvedGrants;
        let mut grants = ResolvedGrants::none();
        grants
            .resources
            .insert(Resource::Sessions, [Verb::Read].into());
        ResolverPolicy {
            profiles: HashMap::from([("operator".to_string(), grants)]),
            oidc: HashMap::from([(
                "test".to_string(),
                OidcMapping {
                    issuer: idp.issuer.clone(),
                    claim_path: "realm_access.roles".into(),
                    profile_map: HashMap::from([("ops".to_string(), "operator".to_string())]),
                    service_profile_map: HashMap::from([(
                        service_client.to_string(),
                        "operator".to_string(),
                    )]),
                },
            )]),
            roster: HashMap::new(),
            roster_conflict: false,
        }
    }

    /// The machine-token subject shapes real issuers mint for client
    /// credentials, all with a mapped human role claim attached.
    fn machine_subject_shapes(client_id: &str) -> [(&'static str, String); 3] {
        [
            ("auth0 @clients", format!("{client_id}@clients")),
            (
                "keycloak service-account id",
                "3f1c2d4e-5a6b-4c7d-8e9f-0a1b2c3d4e5f".to_string(),
            ),
            ("okta equal client", client_id.to_string()),
        ]
    }

    #[tokio::test]
    async fn undeclared_machine_tokens_are_denied_on_both_paths() {
        // An undeclared client with a distinct `sub` used to become a human
        // and reach `profile_map`. With no declaration and no actor claim,
        // nothing establishes the actor class, so every shape is denied.
        let idp = start_idp().await;
        let config = idp.config(OidcValidation::Jwks);
        assert!(
            !config
                .service_clients
                .contains(&"reporting-batch".to_string())
                && !config
                    .interactive_clients
                    .contains(&"reporting-batch".to_string())
                && config.actor_claim.is_empty()
        );
        let provider = OidcAuthProvider::new("test", config).unwrap();
        for (shape, sub) in machine_subject_shapes("reporting-batch") {
            let mut claims = idp.good_claims();
            claims["client_id"] = serde_json::json!("reporting-batch");
            claims["sub"] = serde_json::json!(sub);
            let outcome = provider.verify(&bearer(idp.mint(claims))).await;
            assert!(
                matches!(
                    outcome,
                    AuthOutcome::Denied {
                        reason: DenyReason::BadCredential
                    }
                ),
                "{shape}: undeclared machine token must not verify: {outcome:?}"
            );
        }

        for (shape, sub) in machine_subject_shapes("reporting-batch") {
            let idp = start_idp().await;
            Mock::given(method("POST"))
                .and(path("/introspect"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "active": true, "token_type": "Bearer", "iss": idp.issuer,
                    "sub": sub, "aud": "zeroclaw", "client_id": "reporting-batch",
                    "realm_access": {"roles": ["ops"]},
                })))
                .expect(1)
                .mount(&idp.server)
                .await;
            let provider = idp.provider(OidcValidation::Introspection);
            let outcome = provider.verify(&bearer("opaque-token")).await;
            assert!(
                !outcome.is_allowed(),
                "{shape}: undeclared active introspection must not verify: {outcome:?}"
            );
            idp.server.verify().await;
        }
    }

    #[tokio::test]
    async fn shared_client_tokens_classify_by_actor_claim_and_resolve_apart() {
        // One client (`portal`) issues authorization-code user tokens and
        // client-credentials tokens. It is declared in neither list; the
        // operator names the claim the issuer stamps on one kind only.
        use crate::security::principal_resolver::PrincipalResolver;

        let idp = start_idp().await;
        for (marks, claim, marker_value) in [
            (OidcActorKind::Service, "gty", "client-credentials"),
            (OidcActorKind::Human, "uid", "00u1abcd"),
        ] {
            let mut config = idp.config(OidcValidation::Jwks);
            config.interactive_clients.clear();
            config.actor_claim = claim.into();
            config.actor_claim_marks = marks;
            let provider = OidcAuthProvider::new("test", config).unwrap();
            let policy = resolver_policy(&idp, "portal");
            let resolver = PrincipalResolver::new(policy.clone());

            let mut user = idp.good_claims();
            user["client_id"] = serde_json::json!("portal");
            user["sub"] = serde_json::json!("alice");
            let mut machine = idp.good_claims();
            machine["client_id"] = serde_json::json!("portal");
            machine["sub"] = serde_json::json!("portal@clients");
            match marks {
                OidcActorKind::Service => machine[claim] = serde_json::json!(marker_value),
                OidcActorKind::Human => user[claim] = serde_json::json!(marker_value),
            }

            let user_identity = provider
                .verify(&bearer(idp.mint(user)))
                .await
                .identity()
                .expect("genuine user on a shared client verifies")
                .clone();
            assert_eq!(
                user_identity.subject,
                IdentitySubject::Oidc {
                    issuer: idp.issuer.clone(),
                    subject: "alice".into(),
                },
                "{claim}: the user stays human"
            );
            let resolved = resolver.resolve(&user_identity).expect("human resolves");
            assert_eq!(resolved.principal.actor, ActorKind::Human);
            assert!(resolved.grants.permits(Resource::Sessions, Verb::Read));

            let machine_identity = provider
                .verify(&bearer(idp.mint(machine)))
                .await
                .identity()
                .expect("machine token on a shared client verifies")
                .clone();
            assert_eq!(
                machine_identity.subject,
                IdentitySubject::Service {
                    issuer: idp.issuer.clone(),
                    client_id: "portal".into(),
                },
                "{claim}: the machine token is a service despite its mapped role claim"
            );
            let resolved = resolver
                .resolve(&machine_identity)
                .expect("service resolves");
            assert_eq!(resolved.principal.actor, ActorKind::Service);
            assert!(resolved.grants.permits(Resource::Sessions, Verb::Read));

            // Mixed maps keep the two apart: without a service mapping the
            // machine is entitled to nothing, and without a human mapping the
            // user is, even though both carry the mapped `ops` role.
            let mut service_only = policy.clone();
            service_only
                .oidc
                .get_mut("test")
                .unwrap()
                .profile_map
                .clear();
            let service_only = PrincipalResolver::new(service_only);
            assert!(matches!(
                service_only.resolve(&user_identity),
                Err(DenyReason::NotEntitled)
            ));
            assert!(service_only.resolve(&machine_identity).is_ok());
            let mut human_only = policy;
            human_only
                .oidc
                .get_mut("test")
                .unwrap()
                .service_profile_map
                .clear();
            let human_only = PrincipalResolver::new(human_only);
            assert!(human_only.resolve(&user_identity).is_ok());
            assert!(matches!(
                human_only.resolve(&machine_identity),
                Err(DenyReason::NotEntitled)
            ));
        }
    }

    #[tokio::test]
    async fn actor_claim_evidence_contradicting_a_declaration_is_denied() {
        let idp = start_idp().await;
        let mut config = idp.config(OidcValidation::Jwks);
        config.service_clients = vec!["reporting-batch".into()];
        config.actor_claim = "gty".into();
        config.actor_claim_marks = OidcActorKind::Service;
        let provider = OidcAuthProvider::new("test", config).unwrap();

        // Consistent: declared service carrying the service marker, declared
        // interactive client without it.
        let mut machine = idp.good_claims();
        machine["client_id"] = serde_json::json!("reporting-batch");
        machine["gty"] = serde_json::json!("client-credentials");
        assert!(
            provider
                .verify(&bearer(idp.mint(machine)))
                .await
                .is_allowed()
        );
        assert!(
            provider
                .verify(&bearer(idp.mint(idp.good_claims())))
                .await
                .is_allowed()
        );

        // Contradictions: a declared interactive client presenting machine
        // evidence, and a declared service without it.
        let mut human_with_marker = idp.good_claims();
        human_with_marker["gty"] = serde_json::json!("client-credentials");
        assert!(
            !provider
                .verify(&bearer(idp.mint(human_with_marker)))
                .await
                .is_allowed(),
            "an interactive client's token carrying the service marker is ambiguous"
        );
        let mut service_without_marker = idp.good_claims();
        service_without_marker["client_id"] = serde_json::json!("reporting-batch");
        assert!(
            !provider
                .verify(&bearer(idp.mint(service_without_marker)))
                .await
                .is_allowed(),
            "a declared service's token lacking the service marker is ambiguous"
        );

        // A null marker is absence, not presence.
        let mut null_marker = idp.good_claims();
        null_marker["client_id"] = serde_json::json!("reporting-batch");
        null_marker["gty"] = serde_json::Value::Null;
        assert!(
            !provider
                .verify(&bearer(idp.mint(null_marker)))
                .await
                .is_allowed()
        );
    }

    #[tokio::test]
    async fn interactive_clients_need_a_subject_never_a_sub_client_relationship() {
        let idp = start_idp().await;
        let provider = idp.provider(OidcValidation::Jwks);
        // The declared interactive client is the positive human signal; a
        // token without a subject still authenticates nobody.
        let mut claims = idp.good_claims();
        claims.as_object_mut().unwrap().remove("sub");
        assert!(
            !provider
                .verify(&bearer(idp.mint(claims)))
                .await
                .is_allowed()
        );
        let mut claims = idp.good_claims();
        claims["sub"] = serde_json::json!("  ");
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
        // Permit one refresh, then prove a second unknown key cannot fetch
        // again inside the cooldown. Keep the access-token type valid so
        // both requests reach the key lookup instead of the header gate.
        *provider.jwks_refresh_after.lock() = None;
        let bad = idp.mint_with_header(
            r#"{"alg":"ES256","kid":"rotated-away","typ":"at+jwt"}"#,
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
        assert_eq!(jwks_fetches, 2, "one initial fetch and one bounded refresh");
    }

    #[tokio::test]
    async fn stale_known_kid_refreshes_the_jwks_cache() {
        let idp = start_idp().await;
        let provider = idp.provider(OidcValidation::Jwks);
        let token = idp.mint(idp.good_claims());
        assert!(provider.verify(&bearer(token.clone())).await.is_allowed());
        let before = idp
            .server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|request| request.url.path() == "/jwks")
            .count();
        *provider.jwks_fresh_until.lock() = Some(Instant::now() - Duration::from_secs(1));
        *provider.jwks_refresh_after.lock() = None;
        assert!(provider.verify(&bearer(token)).await.is_allowed());
        let after = idp
            .server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|request| request.url.path() == "/jwks")
            .count();
        assert_eq!(after, before + 1, "a stale known kid must refresh");
    }

    #[test]
    fn discovered_endpoints_require_https_or_exact_loopback() {
        assert!(validate_discovered_endpoint("https://idp.example.com/jwks", "jwks_uri").is_ok());
        assert!(validate_discovered_endpoint("http://127.0.0.1/jwks", "jwks_uri").is_ok());
        assert!(validate_discovered_endpoint("http://idp.example.com/jwks", "jwks_uri").is_err());
        assert!(
            validate_discovered_endpoint("https://user:pass@idp.example.com/jwks", "jwks_uri")
                .is_err()
        );
    }

    #[test]
    fn jwk_eligibility_requires_signature_verification_permission() {
        let mut key = Jwk {
            kid: Some("test".into()),
            kty: "EC".into(),
            alg: Some("ES256".into()),
            key_use: Some("sig".into()),
            key_ops: vec!["verify".into()],
            n: None,
            e: None,
            x: None,
            y: None,
            crv: Some("P-256".into()),
        };
        assert!(is_verification_key(&key));
        key.key_ops = vec!["sign".into()];
        assert!(!is_verification_key(&key));
        key.key_ops.clear();
        key.key_use = Some("enc".into());
        assert!(!is_verification_key(&key));
    }

    #[test]
    fn jwks_rejects_missing_or_duplicate_key_ids() {
        let key = |kid: Option<&str>| Jwk {
            kid: kid.map(str::to_owned),
            kty: "EC".into(),
            alg: Some("ES256".into()),
            key_use: Some("sig".into()),
            key_ops: vec!["verify".into()],
            n: None,
            e: None,
            x: None,
            y: None,
            crv: Some("P-256".into()),
        };
        assert!(
            select_verification_keys(JwkSet {
                keys: vec![key(None)]
            })
            .is_err()
        );
        assert!(
            select_verification_keys(JwkSet {
                keys: vec![key(Some("   "))]
            })
            .is_err()
        );
        assert!(
            select_verification_keys(JwkSet {
                keys: vec![key(Some("same")), key(Some("same"))],
            })
            .is_err()
        );
        let mut ineligible = key(Some("same"));
        ineligible.key_use = Some("enc".into());
        for keys in [
            vec![ineligible.clone(), key(Some("same"))],
            vec![key(Some("same")), ineligible],
        ] {
            assert!(
                select_verification_keys(JwkSet { keys }).is_err(),
                "duplicate ids must be rejected regardless of eligibility or order"
            );
        }
        assert!(
            select_verification_keys(JwkSet {
                keys: vec![key(Some(""))],
            })
            .is_err()
        );
    }

    #[tokio::test]
    async fn unsupported_algorithms_are_denied() {
        let idp = start_idp().await;
        let provider = idp.provider(OidcValidation::Jwks);
        for header in [
            r#"{"alg":"HS256","kid":"test-key","typ":"at+jwt"}"#,
            r#"{"alg":"none","kid":"test-key","typ":"at+jwt"}"#,
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
        // Call discovery directly: an invalid JWT header would otherwise be
        // rejected before the claimed discovery branch is exercised.
        assert!(provider.discovery().await.is_err());
    }

    #[tokio::test]
    async fn introspection_active_token_verifies_with_revalidation_deadline() {
        let idp = start_idp().await;
        Mock::given(method("POST"))
            .and(path("/introspect"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "active": true,
                "token_type": "Bearer",
                "iss": idp.issuer,
                "sub": "bob",
                "aud": "zeroclaw",
                "exp": now_unix() + 600,
                "client_id": "zerocode-cli",
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
    async fn introspection_binds_active_tokens_to_bearer_purpose_and_audience() {
        for (token_type, audience, allowed) in [
            (Some("Bearer"), Some("zeroclaw"), true),
            (Some("MAC"), Some("zeroclaw"), false),
            (None, Some("zeroclaw"), false),
            (Some("Bearer"), Some("other-resource"), false),
            // An active refresh credential reported with its own token type,
            // and an active response that omits the audience entirely.
            (Some("refresh_token"), Some("zeroclaw"), false),
            (Some("Bearer"), None, false),
        ] {
            let idp = start_idp().await;
            let mut response = serde_json::json!({
                "active": true,
                "iss": idp.issuer,
                "sub": "bob",
                "client_id": "zerocode-cli",
            });
            if let Some(token_type) = token_type {
                response["token_type"] = serde_json::json!(token_type);
            }
            if let Some(audience) = audience {
                response["aud"] = serde_json::json!(audience);
            }
            Mock::given(method("POST"))
                .and(path("/introspect"))
                .respond_with(ResponseTemplate::new(200).set_body_json(response))
                .expect(1)
                .mount(&idp.server)
                .await;
            let provider = idp.provider(OidcValidation::Introspection);
            assert_eq!(
                provider.verify(&bearer("opaque-token")).await.is_allowed(),
                allowed,
                "token_type={token_type:?}, audience={audience:?}"
            );
            let request = idp
                .server
                .received_requests()
                .await
                .unwrap()
                .into_iter()
                .find(|request| request.url.path() == "/introspect")
                .expect("introspection request");
            assert_eq!(
                request.body,
                b"token=opaque-token&token_type_hint=access_token"
            );
            idp.server.verify().await;
        }
    }

    #[tokio::test]
    async fn explicit_purpose_markers_deny_non_access_credentials() {
        // A generic `Bearer` token type cannot tell an access token from a
        // refresh token; where the issuer stamps an explicit marker
        // (Keycloak `typ`, Cognito `token_use`), it is enforced on both
        // paths, and an access-token marker still verifies.
        let rows: [(&str, &str, bool); 7] = [
            ("typ", "Bearer", true),
            ("typ", "at+jwt", true),
            ("typ", "Refresh", false),
            ("typ", "ID", false),
            ("typ", "Offline", false),
            ("token_use", "access", true),
            ("token_use", "id", false),
        ];
        for (marker, value, allowed) in rows {
            let idp = start_idp().await;
            Mock::given(method("POST"))
                .and(path("/introspect"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "active": true, "token_type": "Bearer", "iss": idp.issuer,
                    "sub": "bob", "aud": "zeroclaw", "client_id": "zerocode-cli",
                    marker: value,
                })))
                .expect(1)
                .mount(&idp.server)
                .await;
            let provider = idp.provider(OidcValidation::Introspection);
            assert_eq!(
                provider.verify(&bearer("opaque-token")).await.is_allowed(),
                allowed,
                "introspection {marker}={value}"
            );
            idp.server.verify().await;

            let provider = idp.provider(OidcValidation::Jwks);
            let mut claims = idp.good_claims();
            claims[marker] = serde_json::json!(value);
            assert_eq!(
                provider
                    .verify(&bearer(idp.mint(claims)))
                    .await
                    .is_allowed(),
                allowed,
                "jwks payload {marker}={value}"
            );
        }
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
    async fn introspection_mfa_requires_aggregate_or_acr_assurance() {
        let idp = start_idp().await;
        Mock::given(method("POST"))
            .and(path("/introspect"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "active": true, "token_type": "Bearer", "iss": idp.issuer,
                "sub": "bob", "aud": "zeroclaw", "amr": ["otp"],
                "client_id": "zerocode-cli"
            })))
            .mount(&idp.server)
            .await;
        let mut config = idp.config(OidcValidation::Introspection);
        config.require_mfa = true;
        let provider = OidcAuthProvider::new("test", config).unwrap();
        assert!(!provider.verify(&bearer("opaque-token")).await.is_allowed());

        let idp = start_idp().await;
        Mock::given(method("POST"))
            .and(path("/introspect"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "active": true, "token_type": "Bearer", "iss": idp.issuer,
                "sub": "bob", "aud": "zeroclaw", "amr": ["mfa"],
                "client_id": "zerocode-cli"
            })))
            .mount(&idp.server)
            .await;
        let mut config = idp.config(OidcValidation::Introspection);
        config.require_mfa = true;
        let provider = OidcAuthProvider::new("test", config).unwrap();
        assert!(provider.verify(&bearer("opaque-token")).await.is_allowed());
    }

    #[tokio::test]
    async fn unreachable_idp_fails_closed() {
        let idp = start_idp().await;
        let provider = idp.provider(OidcValidation::Introspection);
        assert!(provider.discovery().await.is_ok(), "warm discovery first");
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
                    service_profile_map: HashMap::from([(
                        "reporting-batch".to_string(),
                        "operator".to_string(),
                    )]),
                },
            )]),
            roster: HashMap::new(),
            roster_conflict: false,
        };
        let resolver = PrincipalResolver::new(policy.clone());
        let resolved = resolver.resolve(&identity).expect("resolves");
        assert_eq!(resolved.principal.actor, ActorKind::Human);
        assert!(resolved.grants.permits(Resource::Sessions, Verb::Read));
        assert!(!resolved.grants.admin);
        assert!(
            resolved.principal.id.as_str().starts_with("oidc:"),
            "issuer-keyed canonical principal"
        );

        let mut config = idp.config(OidcValidation::Jwks);
        config.service_clients = vec!["reporting-batch".into()];
        let provider = OidcAuthProvider::new("test", config).unwrap();
        let mut claims = idp.good_claims();
        claims["client_id"] = serde_json::json!("reporting-batch");
        claims["sub"] = serde_json::json!("provider-specific-service-subject");
        let service_identity = provider
            .verify(&bearer(idp.mint(claims)))
            .await
            .identity()
            .expect("configured service verifies")
            .clone();
        let service = resolver
            .resolve(&service_identity)
            .expect("configured service resolves through service map");
        assert_eq!(service.principal.actor, ActorKind::Service);
        assert!(service.grants.permits(Resource::Sessions, Verb::Read));

        let mut human_only_policy = policy;
        human_only_policy
            .oidc
            .get_mut("test")
            .expect("test mapping")
            .service_profile_map
            .clear();
        assert!(matches!(
            PrincipalResolver::new(human_only_policy).resolve(&service_identity),
            Err(DenyReason::NotEntitled)
        ));
    }
}
