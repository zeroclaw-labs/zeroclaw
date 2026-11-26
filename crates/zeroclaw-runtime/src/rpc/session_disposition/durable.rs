//! The native storage effect selected by the RPC session owner.

use std::{io, sync::Arc};

use anyhow::{Result, ensure};
use zeroclaw_infra::{
    acp_session_store::{AcpSessionKillTransition, AcpSessionStore},
    session_backend::{SessionBackend, SessionEffectGuard},
    session_disposition::{
        SessionDispositionBackend, SessionDispositionNotAttempted, SessionDispositionWitness,
    },
};

use crate::control_plane::task_store_sqlite::session_disposition::SessionDispositionOrigin;

type AuthorizedEffect<'a> = Result<Box<dyn SessionEffectGuard + 'a>>;

/// A selected store and its lookup key. Authorization is supplied at effect
/// time and stays inside the selected backend's mutation boundary.
pub(in crate::rpc) enum DurableRemoval {
    ChatDelete {
        backend: Arc<dyn SessionBackend>,
        key: String,
    },
    AcpDelete {
        store: Arc<AcpSessionStore>,
        session_id: String,
    },
    AcpKill {
        store: Arc<AcpSessionStore>,
        session_id: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::rpc) enum DurableRemovalOutcome {
    Changed,
    AlreadyKilled,
    Missing,
}

impl DurableRemoval {
    /// Read the existing backend identity and exact target for the witness.
    /// For ACP the backend supplies the immutable row ID, not the reusable
    /// session UUID. Both methods perform synchronous backend work.
    pub(in crate::rpc) fn origin(&self) -> Result<Option<SessionDispositionOrigin>> {
        let target = match self {
            Self::ChatDelete { backend, key } => {
                Some((backend.session_disposition_identity()?, key.clone()))
            }
            Self::AcpDelete { store, session_id } | Self::AcpKill { store, session_id } => {
                store.session_disposition_target(session_id)?
            }
        };
        Ok(target.map(
            |(identity, backend_key)| SessionDispositionOrigin::Durable {
                backend_key,
                identity,
            },
        ))
    }

    /// Refuse an unreadable JSONL transcript before recording a disposition.
    /// The delete rechecks it under the backend's mutation guard.
    pub(in crate::rpc) fn validate_before_prepare(
        &self,
        origin: &SessionDispositionOrigin,
    ) -> Result<()> {
        if let Self::ChatDelete { backend, key } = self
            && let SessionDispositionOrigin::Durable { identity, .. } = origin
            && identity.backend == SessionDispositionBackend::ChatJsonl
        {
            backend.try_load(key)?;
        }
        Ok(())
    }

    /// Attempt the selected native effect once. A missing row or existing
    /// tombstone is not a successful new commit, and there is no fallback.
    pub(in crate::rpc) fn apply<'a>(
        &self,
        witness: &SessionDispositionWitness,
        owner: Option<&str>,
        authorize: &dyn Fn(Option<&str>) -> AuthorizedEffect<'a>,
    ) -> Result<DurableRemovalOutcome> {
        let deleted = match self {
            Self::ChatDelete { backend, key } => {
                ensure!(
                    key == &witness.backend_key,
                    "session disposition names a different Chat target"
                );
                backend.delete_session_with_disposition(witness, owner, &|actual| {
                    authorize(actual).map_err(|error| {
                        // Preserve typed RPC denials across the native I/O
                        // callback so the caller can retain their exact code.
                        io::Error::other(error.reallocate_into_boxed_dyn_error_without_backtrace())
                    })
                })?
            }
            Self::AcpDelete { store, session_id } => {
                store.delete_session_with_disposition(session_id, witness, owner, authorize)?
            }
            Self::AcpKill { store, session_id } => {
                return Ok(
                    match store.mark_session_killed_with_disposition(
                        session_id, witness, owner, authorize,
                    )? {
                        AcpSessionKillTransition::Marked => DurableRemovalOutcome::Changed,
                        AcpSessionKillTransition::AlreadyKilled => {
                            DurableRemovalOutcome::AlreadyKilled
                        }
                        AcpSessionKillTransition::Missing => DurableRemovalOutcome::Missing,
                    },
                );
            }
        };
        Ok(if deleted {
            DurableRemovalOutcome::Changed
        } else {
            DurableRemovalOutcome::Missing
        })
    }
}

pub(in crate::rpc) fn is_not_attempted(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<io::Error>()
            .and_then(io::Error::get_ref)
            .is_some_and(|source| source.is::<SessionDispositionNotAttempted>())
    })
}
