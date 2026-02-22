use chrono::Utc;
use jsonwebtoken::{decode, encode, Algorithm, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::Mutex;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct TenantRole {
    pub tenant_id: String,
    pub role: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Claims {
    pub sub: String,
    pub email: String,
    pub tenant_roles: Vec<TenantRole>,
    pub iat: i64,
    pub exp: i64,
}

pub struct JwtService {
    encoding_key: EncodingKey,
    decoding_key: DecodingKey,
    /// In-memory revocation set. Holds raw Bearer token strings.
    /// Entries are evicted on process restart; sufficient for session logout.
    revoked: Mutex<HashSet<String>>,
}

impl JwtService {
    pub fn new(secret: &str) -> Self {
        Self {
            encoding_key: EncodingKey::from_secret(secret.as_bytes()),
            decoding_key: DecodingKey::from_secret(secret.as_bytes()),
            revoked: Mutex::new(HashSet::new()),
        }
    }

    /// Revoke a token so subsequent middleware checks reject it.
    pub fn revoke(&self, token: &str) {
        if let Ok(mut set) = self.revoked.lock() {
            set.insert(token.to_string());
        }
    }

    /// Returns true if the token has been explicitly revoked.
    pub fn is_revoked(&self, token: &str) -> bool {
        self.revoked
            .lock()
            .map(|set| set.contains(token))
            .unwrap_or(false)
    }

    /// Issue a JWT for the given user. Lifetime: 24 hours.
    pub fn issue(
        &self,
        user_id: &str,
        email: &str,
        tenant_roles: Vec<TenantRole>,
    ) -> anyhow::Result<String> {
        let now = Utc::now().timestamp();
        let claims = Claims {
            sub: user_id.to_string(),
            email: email.to_string(),
            tenant_roles,
            iat: now,
            exp: now + 86_400, // 24h
        };
        let token = encode(&Header::default(), &claims, &self.encoding_key)
            .map_err(|e| anyhow::anyhow!("jwt encode error: {e}"))?;
        Ok(token)
    }

    /// Verify a JWT and return the decoded claims.
    pub fn verify(&self, token: &str) -> anyhow::Result<Claims> {
        let mut validation = Validation::new(Algorithm::HS256);
        validation.validate_exp = true;
        let data = decode::<Claims>(token, &self.decoding_key, &validation)
            .map_err(|e| anyhow::anyhow!("jwt verify error: {e}"))?;
        Ok(data.claims)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_service() -> JwtService {
        JwtService::new("test-secret-zeroclaw")
    }

    #[test]
    fn test_issue_verify_roundtrip() {
        let svc = make_service();
        let roles = vec![TenantRole {
            tenant_id: "t1".into(),
            role: "admin".into(),
        }];
        let token = svc.issue("user-1", "user@example.com", roles).unwrap();
        let claims = svc.verify(&token).unwrap();
        assert_eq!(claims.sub, "user-1");
        assert_eq!(claims.email, "user@example.com");
    }

    #[test]
    fn test_expired_token_fails() {
        let svc = make_service();
        // Manually craft claims that expired in the past
        let past = Utc::now().timestamp() - 7200; // 2h ago
        let claims = Claims {
            sub: "user-2".into(),
            email: "expired@example.com".into(),
            tenant_roles: vec![],
            iat: past - 86_400,
            exp: past, // already expired
        };
        let token = encode(&Header::default(), &claims, &svc.encoding_key).unwrap();
        let result = svc.verify(&token);
        assert!(result.is_err(), "expired token must be rejected");
    }

    #[test]
    fn test_invalid_signature_fails() {
        let svc = make_service();
        let token = svc.issue("user-3", "user3@example.com", vec![]).unwrap();
        // Verify using a different secret
        let other = JwtService::new("different-secret");
        let result = other.verify(&token);
        assert!(result.is_err(), "wrong-secret verification must fail");
    }

    #[test]
    fn test_claims_contain_tenant_roles() {
        let svc = make_service();
        let roles = vec![
            TenantRole {
                tenant_id: "tenant-a".into(),
                role: "member".into(),
            },
            TenantRole {
                tenant_id: "tenant-b".into(),
                role: "admin".into(),
            },
        ];
        let token = svc
            .issue("user-4", "u4@example.com", roles.clone())
            .unwrap();
        let claims = svc.verify(&token).unwrap();
        assert_eq!(claims.tenant_roles.len(), 2);
        assert_eq!(claims.tenant_roles[0].tenant_id, "tenant-a");
        assert_eq!(claims.tenant_roles[1].role, "admin");
    }
}
