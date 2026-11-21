// src/jid.rs
//! JID (Jabber ID) types that enforce invariants at construction time.
//!
//! `BareJid` is the only entry point for raw strings.  All downstream code
//! receives canonical values from the channel's RFC-aware JID parser.

use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum JidError {
    Malformed(String),
}

impl fmt::Display for JidError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            JidError::Malformed(s) => write!(f, "malformed JID: {:?}", s),
        }
    }
}
impl std::error::Error for JidError {}

/// A canonical bare user JID with no resource (`/`).
///
/// This is the only type accepted by storage, session maps, and trust APIs.
/// Construct with `BareJid::parse(s)` â€” that is the single normalization point.
/// There is no `From<String>` and no `Deref<Target=str>` to prevent silent
/// laundry back into untyped strings.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BareJid(String);

impl BareJid {
    /// Normalize with the same RFC-aware JID parser used by the channel.
    /// Trust/storage identities must be bare, with a localpart; no trimming or
    /// resource stripping can turn a malformed grant into an authorized peer.
    pub fn parse(s: &str) -> Result<Self, JidError> {
        if s.trim() != s || s.contains('/') {
            return Err(JidError::Malformed(s.into()));
        }
        let jid: jid::BareJid = s.parse().map_err(|_| JidError::Malformed(s.into()))?;
        if jid.node().is_none() {
            return Err(JidError::Malformed(s.into()));
        }
        Ok(Self(jid.to_string()))
    }

    /// Returns the JID as a `&str` for SQLite queries and stanza serialization.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for BareJid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl fmt::Debug for BareJid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "BareJid({:?})", self.0)
    }
}

// Serde support: round-trips through `parse` so deserialized values are
// always validated.
impl Serialize for BareJid {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for BareJid {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        BareJid::parse(&s).map_err(serde::de::Error::custom)
    }
}

// TryFrom<&str> and TryFrom<String> as the explicit construction paths.
impl TryFrom<&str> for BareJid {
    type Error = JidError;
    fn try_from(s: &str) -> Result<Self, Self::Error> {
        BareJid::parse(s)
    }
}

impl TryFrom<String> for BareJid {
    type Error = JidError;
    fn try_from(s: String) -> Result<Self, Self::Error> {
        BareJid::parse(&s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn identity_uses_channel_parser_and_rejects_grant_repair() {
        for value in [
            "Alice@EXAMPLE.TEST",
            "alice@example.test",
            "café@example.test",
        ] {
            let canonical: jid::BareJid = value.parse().unwrap();
            let identity = BareJid::parse(value).unwrap();
            assert_eq!(identity.as_str(), canonical.as_str());
            let encoded = serde_json::to_string(&identity).unwrap();
            assert_eq!(serde_json::from_str::<BareJid>(&encoded).unwrap(), identity);
        }
        for value in [
            "alice@example.test/resource",
            " alice@example.test",
            "alice@example.test ",
            "example.test",
            "@example.test",
            "alice@",
            "alice@example.test/",
            "ali ce@example.test",
        ] {
            assert!(
                BareJid::parse(value).is_err(),
                "Accepted repaired identity: {value}"
            );
        }
    }
}
