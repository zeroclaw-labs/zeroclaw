//! The RFC #7141 shared principal resolver: ONE place that maps a
//! provider-verified [`AuthenticatedIdentity`] to a canonical [`Principal`]
//! and its currently-configured grants.
//!
//! Providers verify credentials into identities; they never touch this
//! vocabulary. The resolver owns the explicit claim-to-profile mapping
//! (`[oidc.<alias>].profile_map`), the local roster
//! (`[users.<name>]`), and the compiled `[permission_profiles]` — and it
//! stamps every resolution with the AUTHORIZATION-POLICY GENERATION it was
//! computed from. Consumers hold the `Principal` for the connection's
//! lifetime but re-resolve grants whenever [`PrincipalResolver::generation`]
//! has moved past their stamped value, so removing or narrowing a profile,
//! mapping, or roster entry affects established connections at the next
//! privileged operation — no reconnect or daemon restart required.
//!
//! Deny-by-default: unmapped identities, unmapped claim values, unknown
//! roster ids, and any policy inconsistency resolve to a denial, never to
//! shared-operator or partial access.

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::RwLock;

use zeroclaw_api::grants::ResolvedGrants;
use zeroclaw_api::principal::{AuthenticatedIdentity, DenyReason, IdentitySubject, Principal};
use zeroclaw_config::schema::Config;

/// One identity's resolution at a specific policy generation.
#[derive(Clone, Debug)]
pub struct ResolvedPrincipal {
    /// The canonical non-secret principal record (held per connection).
    pub principal: Principal,
    /// The effective grants at `generation`. Never cached past a
    /// generation change.
    pub grants: ResolvedGrants,
    /// The authorization-policy generation these grants were resolved
    /// from. Compare against [`PrincipalResolver::generation`] before the
    /// next privileged operation; if it moved, re-resolve.
    pub generation: u64,
}

/// The identity-mapping half of one `[oidc.<alias>]` trust relationship.
#[derive(Clone, Debug, Default)]
pub struct OidcMapping {
    pub issuer: String,
    pub claim_path: String,
    /// Claim value → permission-profile alias.
    pub profile_map: HashMap<String, String>,
}

/// One compiled authorization policy: everything the resolver needs at one
/// generation. Immutable once installed; a config change installs a new
/// policy (and bumps the generation) rather than mutating this one.
#[derive(Clone, Debug, Default)]
pub struct ResolverPolicy {
    /// Compiled `[permission_profiles.<alias>]` grant sets.
    pub profiles: HashMap<String, ResolvedGrants>,
    /// `[oidc.<alias>]` identity mappings, by alias.
    pub oidc: HashMap<String, OidcMapping>,
    /// Local roster: durable principal id → the profile aliases granted to
    /// it. Keyed by the EFFECTIVE principal id (`principal_id` override or
    /// entry name), which is what a local provider binds.
    pub roster: HashMap<String, Vec<String>>,
}

impl ResolverPolicy {
    /// Compile the auth sections of a validated [`Config`]. Dangling
    /// references were rejected at config load; the resolver still fails
    /// closed if it meets one at resolve time.
    #[must_use]
    pub fn from_config(config: &Config) -> Self {
        let profiles = config
            .permission_profiles
            .iter()
            .map(|(alias, profile)| (alias.clone(), profile.resolve()))
            .collect();
        let oidc = config
            .oidc
            .iter()
            .map(|(alias, entry)| {
                (
                    alias.clone(),
                    OidcMapping {
                        issuer: entry.issuer.clone(),
                        claim_path: entry.claim_path.clone(),
                        profile_map: entry.profile_map.clone(),
                    },
                )
            })
            .collect();
        let roster = config
            .users
            .iter()
            .map(|(name, user)| {
                (
                    user.effective_principal_id(name).to_owned(),
                    user.permission_profiles
                        .iter()
                        .map(|p| p.trim().to_owned())
                        .filter(|p| !p.is_empty())
                        .collect(),
                )
            })
            .collect();
        Self {
            profiles,
            oidc,
            roster,
        }
    }
}

/// Walk a dotted claim path over a verified-claims document and collect the
/// values found there as strings. A string claim yields itself; an array
/// yields its string members; an object yields its KEYS (the shape
/// Zitadel-style role claims use, where roles are keys of an object).
fn claim_values(claims: &serde_json::Map<String, serde_json::Value>, path: &str) -> Vec<String> {
    let mut cursor: Option<&serde_json::Value> = None;
    for segment in path.split('.') {
        cursor = match cursor {
            None => claims.get(segment),
            Some(value) => value.get(segment),
        };
        if cursor.is_none() {
            return Vec::new();
        }
    }
    match cursor {
        Some(serde_json::Value::String(s)) => vec![s.clone()],
        Some(serde_json::Value::Array(items)) => items
            .iter()
            .filter_map(|v| v.as_str().map(str::to_owned))
            .collect(),
        Some(serde_json::Value::Object(map)) => map.keys().cloned().collect(),
        _ => Vec::new(),
    }
}

/// The generation-aware shared resolver. One instance per daemon; the
/// installed [`ResolverPolicy`] is swapped (never mutated) when the
/// authorization configuration changes.
pub struct PrincipalResolver {
    /// Current policy + the generation it was installed at. One lock so a
    /// reader can never observe a new policy with an old generation.
    state: RwLock<(Arc<ResolverPolicy>, u64)>,
}

impl PrincipalResolver {
    /// Install the initial policy at generation 1 (0 is never a valid
    /// stamped generation, so an uninitialized consumer can't look fresh).
    #[must_use]
    pub fn new(policy: ResolverPolicy) -> Self {
        Self {
            state: RwLock::new((Arc::new(policy), 1)),
        }
    }

    /// Convenience: compile and install from a validated config.
    #[must_use]
    pub fn from_config(config: &Config) -> Self {
        Self::new(ResolverPolicy::from_config(config))
    }

    /// The current authorization-policy generation. Consumers compare
    /// their stamped [`ResolvedPrincipal::generation`] against this before
    /// each privileged operation and re-resolve when it moved.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.state.read().1
    }

    /// Install a new policy, invalidating every previously stamped
    /// generation. Returns the new generation.
    pub fn replace_policy(&self, policy: ResolverPolicy) -> u64 {
        let mut state = self.state.write();
        state.0 = Arc::new(policy);
        state.1 += 1;
        state.1
    }

    /// Re-compile from a validated config and install (see
    /// [`Self::replace_policy`]).
    pub fn replace_from_config(&self, config: &Config) -> u64 {
        self.replace_policy(ResolverPolicy::from_config(config))
    }

    /// Map a provider-verified identity to its canonical principal and the
    /// grants the CURRENT policy assigns it. Fail-closed: any gap —
    /// unknown roster id, missing mapping, missing profile, provider/alias
    /// inconsistency — is a denial, never partial or shared-operator
    /// access.
    pub fn resolve(
        &self,
        identity: &AuthenticatedIdentity,
    ) -> Result<ResolvedPrincipal, DenyReason> {
        let (policy, generation) = {
            let state = self.state.read();
            (Arc::clone(&state.0), state.1)
        };
        let grants = Self::resolve_grants(&policy, identity)?;
        Ok(ResolvedPrincipal {
            principal: Principal::from_identity(identity),
            grants,
            generation,
        })
    }

    fn resolve_grants(
        policy: &ResolverPolicy,
        identity: &AuthenticatedIdentity,
    ) -> Result<ResolvedGrants, DenyReason> {
        match &identity.subject {
            // The trusted single-operator path keeps today's full access;
            // it exists precisely so enforcement can ship without an IdP.
            IdentitySubject::SharedOperator => Ok(ResolvedGrants::all()),
            IdentitySubject::Roster { principal_id } => {
                let Some(profile_aliases) = policy.roster.get(principal_id) else {
                    // An identity outside the roster holds nothing — it
                    // never falls back to shared-operator access.
                    return Err(DenyReason::NotEntitled);
                };
                Self::merge_profiles(policy, profile_aliases.iter().map(String::as_str))
            }
            IdentitySubject::Oidc { issuer, .. } | IdentitySubject::Service { issuer, .. } => {
                // The provider alias names which [oidc.<alias>] mapping
                // applies; the VALIDATED issuer must agree with that
                // entry, so a mis-wired provider cannot borrow another
                // issuer's mapping.
                let Some(alias) = identity.provider_alias.as_deref() else {
                    return Err(DenyReason::Misconfigured);
                };
                let Some(mapping) = policy.oidc.get(alias) else {
                    return Err(DenyReason::Misconfigured);
                };
                if mapping.issuer != *issuer {
                    return Err(DenyReason::Misconfigured);
                }
                let values = claim_values(&identity.claims, &mapping.claim_path);
                // Sort mapped aliases so multi-profile composition is
                // deterministic regardless of claim ordering; drop
                // duplicates so one profile merges once.
                let mut mapped: Vec<&str> = values
                    .iter()
                    .filter_map(|value| mapping.profile_map.get(value))
                    .map(String::as_str)
                    .collect();
                mapped.sort_unstable();
                mapped.dedup();
                if mapped.is_empty() {
                    // Verified, but entitled to nothing: unmapped claims
                    // grant nothing (Rev 8).
                    return Err(DenyReason::NotEntitled);
                }
                Self::merge_profiles(policy, mapped.into_iter())
            }
            // IdentitySubject is #[non_exhaustive]: an identity class this
            // resolver does not recognize maps to nothing (fail closed).
            _ => Err(DenyReason::Misconfigured),
        }
    }

    /// Union the named profiles (deterministic monotonic merge). A named
    /// profile missing from the policy is a configuration inconsistency:
    /// fail closed rather than grant a subset.
    fn merge_profiles<'a>(
        policy: &ResolverPolicy,
        aliases: impl Iterator<Item = &'a str>,
    ) -> Result<ResolvedGrants, DenyReason> {
        let mut grants = ResolvedGrants::none();
        let mut any = false;
        for alias in aliases {
            let Some(profile) = policy.profiles.get(alias) else {
                return Err(DenyReason::Misconfigured);
            };
            grants.merge(profile);
            any = true;
        }
        if any {
            Ok(grants)
        } else {
            Err(DenyReason::NotEntitled)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeroclaw_api::grants::{Resource, Verb, WILDCARD};
    use zeroclaw_api::principal::{ActorKind, AuthMethod, PrincipalId};

    fn grants_for(resource: Resource, verbs: &[Verb]) -> ResolvedGrants {
        let mut g = ResolvedGrants::none();
        g.resources
            .insert(resource, verbs.iter().copied().collect());
        g
    }

    fn policy() -> ResolverPolicy {
        let mut profiles = HashMap::new();
        profiles.insert(
            "reader".to_string(),
            grants_for(Resource::Sessions, &[Verb::Read]),
        );
        let mut ops = grants_for(Resource::Cron, &[Verb::Execute]);
        ops.allowed_tools = vec![WILDCARD.to_string()];
        profiles.insert("ops".to_string(), ops);

        let mut oidc = HashMap::new();
        oidc.insert(
            "corp".to_string(),
            OidcMapping {
                issuer: "https://sso.example.com/realms/main".to_string(),
                claim_path: "realm_access.roles".to_string(),
                profile_map: HashMap::from([
                    ("zeroclaw-readers".to_string(), "reader".to_string()),
                    ("zeroclaw-ops".to_string(), "ops".to_string()),
                ]),
            },
        );

        let mut roster = HashMap::new();
        roster.insert(
            "alice".to_string(),
            vec!["reader".to_string(), "ops".to_string()],
        );

        ResolverPolicy {
            profiles,
            oidc,
            roster,
        }
    }

    fn oidc_identity(roles: serde_json::Value) -> AuthenticatedIdentity {
        let claims = serde_json::json!({ "realm_access": { "roles": roles } });
        let serde_json::Value::Object(claims) = claims else {
            unreachable!()
        };
        AuthenticatedIdentity::new(
            IdentitySubject::Oidc {
                issuer: "https://sso.example.com/realms/main".into(),
                subject: "alice".into(),
            },
            AuthMethod::Oidc,
        )
        .with_provider_alias("corp")
        .with_claims(claims)
    }

    #[test]
    fn shared_operator_resolves_to_admin_grants() {
        let resolver = PrincipalResolver::new(policy());
        let resolved = resolver
            .resolve(&AuthenticatedIdentity::shared_operator(AuthMethod::Native))
            .expect("shared operator resolves");
        assert!(resolved.grants.admin);
        assert_eq!(resolved.principal.id.as_str(), PrincipalId::SHARED_OPERATOR);
        assert_eq!(resolved.generation, 1);
    }

    #[test]
    fn roster_identity_unions_its_profiles() {
        let resolver = PrincipalResolver::new(policy());
        let identity = AuthenticatedIdentity::new(
            IdentitySubject::Roster {
                principal_id: "alice".into(),
            },
            AuthMethod::Peercred,
        );
        let resolved = resolver.resolve(&identity).expect("roster resolves");
        assert_eq!(resolved.principal.id.as_str(), "user:alice");
        assert!(resolved.principal.is_authenticated());
        assert!(resolved.grants.permits(Resource::Sessions, Verb::Read));
        assert!(resolved.grants.permits(Resource::Cron, Verb::Execute));
        assert!(resolved.grants.may_use_tool("anything"), "ops wildcard");
        assert!(!resolved.grants.admin);
        assert!(
            !resolved.grants.permits(Resource::Config, Verb::Update),
            "union grants only what the profiles list"
        );
    }

    #[test]
    fn unknown_roster_identity_is_not_entitled() {
        let resolver = PrincipalResolver::new(policy());
        let identity = AuthenticatedIdentity::new(
            IdentitySubject::Roster {
                principal_id: "mallory".into(),
            },
            AuthMethod::Peercred,
        );
        assert_eq!(
            resolver.resolve(&identity).unwrap_err(),
            DenyReason::NotEntitled,
            "an unmatched identity never falls back to shared-operator access"
        );
    }

    #[test]
    fn roster_naming_a_missing_profile_fails_closed() {
        let mut policy = policy();
        policy
            .roster
            .insert("bob".to_string(), vec!["ghost".to_string()]);
        let resolver = PrincipalResolver::new(policy);
        let identity = AuthenticatedIdentity::new(
            IdentitySubject::Roster {
                principal_id: "bob".into(),
            },
            AuthMethod::Peercred,
        );
        assert_eq!(
            resolver.resolve(&identity).unwrap_err(),
            DenyReason::Misconfigured,
            "a dangling profile reference denies rather than granting a subset"
        );
    }

    #[test]
    fn oidc_claims_map_through_the_alias_mapping() {
        let resolver = PrincipalResolver::new(policy());
        let resolved = resolver
            .resolve(&oidc_identity(serde_json::json!(["zeroclaw-readers"])))
            .expect("mapped claim resolves");
        assert_eq!(
            resolved.principal.id.as_str(),
            "oidc:https%3A//sso.example.com/realms/main:alice"
        );
        assert!(resolved.grants.permits(Resource::Sessions, Verb::Read));
        assert!(!resolved.grants.permits(Resource::Cron, Verb::Execute));
    }

    #[test]
    fn oidc_multi_profile_union_is_order_independent() {
        let resolver = PrincipalResolver::new(policy());
        let ab = resolver
            .resolve(&oidc_identity(serde_json::json!([
                "zeroclaw-readers",
                "zeroclaw-ops"
            ])))
            .expect("resolves");
        let ba = resolver
            .resolve(&oidc_identity(serde_json::json!([
                "zeroclaw-ops",
                "zeroclaw-readers",
                "zeroclaw-ops"
            ])))
            .expect("resolves");
        assert_eq!(ab.grants, ba.grants, "deterministic monotonic union");
        assert!(ab.grants.permits(Resource::Sessions, Verb::Read));
        assert!(ab.grants.permits(Resource::Cron, Verb::Execute));
    }

    #[test]
    fn unmapped_claims_grant_nothing() {
        let resolver = PrincipalResolver::new(policy());
        assert_eq!(
            resolver
                .resolve(&oidc_identity(serde_json::json!(["guests"])))
                .unwrap_err(),
            DenyReason::NotEntitled
        );
        assert_eq!(
            resolver
                .resolve(&oidc_identity(serde_json::json!([])))
                .unwrap_err(),
            DenyReason::NotEntitled
        );
    }

    #[test]
    fn zitadel_object_keyed_roles_map() {
        let resolver = PrincipalResolver::new(policy());
        let resolved = resolver
            .resolve(&oidc_identity(serde_json::json!({
                "zeroclaw-readers": { "276": "org.example.com" }
            })))
            .expect("object-keyed roles resolve");
        assert!(resolved.grants.permits(Resource::Sessions, Verb::Read));
    }

    #[test]
    fn issuer_mismatch_with_alias_mapping_fails_closed() {
        let resolver = PrincipalResolver::new(policy());
        let mut identity = oidc_identity(serde_json::json!(["zeroclaw-readers"]));
        identity.subject = IdentitySubject::Oidc {
            issuer: "https://other-idp.example.com".into(),
            subject: "alice".into(),
        };
        assert_eq!(
            resolver.resolve(&identity).unwrap_err(),
            DenyReason::Misconfigured,
            "a provider cannot borrow another issuer's mapping"
        );
    }

    #[test]
    fn oidc_identity_without_provider_alias_fails_closed() {
        let resolver = PrincipalResolver::new(policy());
        let mut identity = oidc_identity(serde_json::json!(["zeroclaw-readers"]));
        identity.provider_alias = None;
        assert_eq!(
            resolver.resolve(&identity).unwrap_err(),
            DenyReason::Misconfigured
        );
    }

    #[test]
    fn service_identity_resolves_to_a_service_principal() {
        let resolver = PrincipalResolver::new(policy());
        let claims = serde_json::json!({ "realm_access": { "roles": ["zeroclaw-ops"] } });
        let serde_json::Value::Object(claims) = claims else {
            unreachable!()
        };
        let identity = AuthenticatedIdentity::new(
            IdentitySubject::Service {
                issuer: "https://sso.example.com/realms/main".into(),
                client_id: "reporting-batch".into(),
            },
            AuthMethod::Oidc,
        )
        .with_provider_alias("corp")
        .with_claims(claims);
        let resolved = resolver.resolve(&identity).expect("service resolves");
        assert_eq!(resolved.principal.actor, ActorKind::Service);
        assert_eq!(
            resolved.principal.id.as_str(),
            "svc:https%3A//sso.example.com/realms/main:reporting-batch"
        );
        assert!(resolved.grants.permits(Resource::Cron, Verb::Execute));
    }

    #[test]
    fn policy_replacement_bumps_the_generation_and_applies_immediately() {
        let resolver = PrincipalResolver::new(policy());
        let identity = AuthenticatedIdentity::new(
            IdentitySubject::Roster {
                principal_id: "alice".into(),
            },
            AuthMethod::Peercred,
        );
        let before = resolver.resolve(&identity).expect("resolves");
        assert_eq!(before.generation, 1);
        assert!(before.grants.permits(Resource::Cron, Verb::Execute));

        // Narrow alice to the reader profile only.
        let mut narrowed = policy();
        narrowed
            .roster
            .insert("alice".to_string(), vec!["reader".to_string()]);
        let new_generation = resolver.replace_policy(narrowed);
        assert_eq!(new_generation, 2);
        assert_eq!(resolver.generation(), 2);
        assert!(
            before.generation < resolver.generation(),
            "a stamped resolution is detectably stale after the swap"
        );

        let after = resolver.resolve(&identity).expect("resolves");
        assert_eq!(after.generation, 2);
        assert!(after.grants.permits(Resource::Sessions, Verb::Read));
        assert!(
            !after.grants.permits(Resource::Cron, Verb::Execute),
            "narrowed authorization applies without any reconnect"
        );
    }

    #[test]
    fn removing_a_roster_entry_revokes_at_the_next_resolution() {
        let resolver = PrincipalResolver::new(policy());
        let identity = AuthenticatedIdentity::new(
            IdentitySubject::Roster {
                principal_id: "alice".into(),
            },
            AuthMethod::Peercred,
        );
        assert!(resolver.resolve(&identity).is_ok());
        let mut without = policy();
        without.roster.remove("alice");
        resolver.replace_policy(without);
        assert_eq!(
            resolver.resolve(&identity).unwrap_err(),
            DenyReason::NotEntitled
        );
    }

    #[test]
    fn policy_compiles_from_config_sections() {
        use zeroclaw_config::schema::{OidcConfig, PermissionProfileConfig, UserConfig};
        let mut config = Config::default();
        config.permission_profiles.insert(
            "operator".to_string(),
            PermissionProfileConfig {
                allowed_tools: vec!["calculator".to_string()],
                grants: HashMap::from([(Resource::Sessions, vec![Verb::Read])]),
                ..PermissionProfileConfig::default()
            },
        );
        config.users.insert(
            "display-name".to_string(),
            UserConfig {
                principal_id: Some("alice".to_string()),
                uid: Some(1000),
                permission_profiles: vec!["operator".to_string()],
            },
        );
        config.oidc.insert(
            "corp".to_string(),
            OidcConfig {
                issuer: "https://sso.example.com".to_string(),
                claim_path: "groups".to_string(),
                profile_map: HashMap::from([("ops".to_string(), "operator".to_string())]),
            },
        );
        config.validate().expect("valid");

        let resolver = PrincipalResolver::from_config(&config);
        // The roster keys on the durable principal id, not the entry name.
        let identity = AuthenticatedIdentity::new(
            IdentitySubject::Roster {
                principal_id: "alice".into(),
            },
            AuthMethod::Peercred,
        );
        let resolved = resolver.resolve(&identity).expect("resolves");
        assert!(resolved.grants.permits(Resource::Sessions, Verb::Read));
        assert!(resolved.grants.may_use_tool("calculator"));
        assert!(!resolved.grants.may_use_tool("shell"));

        let by_name = AuthenticatedIdentity::new(
            IdentitySubject::Roster {
                principal_id: "display-name".into(),
            },
            AuthMethod::Peercred,
        );
        assert_eq!(
            resolver.resolve(&by_name).unwrap_err(),
            DenyReason::NotEntitled,
            "the display name is not an identity once principal_id is pinned"
        );
    }

    #[test]
    fn claim_path_walks_nested_flat_and_missing_shapes() {
        let claims = serde_json::json!({
            "realm_access": { "roles": ["a", "b"] },
            "groups": ["g1"],
            "plan": "pro",
        });
        let serde_json::Value::Object(claims) = claims else {
            unreachable!()
        };
        assert_eq!(claim_values(&claims, "realm_access.roles"), vec!["a", "b"]);
        assert_eq!(claim_values(&claims, "groups"), vec!["g1"]);
        assert_eq!(claim_values(&claims, "plan"), vec!["pro"]);
        assert!(claim_values(&claims, "missing.path").is_empty());
        assert!(claim_values(&claims, "plan.deeper").is_empty());
    }
}
