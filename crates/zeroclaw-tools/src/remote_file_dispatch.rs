//! Connect validated read requests to the WebDAV adapter. The caller
//! supplies a live operator catalog entry and the existing policy services.

use crate::remote_file_webdav::{
    WebDavDirectoryListing, WebDavError, WebDavRead, WebDavResourceKind,
};
use reqwest::{Url, header::HeaderValue};
use std::net::SocketAddr;
use zeroclaw_config::remote_files::{
    BackendAvailability, Decision, RemoteFilesRequest, RemoteIdentity, Right, Rights, WebDavCatalog,
};

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
    verify_egress: impl AsyncFnOnce(&Url) -> Result<Vec<SocketAddr>, ReadDispatchError>,
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
        verify_egress,
        resolve_authorization,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

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
            async |_| panic!("network must not be called"),
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
