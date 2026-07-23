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
/// Channel principals are stricter: they match only
/// `channel:<channel-key>:<sender>`, never `channel:<sender>` or a bare sender, so
/// same-looking platform ids from different channel aliases cannot collide. A
/// principal with no identity (e.g. the system tick) belongs to no group.
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
        // Non-channel members match by source-qualified or bare identity. Channel
        // members must include the channel key to avoid cross-alias sender collisions.
        let member_keys = match principal.source {
            super::principal::ApprovalSource::Channel => {
                let Some(channel) = principal.channel.as_deref().filter(|c| !c.is_empty()) else {
                    return Vec::new();
                };
                vec![format!("channel:{channel}:{id}")]
            }
            _ => vec![
                format!("{}:{}", principal.source_label(), id),
                id.to_string(),
            ],
        };
        let mut groups: Vec<String> = Vec::new();
        for (group_name, group) in &cfg.groups {
            let is_member = group
                .members
                .iter()
                .any(|m| member_keys.iter().any(|key| m == key));
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
        let cfg = cfg_with(&[("release", &["ZeroClawOperator"])]);
        let r = LocalConfigApprovalIdentityResolver;
        assert!(r.is_member(
            &cfg,
            &ApprovalPrincipal::cli(Some("ZeroClawOperator".into())),
            "release"
        ));
        assert!(r.is_member(
            &cfg,
            &ApprovalPrincipal::http(Some("ZeroClawOperator".into())),
            "release"
        ));
    }

    #[test]
    fn source_qualified_member_scopes_to_one_transport() {
        // `http:ZeroClawOperator` grants to the HTTP ZeroClawOperator but NOT the CLI ZeroClawOperator - so a channel
        // identity does not collide with a same-named identity on another source.
        let cfg = cfg_with(&[("release", &["http:ZeroClawOperator"])]);
        let r = LocalConfigApprovalIdentityResolver;
        assert!(r.is_member(
            &cfg,
            &ApprovalPrincipal::http(Some("ZeroClawOperator".into())),
            "release"
        ));
        assert!(!r.is_member(
            &cfg,
            &ApprovalPrincipal::cli(Some("ZeroClawOperator".into())),
            "release"
        ));
    }

    #[test]
    fn channel_member_is_scoped_to_channel_key() {
        let cfg = cfg_with(&[("release", &["channel:discord.ops:123"])]);
        let r = LocalConfigApprovalIdentityResolver;
        assert!(r.is_member(
            &cfg,
            &ApprovalPrincipal::channel("discord.ops".into(), Some("123".into())),
            "release"
        ));
        assert!(!r.is_member(
            &cfg,
            &ApprovalPrincipal::channel("slack.ops".into(), Some("123".into())),
            "release"
        ));

        let legacy_unscoped = cfg_with(&[("release", &["channel:123", "123"])]);
        assert!(
            r.groups_for(
                &legacy_unscoped,
                &ApprovalPrincipal::channel("discord.ops".into(), Some("123".into()))
            )
            .is_empty(),
            "channel principals must not match unscoped channel or bare sender ids"
        );
    }

    #[test]
    fn resolves_multiple_groups() {
        let cfg = cfg_with(&[
            ("release", &["ZeroClawOperator"]),
            ("sre", &["ZeroClawOperator"]),
        ]);
        let r = LocalConfigApprovalIdentityResolver;
        let mut groups = r.groups_for(
            &cfg,
            &ApprovalPrincipal::cli(Some("ZeroClawOperator".into())),
        );
        groups.sort();
        assert_eq!(groups, vec!["release".to_string(), "sre".to_string()]);
    }

    #[test]
    fn unknown_or_identityless_principal_has_no_groups() {
        let cfg = cfg_with(&[("release", &["ZeroClawOperator"])]);
        let r = LocalConfigApprovalIdentityResolver;
        assert!(
            r.groups_for(&cfg, &ApprovalPrincipal::cli(Some("ZeroClawAgent".into())))
                .is_empty()
        );
        assert!(r.groups_for(&cfg, &ApprovalPrincipal::system()).is_empty());
    }
}
