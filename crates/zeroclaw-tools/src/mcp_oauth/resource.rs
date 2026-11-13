//! Canonical RFC 8707 resource identity for an MCP server.
//!
//! The canonical resource URI is the single identity that binds the
//! `resource=` authorization/token parameters, the RFC 9728 protected-resource
//! document, and the auth-profile storage key. It is derived from the
//! operator-configured URL only, never from server-supplied metadata.
//!
//! Canonicalization rules (deterministic, documented):
//! - scheme and host are lowercased (what `Url::parse` already normalizes);
//! - default ports are omitted (again via `Url`), non-default ports are kept;
//! - userinfo and fragments are rejected outright;
//! - the query is preserved verbatim — RFC 8707 treats it as part of the
//!   resource URI, and dropping it could silently bind a token minted for
//!   one logical endpoint to another; only an empty `?` is dropped, and the
//!   operator owns parameter order (no sorting or re-encoding);
//! - trailing slashes are stripped except that a root path canonicalizes to
//!   the bare origin (so `https://h/`, `https://h`, and `https://h//`-style
//!   spellings map to one identity);
//! - remaining path bytes and case are preserved (path-sensitive identity).

use anyhow::{Context, Result, bail};
use reqwest::Url;
use sha2::{Digest, Sha256};

/// A configured MCP URL resolved to its canonical protected-resource form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonicalResource {
    configured: Url,
    resource: String,
}

impl CanonicalResource {
    /// Derive the canonical protected-resource URI from the configured URL.
    pub fn from_configured(url: &Url) -> Result<Self> {
        if !matches!(url.scheme(), "http" | "https") {
            bail!(
                "MCP OAuth resource scheme must be http or https, got {:?}",
                url.scheme()
            );
        }
        if !url.username().is_empty() || url.password().is_some() {
            bail!("MCP OAuth resource URL must not carry userinfo");
        }
        if url.fragment().is_some() {
            bail!("MCP OAuth resource URL must not carry a fragment");
        }
        let host = url
            .host_str()
            .filter(|host| !host.is_empty())
            .with_context(|| format!("MCP OAuth resource URL has no host: {url}"))?;
        let authority = match url.port() {
            // `Url::port()` already omits the scheme default, so only
            // explicit non-default ports are kept.
            Some(port) => format!("{host}:{port}"),
            None => host.to_string(),
        };
        let trimmed = url.path().trim_end_matches('/');
        // RFC 8707 §3: a root (or empty) path contributes nothing; the
        // resource is the bare origin.
        let path = if trimmed == "/" { "" } else { trimmed };
        let resource = format!("{}://{authority}{path}", url.scheme());
        // RFC 8707 §3: the resource URI carries its query; only a lone empty
        // `?` carries nothing. Byte-verbatim otherwise: reordering or
        // re-encoding would change the identity the AS echoes back.
        let resource = match url.query() {
            Some(query) if !query.is_empty() => format!("{resource}?{query}"),
            _ => resource,
        };
        Ok(Self {
            configured: url.clone(),
            resource,
        })
    }

    /// The absolute-URI value to send as the RFC 8707 `resource=` parameter
    /// in both the authorization and token requests.
    pub fn resource(&self) -> &str {
        &self.resource
    }

    /// The URL exactly as configured (kept for transport reuse).
    pub fn configured(&self) -> &Url {
        &self.configured
    }

    /// RFC 9728 §3 default protected-resource metadata URL: the `.well-known`
    /// segment is inserted between the origin and the resource path.
    pub fn well_known_prm_url(&self) -> Result<Url> {
        let parsed =
            Url::parse(&self.resource).context("canonical resource must re-parse as a URL")?;
        let origin = self.origin_prefix();
        let path = parsed.path().trim_end_matches('/');
        let path = if path == "/" { "" } else { path };
        let url = format!("{origin}/.well-known/oauth-protected-resource{path}");
        Url::parse(&url).with_context(|| format!("invalid derived protected-resource URL: {url}"))
    }

    /// `scheme://host[:port]` for the canonical resource, no trailing path.
    pub fn origin_prefix(&self) -> String {
        let parsed = Url::parse(&self.resource)
            .expect("canonical resource string is always a parseable absolute URL");
        let host = parsed
            .host_str()
            .expect("canonical resource string always carries a host");
        match parsed.port() {
            Some(port) => format!("{}://{host}:{port}", parsed.scheme()),
            None => format!("{}://{host}", parsed.scheme()),
        }
    }

    /// Stable storage key for the auth profile backing this resource. Hashing
    /// the canonical URI keeps a server *rename* from orphaning tokens while a
    /// URL change (an audience change) deliberately starts a new identity.
    pub fn profile_key(&self) -> String {
        let digest = Sha256::digest(self.resource.as_bytes());
        format!("oauth@{}", hex::encode(&digest[..16]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn canonical(raw: &str) -> CanonicalResource {
        CanonicalResource::from_configured(&Url::parse(raw).expect("fixture URL parses"))
            .expect("fixture must canonicalize")
    }

    #[test]
    fn resource_keeps_query_verbatim_and_drops_empty_one() {
        assert_eq!(
            canonical("https://mcp.silpo.ua/mcp").resource(),
            "https://mcp.silpo.ua/mcp"
        );
        assert_eq!(
            canonical("https://mcp.silpo.ua/mcp?session=1").resource(),
            "https://mcp.silpo.ua/mcp?session=1"
        );
        assert_eq!(
            canonical("https://host/mcp?b=2&a=1").resource(),
            "https://host/mcp?b=2&a=1"
        );
        // A lone `?` carries no identity.
        assert_eq!(
            canonical("https://host/mcp?").resource(),
            "https://host/mcp"
        );
    }

    #[test]
    fn query_participates_in_identity_and_not_in_prm_url() {
        // `?a=1` and `?a=2` are different resources with different profiles,
        // while the RFC 9728 §3 well-known URL is path-derived only.
        let a = canonical("https://host/mcp?a=1");
        let b = canonical("https://host/mcp?a=2");
        assert_eq!(a.resource(), "https://host/mcp?a=1");
        assert_ne!(a.profile_key(), b.profile_key());
        assert_eq!(
            a.well_known_prm_url().expect("derived").as_str(),
            "https://host/.well-known/oauth-protected-resource/mcp"
        );
    }

    #[test]
    fn trailing_slashes_and_root_fold_to_one_identity() {
        assert_eq!(
            canonical("https://host/mcp/").resource(),
            "https://host/mcp"
        );
        assert_eq!(canonical("https://host/").resource(), "https://host");
        assert_eq!(canonical("https://host").resource(), "https://host");
        assert_eq!(
            canonical("https://host/v1/mcp//").resource(),
            "https://host/v1/mcp"
        );
    }

    #[test]
    fn default_ports_omitted_explicit_ports_kept() {
        assert_eq!(
            canonical("https://host:443/mcp").resource(),
            "https://host/mcp"
        );
        assert_eq!(
            canonical("http://host:80/mcp").resource(),
            "http://host/mcp"
        );
        assert_eq!(
            canonical("https://host:8443/mcp").resource(),
            "https://host:8443/mcp"
        );
    }

    #[test]
    fn host_is_lowercased_path_case_preserved() {
        assert_eq!(
            canonical("https://HOST.Example.COM/Mcp").resource(),
            "https://host.example.com/Mcp"
        );
    }

    #[test]
    fn userinfo_and_fragments_are_rejected() {
        for raw in [
            "https://user:pass@host/mcp",
            "https://user@host/mcp",
            "https://host/mcp#frag",
        ] {
            let url = Url::parse(raw).expect("fixture URL parses");
            CanonicalResource::from_configured(&url).expect_err("must reject");
        }
    }

    #[test]
    fn non_http_scheme_rejected() {
        let url = Url::parse("ftp://host/mcp").expect("fixture URL parses");
        CanonicalResource::from_configured(&url).expect_err("must reject");
    }

    #[test]
    fn prm_url_inserts_well_known_after_origin() {
        assert_eq!(
            canonical("https://mcp.silpo.ua/mcp")
                .well_known_prm_url()
                .expect("derived")
                .as_str(),
            "https://mcp.silpo.ua/.well-known/oauth-protected-resource/mcp"
        );
        assert_eq!(
            canonical("https://host/deep/path/")
                .well_known_prm_url()
                .expect("derived")
                .as_str(),
            "https://host/.well-known/oauth-protected-resource/deep/path"
        );
        assert_eq!(
            canonical("https://host/")
                .well_known_prm_url()
                .expect("derived")
                .as_str(),
            "https://host/.well-known/oauth-protected-resource"
        );
    }

    #[test]
    fn profile_key_is_stable_and_path_sensitive() {
        let a = canonical("https://host/mcp");
        let b = canonical("https://host/mcp/");
        assert_eq!(a.profile_key(), b.profile_key());
        assert_ne!(
            a.profile_key(),
            canonical("https://host/other").profile_key()
        );
        assert!(a.profile_key().starts_with("oauth@"));
        // 16 hex bytes of SHA-256, enough to make collisions irrelevant while
        // keeping the profile id short.
        assert_eq!(a.profile_key().len(), "oauth@".len() + 32);
    }

    #[test]
    fn origin_prefix_matches_resource_without_path() {
        assert_eq!(
            canonical("https://host:8443/mcp").origin_prefix(),
            "https://host:8443"
        );
        assert_eq!(
            canonical("https://host/mcp").origin_prefix(),
            "https://host"
        );
    }
}
