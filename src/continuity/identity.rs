use std::time::{SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};

use crate::soul::constitution::Constitution;
use crate::soul::model::SoulModel;

use super::types::{Episode, Identity, IdentityCore, Preference};

pub fn identity_from_soul(
    soul: &SoulModel,
    constitution: &Constitution,
    preferences: Vec<Preference>,
    narrative: Vec<Episode>,
    session_count: u64,
) -> Identity {
    identity_from_soul_with_epoch(
        soul,
        constitution,
        preferences,
        narrative,
        session_count,
        None,
    )
}

pub fn identity_from_soul_with_epoch(
    soul: &SoulModel,
    constitution: &Constitution,
    preferences: Vec<Preference>,
    narrative: Vec<Episode>,
    session_count: u64,
    epoch: Option<u64>,
) -> Identity {
    let creation_epoch = epoch.unwrap_or_else(|| {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    });
    Identity {
        core: IdentityCore {
            name: soul.name.clone(),
            constitution_hash: constitution.hash().to_string(),
            creation_epoch,
            immutable_values: soul.values.clone(),
        },
        preferences,
        narrative,
        commitments: Vec::new(),
        session_count,
    }
}

pub fn compute_identity_checksum(identity: &Identity) -> String {
    let mut hasher = Sha256::new();
    hasher.update(identity.core.name.as_bytes());
    hasher.update(identity.core.constitution_hash.as_bytes());
    hasher.update(identity.core.creation_epoch.to_le_bytes());
    for v in &identity.core.immutable_values {
        hasher.update(v.as_bytes());
    }
    for p in &identity.preferences {
        hasher.update(p.key.as_bytes());
        hasher.update(p.value.as_bytes());
        hasher.update(p.confidence.to_le_bytes());
    }
    for ep in &identity.narrative {
        hasher.update(ep.summary.as_bytes());
        hasher.update(ep.timestamp.to_le_bytes());
    }
    hasher.update(identity.session_count.to_le_bytes());
    hex::encode(hasher.finalize())
}

pub fn verify_identity_rebuild(expected: &str, identity: &Identity) -> bool {
    compute_identity_checksum(identity) == expected
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_identity_from_soul() {
        let soul = SoulModel {
            name: "zeroclaw".into(),
            values: vec!["honesty".into(), "autonomy".into()],
            ..Default::default()
        };
        let constitution = Constitution::default_laws();
        let id = identity_from_soul(&soul, &constitution, Vec::new(), Vec::new(), 5);
        assert_eq!(id.core.name, "zeroclaw");
        assert!(!id.core.constitution_hash.is_empty());
        assert_eq!(id.session_count, 5);
        assert_eq!(id.core.immutable_values.len(), 2);
    }
}
