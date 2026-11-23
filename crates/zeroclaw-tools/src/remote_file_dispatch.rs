//! Connect validated read requests to the WebDAV adapter. The caller
//! supplies a live operator catalog entry and the existing policy services.

use crate::remote_file_webdav::{
    WebDavDirectoryListing, WebDavError, WebDavRead, WebDavResourceKind,
};
use reqwest::{Url, header::HeaderValue};
use std::net::SocketAddr;
use std::time::Duration;
use zeroclaw_config::remote_files::{
    BackendAvailability, Decision, RemoteFilesRequest, RemoteIdentity, Right, Rights, WebDavCatalog,
};
use zeroclaw_config::schema::{ProxyConfig, ProxyScope};
use zeroclaw_infra::net_guard::{
    Nat64Prefix, validate_resolved_ips_are_public, validate_resolved_ips_exclude_metadata,
};

/// Deployment egress policy for a registered WebDAV resource. Private
/// network access is an operator decision made for the resolved resource.
pub struct WebDavNetworkPolicy<'a> {
    pub proxy: &'a ProxyConfig,
    pub nat64_prefixes: &'a [Nat64Prefix],
    pub allow_private_endpoint: bool,
}

impl WebDavNetworkPolicy<'_> {
    async fn resolve(&self, root: &Url) -> Result<Vec<SocketAddr>, ReadDispatchError> {
        self.resolve_with(root, async |host, port| {
            tokio::net::lookup_host((host, port))
                .await
                .map(|addresses| addresses.collect())
                .map_err(|_| ReadDispatchError::NetworkDenied)
        })
        .await
    }

    async fn resolve_with(
        &self,
        root: &Url,
        lookup: impl AsyncFnOnce(&str, u16) -> Result<Vec<SocketAddr>, ReadDispatchError>,
    ) -> Result<Vec<SocketAddr>, ReadDispatchError> {
        let (host, port) = crate::remote_file_webdav::validate_webdav_root(root)?;
        if (self.proxy.enabled && self.proxy.scope == ProxyScope::Environment)
            || (self.proxy.has_any_proxy_url()
                && self.proxy.should_apply_to_service("tool.remote_files"))
        {
            return Err(ReadDispatchError::NetworkDenied);
        }

        let addresses = if let Ok(ip) = host.parse() {
            vec![SocketAddr::new(ip, port)]
        } else {
            tokio::time::timeout(Duration::from_secs(5), lookup(&host, port))
                .await
                .map_err(|_| ReadDispatchError::NetworkDenied)??
        };
        if addresses.len() > 32 || addresses.iter().any(|address| address.port() != port) {
            return Err(ReadDispatchError::NetworkDenied);
        }

        let ips: Vec<_> = addresses.iter().map(SocketAddr::ip).collect();
        let validate = if self.allow_private_endpoint {
            validate_resolved_ips_exclude_metadata
        } else {
            validate_resolved_ips_are_public
        };
        validate(&host, &ips, self.nat64_prefixes).map_err(|_| ReadDispatchError::NetworkDenied)?;
        Ok(addresses)
    }
}

pub struct WebDavReadGrant<'a> {
    pub root: &'a Url,
    pub identity: RemoteIdentity<'a>,
    pub rights: &'a Rights,
    pub availability: &'a BackendAvailability,
}

#[derive(Debug, thiserror::Error)]
pub enum ReadDispatchError {
    #[error("remote_files_invalid_request")]
    InvalidRequest,
    #[error("remote_files_unsupported_operation")]
    UnsupportedOperation,
    #[error("remote_files_resource_denied")]
    Denied,
    #[error("remote_files_approval_required")]
    ApprovalRequired,
    #[error("remote_files_network_policy_denied")]
    NetworkDenied,
    #[error("remote_files_credential_unavailable")]
    CredentialUnavailable,
    #[error(transparent)]
    Adapter(#[from] WebDavError),
}

#[derive(Debug, Eq, PartialEq)]
pub enum WebDavReadResult {
    Directory(WebDavDirectoryListing),
    Info {
        kind: WebDavResourceKind,
        size: Option<u64>,
    },
}

/// Resolve the requested alias using the caller's current authorized catalog.
/// No network policy lookup or vault resolution runs for a denied request.
/// Pending approval returns a hold until the runtime supplies a real approval
/// boundary. Catalog lookup itself must be side-effect-free. The credential
/// resolver must recheck the live grant after network resolution.
pub async fn dispatch_webdav_read<'a>(
    request: &RemoteFilesRequest,
    catalog: impl FnOnce(&str) -> Option<WebDavReadGrant<'a>>,
    verify_egress: impl AsyncFnOnce(&Url) -> Result<Vec<SocketAddr>, ReadDispatchError>,
    resolve_authorization: impl AsyncFnOnce(
        &Url,
        RemoteIdentity<'a>,
    ) -> Result<HeaderValue, ReadDispatchError>,
) -> Result<WebDavReadResult, ReadDispatchError> {
    if !request.validate() {
        return Err(ReadDispatchError::InvalidRequest);
    }
    let (resource, operation) = match request {
        RemoteFilesRequest::ListDirectory { resource, .. } => (resource, Right::ListDirectory),
        RemoteFilesRequest::GetFileInfo { resource, .. } => (resource, Right::GetFileInfo),
        _ => return Err(ReadDispatchError::UnsupportedOperation),
    };
    let grant = catalog(resource).ok_or(ReadDispatchError::Denied)?;
    match grant.rights.check(&[operation]) {
        Decision::Denied => return Err(ReadDispatchError::Denied),
        Decision::RequireHumanApproval => return Err(ReadDispatchError::ApprovalRequired),
        Decision::Allowed => {}
    }
    if !matches!(grant.availability, BackendAvailability::Available(operations) if operations.contains(&operation))
    {
        return Err(ReadDispatchError::UnsupportedOperation);
    }
    if !matches!(
        grant.identity,
        RemoteIdentity::Vault {
            totp_alias: None,
            ..
        }
    ) {
        return Err(ReadDispatchError::CredentialUnavailable);
    }

    crate::remote_file_webdav::validate_webdav_root(grant.root)?;
    let addresses = verify_egress(grant.root).await?;
    let adapter = WebDavRead::from_verified_addresses(grant.root.clone(), &addresses)?;
    let authorization = resolve_authorization(grant.root, grant.identity).await?;
    match request {
        RemoteFilesRequest::ListDirectory { path, .. } => Ok(WebDavReadResult::Directory(
            adapter
                .list_directory(path.as_deref(), Some(authorization), 256)
                .await?,
        )),
        RemoteFilesRequest::GetFileInfo { path, .. } => {
            let kind = adapter
                .resource_kind(path, Some(authorization.clone()))
                .await?;
            let size = match kind {
                WebDavResourceKind::File => adapter.file_size(path, Some(authorization)).await?,
                WebDavResourceKind::Collection => None,
            };
            Ok(WebDavReadResult::Info { kind, size })
        }
        _ => Err(ReadDispatchError::UnsupportedOperation),
    }
}

/// Use operator-configured endpoint metadata with the runtime's current
/// resource rights. The caller must check the canonical alias, exact root,
/// and credential identity against the live grant, so edits cannot retarget it.
pub async fn dispatch_catalog_webdav_read<'a>(
    request: &RemoteFilesRequest,
    catalog: &'a WebDavCatalog,
    live_rights: impl FnOnce(&str, &Url, RemoteIdentity<'a>) -> Option<&'a Rights>,
    availability: &'a BackendAvailability,
    network: &WebDavNetworkPolicy<'_>,
    resolve_authorization: impl AsyncFnOnce(
        &Url,
        RemoteIdentity<'a>,
    ) -> Result<HeaderValue, ReadDispatchError>,
) -> Result<WebDavReadResult, ReadDispatchError> {
    dispatch_webdav_read(
        request,
        |alias| {
            let resource = catalog.resource(alias)?;
            let rights = live_rights(
                &alias.to_ascii_lowercase(),
                resource.root(),
                resource.identity(),
            )?;
            Some(WebDavReadGrant {
                root: resource.root(),
                identity: resource.identity(),
                rights,
                availability,
            })
        },
        async |url| network.resolve(url).await,
        resolve_authorization,
    )
    .await
}

/// Resolve a WebDAV credential using the same encrypted records as the
/// existing managed HTTP and browser consumers. The authorization closure
/// must check a dedicated Remote Files grant, not an HTTP or browser grant.
pub(crate) fn resolve_local_webdav_header(
    config_path: &std::path::Path,
    alias: &str,
    authorized: impl Fn() -> bool,
) -> Result<HeaderValue, ReadDispatchError> {
    use base64::Engine;
    use zeroclaw_config::schema::{ScopedApiKeyHeader, ScopedCredentialSource, ScopedHttpAuth};

    if !authorized() {
        return Err(ReadDispatchError::Denied);
    }
    let (credential, revision) =
        crate::scoped_credentials::load_scoped_credential_snapshot(Some(config_path), alias)
            .map_err(|_| ReadDispatchError::CredentialUnavailable)?;
    if !matches!(credential.source, ScopedCredentialSource::Local) {
        return Err(ReadDispatchError::CredentialUnavailable);
    }

    let value = match credential.auth {
        ScopedHttpAuth::ApiKey {
            value,
            header: ScopedApiKeyHeader::Bearer,
        } if !value.is_empty() => {
            format!("Bearer {value}")
        }
        ScopedHttpAuth::ApiKey {
            value,
            header: ScopedApiKeyHeader::Token,
        } if !value.is_empty() => {
            format!("Token {value}")
        }
        ScopedHttpAuth::UserPassword { username, password }
            if !username.is_empty() && !username.contains(':') && !password.is_empty() =>
        {
            let value =
                base64::engine::general_purpose::STANDARD.encode(format!("{username}:{password}"));
            format!("Basic {value}")
        }
        _ => return Err(ReadDispatchError::CredentialUnavailable),
    };

    let mut header =
        HeaderValue::from_str(&value).map_err(|_| ReadDispatchError::CredentialUnavailable)?;
    header.set_sensitive(true);
    let (_, current_revision) =
        crate::scoped_credentials::load_scoped_credential_snapshot(Some(config_path), alias)
            .map_err(|_| ReadDispatchError::CredentialUnavailable)?;
    if current_revision != revision || !authorized() {
        return Err(ReadDispatchError::Denied);
    }
    Ok(header)
}

/// Read using the shared encrypted store and a live Remote Files grant for
/// the exact resource binding. This intermediate entry point stays gated
/// by platform capability and holds requests requiring human approval.
pub async fn dispatch_vault_webdav_read<'a>(
    request: &RemoteFilesRequest,
    catalog: &'a WebDavCatalog,
    config_path: &std::path::Path,
    live_rights: impl Fn(&str, &Url, RemoteIdentity<'a>) -> Option<&'a Rights>,
    availability: &'a BackendAvailability,
    network: &WebDavNetworkPolicy<'_>,
) -> Result<WebDavReadResult, ReadDispatchError> {
    let (alias, right) = match request {
        RemoteFilesRequest::ListDirectory { resource, .. } => (resource, Right::ListDirectory),
        RemoteFilesRequest::GetFileInfo { resource, .. } => (resource, Right::GetFileInfo),
        _ => return Err(ReadDispatchError::UnsupportedOperation),
    };
    let canonical_alias = alias.to_ascii_lowercase();

    dispatch_catalog_webdav_read(
        request,
        catalog,
        &live_rights,
        availability,
        network,
        async |root, identity| {
            let RemoteIdentity::Vault {
                credential_alias,
                totp_alias: None,
            } = identity
            else {
                return Err(ReadDispatchError::CredentialUnavailable);
            };
            resolve_local_webdav_header(config_path, credential_alias, || {
                live_rights(&canonical_alias, root, identity)
                    .is_some_and(|rights| rights.check(&[right]) == Decision::Allowed)
            })
        },
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn webdav_resolves_existing_encrypted_entry_only_with_a_live_dedicated_grant() {
        use std::cell::Cell;
        use zeroclaw_config::{schema::Config, secrets::SecretStore};

        let fixture = tempfile::tempdir().unwrap();
        let path = fixture.path().join("config.toml");
        let mut config = Config::default();
        config.secrets.encrypt = true;
        let record = serde_json::json!({
            "origin": "https://example.test",
            "http_allowed": false,
            "auth": {"kind": "user_password", "username": "fixture", "password": "demo"}
        });
        let value = SecretStore::new(fixture.path(), true)
            .encrypt(&record.to_string())
            .unwrap();
        config
            .http_request
            .scoped_credentials
            .insert("REPORT_LOGIN".into(), value);
        std::fs::write(&path, toml::to_string(&config).unwrap()).unwrap();

        let header = resolve_local_webdav_header(&path, "REPORT_LOGIN", || true).unwrap();
        assert_eq!(header, "Basic Zml4dHVyZTpkZW1v");
        assert!(header.is_sensitive());
        assert!(matches!(
            resolve_local_webdav_header(&path, "REPORT_LOGIN", || false),
            Err(ReadDispatchError::Denied)
        ));
        let calls = Cell::new(0);
        assert!(matches!(
            resolve_local_webdav_header(&path, "REPORT_LOGIN", || {
                calls.set(calls.get() + 1);
                calls.get() == 1
            }),
            Err(ReadDispatchError::Denied)
        ));
    }

    use std::collections::BTreeSet;

    #[tokio::test]
    async fn egress_requires_explicit_private_access_and_always_blocks_metadata() {
        let root = Url::parse("https://files.test/dav/").unwrap();
        let proxy = ProxyConfig::default();
        let mut policy = WebDavNetworkPolicy {
            proxy: &proxy,
            nat64_prefixes: &[],
            allow_private_endpoint: false,
        };

        let addresses = || async {
            Ok(vec![
                "93.184.216.34:443".parse().unwrap(),
                "10.1.2.3:443".parse().unwrap(),
            ])
        };
        assert!(matches!(
            policy
                .resolve_with(&root, async |_, _| addresses().await)
                .await,
            Err(ReadDispatchError::NetworkDenied)
        ));
        policy.allow_private_endpoint = true;
        assert_eq!(
            policy
                .resolve_with(&root, async |_, _| addresses().await)
                .await
                .unwrap()
                .len(),
            2
        );
        assert!(matches!(
            policy
                .resolve_with(&root, async |_, _| Ok(vec![
                    "169.254.169.254:443".parse().unwrap()
                ]))
                .await,
            Err(ReadDispatchError::NetworkDenied)
        ));

        let proxy = ProxyConfig {
            enabled: true,
            scope: ProxyScope::Environment,
            ..ProxyConfig::default()
        };
        let policy = WebDavNetworkPolicy {
            proxy: &proxy,
            nat64_prefixes: &[],
            allow_private_endpoint: true,
        };
        assert!(matches!(
            policy
                .resolve_with(&root, async |_, _| panic!(
                    "proxy conflict must precede DNS"
                ))
                .await,
            Err(ReadDispatchError::NetworkDenied)
        ));
    }

    #[tokio::test]
    async fn catalog_lookup_accepts_case_variants_but_requires_a_live_grant() {
        use zeroclaw_config::remote_files::WebDavResourceConfig;

        let catalog = WebDavCatalog::from_resources(vec![WebDavResourceConfig {
            alias: "REPORTS".into(),
            root: "https://files.test/dav/".into(),
            credential_alias: "DEMO".into(),
        }])
        .unwrap();
        let availability = BackendAvailability::Available(BTreeSet::from([Right::ListDirectory]));
        let request = RemoteFilesRequest::ListDirectory {
            resource: "Reports".into(),
            path: None,
        };
        let proxy = ProxyConfig::default();
        let network = WebDavNetworkPolicy {
            proxy: &proxy,
            nat64_prefixes: &[],
            allow_private_endpoint: false,
        };
        let result = dispatch_catalog_webdav_read(
            &request,
            &catalog,
            |canonical, root, identity| {
                assert_eq!(canonical, "reports");
                assert_eq!(root.as_str(), "https://files.test/dav/");
                assert!(matches!(
                    identity,
                    RemoteIdentity::Vault {
                        credential_alias: "DEMO",
                        ..
                    }
                ));
                None
            },
            &availability,
            &network,
            async |_, _| panic!("vault must not be called"),
        )
        .await;
        assert!(matches!(result, Err(ReadDispatchError::Denied)));
    }

    #[tokio::test]
    async fn authorization_and_parity_precede_network_and_vault() {
        let root = Url::parse("https://files.test/dav/").unwrap();
        let mut rights = Rights::default();
        let identity = RemoteIdentity::Vault {
            credential_alias: "DEMO",
            totp_alias: None,
        };
        let mut availability =
            BackendAvailability::Available(BTreeSet::from([Right::ListDirectory]));
        let request = RemoteFilesRequest::ListDirectory {
            resource: "REPORTS".into(),
            path: None,
        };

        async fn call(
            request: &RemoteFilesRequest,
            root: &Url,
            rights: &Rights,
            identity: RemoteIdentity<'_>,
            availability: &BackendAvailability,
        ) -> Result<WebDavReadResult, ReadDispatchError> {
            dispatch_webdav_read(
                request,
                |alias| {
                    assert_eq!(alias, "REPORTS");
                    Some(WebDavReadGrant {
                        root,
                        identity,
                        rights,
                        availability,
                    })
                },
                async |_| panic!("network must not be called"),
                async |_, _| panic!("vault must not be called"),
            )
            .await
        }

        assert!(matches!(
            call(&request, &root, &rights, identity, &availability).await,
            Err(ReadDispatchError::Denied)
        ));
        rights.operations.insert(Right::ListDirectory);
        rights.require_human_approval = true;
        assert!(matches!(
            call(&request, &root, &rights, identity, &availability).await,
            Err(ReadDispatchError::ApprovalRequired)
        ));
        rights.require_human_approval = false;
        availability = BackendAvailability::AwaitingPlatformParity;
        assert!(matches!(
            call(&request, &root, &rights, identity, &availability).await,
            Err(ReadDispatchError::UnsupportedOperation)
        ));
    }

    #[tokio::test]
    async fn invalid_endpoint_never_resolves_vault_alias() {
        let root = Url::parse("http://files.test/dav/").unwrap();
        let rights = Rights {
            operations: BTreeSet::from([Right::GetFileInfo]),
            require_human_approval: false,
        };
        let availability = BackendAvailability::Available(BTreeSet::from([Right::GetFileInfo]));
        let request = RemoteFilesRequest::GetFileInfo {
            resource: "REPORTS".into(),
            path: "report.txt".into(),
        };
        let result = dispatch_webdav_read(
            &request,
            |_| {
                Some(WebDavReadGrant {
                    root: &root,
                    identity: RemoteIdentity::Vault {
                        credential_alias: "DEMO",
                        totp_alias: None,
                    },
                    rights: &rights,
                    availability: &availability,
                })
            },
            async |_| panic!("network must not be called"),
            async |_, _| panic!("vault must not be called"),
        )
        .await;
        assert!(matches!(
            result,
            Err(ReadDispatchError::Adapter(WebDavError::InvalidEndpoint))
        ));
    }
}
