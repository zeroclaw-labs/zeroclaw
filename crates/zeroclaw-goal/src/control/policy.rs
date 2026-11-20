//! Prospective Goal-policy classification for daemon reload cutovers.
//!
//! The classifier is deliberately pure: it translates a durable Goal record
//! plus a fully loaded successor configuration into an exact revocation
//! target.  SQLite remains responsible for the all-or-nothing cancellation;
//! adapters and reload orchestration must not reconstruct policy from route
//! text on their own.

use anyhow::{Result, ensure};
use std::collections::{HashMap, HashSet};

use crate::{
    control::{GoalIngressContext, GoalSessionKey, GoalSurface},
    task::{GoalPolicyTarget, GoalTaskRegistry, GoalTransitionResult},
};
use zeroclaw_api::task::{TaskKind, TaskRecord, TaskStatus};

/// Why a successor configuration revokes a currently nonterminal Goal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoalPolicyRevocation {
    GoalModeDisabled,
    AgentUnavailable,
    MatrixChannelUnavailable,
    UnsupportedRoute,
}

/// Operation-local projection of canonical Config eligibility, not a live-policy
/// cache. Build it anew at the owning admission/cutover boundary; never retain it
/// as authority across operations. Equality compares effective permissions,
/// ignoring ordering, duplicate bindings and unreachable disabled entries.
#[derive(Debug, Clone, Eq)]
pub struct GoalPolicySnapshot {
    pub goal_mode_enabled: bool,
    pub agents: HashMap<String, GoalAgentPolicySnapshot>,
    pub matrix_channels: HashMap<String, bool>,
}

/// The subset of an agent's validated policy relevant to Goal continuation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoalAgentPolicySnapshot {
    pub enabled: bool,
    pub channels: Vec<String>,
}

#[derive(PartialEq, Eq)]
struct GoalPolicyPermissions<'a> {
    agents: HashSet<&'a str>,
    matrix: HashSet<(&'a str, &'a str)>,
}

impl GoalPolicySnapshot {
    /// Derive a short-lived eligibility input from the owning canonical Config.
    /// This does not validate or publish configuration and does not grant ingress
    /// authentication, principal authority, or execution admission.
    pub fn from_config(config: &zeroclaw_config::schema::Config) -> Self {
        Self {
            goal_mode_enabled: config.goal.enabled,
            agents: config
                .agents
                .iter()
                .map(|(alias, agent)| {
                    (
                        alias.clone(),
                        GoalAgentPolicySnapshot {
                            enabled: agent.enabled,
                            channels: agent
                                .channels
                                .iter()
                                .map(|key| key.as_str().to_owned())
                                .collect(),
                        },
                    )
                })
                .collect(),
            matrix_channels: config
                .channels
                .matrix
                .iter()
                .map(|(alias, channel)| (alias.clone(), channel.enabled))
                .collect(),
        }
    }

    /// Whether this candidate adds usable Goal authorization relative to the
    /// previous policy. The caller must keep execution admission closed until
    /// runtime replacement; this pure result performs no cutover itself.
    /// Budget/verifier/presentation changes and membership reordering are inert.
    pub fn expands_from(&self, previous: &Self) -> bool {
        if !self.goal_mode_enabled {
            return false;
        }
        if !previous.goal_mode_enabled {
            return true;
        }
        let current = self.permissions();
        let old = previous.permissions();
        !current.agents.is_subset(&old.agents) || !current.matrix.is_subset(&old.matrix)
    }

    fn permissions(&self) -> GoalPolicyPermissions<'_> {
        let mut agents = HashSet::new();
        let mut matrix = HashSet::new();
        if self.goal_mode_enabled {
            for (alias, agent) in &self.agents {
                if !agent.enabled {
                    continue;
                }
                agents.insert(alias.as_str());
                for channel in &agent.channels {
                    if let Some(channel_alias) = channel.strip_prefix("matrix.")
                        && self.matrix_channels.get(channel_alias) == Some(&true)
                    {
                        matrix.insert((alias.as_str(), channel_alias));
                    }
                }
            }
        }
        GoalPolicyPermissions { agents, matrix }
    }
}

impl PartialEq for GoalPolicySnapshot {
    fn eq(&self, other: &Self) -> bool {
        self.goal_mode_enabled == other.goal_mode_enabled
            && self.permissions() == other.permissions()
    }
}

/// One observed Goal and the policy consequence selected from a prospective
/// configuration. `Keep` never grants new authority; it only means the Goal
/// remains subject to ordinary restart pause and later explicit resume.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GoalPolicyDecision {
    Keep,
    Revoke {
        target: GoalPolicyTarget,
        reason: GoalPolicyRevocation,
    },
}

/// Classify a nonterminal session-bound Goal against a successor configuration.
///
/// V1 only supports Matrix and the shared IPC/WSS ZeroCode scope. Unsupported
/// routes revoke eligibility; malformed required task/session/epoch identity
/// returns an error even when the candidate globally disables Goal mode.
/// Neither Keep nor this classification grants control authority.
pub fn classify_goal_policy(
    successor: &GoalPolicySnapshot,
    task: &TaskRecord,
) -> Result<GoalPolicyDecision> {
    let target = policy_target(task)?;
    if let Some(reason) = scope_revocation(
        successor,
        &task.agent,
        task.originator_route.as_deref(),
        &target.session_id,
        None,
    )? {
        return Ok(revoked(target, reason));
    }
    Ok(GoalPolicyDecision::Keep)
}

/// Apply the same scope eligibility used for persisted Goals to trusted ingress,
/// without manufacturing a TaskRecord. None means policy-eligible only: the host
/// must still revalidate its principal/session and obtain execution admission.
///
/// # Errors
///
/// Returns an error for a malformed canonical ingress key or agent identity.
pub fn classify_goal_ingress_policy(
    policy: &GoalPolicySnapshot,
    ingress: &GoalIngressContext,
) -> Result<Option<GoalPolicyRevocation>> {
    ingress.session_key().validate_canonical()?;
    scope_revocation(
        policy,
        ingress.agent(),
        Some(ingress.route()),
        &ingress.session_key().durable_id(),
        Some(ingress.surface()),
    )
}

fn scope_revocation(
    successor: &GoalPolicySnapshot,
    agent_alias: &str,
    route: Option<&str>,
    session_id: &str,
    surface: Option<GoalSurface>,
) -> Result<Option<GoalPolicyRevocation>> {
    ensure!(
        !agent_alias.trim().is_empty() && agent_alias.trim() == agent_alias,
        "Goal policy has a noncanonical agent identity"
    );
    if !successor.goal_mode_enabled {
        return Ok(Some(GoalPolicyRevocation::GoalModeDisabled));
    }
    let Some(agent) = successor.agents.get(agent_alias) else {
        return Ok(Some(GoalPolicyRevocation::AgentUnavailable));
    };
    if !agent.enabled {
        return Ok(Some(GoalPolicyRevocation::AgentUnavailable));
    }
    let Some(route) = route else {
        return Ok(Some(GoalPolicyRevocation::UnsupportedRoute));
    };
    if let Some(raw_session_id) = route.strip_prefix("zerocode:") {
        let valid = surface != Some(GoalSurface::Matrix)
            && GoalSessionKey::zero_code(raw_session_id)
                .is_ok_and(|key| key.durable_id() == session_id);
        return Ok((!valid).then_some(GoalPolicyRevocation::UnsupportedRoute));
    }
    let Some((alias, reply_target)) = matrix_route(route) else {
        return Ok(Some(GoalPolicyRevocation::UnsupportedRoute));
    };
    if surface == Some(GoalSurface::ZeroCode) {
        return Ok(Some(GoalPolicyRevocation::UnsupportedRoute));
    }
    GoalSessionKey::matrix(session_id)?;
    let matrix_enabled = successor
        .matrix_channels
        .get(alias)
        .copied()
        .unwrap_or(false);
    let required_channel = format!("matrix.{alias}");
    if !matrix_enabled
        || !agent
            .channels
            .iter()
            .any(|channel| channel.as_str() == required_channel)
        || reply_target.trim().is_empty()
    {
        return Ok(Some(GoalPolicyRevocation::MatrixChannelUnavailable));
    }
    Ok(None)
}

/// Classify the complete observed set before its first durable effect. Selection
/// is deterministic by exact task ID; duplicate identities are errors even if
/// the repeated records would otherwise remain eligible.
///
/// # Errors
///
/// Returns an error if any required identity, kind/status or duplicate task fails
/// validation. No rows are silently skipped and no cancellation is performed.
pub fn select_goal_policy_revocations(
    successor: &GoalPolicySnapshot,
    observed: &[TaskRecord],
) -> Result<Vec<(GoalPolicyTarget, GoalPolicyRevocation)>> {
    let mut seen = HashSet::with_capacity(observed.len());
    let mut revoked = Vec::new();
    for task in observed {
        ensure!(
            seen.insert(task.id.as_str()),
            "Goal policy observation repeats a task identity"
        );
        if let GoalPolicyDecision::Revoke { target, reason } =
            classify_goal_policy(successor, task)?
        {
            revoked.push((target, reason));
        }
    }
    revoked.sort_unstable_by(|(left, _), (right, _)| left.task_id.cmp(&right.task_id));
    Ok(revoked)
}

/// Cancel every nonterminal Goal revoked by a fully validated successor policy.
///
/// Classification precedes one guarded SQLite transaction. If a record changes
/// after observation, the transaction returns `Stale` and the caller must keep
/// the old runtime and worker ownership intact rather than applying a partial
/// cutover.
pub async fn revoke_goals_under_policy(
    registry: &dyn GoalTaskRegistry,
    successor: &GoalPolicySnapshot,
) -> Result<Vec<(GoalPolicyTarget, GoalPolicyRevocation)>> {
    let observed = registry.list_nonterminal_session_goals().await?;
    let revoked = select_goal_policy_revocations(successor, &observed)?;
    if revoked.is_empty() {
        return Ok(Vec::new());
    }
    let targets = revoked
        .iter()
        .map(|(target, _)| target.clone())
        .collect::<Vec<_>>();
    match registry.cancel_policy_targets(&targets).await? {
        GoalTransitionResult::Applied => Ok(revoked),
        GoalTransitionResult::Stale => anyhow::bail!("Goal policy targets changed during cutover"),
        GoalTransitionResult::Missing => {
            anyhow::bail!("Goal policy target disappeared during cutover")
        }
    }
}

fn revoked(target: GoalPolicyTarget, reason: GoalPolicyRevocation) -> GoalPolicyDecision {
    GoalPolicyDecision::Revoke { target, reason }
}

fn policy_target(task: &TaskRecord) -> Result<GoalPolicyTarget> {
    ensure!(
        matches!(task.status, TaskStatus::Running | TaskStatus::Paused),
        "policy classification requires a nonterminal Goal"
    );
    ensure!(
        task.kind == TaskKind::Goal,
        "policy classification requires a Goal task"
    );
    let session_id = task
        .session_id
        .as_deref()
        .ok_or_else(|| anyhow::Error::msg("Goal policy target is missing a session binding"))?;
    let target = GoalPolicyTarget {
        task_id: task.id.clone(),
        session_id: session_id.to_owned(),
        execution_epoch: task.execution_epoch,
    };
    target.validate()?;
    // Validate the persisted durable key independently of the route/policy.
    // A malformed required binding cannot disappear behind global disablement.
    if let Some(raw_session_id) = session_id.strip_prefix("rpc_") {
        GoalSessionKey::zero_code(raw_session_id)?;
    } else {
        GoalSessionKey::matrix(session_id)?;
    }
    Ok(target)
}

fn matrix_route(route: &str) -> Option<(&str, &str)> {
    let mut fields = route.splitn(3, ':');
    (fields.next()? == "matrix").then_some(())?;
    let alias = fields.next()?;
    let reply_target = fields.next()?;
    (!alias.trim().is_empty() && alias.trim() == alias && !reply_target.trim().is_empty())
        .then_some((alias, reply_target))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn task(route: &str) -> TaskRecord {
        let session_id = route
            .strip_prefix("zerocode:")
            .filter(|raw_session_id| !raw_session_id.is_empty())
            .map(|raw_session_id| format!("rpc_{raw_session_id}"))
            .unwrap_or_else(|| "matrix_session".into());
        TaskRecord {
            id: "goal-1".into(),
            kind: TaskKind::Goal,
            agent: "main".into(),
            status: TaskStatus::Running,
            owner_pid: 1,
            owner_boot_id: "boot".into(),
            heartbeat_at: None,
            depth: 0,
            parent_id: None,
            originator_route: Some(route.into()),
            originator_chain: Vec::new(),
            delivered: false,
            idem_key: None,
            principal_id: None,
            session_id: Some(session_id),
            execution_epoch: 3,
            started_at: "2026-09-01T00:00:00Z".into(),
            finished_at: None,
        }
    }

    fn policy() -> GoalPolicySnapshot {
        GoalPolicySnapshot {
            goal_mode_enabled: true,
            agents: HashMap::from([(
                "main".to_owned(),
                GoalAgentPolicySnapshot {
                    enabled: true,
                    channels: vec!["matrix.main".to_owned()],
                },
            )]),
            matrix_channels: HashMap::from([("main".to_owned(), true)]),
        }
    }

    #[test]
    fn successor_policy_revokes_only_removed_goal_scopes() {
        let mut successor = policy();
        assert_eq!(
            classify_goal_policy(&successor, &task("matrix:main:!room:example")).unwrap(),
            GoalPolicyDecision::Keep
        );
        assert_eq!(
            classify_goal_policy(&successor, &task("zerocode:local-session")).unwrap(),
            GoalPolicyDecision::Keep
        );
        successor.matrix_channels.insert("main".into(), false);
        assert!(matches!(
            classify_goal_policy(&successor, &task("matrix:main:!room:example")).unwrap(),
            GoalPolicyDecision::Revoke {
                reason: GoalPolicyRevocation::MatrixChannelUnavailable,
                ..
            }
        ));
        assert_eq!(
            classify_goal_policy(&successor, &task("zerocode:local-session")).unwrap(),
            GoalPolicyDecision::Keep,
            "removing Matrix must not revoke a ZeroCode Goal"
        );
    }

    #[test]
    fn disabled_goal_mode_and_unknown_routes_fail_closed() {
        let mut successor = policy();
        successor.goal_mode_enabled = false;
        assert!(matches!(
            classify_goal_policy(&successor, &task("zerocode:local-session")).unwrap(),
            GoalPolicyDecision::Revoke {
                reason: GoalPolicyRevocation::GoalModeDisabled,
                ..
            }
        ));

        let successor = policy();
        assert!(matches!(
            classify_goal_policy(&successor, &task("telegram:main:chat")).unwrap(),
            GoalPolicyDecision::Revoke {
                reason: GoalPolicyRevocation::UnsupportedRoute,
                ..
            }
        ));

        for route in [
            "zerocode:",
            "zerocode: padded",
            "zerocode:local-session:extra",
        ] {
            // An unsupported route is different from a malformed required
            // durable key. Keep the latter valid while testing the former.
            let mut unsupported = task("zerocode:local-session");
            unsupported.originator_route = Some(route.to_owned());
            assert!(matches!(
                classify_goal_policy(&successor, &unsupported).unwrap(),
                GoalPolicyDecision::Revoke {
                    reason: GoalPolicyRevocation::UnsupportedRoute,
                    ..
                }
            ));
        }

        let mut mismatched = task("zerocode:local-session");
        mismatched.session_id = Some("rpc_other-session".to_owned());
        assert!(matches!(
            classify_goal_policy(&successor, &mismatched).unwrap(),
            GoalPolicyDecision::Revoke {
                reason: GoalPolicyRevocation::UnsupportedRoute,
                ..
            }
        ));
    }

    #[test]
    fn required_identity_errors_precede_disabled_policy() {
        let valid = task("zerocode:opaque_session-42");
        let mut malformed = Vec::new();
        for id in ["", " padded", "padded "] {
            let mut row = valid.clone();
            row.id = id.into();
            malformed.push(row);
        }
        for session in [
            None,
            Some(""),
            Some("rpc_ padded"),
            Some("rpc_"),
            Some("other_key"),
        ] {
            let mut row = valid.clone();
            row.session_id = session.map(str::to_owned);
            malformed.push(row);
        }
        for epoch in [0, -1] {
            let mut row = valid.clone();
            row.execution_epoch = epoch;
            malformed.push(row);
        }
        for alias in ["", " main", "main "] {
            let mut row = valid.clone();
            row.agent = alias.into();
            malformed.push(row);
        }
        let mut terminal = valid.clone();
        terminal.status = TaskStatus::Completed;
        malformed.push(terminal);
        for enabled in [true, false] {
            let mut successor = policy();
            successor.goal_mode_enabled = enabled;
            for row in &malformed {
                assert!(classify_goal_policy(&successor, row).is_err());
                assert!(
                    select_goal_policy_revocations(&successor, &[valid.clone(), row.clone()])
                        .is_err()
                );
            }
        }
        let mut opaque = valid;
        opaque.id = "opaque:task/identity".into();
        opaque.execution_epoch = i64::MAX;
        opaque.principal_id = Some("opaque principal:retain exactly".into());
        let original = serde_json::to_value(&opaque).unwrap();
        assert_eq!(
            classify_goal_policy(&policy(), &opaque).unwrap(),
            GoalPolicyDecision::Keep
        );
        assert_eq!(serde_json::to_value(opaque).unwrap(), original);
    }

    #[test]
    fn trusted_ingress_and_records_share_policy_without_principal_authority() {
        use crate::control::GoalIngressPrincipal;

        for (route, key, principal) in [
            (
                "matrix:main:!opaque:fixture/thread:42",
                GoalSessionKey::matrix("matrix_opaque_sender").unwrap(),
                GoalIngressPrincipal::Matrix {
                    raw_mxid: "@fixture:example.invalid".into(),
                },
            ),
            (
                "zerocode:opaque_session-42",
                GoalSessionKey::zero_code("opaque_session-42").unwrap(),
                GoalIngressPrincipal::ZeroCode {
                    tui_id: "fixture-client".into(),
                },
            ),
        ] {
            let ingress =
                GoalIngressContext::trusted(key.clone(), "main", route, principal).unwrap();
            let mut row = task(route);
            row.session_id = Some(key.durable_id());
            row.principal_id = Some("presentation is not a policy grant".into());
            let mut policies = vec![(policy(), None)];
            let mut global_off = policy();
            global_off.goal_mode_enabled = false;
            policies.push((global_off, Some(GoalPolicyRevocation::GoalModeDisabled)));
            let mut missing = policy();
            missing.agents.remove("main");
            policies.push((missing, Some(GoalPolicyRevocation::AgentUnavailable)));
            let mut agent_off = policy();
            agent_off.agents.get_mut("main").unwrap().enabled = false;
            policies.push((agent_off, Some(GoalPolicyRevocation::AgentUnavailable)));
            let matrix_revocation = (key.surface() == GoalSurface::Matrix)
                .then_some(GoalPolicyRevocation::MatrixChannelUnavailable);
            let mut channel_off = policy();
            channel_off.matrix_channels.insert("main".into(), false);
            policies.push((channel_off, matrix_revocation));
            let mut channel_removed = policy();
            channel_removed.matrix_channels.clear();
            policies.push((channel_removed, matrix_revocation));
            let mut binding_removed = policy();
            binding_removed
                .agents
                .get_mut("main")
                .unwrap()
                .channels
                .clear();
            policies.push((binding_removed, matrix_revocation));
            for (candidate, expected) in policies {
                let record_reason = match classify_goal_policy(&candidate, &row).unwrap() {
                    GoalPolicyDecision::Keep => None,
                    GoalPolicyDecision::Revoke { reason, .. } => Some(reason),
                };
                assert_eq!(record_reason, expected);
                assert_eq!(
                    classify_goal_ingress_policy(&candidate, &ingress).unwrap(),
                    expected
                );
            }
            assert_eq!(
                classify_goal_ingress_policy(&policy(), &ingress).unwrap(),
                None
            );
        }
    }

    #[test]
    fn trusted_surface_cannot_be_reinterpreted_from_route_text() {
        use crate::control::GoalIngressPrincipal;

        let ingress = GoalIngressContext::trusted(
            GoalSessionKey::zero_code("session").unwrap(),
            "main",
            "matrix:main:!room:example.invalid",
            GoalIngressPrincipal::ZeroCode {
                tui_id: "fixture-client".into(),
            },
        )
        .unwrap();
        assert_eq!(
            classify_goal_ingress_policy(&policy(), &ingress).unwrap(),
            Some(GoalPolicyRevocation::UnsupportedRoute)
        );
        for route in [
            "matrix:main:   ",
            "matrix: main:!room:example.invalid",
            "matrix::reply",
        ] {
            let row = task(route);
            assert!(matches!(
                classify_goal_policy(&policy(), &row).unwrap(),
                GoalPolicyDecision::Revoke {
                    reason: GoalPolicyRevocation::UnsupportedRoute,
                    ..
                }
            ));
        }
    }

    #[test]
    fn complete_selection_preserves_unaffected_rows_and_orders_exact_targets() {
        let mut successor = policy();
        successor.agents.insert(
            "unaffected".into(),
            GoalAgentPolicySnapshot {
                enabled: true,
                channels: vec![],
            },
        );
        successor.matrix_channels.clear();
        let mut running = task("matrix:main:!room:example.invalid");
        running.id = "z-running".into();
        let mut paused = running.clone();
        paused.id = "a-paused".into();
        paused.status = TaskStatus::Paused;
        let mut unaffected = task("zerocode:unaffected-session");
        unaffected.id = "unaffected".into();
        unaffected.agent = "unaffected".into();
        let rows = vec![running.clone(), unaffected, paused.clone()];
        let before = serde_json::to_value(&rows).unwrap();
        let selected = select_goal_policy_revocations(&successor, &rows).unwrap();
        assert_eq!(
            selected
                .iter()
                .map(|(target, _)| target.task_id.as_str())
                .collect::<Vec<_>>(),
            vec!["a-paused", "z-running"]
        );
        assert_eq!(selected[0].0, policy_target(&paused).unwrap());
        assert_eq!(selected[1].0, policy_target(&running).unwrap());
        let reverse: Vec<_> = rows.iter().rev().cloned().collect();
        assert_eq!(
            select_goal_policy_revocations(&successor, &reverse).unwrap(),
            selected
        );
        assert_eq!(serde_json::to_value(rows).unwrap(), before);
    }

    #[test]
    fn repeated_task_observations_and_conflicting_targets_fail_closed() {
        let valid = task("zerocode:local-session");
        for (session, epoch) in [
            ("rpc_local-session", 3),
            ("rpc_other-session", 3),
            ("rpc_local-session", 4),
        ] {
            let mut repeated = valid.clone();
            repeated.session_id = Some(session.into());
            repeated.execution_epoch = epoch;
            // Even duplicate Keep observations invalidate the complete set.
            assert!(select_goal_policy_revocations(&policy(), &[valid.clone(), repeated]).is_err());
            assert!(
                GoalPolicyTarget::validate_batch(&[
                    policy_target(&valid).unwrap(),
                    GoalPolicyTarget {
                        task_id: valid.id.clone(),
                        session_id: session.into(),
                        execution_epoch: epoch,
                    },
                ])
                .is_err()
            );
        }
        let mut distinct = policy_target(&valid).unwrap();
        distinct.task_id = "a-different-task".into();
        GoalPolicyTarget::validate_batch(&[policy_target(&valid).unwrap(), distinct]).unwrap();
        GoalPolicyTarget::validate_batch(&[]).unwrap();
    }

    #[test]
    fn policy_membership_ignores_order_duplicates_and_irrelevant_configuration() {
        use zeroclaw_config::{
            providers::ChannelRef,
            schema::{AliasedAgentConfig, Config, MatrixConfig},
        };

        let mut config = Config::default();
        config.goal.enabled = true;
        config.agents.clear();
        config.agents.insert(
            "main".into(),
            AliasedAgentConfig {
                enabled: true,
                channels: vec![ChannelRef::new("matrix.main")],
                ..Default::default()
            },
        );
        config.channels.matrix.insert(
            "main".into(),
            MatrixConfig {
                enabled: true,
                ..Default::default()
            },
        );
        let old = GoalPolicySnapshot::from_config(&config);
        assert_eq!(
            classify_goal_policy(&old, &task("matrix:main:!room:example.invalid")).unwrap(),
            GoalPolicyDecision::Keep
        );
        config.goal.default_token_limit = Some(71);
        config.goal.default_cost_limit_usd = Some(0.25);
        config.wss.enabled = !config.wss.enabled;
        config.agents.get_mut("main").unwrap().channels = vec![
            ChannelRef::new("telegram.unrelated"),
            ChannelRef::new("matrix.main"),
            ChannelRef::new("matrix.main"),
        ];
        let candidate = GoalPolicySnapshot::from_config(&config);
        assert_eq!(candidate, old);
        assert!(!candidate.expands_from(&old));
        assert!(!old.expands_from(&candidate));
        assert_eq!(
            classify_goal_policy(&candidate, &task("zerocode:local-session")).unwrap(),
            GoalPolicyDecision::Keep
        );
    }

    #[test]
    fn policy_expansion_tracks_new_effective_agent_and_matrix_membership() {
        let old = policy();
        let mut off = old.clone();
        off.goal_mode_enabled = false;
        assert!(old.expands_from(&off));
        assert!(!off.expands_from(&old));
        let mut expanded = old.clone();
        expanded.agents.insert(
            "added".into(),
            GoalAgentPolicySnapshot {
                enabled: true,
                channels: vec![],
            },
        );
        assert!(expanded.expands_from(&old));
        assert!(!old.expands_from(&expanded));
        let mut agent_off = old.clone();
        agent_off.agents.get_mut("main").unwrap().enabled = false;
        assert!(old.expands_from(&agent_off));
        assert!(!agent_off.expands_from(&old));
        let mut channel_off = old.clone();
        channel_off.matrix_channels.insert("main".into(), false);
        assert!(old.expands_from(&channel_off));
        assert!(!channel_off.expands_from(&old));
        let mut binding_off = old.clone();
        binding_off.agents.get_mut("main").unwrap().channels.clear();
        assert!(old.expands_from(&binding_off));
        assert!(!binding_off.expands_from(&old));
        let mut mixed = old.clone();
        mixed.matrix_channels.insert("main".into(), false);
        mixed.matrix_channels.insert("replacement".into(), true);
        mixed.agents.get_mut("main").unwrap().channels = vec!["matrix.replacement".into()];
        assert_ne!(mixed, old);
        assert!(mixed.expands_from(&old));
        assert!(old.expands_from(&mixed));
        let mut inaccessible = off.clone();
        inaccessible.agents = expanded.agents;
        inaccessible.matrix_channels.clear();
        assert_eq!(inaccessible, off);
        assert!(!inaccessible.expands_from(&off));
        let mut unused = old.clone();
        unused.matrix_channels.insert("unbound".into(), true);
        assert_eq!(unused, old);
        assert!(!unused.expands_from(&old));
    }

    #[tokio::test]
    async fn unsupported_registry_policy_defaults_never_fabricate_success() {
        let registry = crate::test_support::Registry::default();
        assert!(registry.list_nonterminal_session_goals().await.is_err());
        assert!(registry.cancel_policy_targets(&[]).await.is_err());
        assert!(
            registry
                .cancel_policy_targets(&[policy_target(&task("zerocode:local-session")).unwrap()])
                .await
                .is_err()
        );
        assert!(
            revoke_goals_under_policy(&registry, &policy())
                .await
                .is_err()
        );
        let state = registry.0.lock().unwrap();
        assert!(state.task.is_none());
        assert!(state.intents.is_empty());
        assert_eq!(state.pending_admissions, 0);
    }
}
