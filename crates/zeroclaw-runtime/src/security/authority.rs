//! Authority that is rechecked at the effect, not only at admission.
//!
//! Authorization computed once, against a snapshot of grants and resource
//! state, goes stale when the effect runs later: after a session queue
//! permit, a config write lock, a decision-model wait, a scheduler tick, or
//! on every delivered stream frame. The check was correct when it ran and
//! wrong when it mattered.
//!
//! This module makes the second check, and the commitment that follows it,
//! a type obligation. An operation is written in four steps:
//!
//! ```text
//! admit    gate + selectors on the stamped grants            -> Admitted<Op>
//! wait     the real wait; the site obtains Op::Proof         -> &Op::Proof
//! recheck  Admitted::recheck(inbound, conn, &proof, load)    -> Effect<'_, Op>
//! commit   Effect::commit(inbound, |op, live, grants| sink)  -> R
//! ```
//!
//! What the types establish, and what they do not:
//!
//! - `Effect` has no public constructor and borrows the proof, so an effect
//!   function that takes one cannot be reached without a recheck under a
//!   held guard, and the value cannot outlive that guard. It is `!Send`, so
//!   it cannot be handed to another task.
//! - The proof type is chosen by the operation (`AuthorizedOp::Proof`), so a
//!   permit for one kind of wait cannot accompany an operation of another.
//! - The principal that won admission is the principal that is rechecked;
//!   a different connection is refused.
//! - The resource facts are loaded by a closure the recheck runs after the
//!   proof is held, not passed in from the admitted snapshot.
//! - The generation travels with the resolved grants; `commit` compares it
//!   again immediately before the synchronous sink when the proof does not
//!   itself exclude policy publication (`SerializationProof::EXCLUDES_POLICY_PUBLICATION`).
//!
//! What remains the site's obligation, because a type cannot express it:
//! `commit`'s sink runs synchronously and must be the only path to the raw
//! sink (the architecture ratchet checks that); a proof that does not exclude
//! publication leaves a window between the final generation read and the
//! sink's return, and the effect is defined as committed at the sink's return.
//! A publication in that window is observed, not prevented; a proof that
//! excludes publication (the config write lock, which every accepted-policy
//! publisher takes) closes it.
//!
//! Work that runs outside a connection (cron ticks, SOP drivers, delegation
//! targets) carries a `crate::security::principal_envelope::PrincipalEnvelope`
//! instead of a connection binding; see that module.

use std::marker::PhantomData;
use std::sync::atomic::{AtomicU64, Ordering};

use zeroclaw_api::grants::ResolvedGrants;
use zeroclaw_api::principal::PrincipalId;

use crate::rpc::auth::{AuthDenied, ConnectionAuth, RpcInboundAuth};
use crate::rpc::dispatch::{Method, MethodAuthz};

/// One authority-sensitive operation, implemented per site.
pub trait AuthorizedOp: Sized + Send {
    /// Stable name for audit records and the test pause registry.
    const NAME: &'static str;

    /// The resource facts the predicate needs, loaded fresh at the recheck.
    type Live;

    /// The proof of the serialization this operation's effect runs under.
    /// One kind per operation: a session append is proven by the session
    /// permit and nothing else.
    type Proof: SerializationProof;

    /// The RPC method whose coarse grant this operation requires.
    fn method(&self) -> Method;

    /// The complete fine-grained predicate: selectors, ownership, ceilings.
    /// Called at admission with the stamped grants and a snapshot, and at
    /// recheck with freshly resolved grants and a fresh load. Never split
    /// into an admission half and a recheck half.
    fn predicate(&self, grants: &ResolvedGrants, live: &Self::Live) -> Result<(), AuthDenied>;
}

mod sealed {
    pub trait Sealed {}
    /// Private token so `Effect` cannot be built outside this module.
    pub struct Token(pub(super) ());
}

/// Proof that the caller holds the serialization the effect runs under.
/// Only the guard types in this module implement it.
pub trait SerializationProof: sealed::Sealed {
    /// Whether holding this proof excludes every accepted-policy
    /// publication. The config write lock does: `save_and_swap_config` and
    /// the gateway persist boundary both take it. A session
    /// permit, a decision settlement, a scheduler claim, or a delivery slot
    /// does not, and `Effect::commit` re-reads the generation immediately
    /// before the sink for those.
    const EXCLUDES_POLICY_PUBLICATION: bool;

    /// Short label for audit records.
    fn kind(&self) -> &'static str;
}

/// The config write lock (`RpcContext::config_write_lock`), held through
/// commit by every config mutation and every accepted-policy publication.
impl sealed::Sealed for crate::rpc::context::ConfigWriteGuard {}
impl SerializationProof for crate::rpc::context::ConfigWriteGuard {
    const EXCLUDES_POLICY_PUBLICATION: bool = true;
    fn kind(&self) -> &'static str {
        "config_write_lock"
    }
}

/// The session actor permit (`SessionActorQueue::acquire`). Serializes one
/// session's admitted work; does not order policy publication.
impl sealed::Sealed for zeroclaw_infra::session_queue::SessionGuard {}
impl SerializationProof for zeroclaw_infra::session_queue::SessionGuard {
    const EXCLUDES_POLICY_PUBLICATION: bool = false;
    fn kind(&self) -> &'static str {
        "session_admission"
    }
}

/// A SOP decision model has settled and the run is about to be admitted
/// under the engine lock. Constructed only by the SOP dispatch path that
/// awaited the model; handlers cannot mint one.
#[must_use]
pub struct DecisionSettled(());

impl DecisionSettled {
    /// Call only at the point where the decision wait has returned and the
    /// engine lock is held. The compiler cannot restrict this to that site;
    /// the authority ratchet forbids the identifier outside `sop::dispatch`.
    pub fn after_decision_wait() -> Self {
        Self(())
    }

    /// Test builds only: a fabricated proof for exercising the types.
    #[cfg(any(test, feature = "test-util"))]
    pub fn fabricate_for_tests() -> Self {
        Self(())
    }
}
impl sealed::Sealed for DecisionSettled {}
impl SerializationProof for DecisionSettled {
    const EXCLUDES_POLICY_PUBLICATION: bool = false;
    fn kind(&self) -> &'static str {
        "decision_settled"
    }
}

/// The scheduler holds this tick's claim on a job row. Constructed only by
/// the scheduler after its claim succeeded.
#[must_use]
pub struct SchedulerClaim(());

impl SchedulerClaim {
    /// Call only once the job row is claimed for this tick. The ratchet
    /// forbids the identifier outside `cron::scheduler`.
    pub fn after_claim() -> Self {
        Self(())
    }

    /// Test builds only.
    #[cfg(any(test, feature = "test-util"))]
    pub fn fabricate_for_tests() -> Self {
        Self(())
    }
}
impl sealed::Sealed for SchedulerClaim {}
impl SerializationProof for SchedulerClaim {
    const EXCLUDES_POLICY_PUBLICATION: bool = false;
    fn kind(&self) -> &'static str {
        "scheduler_claim"
    }
}

/// Writer capacity for exactly one frame is reserved and the frame is about
/// to be committed to the outbound queue. Constructed only by the delivery
/// path after `RpcOutbound::reserve` returned, per frame, so per-frame
/// rechecks are the only kind. Disclosure is defined as commitment into the
/// outbound queue; bytes already queued are not recalled.
#[must_use]
pub struct DeliverySlot(());

impl DeliverySlot {
    /// Call once per frame, after the reservation and before the send. The
    /// ratchet forbids the identifier outside the delivery path.
    pub fn after_reservation() -> Self {
        Self(())
    }

    /// Test builds only.
    #[cfg(any(test, feature = "test-util"))]
    pub fn fabricate_for_tests() -> Self {
        Self(())
    }
}
impl sealed::Sealed for DeliverySlot {}
impl SerializationProof for DeliverySlot {
    const EXCLUDES_POLICY_PUBLICATION: bool = false;
    fn kind(&self) -> &'static str {
        "delivery_slot"
    }
}

/// Identifies one invocation of an operation, so a test pause can target
/// exactly the invocation under test and not a neighbour with the same
/// `AuthorizedOp::NAME`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct InvocationId(u64);

impl InvocationId {
    fn next() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Self(NEXT.fetch_add(1, Ordering::Relaxed))
    }

    /// The raw counter value, for audit records.
    pub fn as_u64(self) -> u64 {
        self.0
    }
}

/// Freshly resolved authority: the grants and the generation they were
/// resolved under, obtained together so the generation cannot be stamped on
/// a decision made under a different one.
#[derive(Clone, Debug)]
pub struct AuthoritySnapshot {
    grants: ResolvedGrants,
    generation: u64,
}

impl AuthoritySnapshot {
    /// Liveness (expiry, revalidation deadline, native pairing), a fresh
    /// resolution against the accepted policy, and a refusal if the accepted
    /// state moved between that resolution and this read. Then the coarse
    /// grant for `method`. The generation returned is the one the grants
    /// were resolved under, never a later read.
    pub fn resolve(
        inbound: &RpcInboundAuth,
        conn: &ConnectionAuth,
        method: Method,
    ) -> Result<Self, AuthDenied> {
        inbound.credential_is_live(conn)?;
        let resolved = inbound
            .resolve_current(conn)
            .map_err(AuthDenied::from_deny_reason)?;
        if resolved.generation != inbound.generation() {
            return Err(AuthDenied::auth_required(
                crate::i18n::get_required_cli_string("rpc-auth-revalidation-due"),
            ));
        }
        if let MethodAuthz::Requires(resource, verb) = method.authz()
            && !resolved.grants.permits(resource, verb)
        {
            return Err(AuthDenied::forbidden(format!(
                "Principal is not granted {resource}:{verb} (required by {})",
                method.wire_name()
            )));
        }
        Ok(Self {
            grants: resolved.grants,
            generation: resolved.generation,
        })
    }

    pub fn grants(&self) -> &ResolvedGrants {
        &self.grants
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }
}

/// An operation that passed admission and has not yet been rechecked. It
/// has no effect: nothing accepts it but `Admitted::recheck`.
#[must_use = "an admitted operation has no effect until it is rechecked"]
pub struct Admitted<Op: AuthorizedOp> {
    op: Op,
    principal: PrincipalId,
    admitted_generation: u64,
    invocation: InvocationId,
}

impl<Op: AuthorizedOp> std::fmt::Debug for Admitted<Op> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Admitted")
            .field("op", &Op::NAME)
            .field("principal", &self.principal)
            .field("admitted_generation", &self.admitted_generation)
            .field("invocation", &self.invocation)
            .finish_non_exhaustive()
    }
}

/// The only value a commit accepts. No public constructor; the single way
/// to obtain one is `Admitted::recheck`. It borrows the proof, so it cannot
/// outlive the guard, and it is `!Send`, so it cannot leave the task that
/// holds the guard.
pub struct Effect<'p, Op: AuthorizedOp> {
    op: Op,
    principal: PrincipalId,
    snapshot: AuthoritySnapshot,
    live: Op::Live,
    proof: &'p Op::Proof,
    invocation: InvocationId,
    _not_send: PhantomData<*const ()>,
    _sealed: sealed::Token,
}

impl<Op: AuthorizedOp> std::fmt::Debug for Effect<'_, Op> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Effect")
            .field("op", &Op::NAME)
            .field("principal", &self.principal)
            .field("generation", &self.snapshot.generation)
            .field("proof_kind", &self.proof.kind())
            .field("invocation", &self.invocation)
            .finish_non_exhaustive()
    }
}

impl<Op: AuthorizedOp> Admitted<Op> {
    /// Step 1. Runs the coarse grant for `op.method()` and the full
    /// predicate on the grants stamped on `conn`, against `live` as it is
    /// now. `live` is a snapshot; the recheck loads it again.
    pub fn admit(op: Op, conn: &ConnectionAuth, live: &Op::Live) -> Result<Self, AuthDenied> {
        let method = op.method();
        if let MethodAuthz::Requires(resource, verb) = method.authz()
            && !conn.grants.permits(resource, verb)
        {
            return Err(AuthDenied::forbidden(format!(
                "Principal is not granted {resource}:{verb} (required by {})",
                method.wire_name()
            )));
        }
        op.predicate(&conn.grants, live)?;
        Ok(Self {
            op,
            principal: conn.principal.id.clone(),
            admitted_generation: conn.generation,
            invocation: InvocationId::next(),
        })
    }

    pub fn op(&self) -> &Op {
        &self.op
    }

    pub fn invocation(&self) -> InvocationId {
        self.invocation
    }

    pub fn admitted_generation(&self) -> u64 {
        self.admitted_generation
    }

    /// Step 3. Consumes the admission and, with `proof` held:
    ///
    /// 1. refuses if `conn` is not the principal that won admission;
    /// 2. resolves fresh authority (`AuthoritySnapshot::resolve`);
    /// 3. runs `load_live` to read the resource facts now, not before;
    /// 4. runs the same predicate on both.
    ///
    /// In test builds the pause armed for this invocation fires at
    /// `Stage::BeforeResolve`, after the proof is held and before step 2.
    pub async fn recheck<'p>(
        self,
        inbound: &RpcInboundAuth,
        conn: &ConnectionAuth,
        proof: &'p Op::Proof,
        load_live: impl FnOnce() -> Result<Op::Live, AuthDenied>,
    ) -> Result<Effect<'p, Op>, AuthDenied> {
        if conn.principal.id != self.principal {
            return Err(AuthDenied::forbidden(format!(
                "{} was admitted for a different principal",
                Op::NAME
            )));
        }

        #[cfg(any(test, feature = "test-util"))]
        test_pause::registry()
            .wait_if_armed(
                Op::NAME,
                Some(self.invocation),
                test_pause::Stage::BeforeResolve,
                true,
            )
            .await;

        #[cfg(any(test, feature = "test-util"))]
        match mutation::current(Op::NAME) {
            mutation::AuthorityMutation::AlwaysDeny => {
                return Err(AuthDenied::forbidden(format!(
                    "{} refused by the AlwaysDeny test mutation",
                    Op::NAME
                )));
            }
            mutation::AuthorityMutation::SkipRecheck => {
                // The twin that proves a probe can see an unchecked effect:
                // the stamped grants stand in for a resolution that never ran.
                let live = load_live()?;
                return Ok(Effect {
                    op: self.op,
                    principal: self.principal,
                    snapshot: AuthoritySnapshot {
                        grants: conn.grants.clone(),
                        generation: conn.generation,
                    },
                    live,
                    proof,
                    invocation: self.invocation,
                    _not_send: PhantomData,
                    _sealed: sealed::Token(()),
                });
            }
            _ => {}
        }

        let snapshot = AuthoritySnapshot::resolve(inbound, conn, self.op.method())?;
        let live = load_live()?;
        self.op.predicate(&snapshot.grants, &live)?;
        Ok(Effect {
            op: self.op,
            principal: self.principal,
            snapshot,
            live,
            proof,
            invocation: self.invocation,
            _not_send: PhantomData,
            _sealed: sealed::Token(()),
        })
    }
}

impl<Op: AuthorizedOp> Effect<'_, Op> {
    pub fn op(&self) -> &Op {
        &self.op
    }

    pub fn live(&self) -> &Op::Live {
        &self.live
    }

    pub fn snapshot(&self) -> &AuthoritySnapshot {
        &self.snapshot
    }

    pub fn principal(&self) -> &PrincipalId {
        &self.principal
    }

    pub fn invocation(&self) -> InvocationId {
        self.invocation
    }

    pub fn proof_kind(&self) -> &'static str {
        self.proof.kind()
    }

    /// Step 4. Runs `sink` synchronously with the checked operation, the
    /// loaded resource facts and the fresh grants. The sink must be the only
    /// path to the raw write, start, or disclosure; it receives the target
    /// from `op`, never from a second parameter.
    ///
    /// For a proof that does not exclude policy publication, the generation
    /// is read once more immediately before the sink and the commit fails
    /// closed if it moved since resolution. That narrows the window to the
    /// sink's own execution; it does not close it. For the config write lock
    /// no publication can have landed, so no re-read is needed.
    ///
    /// In test builds the pause for this invocation fires at
    /// `Stage::BeforeCommit`, after the recheck and before the final
    /// generation read; it blocks the calling thread, so drive the operation
    /// from a task on a multi-thread runtime.
    pub fn commit<R>(
        self,
        inbound: &RpcInboundAuth,
        sink: impl FnOnce(&Op, &Op::Live, &ResolvedGrants) -> R,
    ) -> Result<R, AuthDenied> {
        #[cfg(any(test, feature = "test-util"))]
        test_pause::registry().wait_if_armed_blocking(
            Op::NAME,
            Some(self.invocation),
            test_pause::Stage::BeforeCommit,
            true,
        );

        #[cfg(any(test, feature = "test-util"))]
        let skip_final_check = matches!(
            mutation::current(Op::NAME),
            mutation::AuthorityMutation::SkipFinalGenerationCheck
                | mutation::AuthorityMutation::SkipRecheck
        );
        #[cfg(not(any(test, feature = "test-util")))]
        let skip_final_check = false;

        if !Op::Proof::EXCLUDES_POLICY_PUBLICATION && !skip_final_check {
            let now = inbound.generation();
            if now != self.snapshot.generation {
                return Err(AuthDenied::auth_required(format!(
                    "{} lost authority before commit: policy generation moved from {} to {now}",
                    Op::NAME,
                    self.snapshot.generation
                )));
            }
        }
        Ok(sink(&self.op, &self.live, &self.snapshot.grants))
    }
}

/// Test-only mutations of the recheck, so a site's tests can prove their
/// probes and controls observe the effect (the "twins" in the ratchet
/// design). Keyed by `AuthorizedOp::NAME`; process-wide, so tests that set a
/// mutation for the same name must not run concurrently.
#[cfg(any(test, feature = "test-util"))]
pub mod mutation {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};

    #[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
    pub enum AuthorityMutation {
        #[default]
        None,
        /// `recheck` returns an `Effect` built from the stamped grants
        /// without resolving or running the predicate, and `commit` skips
        /// its final generation read. A revoked-scenario probe must then be
        /// positive, or the probe cannot see the effect.
        SkipRecheck,
        /// `recheck` refuses everything. The unchanged-policy control must
        /// then fail, or the control does not exercise the effect.
        AlwaysDeny,
        /// `commit` skips only its final generation read.
        SkipFinalGenerationCheck,
    }

    fn table() -> &'static Mutex<HashMap<&'static str, AuthorityMutation>> {
        static TABLE: OnceLock<Mutex<HashMap<&'static str, AuthorityMutation>>> = OnceLock::new();
        TABLE.get_or_init(|| Mutex::new(HashMap::new()))
    }

    /// The mutation in force for `name`.
    pub fn current(name: &'static str) -> AuthorityMutation {
        table()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(name)
            .copied()
            .unwrap_or_default()
    }

    /// Set a mutation for `name` until the guard drops.
    pub fn set(name: &'static str, mutation: AuthorityMutation) -> MutationGuard {
        table()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(name, mutation);
        MutationGuard { name }
    }

    pub struct MutationGuard {
        name: &'static str,
    }

    impl Drop for MutationGuard {
        fn drop(&mut self) {
            table()
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(self.name);
        }
    }
}

/// Test-only pause points, shared by every site.
///
/// A pause is armed by a test for one operation name and, optionally, one
/// invocation. It fires inside the code under test at a named stage, records
/// which invocation and stage fired and whether the serialization proof was
/// held, and parks until the test releases it. Async and blocking flavours
/// share one `TestPause`.
#[cfg(any(test, feature = "test-util"))]
pub mod test_pause {
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::{Arc, Condvar, Mutex, OnceLock};

    use tokio::sync::Notify;

    use super::InvocationId;

    /// Where in the operation a pause fired.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Stage {
        /// Inside `Admitted::recheck`: proof held, authority not yet resolved.
        BeforeResolve,
        /// Inside `Effect::commit`: recheck done, final generation read and
        /// sink not yet run.
        BeforeCommit,
        /// A storage access in `PausingSessionBackend`, or a site-specific
        /// hook such as `SessionStore`'s prompt registration.
        Access,
    }

    /// One armable pause point.
    #[derive(Default)]
    pub struct TestPause {
        armed: Mutex<Option<Arc<Armed>>>,
    }

    struct Armed {
        /// `None` matches any invocation; `Some` only that one.
        invocation: Option<InvocationId>,
        stage: Option<Stage>,
        entered: Notify,
        released: (Mutex<bool>, Condvar),
        fired: AtomicBool,
        proof_held: AtomicBool,
        fired_invocation: AtomicU64,
        fired_stage: Mutex<Option<Stage>>,
    }

    /// What a test holds while a pause is armed. Dropping it disarms the
    /// pause and releases anything still parked on it.
    pub struct PauseHandle {
        armed: Arc<Armed>,
        owner: Arc<TestPause>,
    }

    impl TestPause {
        /// Arm for any invocation at any stage.
        pub fn arm(self: &Arc<Self>) -> PauseHandle {
            self.arm_at(None, None)
        }

        /// Arm for one invocation (or any, with `None`) at one stage (or
        /// any, with `None`).
        pub fn arm_at(
            self: &Arc<Self>,
            invocation: Option<InvocationId>,
            stage: Option<Stage>,
        ) -> PauseHandle {
            let armed = Arc::new(Armed {
                invocation,
                stage,
                entered: Notify::new(),
                released: (Mutex::new(false), Condvar::new()),
                fired: AtomicBool::new(false),
                proof_held: AtomicBool::new(false),
                fired_invocation: AtomicU64::new(0),
                fired_stage: Mutex::new(None),
            });
            *self.armed.lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::clone(&armed));
            PauseHandle {
                armed,
                owner: Arc::clone(self),
            }
        }

        fn matching(&self, invocation: Option<InvocationId>, stage: Stage) -> Option<Arc<Armed>> {
            let armed = self
                .armed
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .as_ref()
                .map(Arc::clone)?;
            if armed.fired.load(Ordering::SeqCst) {
                return None;
            }
            if let Some(want) = armed.invocation
                && invocation != Some(want)
            {
                return None;
            }
            if let Some(want) = armed.stage
                && want != stage
            {
                return None;
            }
            Some(armed)
        }

        /// Async flavour. A no-op unless armed for this invocation and stage.
        pub async fn wait_if_armed(
            &self,
            invocation: Option<InvocationId>,
            stage: Stage,
            proof_held: bool,
        ) {
            let Some(armed) = self.matching(invocation, stage) else {
                return;
            };
            armed.fire(invocation, stage, proof_held);
            let parked = Arc::clone(&armed);
            tokio::task::spawn_blocking(move || parked.block_until_released())
                .await
                .expect("the pause waiter is never cancelled");
        }

        /// Blocking flavour for synchronous code under test.
        pub fn wait_if_armed_blocking(
            &self,
            invocation: Option<InvocationId>,
            stage: Stage,
            proof_held: bool,
        ) {
            let Some(armed) = self.matching(invocation, stage) else {
                return;
            };
            armed.fire(invocation, stage, proof_held);
            armed.block_until_released();
        }
    }

    impl Armed {
        fn fire(&self, invocation: Option<InvocationId>, stage: Stage, proof_held: bool) {
            self.proof_held.store(proof_held, Ordering::SeqCst);
            self.fired_invocation
                .store(invocation.map_or(0, InvocationId::as_u64), Ordering::SeqCst);
            *self.fired_stage.lock().unwrap_or_else(|e| e.into_inner()) = Some(stage);
            self.fired.store(true, Ordering::SeqCst);
            self.entered.notify_one();
        }

        fn block_until_released(&self) {
            let (lock, cvar) = &self.released;
            let mut released = lock.lock().unwrap_or_else(|e| e.into_inner());
            while !*released {
                released = cvar.wait(released).unwrap_or_else(|e| e.into_inner());
            }
        }

        fn release(&self) {
            let (lock, cvar) = &self.released;
            *lock.lock().unwrap_or_else(|e| e.into_inner()) = true;
            cvar.notify_all();
        }
    }

    impl PauseHandle {
        /// Resolves when the code under test has reached the pause point.
        pub async fn admitted(&self) {
            if self.armed.fired.load(Ordering::SeqCst) {
                return;
            }
            self.armed.entered.notified().await;
        }

        /// Let the parked code continue.
        pub fn release(&self) {
            self.armed.release();
        }

        pub fn fired(&self) -> bool {
            self.armed.fired.load(Ordering::SeqCst)
        }

        /// The guard assertion: whether the serialization proof was held
        /// when the pause fired.
        pub fn proof_was_held_at_pause(&self) -> bool {
            self.armed.proof_held.load(Ordering::SeqCst)
        }

        /// Which invocation fired the pause (`None` before it fires, or when
        /// the firing site had no invocation).
        pub fn fired_invocation(&self) -> Option<InvocationId> {
            match self.armed.fired_invocation.load(Ordering::SeqCst) {
                0 => None,
                id => Some(InvocationId(id)),
            }
        }

        /// Which stage fired the pause.
        pub fn fired_stage(&self) -> Option<Stage> {
            *self
                .armed
                .fired_stage
                .lock()
                .unwrap_or_else(|e| e.into_inner())
        }

        /// The legacy `(entered, release)` shape used by
        /// `SessionStore::set_test_prompt_registration_pause` callers.
        pub fn legacy_pair(&self) -> (Arc<Notify>, Arc<Notify>) {
            let entered = Arc::new(Notify::new());
            let release = Arc::new(Notify::new());
            let armed = Arc::clone(&self.armed);
            let entered_out = Arc::clone(&entered);
            let release_in = Arc::clone(&release);
            zeroclaw_spawn::spawn!(async move {
                armed.entered.notified().await;
                entered_out.notify_one();
                release_in.notified().await;
                armed.release();
            });
            (entered, release)
        }
    }

    impl Drop for PauseHandle {
        fn drop(&mut self) {
            let mut slot = self.owner.armed.lock().unwrap_or_else(|e| e.into_inner());
            if slot
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, &self.armed))
            {
                *slot = None;
            }
            drop(slot);
            self.armed.release();
        }
    }

    /// Process-wide registry of pause points keyed by `AuthorizedOp::NAME`.
    pub struct PauseRegistry {
        points: Mutex<HashMap<&'static str, Arc<TestPause>>>,
    }

    impl PauseRegistry {
        fn point(&self, name: &'static str) -> Arc<TestPause> {
            Arc::clone(
                self.points
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .entry(name)
                    .or_default(),
            )
        }

        fn existing(&self, name: &'static str) -> Option<Arc<TestPause>> {
            self.points
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(name)
                .map(Arc::clone)
        }

        /// Arm for the next invocation of `name` at `stage` (any stage with
        /// `None`). Use when the test cannot know the invocation id before
        /// the operation starts; read it back from `fired_invocation`.
        pub fn arm_next(&self, name: &'static str, stage: Option<Stage>) -> PauseHandle {
            self.point(name).arm_at(None, stage)
        }

        /// Arm for exactly one invocation of `name`.
        pub fn arm_for(
            &self,
            name: &'static str,
            invocation: InvocationId,
            stage: Option<Stage>,
        ) -> PauseHandle {
            self.point(name).arm_at(Some(invocation), stage)
        }

        pub async fn wait_if_armed(
            &self,
            name: &'static str,
            invocation: Option<InvocationId>,
            stage: Stage,
            proof_held: bool,
        ) {
            if let Some(point) = self.existing(name) {
                point.wait_if_armed(invocation, stage, proof_held).await;
            }
        }

        pub fn wait_if_armed_blocking(
            &self,
            name: &'static str,
            invocation: Option<InvocationId>,
            stage: Stage,
            proof_held: bool,
        ) {
            if let Some(point) = self.existing(name) {
                point.wait_if_armed_blocking(invocation, stage, proof_held);
            }
        }
    }

    /// The process-wide registry.
    pub fn registry() -> &'static PauseRegistry {
        static REGISTRY: OnceLock<PauseRegistry> = OnceLock::new();
        REGISTRY.get_or_init(|| PauseRegistry {
            points: Mutex::new(HashMap::new()),
        })
    }

    /// Arm the pause for the next invocation of `name`, at any stage.
    pub fn authority_test_pause(name: &'static str) -> PauseHandle {
        registry().arm_next(name, None)
    }

    /// Arm the pause for one invocation of `name` at one stage.
    pub fn authority_test_pause_for(
        name: &'static str,
        invocation: InvocationId,
        stage: Stage,
    ) -> PauseHandle {
        registry().arm_for(name, invocation, Some(stage))
    }

    use zeroclaw_api::model_provider::ChatMessage;
    use zeroclaw_infra::session_backend::{
        SessionBackend, SessionContext, SessionMetadata, SessionQuery, SessionState,
        TimestampedMessage,
    };

    /// A session backend that parks immediately before every access while
    /// its pause is armed, so a test can change ownership or policy between
    /// an operation's admission and its storage effect.
    ///
    /// The forwarded methods are synchronous and block the calling thread
    /// while parked, so drive the operation under test from a task on a
    /// multi-thread runtime (or `spawn_blocking`) and release from the test
    /// body. This is the crate-level form of the session-tools tests'
    /// `PauseBeforeAccessBackend`.
    pub struct PausingSessionBackend {
        inner: Arc<dyn SessionBackend>,
        pause: Arc<TestPause>,
    }

    impl PausingSessionBackend {
        pub fn new(inner: Arc<dyn SessionBackend>) -> Self {
            Self {
                inner,
                pause: Arc::new(TestPause::default()),
            }
        }

        /// The pause point; arm it with `TestPause::arm`.
        pub fn pause(&self) -> &Arc<TestPause> {
            &self.pause
        }

        /// The wrapped backend, for the test to mutate state behind the
        /// parked operation.
        pub fn inner(&self) -> &Arc<dyn SessionBackend> {
            &self.inner
        }

        fn park(&self) {
            // A backend access holds no serialization proof of its own; the
            // caller that obtained the permit records that separately.
            self.pause
                .wait_if_armed_blocking(None, Stage::Access, false);
        }
    }

    impl SessionBackend for PausingSessionBackend {
        fn load(&self, session_key: &str) -> Vec<ChatMessage> {
            self.park();
            self.inner.load(session_key)
        }
        fn try_load(&self, session_key: &str) -> std::io::Result<Vec<ChatMessage>> {
            self.park();
            self.inner.try_load(session_key)
        }
        fn load_with_timestamps(&self, session_key: &str) -> Vec<TimestampedMessage> {
            self.park();
            self.inner.load_with_timestamps(session_key)
        }
        fn append(&self, session_key: &str, message: &ChatMessage) -> std::io::Result<()> {
            self.park();
            self.inner.append(session_key, message)
        }
        fn remove_last(&self, session_key: &str) -> std::io::Result<bool> {
            self.park();
            self.inner.remove_last(session_key)
        }
        fn rewrite_messages(
            &self,
            session_key: &str,
            messages: &[ChatMessage],
        ) -> std::io::Result<()> {
            self.park();
            self.inner.rewrite_messages(session_key, messages)
        }
        fn update_last(&self, session_key: &str, message: &ChatMessage) -> std::io::Result<bool> {
            self.park();
            self.inner.update_last(session_key, message)
        }
        fn list_sessions(&self) -> Vec<String> {
            self.park();
            self.inner.list_sessions()
        }
        fn list_sessions_with_metadata(&self) -> Vec<SessionMetadata> {
            self.park();
            self.inner.list_sessions_with_metadata()
        }
        fn compact(&self, session_key: &str) -> std::io::Result<()> {
            self.park();
            self.inner.compact(session_key)
        }
        fn cleanup_stale(&self, ttl_hours: u32) -> std::io::Result<usize> {
            self.park();
            self.inner.cleanup_stale(ttl_hours)
        }
        fn search(&self, query: &SessionQuery) -> Vec<SessionMetadata> {
            self.park();
            self.inner.search(query)
        }
        fn clear_messages(&self, session_key: &str) -> std::io::Result<usize> {
            self.park();
            self.inner.clear_messages(session_key)
        }
        fn delete_session(&self, session_key: &str) -> std::io::Result<bool> {
            self.park();
            self.inner.delete_session(session_key)
        }
        fn clear_agent_attribution(&self, agent_alias: &str) -> std::io::Result<usize> {
            self.park();
            self.inner.clear_agent_attribution(agent_alias)
        }
        fn rename_agent_attribution(&self, from: &str, to: &str) -> std::io::Result<usize> {
            self.park();
            self.inner.rename_agent_attribution(from, to)
        }
        fn count_agent_attribution(&self, agent_alias: &str) -> std::io::Result<usize> {
            self.park();
            self.inner.count_agent_attribution(agent_alias)
        }
        fn session_exists(&self, session_key: &str) -> bool {
            self.park();
            self.inner.session_exists(session_key)
        }
        fn set_session_name(&self, session_key: &str, name: &str) -> std::io::Result<()> {
            self.park();
            self.inner.set_session_name(session_key, name)
        }
        fn get_session_name(&self, session_key: &str) -> std::io::Result<Option<String>> {
            self.park();
            self.inner.get_session_name(session_key)
        }
        fn set_session_agent_alias(
            &self,
            session_key: &str,
            agent_alias: &str,
        ) -> std::io::Result<()> {
            self.park();
            self.inner.set_session_agent_alias(session_key, agent_alias)
        }
        fn get_session_agent_alias(&self, session_key: &str) -> std::io::Result<Option<String>> {
            self.park();
            self.inner.get_session_agent_alias(session_key)
        }
        fn set_session_trim_breadcrumb(
            &self,
            session_key: &str,
            present: bool,
        ) -> std::io::Result<()> {
            self.park();
            self.inner.set_session_trim_breadcrumb(session_key, present)
        }
        fn get_session_trim_breadcrumb(&self, session_key: &str) -> std::io::Result<Option<bool>> {
            self.park();
            self.inner.get_session_trim_breadcrumb(session_key)
        }
        fn replace_conversation_state(
            &self,
            session_key: &str,
            messages: &[ChatMessage],
            breadcrumb_present: bool,
        ) -> std::io::Result<()> {
            self.park();
            self.inner
                .replace_conversation_state(session_key, messages, breadcrumb_present)
        }
        fn replace_conversation_state_if_exists(
            &self,
            session_key: &str,
            messages: &[ChatMessage],
            breadcrumb_present: bool,
        ) -> std::io::Result<bool> {
            self.park();
            self.inner.replace_conversation_state_if_exists(
                session_key,
                messages,
                breadcrumb_present,
            )
        }
        fn set_session_context(
            &self,
            session_key: &str,
            context: SessionContext<'_>,
        ) -> std::io::Result<()> {
            self.park();
            self.inner.set_session_context(session_key, context)
        }
        fn get_session_metadata(&self, session_key: &str) -> Option<SessionMetadata> {
            self.park();
            self.inner.get_session_metadata(session_key)
        }
        fn set_session_principal(
            &self,
            session_key: &str,
            principal_id: &str,
        ) -> std::io::Result<()> {
            self.park();
            self.inner.set_session_principal(session_key, principal_id)
        }
        fn delete_session_owned(
            &self,
            session_key: &str,
            owner_principal_id: &str,
        ) -> std::io::Result<bool> {
            self.park();
            self.inner
                .delete_session_owned(session_key, owner_principal_id)
        }
        fn set_session_state(
            &self,
            session_key: &str,
            state: &str,
            turn_id: Option<&str>,
        ) -> std::io::Result<()> {
            self.park();
            self.inner.set_session_state(session_key, state, turn_id)
        }
        fn get_session_state(&self, session_key: &str) -> std::io::Result<Option<SessionState>> {
            self.park();
            self.inner.get_session_state(session_key)
        }
        fn list_running_sessions(&self) -> Vec<SessionMetadata> {
            self.park();
            self.inner.list_running_sessions()
        }
        fn list_stuck_sessions(&self, threshold_secs: u64) -> Vec<SessionMetadata> {
            self.park();
            self.inner.list_stuck_sessions(threshold_secs)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use zeroclaw_api::grants::{Resource, Verb};
    use zeroclaw_config::pairing::{PairingCodePolicy, PairingGuard};
    use zeroclaw_config::schema::{Config, PermissionProfileConfig, UserConfig};

    use crate::rpc::transport::TransportKind;
    use crate::security::auth_provider::Credential;

    const ALICE_UID: u32 = 4242;
    const BOB_UID: u32 = 4243;

    /// A roster with alice holding `sessions:read` on `agents`, and bob
    /// holding the same on every agent.
    fn config_with_alice(agents: &[&str]) -> Config {
        let mut config = Config::default();
        // The selector validation refuses a profile that names an agent the
        // config does not define, and an invalid policy compiles to deny-all.
        for alias in ["alpha", "beta"] {
            config.agents.insert(
                alias.to_string(),
                zeroclaw_config::schema::AliasedAgentConfig::default(),
            );
        }
        config.permission_profiles.insert(
            "reader".into(),
            PermissionProfileConfig {
                grants: std::collections::HashMap::from([(Resource::Sessions, vec![Verb::Read])]),
                allowed_agents: agents.iter().map(|a| (*a).to_string()).collect(),
                ..PermissionProfileConfig::default()
            },
        );
        config.permission_profiles.insert(
            "reader-all".into(),
            PermissionProfileConfig {
                grants: std::collections::HashMap::from([(Resource::Sessions, vec![Verb::Read])]),
                allowed_agents: vec!["*".into()],
                ..PermissionProfileConfig::default()
            },
        );
        config.users.insert(
            "alice".into(),
            UserConfig {
                principal_id: None,
                uid: Some(ALICE_UID),
                permission_profiles: vec!["reader".into()],
            },
        );
        config.users.insert(
            "bob".into(),
            UserConfig {
                principal_id: None,
                uid: Some(BOB_UID),
                permission_profiles: vec!["reader-all".into()],
            },
        );
        config
    }

    fn inbound_for(config: &Config) -> RpcInboundAuth {
        RpcInboundAuth::from_config(
            config,
            Arc::new(PairingGuard::new(true, &[], PairingCodePolicy::default())),
        )
        .expect("valid policy")
    }

    async fn peer_on(inbound: &RpcInboundAuth, uid: u32) -> ConnectionAuth {
        inbound
            .authenticate(
                TransportKind::Local,
                Credential::Peercred { uid },
                None,
                None,
            )
            .await
            .expect("the uid is on the roster")
    }

    /// A read of one agent's sessions under the config write lock: coarse
    /// `sessions:read` plus the agent selector, with the live fact being
    /// which agent the session belongs to.
    struct ReadAgentSessions;

    impl AuthorizedOp for ReadAgentSessions {
        const NAME: &'static str = "test_read_agent_sessions";
        type Live = String;
        type Proof = crate::rpc::context::ConfigWriteGuard;

        fn method(&self) -> Method {
            Method::SessionList
        }

        fn predicate(&self, grants: &ResolvedGrants, live: &String) -> Result<(), AuthDenied> {
            if grants.may_use_agent(live) {
                Ok(())
            } else {
                Err(AuthDenied::forbidden(format!(
                    "not entitled to agent {live:?}"
                )))
            }
        }
    }

    /// The same read, but proven by a decision settlement, which does not
    /// exclude publication, so `commit` performs the final generation read.
    struct ReadAfterDecision;

    impl AuthorizedOp for ReadAfterDecision {
        const NAME: &'static str = "test_read_after_decision";
        type Live = String;
        type Proof = DecisionSettled;

        fn method(&self) -> Method {
            Method::SessionList
        }

        fn predicate(&self, grants: &ResolvedGrants, live: &String) -> Result<(), AuthDenied> {
            ReadAgentSessions.predicate(grants, live)
        }
    }

    fn write_lock() -> crate::rpc::context::ConfigWriteGuard {
        Arc::new(tokio::sync::Mutex::new(()))
            .try_lock_owned()
            .expect("fresh lock")
    }

    fn alpha() -> Result<String, AuthDenied> {
        Ok("alpha".to_string())
    }

    #[tokio::test]
    async fn admit_recheck_commit_runs_the_sink_when_nothing_changed() {
        let inbound = inbound_for(&config_with_alice(&["alpha"]));
        let conn = peer_on(&inbound, ALICE_UID).await;
        let lock = write_lock();
        let admitted = Admitted::admit(ReadAgentSessions, &conn, &"alpha".to_string()).unwrap();
        let effect = admitted
            .recheck(&inbound, &conn, &lock, alpha)
            .await
            .expect("still authorized");
        assert_eq!(effect.proof_kind(), "config_write_lock");
        assert_eq!(effect.snapshot().generation(), inbound.generation());
        let ran = effect
            .commit(&inbound, |_op, live, grants| {
                assert_eq!(live, "alpha");
                grants.permits(Resource::Sessions, Verb::Read)
            })
            .expect("commit runs");
        assert!(ran);
    }

    #[tokio::test]
    async fn admit_refuses_the_coarse_grant_and_the_selector() {
        let inbound = inbound_for(&config_with_alice(&["alpha"]));
        let conn = peer_on(&inbound, ALICE_UID).await;
        let denied = Admitted::admit(ReadAgentSessions, &conn, &"beta".to_string()).unwrap_err();
        assert!(denied.message.contains("beta"), "{denied:?}");
    }

    #[tokio::test]
    async fn recheck_refuses_a_grant_narrowed_after_admission() {
        let inbound = inbound_for(&config_with_alice(&["alpha"]));
        let conn = peer_on(&inbound, ALICE_UID).await;
        let lock = write_lock();
        let admitted = Admitted::admit(ReadAgentSessions, &conn, &"alpha".to_string()).unwrap();
        inbound
            .refresh_from_config(&config_with_alice(&[]))
            .expect("narrowed policy compiles");
        let denied = admitted
            .recheck(&inbound, &conn, &lock, alpha)
            .await
            .unwrap_err();
        assert!(denied.message.contains("alpha"), "{denied:?}");
    }

    /// The consumed right changes while the needed right stays: alice keeps
    /// alpha (admission) but the recheck reads the live resource as beta,
    /// which only the widened policy allows. Proves re-resolution rather
    /// than comparison against the stamped grants.
    #[tokio::test]
    async fn recheck_resolves_fresh_grants_for_the_right_the_execution_consumes() {
        let inbound = inbound_for(&config_with_alice(&["alpha"]));
        let conn = peer_on(&inbound, ALICE_UID).await;
        let lock = write_lock();
        let admitted = Admitted::admit(ReadAgentSessions, &conn, &"alpha".to_string()).unwrap();
        inbound
            .refresh_from_config(&config_with_alice(&["alpha", "beta"]))
            .expect("widened policy compiles");
        let effect = admitted
            .recheck(&inbound, &conn, &lock, || Ok("beta".to_string()))
            .await
            .expect("the widened grant is honoured");
        assert!(effect.snapshot().grants().may_use_agent("beta"));
    }

    #[tokio::test]
    async fn recheck_refuses_a_resource_reowned_after_admission() {
        let inbound = inbound_for(&config_with_alice(&["alpha"]));
        let conn = peer_on(&inbound, ALICE_UID).await;
        let lock = write_lock();
        let admitted = Admitted::admit(ReadAgentSessions, &conn, &"alpha".to_string()).unwrap();
        // Same policy; the loader now reports the resource as beta's.
        let denied = admitted
            .recheck(&inbound, &conn, &lock, || Ok("beta".to_string()))
            .await
            .unwrap_err();
        assert!(denied.message.contains("beta"), "{denied:?}");
    }

    #[tokio::test]
    async fn recheck_refuses_a_different_principal_than_the_one_admitted() {
        let inbound = inbound_for(&config_with_alice(&["alpha"]));
        let alice = peer_on(&inbound, ALICE_UID).await;
        let bob = peer_on(&inbound, BOB_UID).await;
        let lock = write_lock();
        let admitted = Admitted::admit(ReadAgentSessions, &alice, &"alpha".to_string()).unwrap();
        let denied = admitted
            .recheck(&inbound, &bob, &lock, alpha)
            .await
            .unwrap_err();
        assert!(denied.message.contains("different principal"), "{denied:?}");
    }

    #[tokio::test]
    async fn the_generation_travels_with_the_resolved_grants() {
        let inbound = inbound_for(&config_with_alice(&["alpha"]));
        let conn = peer_on(&inbound, ALICE_UID).await;
        let before = inbound.generation();
        let snapshot = AuthoritySnapshot::resolve(&inbound, &conn, Method::SessionList).unwrap();
        assert_eq!(snapshot.generation(), before);
        // A later publication does not relabel an earlier snapshot.
        inbound
            .refresh_from_config(&config_with_alice(&["alpha", "beta"]))
            .unwrap();
        assert_eq!(snapshot.generation(), before);
        assert_ne!(inbound.generation(), before);
    }

    /// A proof that does not exclude publication: a policy published after
    /// the recheck and before the sink fails the commit closed.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn commit_under_a_non_excluding_proof_fails_closed_when_the_generation_moved() {
        let inbound = Arc::new(inbound_for(&config_with_alice(&["alpha"])));
        let conn = peer_on(&inbound, ALICE_UID).await;
        let admitted = Admitted::admit(ReadAfterDecision, &conn, &"alpha".to_string()).unwrap();
        let invocation = admitted.invocation();
        let pause = test_pause::authority_test_pause_for(
            ReadAfterDecision::NAME,
            invocation,
            test_pause::Stage::BeforeCommit,
        );

        let inbound_task = Arc::clone(&inbound);
        let task = zeroclaw_spawn::spawn!(async move {
            let settled = DecisionSettled::fabricate_for_tests();
            let effect = admitted
                .recheck(&inbound_task, &conn, &settled, alpha)
                .await
                .expect("recheck passes under the unchanged policy");
            effect
                .commit(&inbound_task, |_, _, _| "ran")
                .map(str::to_owned)
        });

        pause.admitted().await;
        assert_eq!(pause.fired_stage(), Some(test_pause::Stage::BeforeCommit));
        assert_eq!(pause.fired_invocation(), Some(invocation));
        assert!(pause.proof_was_held_at_pause());
        // Publish between the recheck and the sink. The proof did not
        // exclude this, so the commit must observe it.
        inbound
            .refresh_from_config(&config_with_alice(&["alpha", "beta"]))
            .unwrap();
        pause.release();

        let outcome = task.await.expect("task completes");
        let denied = outcome.unwrap_err();
        assert!(
            denied.message.contains("policy generation moved"),
            "{denied:?}"
        );
    }

    // Compile-time: only the config write lock excludes policy publication.
    const _: () = assert!(
        <crate::rpc::context::ConfigWriteGuard as SerializationProof>::EXCLUDES_POLICY_PUBLICATION
    );
    const _: () = assert!(!<DecisionSettled as SerializationProof>::EXCLUDES_POLICY_PUBLICATION);
    const _: () = assert!(!<SchedulerClaim as SerializationProof>::EXCLUDES_POLICY_PUBLICATION);
    const _: () = assert!(!<DeliverySlot as SerializationProof>::EXCLUDES_POLICY_PUBLICATION);

    #[tokio::test]
    async fn the_pause_fires_after_the_proof_is_held_and_before_re_resolution() {
        let inbound = Arc::new(inbound_for(&config_with_alice(&["alpha"])));
        let conn = peer_on(&inbound, ALICE_UID).await;
        let admitted = Admitted::admit(ReadAgentSessions, &conn, &"alpha".to_string()).unwrap();
        let invocation = admitted.invocation();
        let pause = test_pause::authority_test_pause_for(
            ReadAgentSessions::NAME,
            invocation,
            test_pause::Stage::BeforeResolve,
        );

        let inbound_task = Arc::clone(&inbound);
        let task = zeroclaw_spawn::spawn!(async move {
            let lock = write_lock();
            admitted
                .recheck(&inbound_task, &conn, &lock, alpha)
                .await
                .map(|effect| effect.snapshot().generation())
        });

        pause.admitted().await;
        assert!(pause.proof_was_held_at_pause());
        assert_eq!(pause.fired_stage(), Some(test_pause::Stage::BeforeResolve));
        inbound
            .refresh_from_config(&config_with_alice(&[]))
            .expect("narrowed policy compiles");
        pause.release();

        let outcome = task.await.expect("recheck task completes");
        assert!(
            outcome.is_err(),
            "a narrowing applied during the pause must refuse"
        );
    }

    #[tokio::test]
    async fn a_pause_armed_for_one_invocation_ignores_another() {
        let inbound = inbound_for(&config_with_alice(&["alpha"]));
        let conn = peer_on(&inbound, ALICE_UID).await;
        let first = Admitted::admit(ReadAgentSessions, &conn, &"alpha".to_string()).unwrap();
        let second = Admitted::admit(ReadAgentSessions, &conn, &"alpha".to_string()).unwrap();
        let pause = test_pause::authority_test_pause_for(
            ReadAgentSessions::NAME,
            second.invocation(),
            test_pause::Stage::BeforeResolve,
        );
        let lock = write_lock();
        // The first invocation must not park on a pause armed for the second.
        first
            .recheck(&inbound, &conn, &lock, alpha)
            .await
            .expect("first passes without parking");
        assert!(!pause.fired());
        drop(second);
    }

    #[tokio::test]
    async fn the_skip_recheck_mutation_lets_an_unauthorized_effect_through() {
        let inbound = inbound_for(&config_with_alice(&["alpha"]));
        let conn = peer_on(&inbound, ALICE_UID).await;
        let lock = write_lock();
        let admitted = Admitted::admit(ReadAgentSessions, &conn, &"alpha".to_string()).unwrap();
        inbound
            .refresh_from_config(&config_with_alice(&[]))
            .expect("narrowed policy compiles");
        let _mutation = mutation::set(
            ReadAgentSessions::NAME,
            mutation::AuthorityMutation::SkipRecheck,
        );
        let effect = admitted
            .recheck(&inbound, &conn, &lock, alpha)
            .await
            .expect("the mutation skips the recheck");
        let ran = effect
            .commit(&inbound, |_, _, _| true)
            .expect("commit runs");
        assert!(
            ran,
            "a probe in a twin test must be able to observe this effect"
        );
    }

    #[tokio::test]
    async fn the_always_deny_mutation_refuses_an_unchanged_policy() {
        let inbound = inbound_for(&config_with_alice(&["alpha"]));
        let conn = peer_on(&inbound, ALICE_UID).await;
        let lock = write_lock();
        let admitted = Admitted::admit(ReadAgentSessions, &conn, &"alpha".to_string()).unwrap();
        let _mutation = mutation::set(
            ReadAgentSessions::NAME,
            mutation::AuthorityMutation::AlwaysDeny,
        );
        assert!(
            admitted
                .recheck(&inbound, &conn, &lock, alpha)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn an_unarmed_pause_is_a_no_op() {
        let point = Arc::new(test_pause::TestPause::default());
        point
            .wait_if_armed(None, test_pause::Stage::BeforeResolve, true)
            .await;
        point.wait_if_armed_blocking(None, test_pause::Stage::Access, true);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_pausing_backend_parks_before_the_access_and_sees_the_change() {
        use zeroclaw_api::model_provider::ChatMessage;
        use zeroclaw_infra::session_backend::SessionBackend;

        let tmp = tempfile::TempDir::new().expect("tempdir");
        let inner = zeroclaw_infra::make_session_backend(tmp.path(), "sqlite").expect("backend");
        inner
            .append("shared", &ChatMessage::user("before"))
            .expect("seed");
        let backend = Arc::new(test_pause::PausingSessionBackend::new(Arc::clone(&inner)));
        let handle = backend.pause().arm();

        let reader = Arc::clone(&backend);
        let task = tokio::task::spawn_blocking(move || reader.load("shared"));

        handle.admitted().await;
        assert!(
            !handle.proof_was_held_at_pause(),
            "a backend access holds no proof"
        );
        assert_eq!(handle.fired_stage(), Some(test_pause::Stage::Access));
        inner
            .append("shared", &ChatMessage::user("during"))
            .expect("append while parked");
        handle.release();

        let seen = task.await.expect("reader completes");
        assert_eq!(
            seen.len(),
            2,
            "the parked read sees the change made while it waited"
        );
    }

    #[tokio::test]
    async fn dropping_the_handle_disarms_and_releases() {
        let point = Arc::new(test_pause::TestPause::default());
        let handle = point.arm();
        let waiter = {
            let point = Arc::clone(&point);
            zeroclaw_spawn::spawn!(async move {
                point
                    .wait_if_armed(None, test_pause::Stage::BeforeResolve, false)
                    .await
            })
        };
        handle.admitted().await;
        drop(handle);
        tokio::time::timeout(std::time::Duration::from_secs(2), waiter)
            .await
            .expect("dropping the handle releases the parked waiter")
            .expect("waiter task completes");
        point
            .wait_if_armed(None, test_pause::Stage::BeforeResolve, true)
            .await;
    }
}
