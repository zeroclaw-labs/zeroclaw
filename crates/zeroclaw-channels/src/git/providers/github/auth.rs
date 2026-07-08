//! Authenticates as the GitHub App: private-key loading, RS256 JWT
//! minting, and the installation-token cache.
//!
//! This is the only module that touches key material; keep it that way.
//! No HTTP here: the token-exchange *request* lives in `api`, and the
//! provider wires the two together.

use jsonwebtoken::{Algorithm, EncodingKey, Header};
use serde::Serialize;

use super::payloads::CachedToken;
use crate::git::types::GitChannelError;

/// App JWTs are valid for at most 10 minutes; stay under that and
/// backdate `iat` to absorb clock drift, per GitHub's documented
/// recommendation.
const JWT_BACKDATE_SECS: i64 = 60;
const JWT_LIFETIME_SECS: i64 = 540;

#[derive(Serialize)]
struct Claims {
    iat: i64,
    exp: i64,
    iss: String,
}

pub struct AppAuth {
    app_id: u64,
    /// Inline RS256 private key PEM.
    private_key_pem: Option<String>,
    /// Parsed encoding key, loaded lazily on first use.
    key: parking_lot::Mutex<Option<EncodingKey>>,
    /// Cached installation token (one installation per channel alias).
    token: parking_lot::Mutex<Option<CachedToken>>,
}

impl std::fmt::Debug for AppAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppAuth")
            .field("app_id", &self.app_id)
            .field(
                "private_key_pem",
                &self.private_key_pem.as_ref().map(|_| "***"),
            )
            .field("key", &"<redacted>")
            .field("token", &"<redacted>")
            .finish()
    }
}

impl AppAuth {
    pub fn new(app_id: u64, private_key_pem: Option<String>) -> Self {
        Self {
            app_id,
            private_key_pem: private_key_pem.filter(|p| !p.trim().is_empty()),
            key: parking_lot::Mutex::new(None),
            token: parking_lot::Mutex::new(None),
        }
    }

    /// Mint a short-lived RS256 app JWT (`iss` = app id).
    pub fn mint_jwt(&self) -> Result<String, GitChannelError> {
        let now = chrono::Utc::now().timestamp();
        self.mint_jwt_at(now)
    }

    fn mint_jwt_at(&self, now: i64) -> Result<String, GitChannelError> {
        let key = self.encoding_key()?;
        let claims = Claims {
            iat: now - JWT_BACKDATE_SECS,
            exp: now + JWT_LIFETIME_SECS,
            iss: self.app_id.to_string(),
        };
        Ok(jsonwebtoken::encode(
            &Header::new(Algorithm::RS256),
            &claims,
            &key,
        )?)
    }

    /// Return the cached installation token when still fresh.
    pub fn cached_token(&self) -> Option<String> {
        let guard = self.token.lock();
        guard
            .as_ref()
            .filter(|t| t.is_fresh(chrono::Utc::now()))
            .map(|t| t.token.clone())
    }

    /// Store a freshly exchanged installation token.
    pub fn store_token(&self, token: CachedToken) {
        *self.token.lock() = Some(token);
    }

    fn encoding_key(&self) -> Result<EncodingKey, GitChannelError> {
        if let Some(key) = self.key.lock().as_ref() {
            return Ok(key.clone());
        }
        let key = self.load_key()?;
        *self.key.lock() = Some(key.clone());
        Ok(key)
    }

    fn load_key(&self) -> Result<EncodingKey, GitChannelError> {
        let pem = self
            .private_key_pem
            .as_ref()
            .ok_or(GitChannelError::MissingPrivateKey)?;
        Ok(EncodingKey::from_rsa_pem(pem.as_bytes())?)
    }
}

/// Throwaway 2048-bit RSA key generated for unit tests only, never
/// registered with any real GitHub App. Shared with the channel-level
/// mock-server tests.
#[cfg(test)]
pub(crate) const TEST_KEY_PEM: &str = concat!(
    "-----BEGIN ",
    "PRIVATE KEY-----\n",
    "MIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSjAgEAAoIBAQDtUmF5Tgna21XC
smLfNbwitOFNSyfZ5vbUZQ/XDxmZJ8QZN7APspmBDWTY6QhryeqXk8Ujb/002g72
MPZ+yholAYUbkBVoSgfO1QJGObjPLc5WrA5ESABhXLrm220uhqwbsTZ+/NaoiKK1
bJHxMSoI9kD2EvJV6m3YS6Lwc0PxF3eZHjuzkS5tjF5d0jAORF7Txw+4JWlazuqw
l5HXkrqB5HjJoDWN3s/AII3rA4vcd3ld5DKYfIULehsRmfIfna+/Vl8jJ1eT2ATM
MjLcA6onPXi1ta9T6O3tQmruYxUX5XtsneEgi7UNP8lMkcBGHQKaHKmdBehoScL5
ljlVxGqHAgMBAAECggEACvGsRfLorBm4Ydd0jR2tQSVdWuTqlRrDVWupAR5Us20p
JQCP+mI6+RUBfDgma0PdW9j7xXTPTcQyT/1yCzwcBsOEEav9+2aLA9syYzVUdACF
NTLkrmwVX4R8i8VLkeDOq3hAoG/dy6C6Ueqaev3Qh8Pv12ky7l/L26aE1fVeXn/D
XMxvyhXTqs56OhWcKUmeNSc19mw2FnJRFg8nHmVCjjl6BKG38O4fYREqesqp1LFP
LNzqz08V8zpizhn/GeE9dptpPgH83sQ6O0WeJORa3o/OnFRzrZ5XpVrp3mn4cNSB
fufE/DgnoC9IVcYTGx5GoNPWTr9PTn4G96OBw+FaAQKBgQD67z1kImppQ2IRvvlo
xQaP+OKJ/d19V9KeTphn6DmX32tcfchKVhuWhRIzCTd5MXrUy5wM1LS3bdWwamgn
Ga1nIAbQhOUh+xBm5hTWT1p/MtCcO7LeBb2kg3xlFXu8GBW4edbjQ37ALzYMr8iY
itfTx0qfU9f3MIAlce72gbFQBQKBgQDyHMtLuG5W4WZozuXz9B8ONMlg4ic9Gqs/
Bh4fc2ttLo4Y16Y2vUXB4gMjDhYSa9muEA2FaCM3bxT/UVQ2qqc6jiUantAyOE9w
KZdLGDHsECH1DGyuMYlpBlGvEkD4KOEu7q33M23pKwF8VbE2Y1w5veffmd7lzw2r
qDJcZ7AyGwKBgCO7NU6w594dTjWgr/sPMyQFGJz1nThf7QnFv0Xsd2b81VjSQFb1
c/A2+qRxx4hmV0s9wvbAwwrrhOYeAL6wlVR95vqCMe5oxakhUg5CNmyuW64jghDD
WIG4h1oNeRULiOw/zS2HSuEq19NupG20N49cbW/KjJISQe0TECfhx9HRAoGABYuN
SG2v8UN2Uf4zHBRCRdQFrLdhSK/8rhPYysWc90IytPTzdJt/JoKjqcDf1oor0SXC
+YQ6EkH0DCjzsdDUxa2Nwf9TK2NIxnvdYDXspshzzqX7Mz4lNIeVhVn4rPZaufVz
fI7r/IQko5Fe3q0F5rinv+JJTaAhYwYWKTGiwnMCgYEAr1DXj4oAS0PVmPj5kIpD
A/I4TGY08u1TXTfJpvUA+Zcg1xK5TCWC2yWzBD0L6MJVZrvaHdLYGRtiKqdhmRVv
zhFn/+KgI+cQ5Um9hE2PmBb2As5ko5TP7H8XTHtKkTwwpTH8+yIEI0mNOLfCZc/Z
fjoU4xl9y5oLMVZZ+YPF2qw=
",
    "-----END ",
    "PRIVATE KEY-----\n",
);

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;

    fn auth_with_test_key() -> AppAuth {
        AppAuth::new(12345, Some(TEST_KEY_PEM.to_string()))
    }

    fn decode_payload(jwt: &str) -> serde_json::Value {
        let payload = jwt.split('.').nth(1).unwrap();
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(payload)
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[test]
    fn mint_jwt_signs_rs256_with_app_id_issuer() {
        let auth = auth_with_test_key();
        let jwt = auth.mint_jwt_at(1_000_000).unwrap();

        let header = jsonwebtoken::decode_header(&jwt).unwrap();
        assert_eq!(header.alg, Algorithm::RS256);

        let claims = decode_payload(&jwt);
        assert_eq!(claims["iss"], "12345");
        assert_eq!(claims["iat"], 1_000_000 - JWT_BACKDATE_SECS);
        assert_eq!(claims["exp"], 1_000_000 + JWT_LIFETIME_SECS);
    }

    #[test]
    fn mint_jwt_fails_when_key_unset() {
        let auth = AppAuth::new(1, None);
        let err = auth.mint_jwt().unwrap_err();
        assert!(matches!(err, GitChannelError::MissingPrivateKey));
    }

    #[test]
    fn blank_inline_pem_is_treated_as_unset() {
        let auth = AppAuth::new(9, Some("   ".into()));
        let err = auth.mint_jwt().unwrap_err();
        assert!(matches!(err, GitChannelError::MissingPrivateKey));
    }

    #[test]
    fn mint_jwt_fails_on_garbage_key() {
        let auth = AppAuth::new(1, Some("not a pem".into()));
        let err = auth.mint_jwt().unwrap_err();
        assert!(matches!(err, GitChannelError::Jwt(_)));
    }

    #[test]
    fn debug_redacts_inline_private_key() {
        let auth = AppAuth::new(
            7,
            Some(
                "-----BEGIN RSA PRIVATE KEY-----\nSUPERSECRETPEM\n-----END RSA PRIVATE KEY-----"
                    .into(),
            ),
        );
        let out = format!("{auth:?}");
        assert!(
            !out.contains("SUPERSECRETPEM"),
            "Debug must not print the raw private key PEM"
        );
        assert!(out.contains("***"), "Debug must mask the private key");
    }

    #[test]
    fn token_cache_returns_only_fresh_tokens() {
        let auth = auth_with_test_key();
        assert!(auth.cached_token().is_none());

        auth.store_token(CachedToken {
            token: "fresh".into(),
            expires_at: chrono::Utc::now() + chrono::Duration::seconds(3600),
        });
        assert_eq!(auth.cached_token().as_deref(), Some("fresh"));

        auth.store_token(CachedToken {
            token: "stale".into(),
            expires_at: chrono::Utc::now() + chrono::Duration::seconds(10),
        });
        assert!(auth.cached_token().is_none());
    }
}
