use async_trait::async_trait;
use futures_util::StreamExt;
use powersync::error::PowerSyncError;
use powersync::{BackendConnector, PowerSyncCredentials, PowerSyncDatabase};
use serde::{Deserialize, Serialize};

/// Credentials response from Django sync token endpoint.
#[derive(Debug, Deserialize)]
struct TokenResponse {
    token: String,
    endpoint: String,
    #[allow(dead_code)]
    expires_at: String,
}

/// CRUD upload request sent to Django.
#[derive(Debug, Serialize)]
struct UploadBatch {
    operations: Vec<CrudOp>,
}

/// Single CRUD operation for upload.
#[derive(Debug, Serialize)]
struct CrudOp {
    table: String,
    op: String,
    id: String,
    data: Option<serde_json::Map<String, serde_json::Value>>,
}

/// Configuration for the createOS backend connector.
#[derive(Debug, Clone)]
pub struct ConnectorConfig {
    /// API base URL (e.g., "https://api.lightwave-media.ltd")
    pub api_base: String,
    /// Tenant schema name (e.g., "lightwave_media")
    pub tenant: String,
    /// Device ID for this Augusta instance
    pub device_id: String,
    /// API key or session token for authentication
    pub api_key: Option<String>,
}

/// Backend connector implementing PowerSync's sync protocol.
///
/// Handles:
/// - `fetch_credentials()`: gets JWT from Django `/v1/{tenant}/sync/token/`
/// - `upload_data()`: pushes local CRUD ops to Django `/v1/{tenant}/sync/upload/`
pub struct CreateOsConnector {
    config: ConnectorConfig,
    http: reqwest::Client,
    db: PowerSyncDatabase,
}

impl CreateOsConnector {
    pub fn new(config: ConnectorConfig, db: PowerSyncDatabase) -> Self {
        Self {
            config,
            http: reqwest::Client::new(),
            db,
        }
    }

    fn token_url(&self) -> String {
        format!(
            "{}/v1/{}/sync/token/",
            self.config.api_base, self.config.tenant
        )
    }

    fn upload_url(&self) -> String {
        format!(
            "{}/v1/{}/sync/upload/",
            self.config.api_base, self.config.tenant
        )
    }

    fn auth_headers(&self) -> reqwest::header::HeaderMap {
        let mut headers = reqwest::header::HeaderMap::new();
        if let Some(ref key) = self.config.api_key {
            if let Ok(val) = reqwest::header::HeaderValue::from_str(&format!("Bearer {key}")) {
                headers.insert(reqwest::header::AUTHORIZATION, val);
            }
        }
        headers
    }
}

#[async_trait]
impl BackendConnector for CreateOsConnector {
    async fn fetch_credentials(&self) -> Result<PowerSyncCredentials, PowerSyncError> {
        let resp = self
            .http
            .post(&self.token_url())
            .headers(self.auth_headers())
            .json(&serde_json::json!({
                "device_id": self.config.device_id,
            }))
            .send()
            .await
            .map_err(|e| {
                PowerSyncError::from(std::io::Error::new(
                    std::io::ErrorKind::ConnectionRefused,
                    format!("Token request failed: {e}"),
                ))
            })?;

        if !resp.status().is_success() {
            return Err(PowerSyncError::from(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!("Token endpoint returned {}", resp.status()),
            )));
        }

        let token_resp: TokenResponse = resp.json().await.map_err(|e| {
            PowerSyncError::from(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("Token parse failed: {e}"),
            ))
        })?;

        Ok(PowerSyncCredentials {
            endpoint: token_resp.endpoint,
            token: token_resp.token,
        })
    }

    async fn upload_data(&self) -> Result<(), PowerSyncError> {
        // Process all pending CRUD transactions
        let mut stream = self.db.crud_transactions();

        while let Some(tx_result) = stream.next().await {
            let tx = tx_result?;

            let mut ops = Vec::new();
            for entry in &tx.crud {
                let op = match entry.update_type {
                    powersync::UpdateType::Put => "PUT",
                    powersync::UpdateType::Patch => "PATCH",
                    powersync::UpdateType::Delete => "DELETE",
                };
                ops.push(CrudOp {
                    table: entry.table.clone(),
                    op: op.to_string(),
                    id: entry.id.clone(),
                    data: entry.data.clone(),
                });
            }

            let batch = UploadBatch { operations: ops };

            let resp = self
                .http
                .post(&self.upload_url())
                .headers(self.auth_headers())
                .json(&batch)
                .send()
                .await
                .map_err(|e| {
                    PowerSyncError::from(std::io::Error::new(
                        std::io::ErrorKind::ConnectionRefused,
                        format!("Upload request failed: {e}"),
                    ))
                })?;

            if !resp.status().is_success() {
                tracing::warn!(
                    "Upload batch failed with status {}: skipping transaction",
                    resp.status()
                );
                continue;
            }

            tx.complete().await?;
        }

        Ok(())
    }
}
