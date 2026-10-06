//! Per-session output through the shared held-authority enqueue helper.

use super::{auth::ConnectionAuth, context::RpcContext};
use std::sync::{Arc, Weak};
use tokio_util::sync::CancellationToken;
use zeroclaw_api::jsonrpc::RpcOutbound;
use zeroclaw_rpc_proto::Method;

/// The connection's immutable identity evidence, used to resolve live
/// authority on every send. The weak context avoids a session/channel cycle.
#[derive(Clone)]
pub(crate) struct SessionEmissionAuthority {
    context: Weak<RpcContext>,
    binding: Option<ConnectionAuth>,
    cancel: CancellationToken,
    remote: bool,
    tui_id: Option<String>,
}

impl SessionEmissionAuthority {
    pub(crate) fn new(
        context: &Arc<RpcContext>,
        binding: Option<ConnectionAuth>,
        cancel: CancellationToken,
        remote: bool,
        tui_id: Option<String>,
    ) -> Self {
        Self {
            context: Arc::downgrade(context),
            binding,
            cancel,
            remote,
            tui_id,
        }
    }

    /// Reserve capacity without a session/authority lock. The predicate
    /// resolves current grants and holds the canonical owner/incarnation
    /// through the shared helper's enqueue. Contention retries without holding
    /// capacity or authority; a refusal is never cached as a later allowance.
    pub(crate) async fn send(
        &self,
        rpc: &RpcOutbound,
        session_id: &str,
        generation: Option<u64>,
        data: bool,
        packet: String,
    ) -> bool {
        let Some(ctx) = self.context.upgrade() else {
            return false;
        };
        loop {
            let mut owner_hold = None;
            let mut contended = false;
            let sent = super::dispatch::deliver_frame(
                rpc,
                &self.cancel,
                &ctx.auth,
                |authority| {
                    let Some(hold) = ctx.sessions.try_effect_guard() else {
                        contended = true;
                        return false;
                    };
                    let session = hold.session(session_id);
                    if generation.is_some() && session.map(|s| s.generation) != generation {
                        return false;
                    }
                    if data && session.is_none() {
                        return false;
                    }
                    if self.remote
                        && !matches!(session.and_then(|s| s.owner_tui_id.as_deref()),
                    Some(owner) if self.tui_id.as_deref() == Some(owner))
                    {
                        return false;
                    }
                    if let Some(binding) = self.binding.as_ref() {
                        let grants = match super::dispatch::current_authority_under(
                            authority,
                            binding,
                            Method::SessionPrompt,
                        ) {
                            Ok(grants) => grants,
                            Err(denied) => {
                                super::dispatch::audit_denial(
                                    Some(binding),
                                    Method::SessionPrompt,
                                    &denied,
                                );
                                return false;
                            }
                        };
                        if let Some(session) = session {
                            if !grants.admin
                                && binding.principal.is_authenticated()
                                && session.owner_principal_id.as_deref()
                                    != Some(binding.principal.id.as_str())
                            {
                                return false;
                            }
                            if data && !grants.may_use_agent(&session.agent_alias) {
                                return false;
                            }
                        }
                    }
                    owner_hold = Some(hold);
                    true
                },
                packet.clone(),
            )
            .await;
            drop(owner_hold);
            if sent || !contended {
                return sent;
            }
            tokio::select! {
                biased;
                () = self.cancel.cancelled() => return false,
                hold = ctx.sessions.effect_guard() => drop(hold),
            }
        }
    }
}

/// The immutable incarnation this prompt's output belongs to. Authority is
/// resolved afresh; this correlation is created at prompt admission.
#[derive(Clone)]
pub struct SessionOutput {
    pub(crate) authority: SessionEmissionAuthority,
    pub(crate) generation: Option<u64>,
}

tokio::task_local! {
    static OUTPUT: std::cell::RefCell<Option<SessionOutput>>;
}

pub(crate) async fn scope<T>(future: impl std::future::Future<Output = T>) -> T {
    OUTPUT.scope(std::cell::RefCell::new(None), future).await
}

pub(crate) fn bind(output: SessionOutput) {
    let _ = OUTPUT.try_with(|current| *current.borrow_mut() = Some(output));
}

pub(crate) fn current() -> Option<SessionOutput> {
    OUTPUT
        .try_with(|current| current.borrow().clone())
        .ok()
        .flatten()
}
