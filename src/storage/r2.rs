//! Cloudflare R2 (S3-compatible) pre-signed URL generation.
//!
//! Generates AWS Signature V4 pre-signed URLs for R2 without requiring
//! a full AWS SDK. Uses `hmac`, `sha2`, and `hex` crates already in the
//! dependency tree.
//!
//! **Use case**: Image PDF upload flow — client uploads directly to R2,
//! Railway server then downloads from R2 to call Upstage with operator key.
//! Operator API keys NEVER leave the server.
//!
//! **Env vars** (set on Railway):
//! - `R2_ACCOUNT_ID` — Cloudflare account ID
//! - `R2_ACCESS_KEY_ID` — R2 API token access key
//! - `R2_SECRET_ACCESS_KEY` — R2 API token secret key
//! - `R2_BUCKET_NAME` — Bucket name for document uploads
//! - `R2_PUBLIC_URL` — Optional public/custom domain URL for downloads

use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

type HmacSha256 = Hmac<Sha256>;

/// R2 storage configuration loaded from environment variables.
#[derive(Debug, Clone)]
pub struct R2Config {
    pub account_id: String,
    pub access_key_id: String,
    pub secret_access_key: String,
    pub bucket_name: String,
    /// Optional public/custom domain for read access.
    pub public_url: Option<String>,
}

impl R2Config {
    /// Load configuration from environment variables.
    /// Returns `None` if required vars are missing.
    pub fn from_env() -> Option<Self> {
        let account_id = std::env::var("R2_ACCOUNT_ID").ok()?;
        let access_key_id = std::env::var("R2_ACCESS_KEY_ID").ok()?;
        let secret_access_key = std::env::var("R2_SECRET_ACCESS_KEY").ok()?;
        let bucket_name =
            std::env::var("R2_BUCKET_NAME").unwrap_or_else(|_| "moa-documents".to_string());
        let public_url = std::env::var("R2_PUBLIC_URL").ok();

        Some(Self {
            account_id,
            access_key_id,
            secret_access_key,
            bucket_name,
            public_url,
        })
    }

    /// The virtual-hosted-style host for this bucket.
    fn host(&self) -> String {
        format!(
            "{}.{}.r2.cloudflarestorage.com",
            self.bucket_name, self.account_id
        )
    }

    /// Generate a pre-signed PUT URL for uploading a file to R2.
    ///
    /// The URL is valid for `expires_secs` (default 900 = 15 minutes).
    /// The client uploads directly using this URL with a PUT request.
    pub fn presigned_put_url(
        &self,
        object_key: &str,
        content_type: &str,
        expires_secs: u64,
    ) -> String {
        self.presigned_url("PUT", object_key, Some(content_type), expires_secs)
    }

    /// Generate a pre-signed GET URL for downloading a file from R2.
    ///
    /// If a public URL is configured, returns that instead.
    pub fn presigned_get_url(&self, object_key: &str, expires_secs: u64) -> String {
        if let Some(ref public_url) = self.public_url {
            return format!("{}/{}", public_url.trim_end_matches('/'), object_key);
        }
        self.presigned_url("GET", object_key, None, expires_secs)
    }

    /// Download an object from R2 using internal server-side access.
    ///
    /// Returns the raw bytes. Used by Railway to fetch uploaded PDFs
    /// before sending to Upstage.
    pub async fn download_object(&self, object_key: &str) -> anyhow::Result<Vec<u8>> {
        let url = self.presigned_get_url(object_key, 300);
        let client = reqwest::Client::new();
        let response = client
            .get(&url)
            .timeout(std::time::Duration::from_secs(120))
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            anyhow::bail!("R2 download failed (HTTP {status}) for key '{object_key}'");
        }

        Ok(response.bytes().await?.to_vec())
    }

    /// Delete an object from R2 (cleanup after processing).
    pub async fn delete_object(&self, object_key: &str) -> anyhow::Result<()> {
        let url = self.presigned_url("DELETE", object_key, None, 300);
        let client = reqwest::Client::new();
        let response = client
            .delete(&url)
            .timeout(std::time::Duration::from_secs(30))
            .send()
            .await?;

        if !response.status().is_success() && response.status().as_u16() != 404 {
            let status = response.status();
            tracing::warn!("R2 delete failed (HTTP {status}) for key '{object_key}'");
        }

        Ok(())
    }

    /// Generate an AWS Signature V4 pre-signed URL for R2.
    ///
    /// R2 is S3-compatible, so we use the standard S3v4 signing process.
    fn presigned_url(
        &self,
        method: &str,
        object_key: &str,
        content_type: Option<&str>,
        expires_secs: u64,
    ) -> String {
        let now = chrono::Utc::now();
        let date_stamp = now.format("%Y%m%d").to_string();
        let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();
        let region = "auto"; // R2 uses "auto" region
        let service = "s3";

        let credential_scope = format!("{date_stamp}/{region}/{service}/aws4_request");
        let credential = format!("{}/{credential_scope}", self.access_key_id);

        let host = self.host();

        // Canonical query string (sorted)
        let mut query_params = BTreeMap::new();
        query_params.insert("X-Amz-Algorithm", "AWS4-HMAC-SHA256".to_string());
        query_params.insert("X-Amz-Credential", credential.clone());
        query_params.insert("X-Amz-Date", amz_date.clone());
        query_params.insert("X-Amz-Expires", expires_secs.to_string());
        query_params.insert("X-Amz-SignedHeaders", "host".to_string());

        let canonical_querystring: String = query_params
            .iter()
            .map(|(k, v)| format!("{}={}", uri_encode(k), uri_encode(v)))
            .collect::<Vec<_>>()
            .join("&");

        // Canonical headers
        let canonical_headers = format!("host:{host}\n");
        let signed_headers = "host";

        // For pre-signed URLs, payload hash is UNSIGNED-PAYLOAD
        let payload_hash = "UNSIGNED-PAYLOAD";

        // Canonical request
        let canonical_request = format!(
            "{method}\n/{object_key}\n{canonical_querystring}\n{canonical_headers}\n{signed_headers}\n{payload_hash}"
        );

        // String to sign
        let canonical_request_hash = hex::encode(Sha256::digest(canonical_request.as_bytes()));
        let string_to_sign = format!(
            "AWS4-HMAC-SHA256\n{amz_date}\n{credential_scope}\n{canonical_request_hash}"
        );

        // Signing key
        let signing_key =
            derive_signing_key(&self.secret_access_key, &date_stamp, region, service);

        // Signature
        let signature = hex::encode(hmac_sha256(&signing_key, string_to_sign.as_bytes()));

        // Construct final URL (virtual-hosted style: bucket in hostname)
        let _ = content_type; // content_type is set by the client in the PUT header
        format!(
            "https://{host}/{object_key}?{canonical_querystring}&X-Amz-Signature={signature}"
        )
    }
}

/// Generate a unique object key for a document upload.
///
/// Format: `documents/{user_id}/{uuid}/{filename}`
pub fn generate_object_key(user_id: &str, filename: &str) -> String {
    let uuid = uuid::Uuid::new_v4();
    let safe_filename = filename
        .replace('/', "_")
        .replace('\\', "_")
        .replace("..", "_");
    format!("documents/{user_id}/{uuid}/{safe_filename}")
}

// ── AWS Signature V4 helpers ────────────────────────────────────

fn hmac_sha256(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut mac =
        HmacSha256::new_from_slice(key).expect("HMAC can take key of any size");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

fn derive_signing_key(secret: &str, date: &str, region: &str, service: &str) -> Vec<u8> {
    let k_date = hmac_sha256(format!("AWS4{secret}").as_bytes(), date.as_bytes());
    let k_region = hmac_sha256(&k_date, region.as_bytes());
    let k_service = hmac_sha256(&k_region, service.as_bytes());
    hmac_sha256(&k_service, b"aws4_request")
}

/// URI-encode a string per AWS S3v4 requirements.
fn uri_encode(s: &str) -> String {
    let mut result = String::new();
    for byte in s.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                result.push(byte as char);
            }
            _ => {
                result.push_str(&format!("%{:02X}", byte));
            }
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> R2Config {
        R2Config {
            account_id: "test_account_id".to_string(),
            access_key_id: "test_access_key".to_string(),
            secret_access_key: "test_secret_key".to_string(),
            bucket_name: "test-bucket".to_string(),
            public_url: None,
        }
    }

    #[test]
    fn presigned_put_url_contains_required_params() {
        let config = test_config();
        let url = config.presigned_put_url("docs/test.pdf", "application/pdf", 900);

        assert!(url.contains("X-Amz-Algorithm=AWS4-HMAC-SHA256"));
        assert!(url.contains("X-Amz-Credential="));
        assert!(url.contains("X-Amz-Expires=900"));
        assert!(url.contains("X-Amz-Signature="));
        assert!(url.contains("docs/test.pdf"));
        assert!(url.contains("r2.cloudflarestorage.com"));
    }

    #[test]
    fn presigned_get_url_uses_public_url_when_configured() {
        let mut config = test_config();
        config.public_url = Some("https://cdn.example.com".to_string());

        let url = config.presigned_get_url("docs/test.pdf", 900);
        assert_eq!(url, "https://cdn.example.com/docs/test.pdf");
    }

    #[test]
    fn presigned_get_url_generates_signed_url_without_public() {
        let config = test_config();
        let url = config.presigned_get_url("docs/test.pdf", 900);

        assert!(url.contains("X-Amz-Signature="));
        assert!(url.contains("docs/test.pdf"));
    }

    #[test]
    fn generate_object_key_is_safe() {
        let key = generate_object_key("user_123", "../../etc/passwd");
        assert!(!key.contains(".."));
        assert!(key.starts_with("documents/user_123/"));
    }

    #[test]
    fn uri_encode_handles_special_chars() {
        assert_eq!(uri_encode("hello world"), "hello%20world");
        assert_eq!(uri_encode("a/b+c"), "a%2Fb%2Bc");
        assert_eq!(uri_encode("test-key_1.pdf"), "test-key_1.pdf");
    }

    #[test]
    fn signing_key_derivation_is_deterministic() {
        let key1 = derive_signing_key("secret", "20240101", "auto", "s3");
        let key2 = derive_signing_key("secret", "20240101", "auto", "s3");
        assert_eq!(key1, key2);
    }
}
