//! Concrete host capability services granted to WIT plugins.
//!
//! These implement the deny-by-default capability traits from `zeroclaw-plugins`
//! with the agent runtime's real policy: HTTP egress under the same SSRF
//! allowlist as the built-in `http_request` tool (with host-side credential
//! injection), rooted workspace reads, and existence-only secret checks. They
//! live here (not in `zeroclaw-plugins`) because they need config, the SSRF
//! guard, and the secret store — dependencies the sandbox crate must not carry.
//!
//! `tool-invoke` is intentionally NOT wired yet (it stays deny-by-default); the
//! alias-indirection + recursion-guard design is a follow-up.
//!
//! The module is gated at its `mod` declaration in `tools/mod.rs`, so no inner
//! `#![cfg]` is needed here (a second one would be a duplicated-attribute lint).

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use zeroclaw_config::schema::HttpRequestConfig;
use zeroclaw_config::secrets::SecretStore;
use zeroclaw_plugins::{
    CredentialGrant, PluginManifest, PluginPermission, WasmHostError, WasmHostHttp,
    WasmHostSecrets, WasmHostWorkspace, WasmHttpRequest, WasmHttpResponse, WitToolHost,
};
use zeroclaw_tools::helpers::domain_guard;

/// Build the host services a plugin is granted, starting from deny-all and
/// enabling only the capabilities its manifest declares.
pub fn build_wit_host(
    manifest: &PluginManifest,
    http_config: &HttpRequestConfig,
    workspace_dir: &Path,
    secrets: Arc<PluginSecretSource>,
) -> WitToolHost {
    let mut host = WitToolHost::deny_all();
    let perms = &manifest.permissions;

    if perms.contains(&PluginPermission::HttpClient) {
        host = host.with_http(Arc::new(PluginHttp::new(
            http_config,
            manifest.credentials.clone(),
            secrets.clone(),
        )));
    }
    // `WorkspaceRead` (and the deprecated `FileRead` alias) grant rooted reads.
    if perms.iter().any(|p| {
        matches!(
            p,
            PluginPermission::WorkspaceRead | PluginPermission::FileRead
        )
    }) {
        host = host.with_workspace(Arc::new(RootedWorkspace::new(workspace_dir.to_path_buf())));
    }
    if perms.contains(&PluginPermission::SecretExists) {
        host = host.with_secrets(secrets);
    }
    // Clock is always available (no secret surface); `deny_all` already supplies
    // the system clock. `ToolInvoke` is deferred — left at deny-by-default.
    host
}

// ── Secrets ─────────────────────────────────────────────────────────────────

/// Resolves named secrets for plugins. Provides existence checks for the guest
/// (`secret-exists`) and host-only value resolution for credential injection.
/// Values are never returned to WASM.
pub struct PluginSecretSource {
    /// Named secrets from `[http].secrets` (values may be encrypted at rest).
    named: HashMap<String, String>,
    store: SecretStore,
}

impl PluginSecretSource {
    pub fn new(named: HashMap<String, String>, store: SecretStore) -> Self {
        Self { named, store }
    }

    /// Host-only: resolve a secret to its plaintext value for injection. Tries
    /// the named config secrets (decrypting if needed), then the environment.
    fn resolve(&self, name: &str) -> Option<String> {
        if let Some(value) = self.named.get(name) {
            if SecretStore::is_encrypted(value) {
                return self.store.decrypt(value).ok();
            }
            return Some(value.clone());
        }
        std::env::var(name).ok()
    }
}

impl WasmHostSecrets for PluginSecretSource {
    fn exists(&self, name: &str) -> bool {
        self.named.contains_key(name) || std::env::var(name).is_ok()
    }
}

// ── Workspace ───────────────────────────────────────────────────────────────

/// Read-only workspace access rooted at the agent workspace. Rejects absolute
/// paths and `..` traversal, and verifies the resolved path stays under the root.
pub struct RootedWorkspace {
    root: PathBuf,
}

impl RootedWorkspace {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }
}

impl WasmHostWorkspace for RootedWorkspace {
    fn read(&self, path: &str) -> Option<String> {
        let candidate = Path::new(path);
        if candidate.is_absolute()
            || candidate
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            return None;
        }
        let full = self.root.join(candidate);
        // Defense in depth: the canonical path must remain under the root.
        let canonical_root = self.root.canonicalize().ok()?;
        let canonical_full = full.canonicalize().ok()?;
        if !canonical_full.starts_with(&canonical_root) {
            return None;
        }
        std::fs::read_to_string(canonical_full).ok()
    }
}

// ── HTTP egress ─────────────────────────────────────────────────────────────

/// HTTP egress for plugins: the same SSRF allowlist policy as the built-in
/// `http_request` tool, plus host-side credential injection. The guest supplies
/// the request; this host applies the allowlist, injects credentials, and caps
/// the response size.
pub struct PluginHttp {
    allowed_domains: Vec<String>,
    allow_private_hosts: bool,
    allowed_private_hosts: Vec<String>,
    max_response_bytes: usize,
    timeout: Duration,
    credentials: Vec<CredentialGrant>,
    secrets: Arc<PluginSecretSource>,
}

impl PluginHttp {
    pub fn new(
        http_config: &HttpRequestConfig,
        credentials: Vec<CredentialGrant>,
        secrets: Arc<PluginSecretSource>,
    ) -> Self {
        Self {
            allowed_domains: domain_guard::normalize_allowed_domains(
                http_config.allowed_domains.clone(),
                "plugin.http.allowed_domains",
            )
            .unwrap_or_default(),
            allow_private_hosts: http_config.allow_private_hosts,
            allowed_private_hosts: domain_guard::normalize_allowed_domains(
                http_config.allowed_private_hosts.clone(),
                "plugin.http.allowed_private_hosts",
            )
            .unwrap_or_default(),
            max_response_bytes: http_config.max_response_size,
            timeout: Duration::from_secs(http_config.timeout_secs.max(1)),
            credentials,
            secrets,
        }
    }

    /// Enforce the SSRF allowlist for a host, mirroring `http_request`.
    fn check_host(&self, host: &str) -> Result<(), WasmHostError> {
        if domain_guard::is_private_or_local_host(host)
            && !(self.allow_private_hosts
                && domain_guard::host_matches_allowlist(host, &self.allowed_private_hosts))
        {
            return Err(WasmHostError::Denied(format!(
                "host '{host}' is private/local and not in allowed_private_hosts"
            )));
        }
        if !domain_guard::host_matches_allowlist(host, &self.allowed_domains) {
            return Err(WasmHostError::Denied(format!(
                "host '{host}' is not in the plugin HTTP allowlist"
            )));
        }
        Ok(())
    }
}

impl WasmHostHttp for PluginHttp {
    fn request(&self, request: WasmHttpRequest) -> Result<WasmHttpResponse, WasmHostError> {
        let url = reqwest::Url::parse(&request.url)
            .map_err(|e| WasmHostError::Denied(format!("invalid request URL: {e}")))?;
        let host = url
            .host_str()
            .ok_or_else(|| WasmHostError::Denied("request URL has no host".to_string()))?;
        self.check_host(host)?;

        let method = reqwest::Method::from_bytes(request.method.to_uppercase().as_bytes())
            .map_err(|_| {
                WasmHostError::Denied(format!("unsupported method: {}", request.method))
            })?;

        let timeout = request
            .timeout_ms
            .map(|ms| Duration::from_millis(u64::from(ms)))
            .unwrap_or(self.timeout);
        let client = reqwest::blocking::Client::builder()
            .timeout(timeout)
            .build()
            .map_err(|e| WasmHostError::Failed(format!("failed to build HTTP client: {e}")))?;

        let mut builder = client.request(method, url.clone());

        // Guest-supplied headers.
        let guest_headers: HashMap<String, String> = if request.headers_json.trim().is_empty() {
            HashMap::new()
        } else {
            serde_json::from_str(&request.headers_json)
                .map_err(|e| WasmHostError::Denied(format!("invalid headers JSON: {e}")))?
        };
        for (name, value) in &guest_headers {
            builder = builder.header(name, value);
        }
        // Host-injected credentials (the guest never sees these values).
        for grant in &self.credentials {
            if grant.matches_url(&request.url)
                && let Some(secret) = self.secrets.resolve(&grant.secret)
            {
                builder = builder.header(&grant.header, grant.render(&secret));
            }
        }
        if let Some(body) = request.body {
            builder = builder.body(body);
        }

        let response = builder
            .send()
            .map_err(|e| WasmHostError::Failed(format!("HTTP request failed: {e}")))?;

        let status = response.status().as_u16();
        let mut headers = serde_json::Map::new();
        for (name, value) in response.headers() {
            if let Ok(value) = value.to_str() {
                headers.insert(
                    name.as_str().to_string(),
                    serde_json::Value::String(value.to_string()),
                );
            }
        }
        let headers_json = serde_json::Value::Object(headers).to_string();

        let mut body = Vec::new();
        if self.max_response_bytes > 0 {
            response
                .take(self.max_response_bytes as u64)
                .read_to_end(&mut body)
                .map_err(|e| {
                    WasmHostError::FailedAfterRequestSent(format!("failed to read response: {e}"))
                })?;
        } else {
            let mut response = response;
            response.read_to_end(&mut body).map_err(|e| {
                WasmHostError::FailedAfterRequestSent(format!("failed to read response: {e}"))
            })?;
        }

        Ok(WasmHttpResponse {
            status,
            headers_json,
            body,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secrets(named: &[(&str, &str)]) -> Arc<PluginSecretSource> {
        let map = named
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        // `false` = encryption disabled; values are treated as plaintext.
        Arc::new(PluginSecretSource::new(
            map,
            SecretStore::new(Path::new("/tmp/zc-plugin-host-test"), false),
        ))
    }

    fn http(allowed: &[&str], private: bool, creds: Vec<CredentialGrant>) -> PluginHttp {
        let cfg = HttpRequestConfig {
            allowed_domains: allowed.iter().map(|s| s.to_string()).collect(),
            allow_private_hosts: private,
            ..HttpRequestConfig::default()
        };
        PluginHttp::new(&cfg, creds, secrets(&[("API_KEY", "sekret")]))
    }

    #[test]
    fn denies_host_outside_allowlist() {
        let client = http(&["example.com"], false, vec![]);
        let err = client
            .request(WasmHttpRequest {
                method: "GET".into(),
                url: "https://evil.test/".into(),
                headers_json: "{}".into(),
                body: None,
                timeout_ms: None,
            })
            .unwrap_err();
        assert!(matches!(err, WasmHostError::Denied(_)), "got {err:?}");
    }

    #[test]
    fn denies_private_host_by_default() {
        let client = http(&["*"], false, vec![]);
        let err = client
            .request(WasmHttpRequest {
                method: "GET".into(),
                url: "http://127.0.0.1:8080/".into(),
                headers_json: "{}".into(),
                body: None,
                timeout_ms: None,
            })
            .unwrap_err();
        assert!(matches!(err, WasmHostError::Denied(_)), "got {err:?}");
    }

    #[test]
    fn secret_source_exists_but_resolves_host_side_only() {
        let source = secrets(&[("API_KEY", "sekret")]);
        assert!(source.exists("API_KEY"));
        assert!(!source.exists("MISSING_KEY_XYZ"));
        assert_eq!(source.resolve("API_KEY").as_deref(), Some("sekret"));
    }

    #[test]
    fn credential_grant_matches_and_renders() {
        let grant = CredentialGrant {
            secret: "API_KEY".into(),
            header: "Authorization".into(),
            value_template: "Bearer {secret}".into(),
            url_prefix: Some("https://api.example.com/".into()),
        };
        assert!(grant.matches_url("https://api.example.com/v1/x"));
        assert!(!grant.matches_url("https://other.test/"));
        assert_eq!(grant.render("sekret"), "Bearer sekret");
    }

    #[test]
    fn workspace_rejects_traversal_and_absolute() {
        let ws = RootedWorkspace::new(PathBuf::from("/tmp/zc-ws-root"));
        assert!(ws.read("../etc/passwd").is_none());
        assert!(ws.read("/etc/passwd").is_none());
    }
}
