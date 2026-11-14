//! Transactional replenishment of existing legacy one-time prekeys.
use crate::xmpp_omemo::store::{identity, load};
use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};

use rusqlite::{Connection, TransactionBehavior, params};
use serde_json::Value;
use zeroize::Zeroizing;
const ROOT: &str = "omemo//eu.siacs.conversations.axolotl";
/// Preserve identity/signed keys and every historical ID; never recycle an ID
/// whose old bundle may remain cached by a peer. Returns newly generated count.
pub fn replenish(conn: &mut Connection, account: &str, target: usize) -> Result<usize> {
    ensure!((1..=1000).contains(&target), "Invalid prekey target");
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let (_, own, _) = identity(&tx, account)?;
    let mut state = load(&tx, &format!("{ROOT}/x3dh"))?;
    ensure!(state["version"] == "1.0.0", "Unsupported X3DH schema");
    let identity_secret = state["identity_key"]["secret"]
        .as_str()
        .context("Missing X3DH identity")?;
    let bytes: Vec<u8> = identity_secret
        .chars()
        .map(|c| u8::try_from(u32::from(c)))
        .collect::<std::result::Result<_, _>>()?;
    let bytes = Zeroizing::new(bytes);
    let mut raw = Zeroizing::new(<[u8; 32]>::try_from(bytes.as_slice())?);
    ensure!(
        state["identity_key"]["version"] == "1.0.0",
        "Unsupported X3DH identity schema"
    );
    match state["identity_key"]["secret_type"].as_str() {
        Some("PRIV") => {}
        Some("SEED") => {
            use sha2::{Digest, Sha512};
            let digest = Zeroizing::new(Sha512::digest(*raw).to_vec());
            raw.copy_from_slice(&digest[..32]);
        }
        _ => anyhow::bail!("Unsupported X3DH identity representation"),
    }
    ensure!(
        *x25519_dalek::PublicKey::from(&x25519_dalek::StaticSecret::from(*raw)).as_bytes() == own,
        "X3DH identity mismatch"
    );
    let mut ids = load(&tx, &format!("{ROOT}/pre_key_ids"))?;
    let ids = ids.as_object_mut().context("Invalid prekey index")?;
    ensure!(ids.len() <= 100000, "Prekey index maintenance required");
    let mut used = std::collections::HashSet::new();
    let mut next = 0u32;
    for (encoded, value) in ids.iter() {
        let key = STANDARD.decode(encoded)?;
        ensure!(key.len() == 32, "Invalid indexed prekey");
        let id = u32::try_from(value.as_u64().context("Invalid prekey ID")?)?;
        ensure!(
            (1..=i32::MAX as u32).contains(&id) && used.insert(id),
            "Invalid or duplicate prekey ID"
        );
        next = next.max(id);
    }
    let counter = load(&tx, &format!("{ROOT}/pre_key_id_counter"))?;
    let counter = u32::try_from(counter.as_u64().context("Invalid prekey counter")?)?;
    ensure!(
        counter >= next && counter <= i32::MAX as u32,
        "Prekey counter disagrees with historical IDs"
    );
    next = counter;
    let entries = state["pre_keys"]
        .as_array_mut()
        .context("Missing prekeys")?;
    ensure!(entries.len() <= 1000, "Invalid prekey inventory");
    let mut public_seen = std::collections::HashSet::new();
    for entry in entries.iter() {
        let bytes: Vec<u8> = entry
            .as_str()
            .context("Invalid prekey")?
            .chars()
            .map(|c| u8::try_from(u32::from(c)))
            .collect::<std::result::Result<_, _>>()?;
        let bytes = Zeroizing::new(bytes);
        let raw: [u8; 32] = bytes.as_slice().try_into()?;
        let public = x25519_dalek::PublicKey::from(&x25519_dalek::StaticSecret::from(raw));
        ensure!(
            public.as_bytes() != &own
                && public_seen.insert(*public.as_bytes())
                && ids.contains_key(&STANDARD.encode(public.as_bytes())),
            "Unindexed or duplicate prekey"
        );
    }
    let count = target.saturating_sub(entries.len());
    ensure!(
        ids.len() + count <= 100000 && u64::from(next) + count as u64 <= i32::MAX as u64,
        "Prekey ID exhaustion"
    );
    for _ in 0..count {
        let mut secret = Zeroizing::new([0u8; 32]);
        getrandom::fill(secret.as_mut()).map_err(|_| {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail),
                "Prekey randomness unavailable"
            );
            anyhow::Error::msg("Prekey randomness unavailable")
        })?;
        let public = x25519_dalek::PublicKey::from(&x25519_dalek::StaticSecret::from(*secret));
        ensure!(
            public.as_bytes() != &own
                && public_seen.insert(*public.as_bytes())
                && !ids.contains_key(&STANDARD.encode(public.as_bytes())),
            "Prekey collision"
        );
        next = next.checked_add(1).context("Prekey ID overflow")?;
        ids.insert(STANDARD.encode(public.as_bytes()), Value::from(next));
        entries.push(Value::String(
            secret.iter().map(|b| char::from(*b)).collect(),
        ));
    }
    if count > 0 {
        let encoded = Zeroizing::new(serde_json::to_string(&state)?);
        tx.execute(
            "UPDATE state SET value=?1 WHERE key=?2",
            params![encoded.as_str(), format!("{ROOT}/x3dh")],
        )?;
        let encoded = Zeroizing::new(serde_json::to_string(&ids)?);
        tx.execute(
            "UPDATE state SET value=?1 WHERE key=?2",
            params![encoded.as_str(), format!("{ROOT}/pre_key_ids")],
        )?;
    }
    if count > 0 {
        ensure!(
            tx.execute(
                "UPDATE state SET value=?1 WHERE key=?2",
                params![next.to_string(), format!("{ROOT}/pre_key_id_counter")]
            )? == 1,
            "Missing canonical prekey counter"
        );
    }
    tx.commit()?;
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn invalid_target_refuses_before_state_access() -> Result<()> {
        let mut db = Connection::open_in_memory()?;
        for target in [0, 1001, usize::MAX] {
            assert!(
                replenish(&mut db, "synthetic@example.test", target)
                    .unwrap_err()
                    .to_string()
                    .contains("Invalid prekey target")
            );
        }
        let tables: i64 = db.query_row("SELECT count(*) FROM sqlite_master", [], |r| r.get(0))?;
        assert_eq!(tables, 0);
        Ok(())
    }
}
