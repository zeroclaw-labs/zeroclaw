use serde::{Deserialize, Serialize};

/// Resolved Microsoft 365 configuration with all secrets decrypted and defaults applied.
#[derive(Clone, Serialize, Deserialize)]
pub struct Microsoft365ResolvedConfig {
    pub tenant_id: String,
    pub client_id: String,
    pub client_secret: Option<String>,
    pub auth_flow: String,
    pub scopes: Vec<String>,
    pub token_cache_encrypted: bool,
    pub user_id: String,
}

impl std::fmt::Debug for Microsoft365ResolvedConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Microsoft365ResolvedConfig")
            .field("tenant_id", &self.tenant_id)
            .field("client_id", &self.client_id)
            .field("client_secret", &self.client_secret.as_ref().map(|_| "***"))
            .field("auth_flow", &self.auth_flow)
            .field("scopes", &self.scopes)
            .field("token_cache_encrypted", &self.token_cache_encrypted)
            .field("user_id", &self.user_id)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolved_config_serialization_roundtrip() {
        let config = Microsoft365ResolvedConfig {
            tenant_id: "test-tenant".into(),
            client_id: "test-client".into(),
            client_secret: Some("secret".into()),
            auth_flow: "client_credentials".into(),
            scopes: vec!["https://graph.microsoft.com/.default".into()],
            token_cache_encrypted: false,
            user_id: "me".into(),
        };

        let json = serde_json::to_string(&config).unwrap();
        let parsed: Microsoft365ResolvedConfig = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.tenant_id, "test-tenant");
        assert_eq!(parsed.client_id, "test-client");
        assert_eq!(parsed.client_secret.as_deref(), Some("secret"));
        assert_eq!(parsed.auth_flow, "client_credentials");
        assert_eq!(parsed.scopes.len(), 1);
        assert_eq!(parsed.user_id, "me");
    }
}
