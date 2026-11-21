//! Encrypted persistence for MCP OAuth grants.
//!
//! Canonical source of truth: the shared auth-profile store
//! (`zeroclaw_providers::auth::profiles`), keyed by the canonical RFC 8707
//! resource so a server rename cannot orphan a grant and a URL change
//! cannot silently reuse one (the audience changed). Tokens live there and
//! nowhere else — never in `config.toml`, never in logs.

use anyhow::{Context, Result, bail};
use reqwest::Url;
use std::path::Path;
use zeroclaw_providers::auth::profiles::{
    AuthProfile, AuthProfilesStore, REQUIRES_ENCRYPTED_STORAGE, TokenSet, profile_id,
};

use super::resource::CanonicalResource;

/// The `model_provider` namespace for MCP OAuth grants. It shares the store
/// with model-provider profiles but no name space collisions occur: MCP
/// profile names are the fixed `oauth@<hash>` form.
pub const MCP_MODEL_PROVIDER: &str = "mcp";

/// Metadata keys carried on the profile (public values; never tokens).
pub const META_CLIENT_ID: &str = "client_id";
pub const META_ISSUER: &str = "issuer";
pub const META_RESOURCE: &str = "resource";

/// A usable stored authorization: enough to refresh and to attach bearers.
#[derive(Clone)]
pub struct McpAuthorization {
    pub client_id: String,
    pub issuer: Url,
    pub tokens: TokenSet,
}

/// Token material stays out of `Debug` (module contract): identity and
/// expiry are diagnostic, the three token fields are redacted.
impl std::fmt::Debug for McpAuthorization {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpAuthorization")
            .field("client_id", &self.client_id)
            .field("issuer", &self.issuer)
            .field("expires_at", &self.tokens.expires_at)
            .field("access_token", &"[redacted]")
            .field(
                "refresh_token",
                &self.tokens.refresh_token.as_ref().map(|_| "[redacted]"),
            )
            .field(
                "id_token",
                &self.tokens.id_token.as_ref().map(|_| "[redacted]"),
            )
            .finish()
    }
}

/// One stable log shape for "the stored profile exists but cannot drive a
/// refresh", so the reason is machine-readable and the human text is fixed.
fn log_profile_unusable(resource: &str, reason: &str) {
    ::zeroclaw_log::record!(
        WARN,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
            .with_attrs(::serde_json::json!({
                "mcp_oauth": "profile-load",
                "resource": resource,
                "reason": reason,
            })),
        "MCP OAuth stored profile is unusable; re-authorizing"
    );
}

/// Profile name for a canonical resource: stable across server renames,
/// different across resource-URL changes (intentional audience separation).
/// `profile_key()` already carries the design §4 `oauth@<16-byte-hex>`
/// shape, so the name is that verbatim and profile ids land as
/// `mcp:oauth@<32hex>` — never a doubled prefix.
pub fn profile_name_for(canonical: &CanonicalResource) -> String {
    canonical.profile_key()
}

/// Reads and writes MCP OAuth grants in the encrypted auth-profile store.
///
/// Construction fails closed when encryption is disabled: `SecretStore`
/// stores plaintext verbatim when `encrypt_secrets` is false, and a
/// long-lived MCP refresh token in a plaintext state file is the exact leak
/// this feature promises not to have. (Provider OAuth profiles accept
/// plaintext by operator choice; MCP OAuth does not offer that choice.)
///
/// That refusal only binds writers this constructor sees, so the requirement
/// travels with each row as [`REQUIRES_ENCRYPTED_STORAGE`] and the shared
/// profile store writes a marked row sealed whatever its own setting is:
/// flipping `secrets.encrypt` off afterwards cannot expose a signed-in grant.
#[derive(Clone)]
pub struct McpTokenStore {
    store: AuthProfilesStore,
}

impl std::fmt::Debug for McpTokenStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // No Debug for the inner store's secret material by design.
        f.debug_struct("McpTokenStore").finish_non_exhaustive()
    }
}

impl McpTokenStore {
    pub fn new(state_dir: &Path, encrypt_secrets: bool) -> Result<Self> {
        if !encrypt_secrets {
            bail!(
                "MCP OAuth requires encrypted secret storage: set `secrets.encrypt = true` \
                 so OAuth tokens for MCP servers are never stored as plaintext"
            );
        }
        Ok(Self {
            store: AuthProfilesStore::new(state_dir, true),
        })
    }

    async fn load_profile(&self, canonical: &CanonicalResource) -> Result<Option<AuthProfile>> {
        let id = profile_id(MCP_MODEL_PROVIDER, &profile_name_for(canonical));
        let data = self
            .store
            .load()
            .await
            .with_context(|| format!("cannot read the auth-profile store for {id}"))?;
        Ok(data.profiles.get(&id).cloned())
    }

    /// Load the stored grant, if any. A profile that is missing the
    /// non-token metadata it needs to be usable (client id, issuer) yields
    /// `Ok(None)`: the flow re-authorizes from scratch, which is the safe
    /// repair for state this version did not write.
    pub async fn load_authorization(
        &self,
        canonical: &CanonicalResource,
    ) -> Result<Option<McpAuthorization>> {
        let Some(profile) = self.load_profile(canonical).await? else {
            return Ok(None);
        };
        let tokens = match profile.token_set {
            Some(tokens) if !tokens.access_token.is_empty() => tokens,
            _ => {
                log_profile_unusable(canonical.resource(), "no usable token set");
                return Ok(None);
            }
        };
        let client_id = match profile.metadata.get(META_CLIENT_ID) {
            Some(id) if !id.is_empty() => id.clone(),
            _ => {
                log_profile_unusable(canonical.resource(), "missing client_id metadata");
                return Ok(None);
            }
        };
        let issuer = match profile
            .metadata
            .get(META_ISSUER)
            .and_then(|issuer| Url::parse(issuer).ok())
        {
            Some(issuer) => issuer,
            None => {
                log_profile_unusable(canonical.resource(), "unusable issuer metadata");
                return Ok(None);
            }
        };
        Ok(Some(McpAuthorization {
            client_id,
            issuer,
            tokens,
        }))
    }

    /// Persist a freshly completed authorization (tokens + client identity)
    /// for this canonical resource, replacing any previous grant.
    pub async fn save_authorization(
        &self,
        canonical: &CanonicalResource,
        client_id: &str,
        issuer: &Url,
        tokens: TokenSet,
    ) -> Result<()> {
        let mut profile =
            AuthProfile::new_oauth(MCP_MODEL_PROVIDER, &profile_name_for(canonical), tokens);
        profile
            .metadata
            .insert(META_CLIENT_ID.to_string(), client_id.to_string());
        profile
            .metadata
            .insert(META_ISSUER.to_string(), issuer.as_str().to_string());
        profile
            .metadata
            .insert(META_RESOURCE.to_string(), canonical.resource().to_string());
        // The refusal above only binds writers this constructor sees. Grants
        // live in the profile store every other credential writes to, so the
        // requirement travels with the row: a provider token pasted, or a
        // different sign-in logged out, while `secrets.encrypt = false`
        // re-saves this whole file, and a marked row is written sealed by that
        // writer too instead of being dropped into plaintext.
        profile
            .metadata
            .insert(REQUIRES_ENCRYPTED_STORAGE.to_string(), "true".to_string());
        self.store
            .upsert_profile(profile, false)
            .await
            .with_context(|| {
                format!(
                    "cannot store the MCP OAuth grant for {}",
                    canonical.resource()
                )
            })
    }

    /// Rotate the stored tokens of `canonical`'s grant, but only while the
    /// profile on disk still belongs to the party that minted them
    /// (`minted_by_client_id` + `minted_by_issuer`). The documented operator
    /// flow signs a resource in from ANOTHER process while this daemon runs:
    /// if that sign-in replaced the authorization server (or the client)
    /// during our token round-trip, writing our tokens into the replacement
    /// profile would pair the replacement's `issuer` and `client_id` with
    /// tokens it never minted, and the next restart would redeem them at the
    /// wrong token endpoint. The check runs inside the store's own profile
    /// lock, and a replacement sign-in is left untouched. A vanished profile
    /// fails closed rather than leaving a half-populated write behind, and the
    /// return value says whether the write landed.
    pub async fn save_refreshed_tokens(
        &self,
        canonical: &CanonicalResource,
        tokens: TokenSet,
        minted_by_client_id: &str,
        minted_by_issuer: &Url,
    ) -> Result<bool> {
        let id = profile_id(MCP_MODEL_PROVIDER, &profile_name_for(canonical));
        let issuer = minted_by_issuer.as_str();
        let mut applied = false;
        self.store
            .update_profile(&id, |profile| {
                let minted_by_us = profile
                    .metadata
                    .get(META_ISSUER)
                    .is_some_and(|stored| stored == issuer)
                    && profile
                        .metadata
                        .get(META_CLIENT_ID)
                        .is_some_and(|stored| stored == minted_by_client_id);
                if !minted_by_us {
                    return Ok(());
                }
                // RFC 6749 §6 (the norm: an omitted `refresh_token` on refresh means
                // the old one stands) + S2-2: an absent (or normalized-
                // empty, see tokens.rs) refresh_token in a refresh response
                // means "the old one stands". Overwriting it with `None`
                // would strand the grant at the browser flow.
                let refresh_token = tokens.refresh_token.clone().or_else(|| {
                    profile
                        .token_set
                        .as_ref()
                        .and_then(|t| t.refresh_token.clone())
                });
                profile.token_set = Some(TokenSet {
                    refresh_token,
                    ..tokens.clone()
                });
                applied = true;
                Ok(())
            })
            .await
            .with_context(|| {
                format!(
                    "cannot rotate the MCP OAuth tokens for {}",
                    canonical.resource()
                )
            })?;
        Ok(applied)
    }

    /// Remove `canonical`'s stored grant, but only while it still holds the
    /// access token the caller proved spent (`dead_access`). This is the
    /// removal half of the ownership rule `save_refreshed_tokens` applies to
    /// writes: the compare and the removal share one lock acquisition, so a
    /// sign-in another process committed between the caller's inspection and
    /// this call is never deleted, and the next verdict cannot lose a
    /// credential it never disproved. Returns whether the profile was
    /// removed; a vanished profile and a profile belonging to someone else
    /// both report `false`.
    pub async fn clear_if_access_matches(
        &self,
        canonical: &CanonicalResource,
        dead_access: &str,
    ) -> Result<bool> {
        let id = profile_id(MCP_MODEL_PROVIDER, &profile_name_for(canonical));
        self.store
            .remove_profile_if(&id, |profile| {
                profile
                    .token_set
                    .as_ref()
                    .is_some_and(|tokens| tokens.access_token == dead_access)
            })
            .await
            .with_context(|| {
                format!(
                    "cannot clear the MCP OAuth grant for {}",
                    canonical.resource()
                )
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn canonical(raw: &str) -> CanonicalResource {
        CanonicalResource::from_configured(&Url::parse(raw).expect("url")).expect("canonical")
    }

    fn tokens(access: &str) -> TokenSet {
        TokenSet {
            access_token: access.to_string(),
            refresh_token: Some(format!("refresh-{access}")),
            id_token: None,
            expires_at: Some(chrono::Utc::now() + chrono::Duration::hours(1)),
            token_type: Some("Bearer".to_string()),
            scope: None,
        }
    }

    #[test]
    fn construction_fails_closed_without_encryption() {
        let tmp = TempDir::new().expect("tmp");
        let err = McpTokenStore::new(tmp.path(), false).expect_err("must fail closed");
        assert!(err.to_string().contains("secrets.encrypt"), "got: {err}");
        // The message is actionable and mentions no token material.
    }

    #[tokio::test]
    async fn rotation_keeps_stored_refresh_token_when_response_omits_it() {
        // S2-2 / RFC 8252 §4.1.3: an omitted refresh_token in a refresh
        // response means "the old one stands" — overwriting the profile
        // with None would strand the grant at the next browser flow.
        let tmp = TempDir::new().expect("tmp");
        let store = McpTokenStore::new(tmp.path(), true).expect("store");
        let canonical = canonical("https://mcp.example.com/mcp");
        store
            .save_authorization(
                &canonical,
                "client-abc",
                &Url::parse("https://auth.example.com").expect("url"),
                tokens("at-1"),
            )
            .await
            .expect("seed");

        let mut rotated = tokens("at-2");
        rotated.refresh_token = None;
        store
            .save_refreshed_tokens(
                &canonical,
                rotated,
                "client-abc",
                &Url::parse("https://auth.example.com").expect("url"),
            )
            .await
            .expect("rotate without new refresh token");
        let loaded = store
            .load_authorization(&canonical)
            .await
            .expect("load")
            .expect("present");
        assert_eq!(loaded.tokens.access_token, "at-2");
        assert_eq!(
            loaded.tokens.refresh_token.as_deref(),
            Some("refresh-at-1"),
            "omitted refresh_token must keep the stored one"
        );

        // A rotated refresh token replaces it, of course.
        store
            .save_refreshed_tokens(
                &canonical,
                tokens("at-3"),
                "client-abc",
                &Url::parse("https://auth.example.com").expect("url"),
            )
            .await
            .expect("rotate with new refresh token");
        let loaded = store
            .load_authorization(&canonical)
            .await
            .expect("load")
            .expect("present");
        assert_eq!(
            loaded.tokens.refresh_token.as_deref(),
            Some("refresh-at-3"),
            "present refresh_token must replace the old one"
        );
    }

    #[tokio::test]
    async fn round_trip_keeps_tokens_encrypted_on_disk() {
        let tmp = TempDir::new().expect("tmp");
        let store = McpTokenStore::new(tmp.path(), true).expect("store");
        let canonical = canonical("https://mcp.example.com/mcp");
        assert!(
            store
                .load_authorization(&canonical)
                .await
                .expect("load")
                .is_none()
        );

        store
            .save_authorization(
                &canonical,
                "client-abc",
                &Url::parse("https://auth.example.com").expect("url"),
                tokens("fresh-access"),
            )
            .await
            .expect("save");

        let loaded = store
            .load_authorization(&canonical)
            .await
            .expect("load")
            .expect("present");
        assert_eq!(loaded.client_id, "client-abc");
        assert_eq!(loaded.issuer.as_str(), "https://auth.example.com/");
        assert_eq!(loaded.tokens.access_token, "fresh-access");

        let raw = tokio::fs::read_to_string(store.store.path())
            .await
            .expect("read raw profiles");
        assert!(raw.contains("enc2:"), "tokens must be encrypted on disk");
        assert!(
            !raw.contains("fresh-access") && !raw.contains("refresh-fresh-access"),
            "plaintext token material on disk"
        );
        // Public metadata may appear unencrypted and must be harmless.
        assert!(raw.contains("client-abc"));
    }

    #[test]
    fn profile_name_is_the_frozen_oauth_hash_shape() {
        let canonical = canonical("https://mcp.example.com/mcp");
        let name = profile_name_for(&canonical);
        let (prefix, hash) = name.split_once('@').expect("oauth@ prefix");
        assert_eq!(prefix, "oauth");
        assert_eq!(hash.len(), 32, "16-byte hash as hex: {name}");
        assert!(hash.bytes().all(|b| b.is_ascii_hexdigit()));
        assert!(!name.contains("oauth@oauth@"), "doubled prefix: {name}");
    }

    #[tokio::test]
    async fn debug_output_redacts_token_material() {
        let tmp = TempDir::new().expect("tmp");
        let store = McpTokenStore::new(tmp.path(), true).expect("store");
        let canonical = canonical("https://mcp.example.com/mcp");
        store
            .save_authorization(
                &canonical,
                "cid",
                &Url::parse("https://auth.example.com").expect("url"),
                tokens("topsecret-access"),
            )
            .await
            .expect("save");
        let loaded = store
            .load_authorization(&canonical)
            .await
            .expect("load")
            .expect("present");
        let shown = format!("{loaded:?}");
        assert!(
            !shown.contains("topsecret-access"),
            "Debug leaked the access token: {shown}"
        );
        assert!(
            !shown.contains("refresh-topsecret-access"),
            "Debug leaked the refresh token"
        );
        assert!(shown.contains("cid"), "identity must remain diagnostic");
    }

    #[tokio::test]
    async fn identity_tracks_the_canonical_resource_not_the_url_shape() {
        let tmp = TempDir::new().expect("tmp");
        let store = McpTokenStore::new(tmp.path(), true).expect("store");
        let a = canonical("https://mcp.example.com/mcp");
        let same = canonical("https://mcp.example.com/mcp/"); // trailing slash folds
        let other = canonical("https://mcp.example.com/v2/mcp");

        store
            .save_authorization(
                &a,
                "cid",
                &Url::parse("https://auth.example.com").expect("url"),
                tokens("one"),
            )
            .await
            .expect("save");

        assert!(
            store
                .load_authorization(&same)
                .await
                .expect("load")
                .is_some(),
            "the canonicalized alias must find the same grant"
        );
        assert!(
            store
                .load_authorization(&other)
                .await
                .expect("load")
                .is_none(),
            "a different canonical resource must not see this grant"
        );
    }

    #[tokio::test]
    async fn refresh_rotation_keeps_identity_and_replaces_tokens() {
        let tmp = TempDir::new().expect("tmp");
        let store = McpTokenStore::new(tmp.path(), true).expect("store");
        let canonical = canonical("https://mcp.example.com/mcp");
        store
            .save_authorization(
                &canonical,
                "cid",
                &Url::parse("https://auth.example.com").expect("url"),
                tokens("old"),
            )
            .await
            .expect("save");

        store
            .save_refreshed_tokens(
                &canonical,
                tokens("rotated"),
                "cid",
                &Url::parse("https://auth.example.com").expect("url"),
            )
            .await
            .expect("rotate");

        let loaded = store
            .load_authorization(&canonical)
            .await
            .expect("load")
            .expect("present");
        assert_eq!(loaded.tokens.access_token, "rotated");
        assert_eq!(loaded.client_id, "cid");

        // The rotation edited the row it loaded, so the storage promise the
        // sign-in made travels with it.
        let rows = AuthProfilesStore::new(tmp.path(), true)
            .load()
            .await
            .expect("profiles");
        let row = rows
            .profiles
            .get(&profile_id(
                MCP_MODEL_PROVIDER,
                &profile_name_for(&canonical),
            ))
            .expect("the MCP row");
        assert_eq!(
            row.metadata
                .get(REQUIRES_ENCRYPTED_STORAGE)
                .map(String::as_str),
            Some("true"),
            "a rotation must keep the storage requirement the sign-in declared"
        );

        // Rotating onto a vanished profile fails closed.
        assert!(
            store
                .clear_if_access_matches(&canonical, "rotated")
                .await
                .expect("clear"),
            "the profile still holding the spent token must be removed"
        );
        store
            .save_refreshed_tokens(
                &canonical,
                tokens("nowhere"),
                "cid",
                &Url::parse("https://auth.example.com").expect("url"),
            )
            .await
            .expect_err("must fail closed");
    }

    #[tokio::test]
    async fn a_rotation_never_rewrites_another_sign_ins_profile() {
        // The daemon holds issuer A's grant and rotates it while the
        // documented operator flow replaces this resource's sign-in with one
        // from issuer B. Writing A's tokens into B's profile would pair B's
        // `issuer` and `client_id` with tokens B never minted, and the next
        // restart would redeem a foreign refresh token at B's endpoint.
        let tmp = TempDir::new().expect("tmp");
        let store = McpTokenStore::new(tmp.path(), true).expect("store");
        let canonical = canonical("https://mcp.example.com/mcp");
        let issuer_a = Url::parse("https://auth-a.example").expect("url");
        let issuer_b = Url::parse("https://auth-b.example").expect("url");
        store
            .save_authorization(&canonical, "cid-a", &issuer_a, tokens("at-a"))
            .await
            .expect("seed A");
        store
            .save_authorization(&canonical, "cid-b", &issuer_b, tokens("at-b"))
            .await
            .expect("operator signs in at B");

        let applied = store
            .save_refreshed_tokens(&canonical, tokens("at-a2"), "cid-a", &issuer_a)
            .await
            .expect("rotate A");
        assert!(
            !applied,
            "a profile another sign-in owns must not absorb our rotation"
        );
        let loaded = store
            .load_authorization(&canonical)
            .await
            .expect("load")
            .expect("present");
        assert_eq!(
            loaded.tokens.access_token, "at-b",
            "the replacement sign-in must survive untouched"
        );

        // The replacement's own rotation still lands.
        let applied = store
            .save_refreshed_tokens(&canonical, tokens("at-b2"), "cid-b", &issuer_b)
            .await
            .expect("rotate B");
        assert!(applied, "our own party's profile must still rotate");
        let loaded = store
            .load_authorization(&canonical)
            .await
            .expect("load")
            .expect("present");
        assert_eq!(loaded.tokens.access_token, "at-b2");
        assert_eq!(loaded.client_id, "cid-b");
    }

    #[tokio::test]
    async fn a_spent_verdict_removes_only_the_profile_it_disproved() {
        // The removal half of the ownership rule. The compare and the
        // removal run under one lock acquisition, so a sign-in that landed
        // while this process was proving its own token dead is never
        // deleted, and an owned profile is deleted for real rather than
        // left as a credential-less shell.
        let tmp = TempDir::new().expect("tmp");
        let store = McpTokenStore::new(tmp.path(), true).expect("store");
        let canonical = canonical("https://mcp.example.com/mcp");
        store
            .save_authorization(
                &canonical,
                "cid",
                &Url::parse("https://auth.example.com").expect("url"),
                tokens("at-old"),
            )
            .await
            .expect("seed");

        let removed = store
            .clear_if_access_matches(&canonical, "at-other")
            .await
            .expect("clear attempt");
        assert!(!removed, "a profile holding another token stays");
        let loaded = store
            .load_authorization(&canonical)
            .await
            .expect("load")
            .expect("still present");
        assert_eq!(loaded.tokens.access_token, "at-old");

        let removed = store
            .clear_if_access_matches(&canonical, "at-old")
            .await
            .expect("clear");
        assert!(removed, "the disproved profile is removed");
        assert!(
            store
                .load_authorization(&canonical)
                .await
                .expect("load")
                .is_none(),
            "the row must actually go away"
        );
    }

    #[tokio::test]
    async fn unusable_profile_degrades_to_reauthorize_not_hard_failure() {
        let tmp = TempDir::new().expect("tmp");
        let store = McpTokenStore::new(tmp.path(), true).expect("store");
        let canonical = canonical("https://mcp.example.com/mcp");
        // Write a profile whose metadata lacks the client_id by hand.
        let mut profile = AuthProfile::new_oauth(
            MCP_MODEL_PROVIDER,
            &profile_name_for(&canonical),
            tokens("whatever"),
        );
        store
            .store
            .upsert_profile(profile.clone(), false)
            .await
            .expect("seed");
        assert!(
            store
                .load_authorization(&canonical)
                .await
                .expect("load")
                .is_none(),
            "a profile without client_id metadata must not be treated as usable"
        );
        profile
            .metadata
            .insert(META_CLIENT_ID.to_string(), "cid".to_string());
        profile
            .metadata
            .insert(META_ISSUER.to_string(), "not a url".to_string());
        store
            .store
            .upsert_profile(profile, false)
            .await
            .expect("seed 2");
        assert!(
            store
                .load_authorization(&canonical)
                .await
                .expect("load")
                .is_none(),
            "an unparseable issuer must not be treated as usable"
        );
    }
}
