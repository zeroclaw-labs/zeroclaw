//! Approval broker (EPIC G, Phase 5).
//!
//! A thin authorization layer that WRAPS the single gate-clearing chokepoint
//! ([`SopEngine::resolve_gate`]) - it never opens a second gate-clearing path. It
//! adds, on top of `approval_mode`:
//!   * required-group membership (via [`ApprovalIdentityResolver`]),
//!   * quorum (N distinct approvers, counted from the append-only `gate_vote`
//!     ledger rows so a partial quorum survives a restart),
//!   * a route-adapter seam ([`ApprovalRouteAdapter`]) for delivering approval
//!     notices to a route (and, on timeout, a distinct second route).
//!
//! When a step names no policy (or the config has none) the broker is a pass-through
//! to `resolve_gate` - unchanged behavior. The chokepoint keeps its audit-first,
//! fail-closed contract; the broker only gates who may reach it and how many times.

use std::sync::Arc;

use zeroclaw_config::schema::{ApprovalPolicyConfig, SopApprovalConfig};

use super::decision::{ApprovalDecision, ResolveOutcome};
use super::identity::{ApprovalIdentityResolver, LocalConfigApprovalIdentityResolver};
use super::principal::{ApprovalPrincipal, ApprovalSource};
use crate::sop::engine::{GateState, SopEngine};

/// Why an approval notice is being delivered. Route adapters render this so an
/// on-call escalation cannot be mistaken for the initial request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalNoticeKind {
    Request,
    Escalation,
}

impl ApprovalNoticeKind {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Request => "request",
            Self::Escalation => "escalation",
        }
    }
}

/// Deliver an approval notice to a named route (channel). The seam that lets
/// approvals reach approvers beyond the originating channel (the cross-channel HITL
/// gap) and that escalation uses to reach a distinct SECOND route on timeout.
/// Delivery is best-effort: a route error must never clear or block a gate (the gate
/// state is the source of truth; the route is only a notice).
pub trait ApprovalRouteAdapter: Send + Sync {
    fn deliver(
        &self,
        notice: ApprovalNoticeKind,
        route: &str,
        run_id: &str,
        sop_name: &str,
        step: u32,
    ) -> anyhow::Result<()>;
}

/// A no-op route adapter: logs the delivery intent but sends nowhere. The default
/// until real channel delivery is wired (a follow-up); keeps the broker and
/// escalation functional without a channel dependency.
pub struct NoopRouteAdapter;

impl ApprovalRouteAdapter for NoopRouteAdapter {
    fn deliver(
        &self,
        notice: ApprovalNoticeKind,
        route: &str,
        run_id: &str,
        sop_name: &str,
        step: u32,
    ) -> anyhow::Result<()> {
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(
                ::serde_json::json!({
                    "notice_kind": notice.label(), "route": route, "run_id": run_id,
                    "sop_name": sop_name, "step": step
                })
            ),
            &format!(
                "approval {} route delivery (noop): run {run_id} step {step} -> route '{route}'",
                notice.label()
            )
        );
        Ok(())
    }
}

/// The outcome of a broker-mediated gate resolution.
#[derive(Debug)]
pub enum BrokerOutcome {
    /// Quorum (or a single approval) was satisfied and the chokepoint ran.
    Resolved(ResolveOutcome),
    /// A valid approval was recorded but more distinct approvers are still needed.
    PendingQuorum { have: usize, need: usize },
    /// The principal is not a member of the policy's required group; the gate is
    /// left untouched.
    NotAuthorized { required_group: String },
    /// The waiting step names an approval policy that is absent from
    /// `[sop.approval].policies`. FAIL CLOSED: the gate is left untouched rather
    /// than treated as an unpoliced (quorum-1, no-membership) gate, so a typo can
    /// never downgrade a policied step to open approval.
    PolicyMissing { name: String },
    /// The run is not a waiting gate (unknown / already-resolved / not applicable).
    NotWaiting,
}

impl BrokerOutcome {
    /// A stable wire label for the outcome (WS / CLI surfaces).
    pub fn label(&self) -> String {
        match self {
            BrokerOutcome::Resolved(o) => o.label().to_string(),
            BrokerOutcome::NotWaiting => "not_waiting".to_string(),
            BrokerOutcome::PendingQuorum { have, need } => {
                format!("pending_quorum ({have}/{need})")
            }
            BrokerOutcome::NotAuthorized { required_group } => {
                format!("not_authorized (requires group '{required_group}')")
            }
            BrokerOutcome::PolicyMissing { name } => {
                format!("policy_missing ('{name}')")
            }
        }
    }
}

/// The approval policy in effect for a run's current step (from live config).
enum StepPolicy {
    /// The step names no policy: unpoliced, quorum-1 pass-through (old behavior).
    Unpoliced,
    /// The step names a policy present in config. Carries the policy NAME (the
    /// `[sop.approval].policies.<name>` map key) so a quorum vote can be scoped to the
    /// policy in effect when it was cast.
    Named {
        name: String,
        config: ApprovalPolicyConfig,
    },
    /// The step names a policy ABSENT from config: fail closed, never treat as
    /// unpoliced.
    MissingNamed(String),
}

/// Authorization + quorum layer over `resolve_gate`. Holds NO copy of the approval
/// policy/group config - it resolves every policy and membership decision from the
/// engine's live `[sop.approval]` at use-time (single source of truth), so a config
/// reload cannot leave the broker deciding on stale rules. It carries only the
/// identity-resolution seam and the route adapter.
pub struct ApprovalBroker {
    resolver: Arc<dyn ApprovalIdentityResolver>,
    route: Arc<dyn ApprovalRouteAdapter>,
}

impl ApprovalBroker {
    /// Build with an explicit identity resolver and route adapter.
    pub fn new(
        resolver: Arc<dyn ApprovalIdentityResolver>,
        route: Arc<dyn ApprovalRouteAdapter>,
    ) -> Self {
        Self { resolver, route }
    }

    /// Build with the config-backed local resolver and the given route adapter.
    /// Policies/groups are read from the engine's live config at resolve-time, so
    /// there is no per-broker config to keep in sync.
    pub fn with_route(route: Arc<dyn ApprovalRouteAdapter>) -> Self {
        Self::new(Arc::new(LocalConfigApprovalIdentityResolver), route)
    }

    /// A broker with the local resolver and a no-op route. With no `[sop.approval]`
    /// policy in the engine config `resolve` behaves exactly like `resolve_gate`
    /// (the engine default, so behavior is unchanged until a policy is configured).
    pub fn disabled() -> Self {
        Self::with_route(Arc::new(NoopRouteAdapter))
    }

    /// The escalation route for a named policy (Phase 10), read from live config.
    /// An empty string is treated the same as `None` (re-surface to the same
    /// route) - the config contract says "`None`/empty" - so a blank
    /// `escalation_route = ""` does not route a timeout notice nowhere.
    pub fn escalation_route(&self, cfg: &SopApprovalConfig, policy_name: &str) -> Option<String> {
        cfg.policies
            .get(policy_name)
            .and_then(|p| p.escalation_route.clone())
            .filter(|r| !r.is_empty())
    }

    /// Deliver an escalation notice to a route (best-effort).
    pub fn deliver_escalation(&self, route: &str, run_id: &str, sop_name: &str, step: u32) {
        if let Err(e) = self.route.deliver(
            ApprovalNoticeKind::Escalation,
            route,
            run_id,
            sop_name,
            step,
        ) {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({
                        "route": route, "run_id": run_id, "error": e.to_string()
                    })),
                "approval escalation route delivery failed (gate unaffected)"
            );
        }
    }

    /// The request route for a named policy: the channel the INITIAL approval
    /// request is delivered to when a run parks at a gate this policy governs. Read
    /// from live config; an empty string is treated as `None` (no out-of-band
    /// request notice), matching the config contract. This is a DISTINCT lifecycle
    /// event from [`escalation_route`](Self::escalation_route) - the request fires on
    /// park, the escalation only if the gate later times out.
    pub fn request_route(&self, cfg: &SopApprovalConfig, policy_name: &str) -> Option<String> {
        cfg.policies
            .get(policy_name)
            .and_then(|p| p.request_route.clone())
            .filter(|r| !r.is_empty())
    }

    /// Deliver the initial approval-request notice to a route (best-effort). Fired
    /// when a run parks at a policied gate; a delivery failure never blocks or clears
    /// the gate (the gate is the source of truth, this is only a notice).
    pub fn deliver_request(&self, route: &str, run_id: &str, sop_name: &str, step: u32) {
        if let Err(e) =
            self.route
                .deliver(ApprovalNoticeKind::Request, route, run_id, sop_name, step)
        {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({
                        "route": route, "run_id": run_id, "error": e.to_string()
                    })),
                "approval request route delivery failed (gate unaffected)"
            );
        }
    }

    /// The approval policy that applies to the run's currently-waiting step, resolved
    /// from the engine's live config. Three-state so a NAMED-but-absent policy is
    /// distinguished from no policy at all - the caller fails closed on the former.
    fn step_policy(&self, engine: &SopEngine, run_id: &str) -> StepPolicy {
        let Some(name) = engine.current_step_policy_name(run_id) else {
            return StepPolicy::Unpoliced;
        };
        match engine.approval_config().policies.get(&name) {
            Some(p) => StepPolicy::Named {
                name,
                config: p.clone(),
            },
            None => StepPolicy::MissingNamed(name),
        }
    }

    /// Resolve a gate through the broker: enforce membership + quorum, then call the
    /// chokepoint. `engine` is the authoritative gate owner; the broker only decides
    /// whether (and when) to reach `resolve_gate`.
    pub fn resolve(
        &self,
        engine: &mut SopEngine,
        run_id: &str,
        decision: ApprovalDecision,
        principal: ApprovalPrincipal,
    ) -> anyhow::Result<BrokerOutcome> {
        let step = match engine.gate_state(run_id) {
            GateState::Waiting { step } => step,
            GateState::AlreadyResolved => {
                return Ok(BrokerOutcome::Resolved(ResolveOutcome::AlreadyResolved));
            }
            GateState::NotApplicable => return Ok(BrokerOutcome::NotWaiting),
        };

        // FAIL CLOSED: a step that names a policy absent from config leaves the gate
        // waiting rather than falling through to an unpoliced (quorum-1) resolution.
        let policy: Option<(String, ApprovalPolicyConfig)> = match self.step_policy(engine, run_id)
        {
            StepPolicy::MissingNamed(name) => return Ok(BrokerOutcome::PolicyMissing { name }),
            StepPolicy::Unpoliced => None,
            StepPolicy::Named { name, config } => Some((name, config)),
        };

        // Required-group membership gates BOTH approve and deny: only an authorized
        // principal may act on the gate at all. Resolved against the LIVE config.
        // An empty string is treated the same as `None` (no membership gate) - the
        // config contract says "`None`/empty" - so a blank `required_group = ""`
        // does not lock every principal out of a policy nobody could ever satisfy.
        if let Some(group) = policy
            .as_ref()
            .and_then(|(_, p)| p.required_group.as_deref())
            .filter(|g| !g.is_empty())
            && !self
                .resolver
                .is_member(engine.approval_config(), &principal, group)
        {
            return Ok(BrokerOutcome::NotAuthorized {
                required_group: group.to_string(),
            });
        }

        match decision {
            // A single authorized deny cancels the run (no quorum on denial - fail-safe).
            ApprovalDecision::Deny { .. } => Ok(BrokerOutcome::Resolved(
                engine.resolve_gate(run_id, decision, principal)?,
            )),
            ApprovalDecision::Approve => {
                // Unpoliced (no named policy) clears immediately - quorum-1 pass-through.
                let Some((policy_name, cfg)) = policy.as_ref() else {
                    return Ok(BrokerOutcome::Resolved(
                        engine.resolve_gate(run_id, decision, principal)?,
                    ));
                };
                let need = (cfg.quorum.max(1)) as usize;
                if need <= 1 {
                    return Ok(BrokerOutcome::Resolved(
                        engine.resolve_gate(run_id, decision, principal)?,
                    ));
                }
                // Refuse to record a quorum vote from a principal `approval_mode` would
                // reject outright (the agent under OutOfBandRequired, an out-of-band
                // principal under AgentTool). `resolve_gate` enforces this itself, but
                // only the FINAL vote that reaches quorum ever calls it - without this,
                // the first N-1 votes could durably record a vote from a principal who
                // could never actually clear this gate, contributing toward a quorum a
                // different, valid principal later completes.
                if super::resolve::is_rejected_by_approval_mode(
                    engine.config().approval_mode,
                    &principal,
                ) {
                    return Ok(BrokerOutcome::Resolved(
                        ResolveOutcome::RejectedSelfApproval,
                    ));
                }
                // Refuse to record a quorum vote while the run's parked snapshot has
                // not yet been durably persisted (A-core's `is_park_persist_pending`).
                // A quorum vote is recorded BEFORE `resolve_gate` runs (only the FINAL
                // vote that reaches quorum calls it), so `resolve_gate`'s own pending-
                // persist guard cannot protect the first N-1 votes: recording one now
                // would durably outlive the run if its park never manages to persist
                // and is lost across a restart (an orphaned `gate_vote` row for a run
                // that no longer exists). Fail closed BEFORE the vote append, matching
                // `resolve_gate`'s own pre-claim/pre-ledger discipline.
                if engine.is_park_persist_pending(run_id) {
                    anyhow::bail!(
                        "cannot record approval vote for run {run_id}: its parked snapshot is not yet durably persisted (retrying)"
                    );
                }
                // Quorum > 1: durably record this vote (scoped to the CURRENT policy),
                // then count distinct approvers FOR THE CURRENT STEP so a multi-gate run
                // does not reuse earlier votes.
                engine.record_gate_vote(run_id, step, policy_name, &principal)?;
                // Count only votes cast under the current policy whose voter is STILL a
                // member of the current required group - so a mid-flight policy or group
                // change cannot let a stale vote count toward the new quorum. Propagates
                // a gate-ledger read failure (the vote above is durably recorded, so
                // failing here leaves the gate waiting for a retry, not a bogus quorum).
                let have = self.count_qualified_voters(
                    engine,
                    run_id,
                    step,
                    policy_name,
                    cfg.required_group.as_deref().filter(|g| !g.is_empty()),
                )?;
                if have >= need {
                    Ok(BrokerOutcome::Resolved(
                        engine.resolve_gate(run_id, decision, principal)?,
                    ))
                } else {
                    Ok(BrokerOutcome::PendingQuorum { have, need })
                }
            }
        }
    }

    /// Count the distinct approvers whose recorded vote (a) was cast under the
    /// `policy_name` currently enforced for this step AND (b) is STILL a member of the
    /// current `required_group`. Scoping to the policy name means a mid-flight policy
    /// change resets the tally (a stale vote under the old policy no longer counts);
    /// revalidating membership against the LIVE config means a voter removed from the
    /// group mid-flight stops counting. Dedup is by the canonical `voter_key`, so one
    /// paired token presented over HTTP and WS still counts once. A vote with no
    /// recorded policy (cast before this field existed) matches no named policy and is
    /// therefore never counted - it cannot silently satisfy the new policy.
    fn count_qualified_voters(
        &self,
        engine: &SopEngine,
        run_id: &str,
        step: u32,
        policy_name: &str,
        required_group: Option<&str>,
    ) -> anyhow::Result<usize> {
        let votes = engine.gate_votes_for_step(run_id, step)?;
        let cfg = engine.approval_config();
        let mut qualified: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for vote in votes {
            // Scope to the policy in effect NOW.
            if vote.policy.as_deref() != Some(policy_name) {
                continue;
            }
            // Revalidate the recorded voter against the CURRENT required group.
            if let Some(group) = required_group {
                let Some(principal) = vote_principal(vote.source.as_deref(), vote.identity.clone())
                else {
                    continue;
                };
                if !self.resolver.is_member(cfg, &principal, group) {
                    continue;
                }
            }
            qualified.insert(vote.voter_key);
        }
        Ok(qualified.len())
    }
}

/// Reconstruct the minimal principal needed to REVALIDATE a recorded vote against the
/// current required group. Membership (`groups_for`) keys only on the source label and
/// identity, so the back-channel is irrelevant and dropped. Returns `None` for an
/// unrecognized source label (a vote we cannot re-authorize does not count).
fn vote_principal(source: Option<&str>, identity: Option<String>) -> Option<ApprovalPrincipal> {
    let source = match source? {
        "agent" => ApprovalSource::Agent,
        "cli" => ApprovalSource::Cli,
        "ws" => ApprovalSource::Ws,
        "http" => ApprovalSource::Http,
        "system" => ApprovalSource::System,
        _ => return None,
    };
    Some(ApprovalPrincipal {
        source,
        identity,
        channel: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sop::engine::now_iso8601;
    use crate::sop::store::SopRunStore;
    use crate::sop::types::{
        Sop, SopAdmissionPolicy, SopEvent, SopExecutionMode, SopPriority, SopRunAction,
        SopRunStatus, SopStep, SopStepKind, SopTrigger, SopTriggerSource,
    };
    use std::collections::HashMap;
    use zeroclaw_config::schema::{ApprovalGroupConfig, ApprovalPolicyConfig, SopConfig};

    fn manual() -> SopEvent {
        SopEvent {
            source: SopTriggerSource::Manual,
            topic: None,
            payload: None,
            timestamp: now_iso8601(),
        }
    }

    /// A supervised SOP whose first (approval) step names `policy`.
    fn policy_sop(policy: &str) -> Sop {
        Sop {
            name: "deploy".into(),
            description: "t".into(),
            version: "1.0.0".into(),
            priority: SopPriority::Normal,
            execution_mode: SopExecutionMode::Supervised,
            triggers: vec![SopTrigger::Manual],
            steps: vec![
                SopStep {
                    number: 1,
                    title: "gate".into(),
                    requires_confirmation: true,
                    kind: SopStepKind::Execute,
                    policy: Some(policy.into()),
                    ..SopStep::default()
                },
                SopStep {
                    number: 2,
                    title: "go".into(),
                    kind: SopStepKind::Execute,
                    ..SopStep::default()
                },
            ],
            cooldown_secs: 0,
            max_concurrent: 1,
            location: None,
            deterministic: false,
            admission_policy: SopAdmissionPolicy::Parallel,
            max_pending_approvals: 0,
            agent: None,
        }
    }

    /// approval config: group `release` with the given members, policy `prod`
    /// requiring that group and the given quorum.
    fn approval_cfg(members: &[&str], quorum: u32) -> SopApprovalConfig {
        let mut groups = HashMap::new();
        groups.insert(
            "release".to_string(),
            ApprovalGroupConfig {
                members: members.iter().map(|m| m.to_string()).collect(),
            },
        );
        let mut policies = HashMap::new();
        policies.insert(
            "prod".to_string(),
            ApprovalPolicyConfig {
                required_group: Some("release".into()),
                quorum,
                request_route: None,
                escalation_route: None,
            },
        );
        SopApprovalConfig { groups, policies }
    }

    /// Build an engine whose LIVE config carries `cfg` (the broker resolves policies
    /// and membership from there - the single source of truth), parked at the gate.
    fn engine_with_broker(cfg: SopApprovalConfig) -> (SopEngine, String) {
        engine_with_broker_step("prod", cfg)
    }

    /// Like `engine_with_broker`, but the SOP's first step names `step_policy` (so a
    /// test can name a policy that is absent from `cfg`).
    fn engine_with_broker_step(step_policy: &str, cfg: SopApprovalConfig) -> (SopEngine, String) {
        let broker = Arc::new(ApprovalBroker::disabled());
        let sop_config = SopConfig {
            approval: cfg,
            ..SopConfig::default()
        };
        let mut e = SopEngine::new(sop_config).with_approval_broker(broker);
        e.set_sops_for_test(vec![policy_sop(step_policy)]);
        let action = e.start_run("deploy", manual()).unwrap();
        let id = match action {
            SopRunAction::WaitApproval { run_id, .. } => run_id,
            other => panic!("expected WaitApproval, got {other:?}"),
        };
        (e, id)
    }

    #[test]
    fn non_member_is_not_authorized_and_gate_stays_open() {
        let (mut e, id) = engine_with_broker(approval_cfg(&["alice"], 1));
        let out = e
            .resolve_via_broker(
                &id,
                ApprovalDecision::Approve,
                ApprovalPrincipal::cli(Some("mallory".into())),
            )
            .unwrap();
        assert!(matches!(out, BrokerOutcome::NotAuthorized { .. }));
        assert!(
            matches!(e.gate_state(&id), GateState::Waiting { .. }),
            "an unauthorized attempt must leave the gate waiting"
        );
    }

    #[test]
    fn member_single_quorum_resolves() {
        let (mut e, id) = engine_with_broker(approval_cfg(&["alice"], 1));
        let out = e
            .resolve_via_broker(
                &id,
                ApprovalDecision::Approve,
                ApprovalPrincipal::cli(Some("alice".into())),
            )
            .unwrap();
        assert!(matches!(
            out,
            BrokerOutcome::Resolved(ResolveOutcome::Resumed(_))
        ));
    }

    #[test]
    fn quorum_of_two_needs_two_distinct_approvers() {
        let (mut e, id) = engine_with_broker(approval_cfg(&["alice", "bob"], 2));
        // First authorized approval: recorded, still pending.
        let first = e
            .resolve_via_broker(
                &id,
                ApprovalDecision::Approve,
                ApprovalPrincipal::cli(Some("alice".into())),
            )
            .unwrap();
        assert!(
            matches!(first, BrokerOutcome::PendingQuorum { have: 1, need: 2 }),
            "one vote is not a quorum of two"
        );
        // A repeat vote by the same identity does NOT advance the quorum.
        let dup = e
            .resolve_via_broker(
                &id,
                ApprovalDecision::Approve,
                ApprovalPrincipal::cli(Some("alice".into())),
            )
            .unwrap();
        assert!(matches!(
            dup,
            BrokerOutcome::PendingQuorum { have: 1, need: 2 }
        ));
        // A second distinct approver satisfies the quorum and clears the gate.
        let second = e
            .resolve_via_broker(
                &id,
                ApprovalDecision::Approve,
                ApprovalPrincipal::cli(Some("bob".into())),
            )
            .unwrap();
        assert!(matches!(
            second,
            BrokerOutcome::Resolved(ResolveOutcome::Resumed(_))
        ));
    }

    #[test]
    fn stale_vote_under_old_policy_does_not_count_after_policy_change() {
        // A run parks under policy `old` (quorum 2)
        // and one `old` member votes; a SOP reload re-points the step at policy `new`
        // (a DIFFERENT group). Before the fix the old vote shared the (run, step) key and
        // counted toward `new`, so a SINGLE `new` approver cleared a quorum-2 gate. Votes
        // are now scoped to the policy in effect when they were cast.
        let mut groups = HashMap::new();
        groups.insert(
            "g_old".to_string(),
            ApprovalGroupConfig {
                members: vec!["alice".into()],
            },
        );
        groups.insert(
            "g_new".to_string(),
            ApprovalGroupConfig {
                members: vec!["bob".into()],
            },
        );
        let mut policies = HashMap::new();
        policies.insert(
            "old".to_string(),
            ApprovalPolicyConfig {
                required_group: Some("g_old".into()),
                quorum: 2,
                ..Default::default()
            },
        );
        policies.insert(
            "new".to_string(),
            ApprovalPolicyConfig {
                required_group: Some("g_new".into()),
                quorum: 2,
                ..Default::default()
            },
        );
        let (mut e, id) = engine_with_broker_step("old", SopApprovalConfig { groups, policies });

        // alice (a g_old member) votes under `old`: recorded, quorum not yet met.
        let first = e
            .resolve_via_broker(
                &id,
                ApprovalDecision::Approve,
                ApprovalPrincipal::cli(Some("alice".into())),
            )
            .unwrap();
        assert!(matches!(
            first,
            BrokerOutcome::PendingQuorum { have: 1, need: 2 }
        ));

        // A SOP reload re-points the parked step at policy `new` (group g_new).
        e.set_sops_for_test(vec![policy_sop("new")]);

        // bob (a g_new member) votes under `new`. alice's stale `old` vote must NOT count.
        let second = e
            .resolve_via_broker(
                &id,
                ApprovalDecision::Approve,
                ApprovalPrincipal::cli(Some("bob".into())),
            )
            .unwrap();
        assert!(
            matches!(second, BrokerOutcome::PendingQuorum { have: 1, need: 2 }),
            "a vote cast under the old policy must not count toward the new policy's quorum, got {second:?}"
        );
    }

    #[test]
    fn revoked_member_vote_does_not_count_after_config_reload() {
        // Membership revocation: alice votes under `prod`
        // (group `release`), then a live config reload revokes alice from `release` while
        // the gate is parked. Her earlier vote is REVALIDATED against the live group at
        // count time, so it stops counting - the one remaining member cannot alone clear
        // a quorum of two.
        let (mut e, id) = engine_with_broker(approval_cfg(&["alice", "bob"], 2));
        let first = e
            .resolve_via_broker(
                &id,
                ApprovalDecision::Approve,
                ApprovalPrincipal::cli(Some("alice".into())),
            )
            .unwrap();
        assert!(matches!(
            first,
            BrokerOutcome::PendingQuorum { have: 1, need: 2 }
        ));

        // Live config reload revokes alice from the release group (bob remains).
        e.set_approval_config_for_test(approval_cfg(&["bob"], 2));

        // bob votes. alice's earlier vote is revalidated against the CURRENT group and
        // dropped (she is no longer a member), so quorum is still not met.
        let second = e
            .resolve_via_broker(
                &id,
                ApprovalDecision::Approve,
                ApprovalPrincipal::cli(Some("bob".into())),
            )
            .unwrap();
        assert!(
            matches!(second, BrokerOutcome::PendingQuorum { have: 1, need: 2 }),
            "a voter revoked from the required group must stop counting toward quorum, got {second:?}"
        );
    }

    /// Delegates to an in-memory store but fails every `save_run`, to prove a
    /// quorum vote cannot be recorded while a park's snapshot cannot be durably
    /// persisted (mirrors `crate::sop::engine::tests::FailingSaveStore`).
    struct FailingSaveStore {
        inner: crate::sop::store::InMemoryRunStore,
    }
    impl crate::sop::store::SopRunStore for FailingSaveStore {
        fn save_run(
            &self,
            _r: &crate::sop::store::PersistedRun,
        ) -> Result<(), crate::sop::store::StoreError> {
            Err(crate::sop::store::StoreError::Backend(
                "injected save_run failure".into(),
            ))
        }
        fn finish_run(
            &self,
            id: &str,
            t: &crate::sop::store::PersistedRun,
        ) -> Result<(), crate::sop::store::StoreError> {
            self.inner.finish_run(id, t)
        }
        fn load_active_runs(
            &self,
        ) -> Result<Vec<crate::sop::store::PersistedRun>, crate::sop::store::StoreError> {
            self.inner.load_active_runs()
        }
        fn load_terminal_runs(
            &self,
            limit: usize,
        ) -> Result<Vec<crate::sop::store::PersistedRun>, crate::sop::store::StoreError> {
            self.inner.load_terminal_runs(limit)
        }
        fn load_run(
            &self,
            id: &str,
        ) -> Result<Option<crate::sop::store::PersistedRun>, crate::sop::store::StoreError>
        {
            self.inner.load_run(id)
        }
        fn last_terminal_completed_at(
            &self,
            s: &str,
        ) -> Result<Option<String>, crate::sop::store::StoreError> {
            self.inner.last_terminal_completed_at(s)
        }
        fn try_claim_run(
            &self,
            id: &str,
            s: &str,
            p: usize,
            g: usize,
        ) -> Result<Option<crate::sop::store::ClaimToken>, crate::sop::store::StoreError> {
            self.inner.try_claim_run(id, s, p, g)
        }
        fn renew_claim_for_restore(
            &self,
            id: &str,
            s: &str,
        ) -> Result<crate::sop::store::ClaimToken, crate::sop::store::StoreError> {
            self.inner.renew_claim_for_restore(id, s)
        }
        fn claim_counts(&self, s: &str) -> Result<(usize, usize), crate::sop::store::StoreError> {
            self.inner.claim_counts(s)
        }
        fn heartbeat_claim(
            &self,
            t: &crate::sop::store::ClaimToken,
        ) -> Result<(), crate::sop::store::StoreError> {
            self.inner.heartbeat_claim(t)
        }
        fn release_claim(
            &self,
            t: &crate::sop::store::ClaimToken,
        ) -> Result<(), crate::sop::store::StoreError> {
            self.inner.release_claim(t)
        }
        fn expired_claims(
            &self,
            n: &str,
        ) -> Result<Vec<crate::sop::store::ClaimToken>, crate::sop::store::StoreError> {
            self.inner.expired_claims(n)
        }
        fn append_event(
            &self,
            e: &crate::sop::store::SopEventRecord,
        ) -> Result<u64, crate::sop::store::StoreError> {
            self.inner.append_event(e)
        }
        fn list_events(
            &self,
            id: &str,
        ) -> Result<Vec<crate::sop::store::SopEventRecord>, crate::sop::store::StoreError> {
            self.inner.list_events(id)
        }
        fn save_proposal(
            &self,
            p: &crate::sop::store::ProposalRecord,
        ) -> Result<(), crate::sop::store::StoreError> {
            self.inner.save_proposal(p)
        }
        fn load_proposal(
            &self,
            id: &str,
        ) -> Result<Option<crate::sop::store::ProposalRecord>, crate::sop::store::StoreError>
        {
            self.inner.load_proposal(id)
        }
        fn list_proposals(
            &self,
            s: Option<crate::sop::store::ProposalStatus>,
        ) -> Result<Vec<crate::sop::store::ProposalRecord>, crate::sop::store::StoreError> {
            self.inner.list_proposals(s)
        }
        fn prune(
            &self,
            p: &crate::sop::store::RetentionPolicy,
        ) -> Result<usize, crate::sop::store::StoreError> {
            self.inner.prune(p)
        }
        fn health_check(&self) -> bool {
            self.inner.health_check()
        }
        fn backend(&self) -> &'static str {
            "failing-save-test"
        }
    }

    /// Delegates to an in-memory store but fails every `list_events`, to prove a
    /// gate-ledger read failure during quorum counting is surfaced (the resolve
    /// errors, the gate stays waiting) rather than swallowed into 0 votes. `save_run`
    /// and `append_event` succeed, so the run parks durably and the vote records -
    /// modelling the "vote appended, then the ledger read fails" case exactly.
    struct ListEventsFailsStore {
        inner: crate::sop::store::InMemoryRunStore,
    }
    impl crate::sop::store::SopRunStore for ListEventsFailsStore {
        fn save_run(
            &self,
            r: &crate::sop::store::PersistedRun,
        ) -> Result<(), crate::sop::store::StoreError> {
            self.inner.save_run(r)
        }
        fn finish_run(
            &self,
            id: &str,
            t: &crate::sop::store::PersistedRun,
        ) -> Result<(), crate::sop::store::StoreError> {
            self.inner.finish_run(id, t)
        }
        fn load_terminal_runs(
            &self,
            limit: usize,
        ) -> Result<Vec<crate::sop::store::PersistedRun>, crate::sop::store::StoreError> {
            self.inner.load_terminal_runs(limit)
        }
        fn load_active_runs(
            &self,
        ) -> Result<Vec<crate::sop::store::PersistedRun>, crate::sop::store::StoreError> {
            self.inner.load_active_runs()
        }
        fn load_run(
            &self,
            id: &str,
        ) -> Result<Option<crate::sop::store::PersistedRun>, crate::sop::store::StoreError>
        {
            self.inner.load_run(id)
        }
        fn last_terminal_completed_at(
            &self,
            s: &str,
        ) -> Result<Option<String>, crate::sop::store::StoreError> {
            self.inner.last_terminal_completed_at(s)
        }
        fn try_claim_run(
            &self,
            id: &str,
            s: &str,
            p: usize,
            g: usize,
        ) -> Result<Option<crate::sop::store::ClaimToken>, crate::sop::store::StoreError> {
            self.inner.try_claim_run(id, s, p, g)
        }
        fn renew_claim_for_restore(
            &self,
            id: &str,
            s: &str,
        ) -> Result<crate::sop::store::ClaimToken, crate::sop::store::StoreError> {
            self.inner.renew_claim_for_restore(id, s)
        }
        fn claim_counts(&self, s: &str) -> Result<(usize, usize), crate::sop::store::StoreError> {
            self.inner.claim_counts(s)
        }
        fn heartbeat_claim(
            &self,
            t: &crate::sop::store::ClaimToken,
        ) -> Result<(), crate::sop::store::StoreError> {
            self.inner.heartbeat_claim(t)
        }
        fn release_claim(
            &self,
            t: &crate::sop::store::ClaimToken,
        ) -> Result<(), crate::sop::store::StoreError> {
            self.inner.release_claim(t)
        }
        fn expired_claims(
            &self,
            n: &str,
        ) -> Result<Vec<crate::sop::store::ClaimToken>, crate::sop::store::StoreError> {
            self.inner.expired_claims(n)
        }
        fn append_event(
            &self,
            e: &crate::sop::store::SopEventRecord,
        ) -> Result<u64, crate::sop::store::StoreError> {
            self.inner.append_event(e)
        }
        fn list_events(
            &self,
            _id: &str,
        ) -> Result<Vec<crate::sop::store::SopEventRecord>, crate::sop::store::StoreError> {
            Err(crate::sop::store::StoreError::Backend(
                "injected list_events failure".into(),
            ))
        }
        fn save_proposal(
            &self,
            p: &crate::sop::store::ProposalRecord,
        ) -> Result<(), crate::sop::store::StoreError> {
            self.inner.save_proposal(p)
        }
        fn load_proposal(
            &self,
            id: &str,
        ) -> Result<Option<crate::sop::store::ProposalRecord>, crate::sop::store::StoreError>
        {
            self.inner.load_proposal(id)
        }
        fn list_proposals(
            &self,
            s: Option<crate::sop::store::ProposalStatus>,
        ) -> Result<Vec<crate::sop::store::ProposalRecord>, crate::sop::store::StoreError> {
            self.inner.list_proposals(s)
        }
        fn prune(
            &self,
            p: &crate::sop::store::RetentionPolicy,
        ) -> Result<usize, crate::sop::store::StoreError> {
            self.inner.prune(p)
        }
        fn health_check(&self) -> bool {
            self.inner.health_check()
        }
        fn backend(&self) -> &'static str {
            "list-events-fails-test"
        }
    }

    #[test]
    fn quorum_vote_read_failure_is_surfaced_not_swallowed() {
        // After a vote is durably appended,
        // counting distinct voters reads the gate ledger. If that read fails, the
        // broker must SURFACE the error (leaving the gate waiting for a retry), not
        // collapse to a bogus `PendingQuorum { have: 0 }` that swallows the failure.
        let store = std::sync::Arc::new(ListEventsFailsStore {
            inner: crate::sop::store::InMemoryRunStore::new(),
        });
        let broker = Arc::new(ApprovalBroker::disabled());
        let sop_config = SopConfig {
            approval: approval_cfg(&["alice", "bob"], 2),
            ..SopConfig::default()
        };
        let mut e = SopEngine::new(sop_config)
            .with_approval_broker(broker)
            .with_store(store.clone());
        e.set_sops_for_test(vec![policy_sop("prod")]);
        let action = e.start_run("deploy", manual()).unwrap();
        let id = match action {
            SopRunAction::WaitApproval { run_id, .. } => run_id,
            other => panic!("expected WaitApproval, got {other:?}"),
        };

        let res = e.resolve_via_broker(
            &id,
            ApprovalDecision::Approve,
            ApprovalPrincipal::cli(Some("alice".into())),
        );
        assert!(
            res.is_err(),
            "a gate-ledger read failure during quorum counting must surface, not report PendingQuorum(0)"
        );
    }

    #[test]
    fn quorum_vote_refused_while_park_persist_is_pending() {
        // Regression: a quorum vote is recorded before `resolve_gate` runs (only
        // the final vote that reaches quorum calls it), so `resolve_gate`'s own
        // `is_park_persist_pending` guard cannot protect the first N-1 votes. A
        // vote recorded while the run's parked snapshot is not yet durable would
        // durably outlive the run if it never manages to persist and is lost across
        // a restart, leaving an orphaned `gate_vote` row for a run that no longer
        // exists. The broker must refuse to record a vote at all while that
        // pending-persist state holds.
        let store = std::sync::Arc::new(FailingSaveStore {
            inner: crate::sop::store::InMemoryRunStore::new(),
        });
        let broker = Arc::new(ApprovalBroker::disabled());
        let sop_config = SopConfig {
            approval: approval_cfg(&["alice", "bob"], 2),
            ..SopConfig::default()
        };
        let mut e = SopEngine::new(sop_config)
            .with_approval_broker(broker)
            .with_store(store.clone());
        e.set_sops_for_test(vec![policy_sop("prod")]);
        let action = e.start_run("deploy", manual()).unwrap();
        let id = match action {
            SopRunAction::WaitApproval { run_id, .. } => run_id,
            other => panic!("expected WaitApproval, got {other:?}"),
        };
        assert_eq!(
            store.claim_counts("deploy").unwrap(),
            (1, 1),
            "the exec claim is KEPT when the parked snapshot cannot be persisted"
        );

        let res = e.resolve_via_broker(
            &id,
            ApprovalDecision::Approve,
            ApprovalPrincipal::cli(Some("alice".into())),
        );
        assert!(
            res.is_err(),
            "the vote must be refused while the park's snapshot is not yet durably persisted"
        );
        assert_eq!(
            e.gate_votes_for_step(&id, 1).unwrap().len(),
            0,
            "no gate_vote row must be recorded for a refused vote attempt"
        );
        assert_eq!(
            store.claim_counts("deploy").unwrap(),
            (1, 1),
            "the pre-existing kept claim must survive the refused vote attempt"
        );
        assert!(
            matches!(e.gate_state(&id), GateState::Waiting { .. }),
            "the gate stays waiting, re-resolvable once the park persists"
        );
    }

    #[test]
    fn quorum_vote_refused_from_a_principal_approval_mode_rejects() {
        // Regression: `resolve_gate` enforces `approval_mode` (for example,
        // `OutOfBandRequired` rejects the agent principal), but a quorum vote is
        // recorded before `resolve_gate` runs - only the final vote that reaches
        // quorum ever calls it. Without this guard, an agent principal under
        // `OutOfBandRequired` (or an out-of-band principal under `AgentTool`) could
        // durably record a vote toward a quorum it could never actually clear, even
        // though `approval_mode` says it must not participate at all.
        let broker = Arc::new(ApprovalBroker::disabled());
        // "bot" is a bare (any-source) group member, so membership passes; the mode
        // check must be what blocks this, not group authorization.
        let sop_config = SopConfig {
            approval: approval_cfg(&["bot", "alice"], 2),
            approval_mode: zeroclaw_config::schema::ApprovalMode::OutOfBandRequired,
            ..SopConfig::default()
        };
        let mut e = SopEngine::new(sop_config).with_approval_broker(broker);
        e.set_sops_for_test(vec![policy_sop("prod")]);
        let action = e.start_run("deploy", manual()).unwrap();
        let id = match action {
            SopRunAction::WaitApproval { run_id, .. } => run_id,
            other => panic!("expected WaitApproval, got {other:?}"),
        };

        let out = e
            .resolve_via_broker(
                &id,
                ApprovalDecision::Approve,
                ApprovalPrincipal::agent("bot"),
            )
            .unwrap();
        assert!(
            matches!(
                out,
                BrokerOutcome::Resolved(ResolveOutcome::RejectedSelfApproval)
            ),
            "the agent principal must be rejected under OutOfBandRequired, got {out:?}"
        );
        assert_eq!(
            e.gate_votes_for_step(&id, 1).unwrap().len(),
            0,
            "no gate_vote row must be recorded for a principal approval_mode rejects"
        );
        assert!(
            matches!(e.gate_state(&id), GateState::Waiting { .. }),
            "the gate stays waiting"
        );

        // An out-of-band principal's vote is unaffected and is still recorded.
        let out2 = e
            .resolve_via_broker(
                &id,
                ApprovalDecision::Approve,
                ApprovalPrincipal::cli(Some("alice".into())),
            )
            .unwrap();
        assert!(
            matches!(out2, BrokerOutcome::PendingQuorum { have: 1, need: 2 }),
            "a valid out-of-band vote is still recorded, got {out2:?}"
        );
    }

    #[test]
    fn no_policy_is_pass_through() {
        // A step with no policy (disabled broker) resolves on a single approval.
        let broker = Arc::new(ApprovalBroker::disabled());
        let mut e = SopEngine::new(SopConfig::default()).with_approval_broker(broker);
        let mut sop = policy_sop("prod");
        sop.steps[0].policy = None;
        e.set_sops_for_test(vec![sop]);
        let action = e.start_run("deploy", manual()).unwrap();
        let id = match action {
            SopRunAction::WaitApproval { run_id, .. } => run_id,
            other => panic!("expected WaitApproval, got {other:?}"),
        };
        let out = e
            .resolve_via_broker(&id, ApprovalDecision::Approve, ApprovalPrincipal::cli(None))
            .unwrap();
        assert!(matches!(
            out,
            BrokerOutcome::Resolved(ResolveOutcome::Resumed(_))
        ));
    }

    #[test]
    fn empty_required_group_is_treated_as_no_membership_gate() {
        // Regression: the config contract says `required_group`'s `None` and empty
        // forms both mean no membership gate, but the broker only special-cased
        // `None`. A blank `required_group = ""` matched `Some("")` and gated every
        // principal against a group nobody could ever be a member of, permanently
        // stuck. An empty string must behave exactly like `None`.
        let mut policies = HashMap::new();
        policies.insert(
            "prod".to_string(),
            ApprovalPolicyConfig {
                required_group: Some(String::new()),
                quorum: 1,
                request_route: None,
                escalation_route: None,
            },
        );
        let cfg = SopApprovalConfig {
            groups: HashMap::new(),
            policies,
        };
        let broker = Arc::new(ApprovalBroker::disabled());
        let mut e = SopEngine::new(SopConfig {
            approval: cfg,
            ..SopConfig::default()
        })
        .with_approval_broker(broker);
        e.set_sops_for_test(vec![policy_sop("prod")]);
        let action = e.start_run("deploy", manual()).unwrap();
        let id = match action {
            SopRunAction::WaitApproval { run_id, .. } => run_id,
            other => panic!("expected WaitApproval, got {other:?}"),
        };
        // No group in config has this identity as a member (there are no groups at
        // all) - a real group gate would reject this. An empty-string gate must not.
        let out = e
            .resolve_via_broker(
                &id,
                ApprovalDecision::Approve,
                ApprovalPrincipal::cli(Some("anyone".into())),
            )
            .unwrap();
        assert!(
            matches!(out, BrokerOutcome::Resolved(ResolveOutcome::Resumed(_))),
            "an empty required_group must not gate membership at all, got {out:?}"
        );
    }

    #[test]
    fn escalation_route_empty_string_is_treated_as_none() {
        // Regression: the config contract says `escalation_route`'s `None` and
        // empty forms both re-surface to the same route, but
        // `ApprovalBroker::escalation_route` returned `Some("")` verbatim. Timeout
        // delivery would then send an escalation notice to a nonsensical empty
        // route name.
        let mut policies = HashMap::new();
        policies.insert(
            "prod".to_string(),
            ApprovalPolicyConfig {
                required_group: None,
                quorum: 1,
                request_route: None,
                escalation_route: Some(String::new()),
            },
        );
        let cfg = SopApprovalConfig {
            groups: HashMap::new(),
            policies,
        };
        let broker = ApprovalBroker::disabled();
        assert_eq!(
            broker.escalation_route(&cfg, "prod"),
            None,
            "an empty escalation_route must resolve to None, not Some(\"\")"
        );
    }

    #[test]
    fn request_route_reads_the_policy_and_treats_empty_as_none() {
        let mut policies = HashMap::new();
        policies.insert(
            "prod".to_string(),
            ApprovalPolicyConfig {
                required_group: None,
                quorum: 1,
                request_route: Some("discord.ops:123".to_string()),
                escalation_route: None,
            },
        );
        policies.insert(
            "blank".to_string(),
            ApprovalPolicyConfig {
                required_group: None,
                quorum: 1,
                request_route: Some(String::new()),
                escalation_route: None,
            },
        );
        let cfg = SopApprovalConfig {
            groups: HashMap::new(),
            policies,
        };
        let broker = ApprovalBroker::disabled();
        assert_eq!(
            broker.request_route(&cfg, "prod").as_deref(),
            Some("discord.ops:123"),
            "a configured request_route is returned verbatim"
        );
        assert_eq!(
            broker.request_route(&cfg, "blank"),
            None,
            "an empty request_route resolves to None (no out-of-band notice)"
        );
        assert_eq!(
            broker.request_route(&cfg, "absent"),
            None,
            "an unknown policy has no request_route"
        );
    }

    #[test]
    fn member_deny_cancels_without_quorum() {
        let (mut e, id) = engine_with_broker(approval_cfg(&["alice", "bob"], 2));
        let out = e
            .resolve_via_broker(
                &id,
                ApprovalDecision::Deny { reason: None },
                ApprovalPrincipal::cli(Some("alice".into())),
            )
            .unwrap();
        assert!(matches!(
            out,
            BrokerOutcome::Resolved(ResolveOutcome::Denied)
        ));
        assert!(matches!(
            e.get_run(&id).map(|r| r.status),
            Some(SopRunStatus::Cancelled)
        ));
    }

    #[test]
    fn missing_named_policy_fails_closed_and_gate_stays_open() {
        // The step names policy "prod" but the config defines no such policy. The
        // broker must FAIL CLOSED (PolicyMissing) rather than fall through to an
        // unpoliced quorum-1 resolution that any principal could clear - a typo must
        // never downgrade a policied gate to open approval.
        let (mut e, id) = engine_with_broker_step("prod", SopApprovalConfig::default());
        let out = e
            .resolve_via_broker(
                &id,
                ApprovalDecision::Approve,
                ApprovalPrincipal::cli(Some("anyone".into())),
            )
            .unwrap();
        assert!(
            matches!(out, BrokerOutcome::PolicyMissing { .. }),
            "a step naming an absent policy must fail closed, got {out:?}"
        );
        assert!(
            matches!(e.gate_state(&id), GateState::Waiting { .. }),
            "the gate must stay waiting when its named policy is missing"
        );
    }

    #[test]
    fn one_gateway_credential_over_http_and_ws_cannot_meet_quorum_of_two() {
        // B1: HTTP and WS authenticate via the SAME paired token, so a single subject
        // presented over both transports is ONE canonical voter and CANNOT satisfy a
        // quorum of two. A second, genuinely-distinct subject is required to clear it.
        // Members are granted as bare subjects (any source): `subj-1`, `subj-2`.
        let mut groups = HashMap::new();
        groups.insert(
            "release".to_string(),
            ApprovalGroupConfig {
                members: vec!["subj-1".into(), "subj-2".into()],
            },
        );
        let mut policies = HashMap::new();
        policies.insert(
            "prod".to_string(),
            ApprovalPolicyConfig {
                required_group: Some("release".into()),
                quorum: 2,
                request_route: None,
                escalation_route: None,
            },
        );
        let (mut e, id) = engine_with_broker_step("prod", SopApprovalConfig { groups, policies });

        // subj-1 approves over HTTP: 1/2.
        let first = e
            .resolve_via_broker(
                &id,
                ApprovalDecision::Approve,
                ApprovalPrincipal::http(Some("subj-1".into())),
            )
            .unwrap();
        assert!(
            matches!(first, BrokerOutcome::PendingQuorum { have: 1, need: 2 }),
            "one subject is one vote, got {first:?}"
        );
        // The SAME subject approves over WS: still 1/2 - the paired credential cannot
        // vote twice by switching transport.
        let same = e
            .resolve_via_broker(
                &id,
                ApprovalDecision::Approve,
                ApprovalPrincipal::ws("conn-1".into(), Some("subj-1".into())),
            )
            .unwrap();
        assert!(
            matches!(same, BrokerOutcome::PendingQuorum { have: 1, need: 2 }),
            "the same subject over HTTP+WS is ONE voter, got {same:?}"
        );
        // A genuinely distinct subject clears the gate.
        let second = e
            .resolve_via_broker(
                &id,
                ApprovalDecision::Approve,
                ApprovalPrincipal::http(Some("subj-2".into())),
            )
            .unwrap();
        assert!(
            matches!(second, BrokerOutcome::Resolved(ResolveOutcome::Resumed(_))),
            "a second distinct subject satisfies quorum, got {second:?}"
        );
    }

    #[test]
    fn approval_config_round_trips_from_toml() {
        // Pin the exact `[sop.approval]` TOML shape the broker depends on, so a schema
        // rename cannot silently break group/policy resolution.
        let toml = r#"
[groups.release]
members = ["http:abc123", "cli:test_user"]

[policies.prod]
required_group = "release"
quorum = 2
request_route = "discord.ops:111222333"
escalation_route = "discord.oncall:444555666"
"#;
        let cfg: SopApprovalConfig = toml::from_str(toml).expect("parse [sop.approval]");
        let group = cfg.groups.get("release").expect("release group");
        assert_eq!(
            group.members,
            vec!["http:abc123".to_string(), "cli:test_user".to_string()]
        );
        let policy = cfg.policies.get("prod").expect("prod policy");
        assert_eq!(policy.required_group.as_deref(), Some("release"));
        assert_eq!(policy.quorum, 2);
        // Route values are `channel:recipient` (what the real ChannelRouteAdapter
        // parses), not a bare channel name.
        assert_eq!(
            policy.request_route.as_deref(),
            Some("discord.ops:111222333")
        );
        assert_eq!(
            policy.escalation_route.as_deref(),
            Some("discord.oncall:444555666")
        );
    }
}
