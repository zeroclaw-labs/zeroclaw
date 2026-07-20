//! Approval identity resolution seam.
//!
//! Maps a transport-derived [`ApprovalPrincipal`] to the approver groups it belongs
//! to, so the broker can enforce a policy's required-group membership and quorum.
//!
//! This is a PERMANENT seam, not a stopgap. Many approvals are channel-native acts
//! by a user known only by their channel identity - a paired gateway user, a forge
//! login - who may never hold a first-class auth account. The identity such a
//! principal carries is transport-DERIVED (built from the resolved/paired
//! connection, never from a client-supplied body), so it is a legitimate trust
//! anchor for granting approval rights. A future auth system AUGMENTS this by adding
//! another resolver alongside the config-backed one (a junction of identity
//! sources); it does not replace channel-provided identities. That resolver is where
//! canonical-identity LINKING belongs: one person's several channel identities (e.g.
//! `github:octocat`, `discord:123...`, `email:user@example.invalid`) map to a
//! single canonical user, so any of them resolves to the same groups. Until then,
//! the config-backed resolver can grant a group to each channel identity directly.

use zeroclaw_config::schema::SopApprovalConfig;

use super::principal::ApprovalPrincipal;

/// Resolve an approval principal to the approver groups it is a member of, against
/// the live `[sop.approval]` config passed in at use-time (the single source of
/// truth - never a cloned copy that could drift on reload).
pub trait ApprovalIdentityResolver: Send + Sync {
    /// The groups this principal belongs to under `cfg` (may be empty).
    fn groups_for(&self, cfg: &SopApprovalConfig, principal: &ApprovalPrincipal) -> Vec<String>;

    /// Whether the principal is a member of `group` under `cfg`.
    fn is_member(
        &self,
        cfg: &SopApprovalConfig,
        principal: &ApprovalPrincipal,
        group: &str,
    ) -> bool {
        self.groups_for(cfg, principal).iter().any(|g| g == group)
    }
}

/// Config-backed resolver over `[sop.approval].groups.*.members`. Stateless: it
/// reads the config handed to it on each call, so there is no second copy of the
/// membership map to go stale. A member entry may be **source-qualified**
/// (`<source>:<identity>`, e.g. `http:<subject>`, `agent:<alias>`) to grant rights
/// on ONE transport only - so a subject on the gateway and the same string on the
/// agent tool do not collide - or a **bare** identity to grant it from any source.
/// A principal with no identity (e.g. the system tick) belongs to no group.
///
/// NOTE on the identity each surface actually PRODUCES today. The gateway HTTP and
/// WS paths both produce the paired-token hash (a stable per-device subject); they
/// share it, so for quorum they collapse to ONE canonical `gateway` voter (see
/// [`super::principal::ApprovalPrincipal::voter_key`]). The agent tool produces the
/// agent alias. The loopback CLI (`zeroclaw sop approve`) is currently ANONYMOUS -
/// the admin path builds `ApprovalPrincipal::cli(None)` - so a `cli:<user>` group
/// member is NOT satisfiable yet; it is reserved for a future CLI that forwards a
/// trusted local identity, so do not gate a policy on `cli:<user>` expecting the
/// current CLI to meet it. A future auth resolver (the documented junction seam) is
/// where a per-PERSON canonical identity - linking a user's several device/channel
/// subjects to one account - belongs; until then membership is per-subject.
pub struct LocalConfigApprovalIdentityResolver;

impl ApprovalIdentityResolver for LocalConfigApprovalIdentityResolver {
    fn groups_for(&self, cfg: &SopApprovalConfig, principal: &ApprovalPrincipal) -> Vec<String> {
        let Some(id) = principal.identity.as_deref() else {
            return Vec::new();
        };
        // A member is matched by its source-qualified identity (precise, no
        // cross-channel collision) OR its bare identity (an any-source grant).
        let qualified = format!("{}:{}", principal.source_label(), id);
        let mut groups: Vec<String> = Vec::new();
        for (group_name, group) in &cfg.groups {
            let is_member = group.members.iter().any(|m| m == &qualified || m == id);
            if is_member && !groups.iter().any(|g| g == group_name) {
                groups.push(group_name.clone());
            }
        }
        groups
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use zeroclaw_config::schema::ApprovalGroupConfig;

    fn cfg_with(groups: &[(&str, &[&str])]) -> SopApprovalConfig {
        let mut map = HashMap::new();
        for (name, members) in groups {
            map.insert(
                name.to_string(),
                ApprovalGroupConfig {
                    members: members.iter().map(|m| m.to_string()).collect(),
                },
            );
        }
        SopApprovalConfig {
            groups: map,
            ..Default::default()
        }
    }

    #[test]
    fn bare_member_matches_any_source() {
        let cfg = cfg_with(&[("release", &["alice"])]);
        let r = LocalConfigApprovalIdentityResolver;
        assert!(r.is_member(
            &cfg,
            &ApprovalPrincipal::cli(Some("alice".into())),
            "release"
        ));
        assert!(r.is_member(
            &cfg,
            &ApprovalPrincipal::http(Some("alice".into())),
            "release"
        ));
    }

    #[test]
    fn source_qualified_member_scopes_to_one_transport() {
        // `http:alice` grants to the HTTP alice but NOT the CLI alice - so a channel
        // identity does not collide with a same-named identity on another source.
        let cfg = cfg_with(&[("release", &["http:alice"])]);
        let r = LocalConfigApprovalIdentityResolver;
        assert!(r.is_member(
            &cfg,
            &ApprovalPrincipal::http(Some("alice".into())),
            "release"
        ));
        assert!(!r.is_member(
            &cfg,
            &ApprovalPrincipal::cli(Some("alice".into())),
            "release"
        ));
    }

    #[test]
    fn resolves_multiple_groups() {
        let cfg = cfg_with(&[("release", &["alice"]), ("sre", &["alice"])]);
        let r = LocalConfigApprovalIdentityResolver;
        let mut groups = r.groups_for(&cfg, &ApprovalPrincipal::cli(Some("alice".into())));
        groups.sort();
        assert_eq!(groups, vec!["release".to_string(), "sre".to_string()]);
    }

    #[test]
    fn unknown_or_identityless_principal_has_no_groups() {
        let cfg = cfg_with(&[("release", &["alice"])]);
        let r = LocalConfigApprovalIdentityResolver;
        assert!(
            r.groups_for(&cfg, &ApprovalPrincipal::cli(Some("mallory".into())))
                .is_empty()
        );
        assert!(r.groups_for(&cfg, &ApprovalPrincipal::system()).is_empty());
    }
}
