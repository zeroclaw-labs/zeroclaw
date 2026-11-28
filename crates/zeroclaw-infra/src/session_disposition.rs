//! Operation-specific evidence for committed durable session effects.
//!
//! Backends attest to their own effects only. The task control plane owns
//! cleanup authority and the Goal binding; a receipt never authorizes a
//! session mutation or identifies which Goal to dispose.

use std::{fmt, io};

pub(crate) mod jsonl;
pub(crate) mod sqlite;

/// A witnessed JSONL deletion refused before any staging, sync or rename.
/// This ephemeral error is usable only by the effect's in-process owner.
#[derive(Debug)]
pub struct SessionDispositionNotAttempted(io::Error);

impl fmt::Display for SessionDispositionNotAttempted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

impl std::error::Error for SessionDispositionNotAttempted {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.0)
    }
}

#[cfg(unix)]
pub(crate) fn not_attempted(error: io::Error) -> io::Error {
    io::Error::new(error.kind(), SessionDispositionNotAttempted(error))
}

/// Durable session owner that can emit the witness.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionDispositionBackend {
    ChatSqlite,
    ChatJsonl,
    AcpSqlite,
}

/// Exact configured backend instance. A replacement at the same path is
/// not the same instance.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionDispositionIdentity {
    pub backend: SessionDispositionBackend,
    pub instance_id: String,
}

/// Session effect being attested. Replacement and close of a live
/// incarnation are handled by the runtime's live owner, not these stores.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionDispositionEffect {
    Delete,
    Kill,
}

impl SessionDispositionEffect {
    /// Stable private storage value, independent of RPC method spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Delete => "delete",
            Self::Kill => "kill",
        }
    }
}

/// Exact receipt key created by the trusted transition owner before effect.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionDispositionWitness {
    pub operation_id: String,
    pub effect: SessionDispositionEffect,
    pub backend_key: String,
    pub identity: SessionDispositionIdentity,
}

/// Observation from the original backend. Only exact positive evidence
/// permits post-commit cleanup.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionDispositionObservation {
    Committed,
    NotCommitted,
    Indeterminate,
}
