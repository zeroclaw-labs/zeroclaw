//! Passive legacy session installation. One-time prekey consumption, metadata,
//! ratchet and replay marker are committed only after full payload authentication.
use crate::xmpp_omemo::{
    legacy::LegacySession,
    omemo::{
        crypto,
        keys::{AesGcmKey, GcmNonce, PublicKey, Secret},
        protocol::{DoubleRatchet, KeyPair},
        wire::PreKeySignalMessage,
    },
    store::{Inbound, identity, load},
};
use anyhow::{Context, Result, ensure};
use base64::{
    Engine,
    engine::general_purpose::{STANDARD, URL_SAFE},
};
use curve25519_dalek::edwards::CompressedEdwardsY;
use rusqlite::{Connection, Transaction, TransactionBehavior, params};
use serde_json::{Value, json};
use zeroize::Zeroizing;

const ROOT: &str = "omemo//eu.siacs.conversations.axolotl";
fn latin(value: &Value) -> Result<[u8; 32]> {
    let text = value.as_str().context("Missing private key")?;
    let v: Vec<u8> = text
        .chars()
        .map(|c| u8::try_from(u32::from(c)))
        .collect::<std::result::Result<_, _>>()?;
    v.try_into().map_err(|_| {
        ::zeroclaw_log::record!(
            ERROR,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail),
            "Invalid private key length"
        );
        anyhow::Error::msg("Invalid private key length")
    })
}
fn pair(private: [u8; 32]) -> KeyPair {
    let public = x25519_dalek::PublicKey::from(&x25519_dalek::StaticSecret::from(private));
    KeyPair {
        public_key: PublicKey::new(*public.as_bytes()),
        private_key: Secret::new(private),
    }
}
fn mapped(tx: &Transaction<'_>, name: &str, id: u32) -> Result<[u8; 32]> {
    let map = load(tx, &format!("{ROOT}/{name}"))?;
    let matches: Vec<_> = map
        .as_object()
        .context("Invalid key index")?
        .iter()
        .filter(|(_, v)| v.as_u64() == Some(u64::from(id)))
        .collect();
    if matches.is_empty() {
        return Err(crate::xmpp_omemo::RejectedInbound.into());
    }
    ensure!(matches.len() == 1, "Ambiguous local prekey ID");
    STANDARD.decode(matches[0].0)?.try_into().map_err(|_| {
        ::zeroclaw_log::record!(
            ERROR,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail),
            "Invalid indexed key"
        );
        anyhow::Error::msg("Invalid indexed key")
    })
}
fn put(tx: &Transaction<'_>, key: &str, value: &Value) -> Result<()> {
    let text = Zeroizing::new(serde_json::to_string(value)?);
    tx.execute("INSERT INTO state(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",params![key,text.as_str()])?;
    Ok(())
}
fn append(tx: &Transaction<'_>, key: &str, value: Value) -> Result<()> {
    let raw: Option<String> = tx
        .query_row("SELECT value FROM state WHERE key=?1", [key], |r| r.get(0))
        .optional()?;
    let mut values: Value = match raw {
        Some(v) => serde_json::from_str(&v)?,
        None => json!([]),
    };
    let items = values.as_array_mut().context("Invalid session index")?;
    if !items.contains(&value) {
        items.push(value);
    }
    put(tx, key, &values)
}
use rusqlite::OptionalExtension;

pub fn receive_fresh_with(
    conn: &mut Connection,
    input: Inbound<'_>,
    peer_ed: [u8; 32],
    persist: impl FnOnce(&Transaction<'_>, &[u8]) -> Result<()>,
) -> Result<Vec<u8>> {
    ensure!(
        !input.stanza_id.is_empty() && input.stanza_id.len() <= 512,
        crate::xmpp_omemo::RejectedInbound
    );
    ensure!(
        !input.peer.contains('/') && input.peer.contains('@') && input.peer.len() <= 1023,
        crate::xmpp_omemo::RejectedInbound
    );
    ensure!(
        (1..=i32::MAX as u32).contains(&input.peer_device),
        crate::xmpp_omemo::RejectedInbound
    );
    ensure!(
        input.wire.len() <= 8192 && input.ciphertext.len() <= 1024 * 1024,
        crate::xmpp_omemo::RejectedInbound
    );
    let exchange =
        PreKeySignalMessage::deserialize(input.wire).ok_or(crate::xmpp_omemo::RejectedInbound)?;
    let point = CompressedEdwardsY(peer_ed)
        .decompress()
        .ok_or(crate::xmpp_omemo::RejectedInbound)?;
    ensure!(
        !point.is_small_order() && point.is_torsion_free(),
        crate::xmpp_omemo::RejectedInbound
    );
    let peer_curve = point.to_montgomery().to_bytes();
    ensure!(
        peer_curve == input.pinned_fingerprint && exchange.identity_key == peer_curve,
        crate::xmpp_omemo::RejectedInbound
    );
    let prekey_id = exchange
        .pre_key_id
        .ok_or(crate::xmpp_omemo::RejectedInbound)?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let (private, public, own_id) = identity(&tx, input.account)?;
    let prefix = format!("{ROOT}/{}/{}/", input.peer, input.peer_device);
    let exists: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM state WHERE key=?1)",
        [format!("{prefix}double_ratchet")],
        |r| r.get(0),
    )?;
    ensure!(!exists, "Refusing to replace an existing session");
    let seen: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM seen WHERE sender=?1 AND id=?2)",
        params![input.peer, input.stanza_id],
        |r| r.get(0),
    )?;
    ensure!(!seen, crate::xmpp_omemo::RejectedInbound);
    let spk_pub = mapped(&tx, "signed_pre_key_ids", exchange.signed_pre_key_id)?;
    let pk_pub = mapped(&tx, "pre_key_ids", prekey_id)?;
    let mut state = load(&tx, &format!("{ROOT}/x3dh"))?;
    ensure!(state["version"] == "1.0.0", "Unsupported X3DH schema");
    ensure!(
        state["identity_key"]["version"] == "1.0.0",
        "Unsupported X3DH identity schema"
    );
    let mut stored_identity = latin(&state["identity_key"]["secret"])?;
    match state["identity_key"]["secret_type"].as_str() {
        Some("SEED") => {
            use sha2::{Digest, Sha512};
            let digest = Zeroizing::new(Sha512::digest(stored_identity).to_vec());
            stored_identity.copy_from_slice(&digest[..32]);
            stored_identity[0] &= 248;
            stored_identity[31] &= 127;
            stored_identity[31] |= 64;
        }
        Some("PRIV") => {}
        _ => anyhow::bail!("Unsupported X3DH identity representation"),
    }
    ensure!(
        pair(stored_identity).public_key.as_raw() == &public,
        "X3DH identity does not match the canonical identity"
    );
    let mut signed = None;
    for field in ["signed_pre_key", "old_signed_pre_key"] {
        if state[field].is_null() {
            continue;
        }
        ensure!(
            state[field]["version"] == "1.0.0",
            "Unsupported signed prekey schema"
        );
        let candidate = pair(latin(&state[field]["priv"])?);
        if candidate.public_key.as_raw() == &spk_pub {
            ensure!(signed.is_none(), "Ambiguous signed prekey");
            signed = Some(candidate);
        }
    }
    let entries = state["pre_keys"]
        .as_array_mut()
        .context("Missing prekeys")?;
    ensure!(entries.len() <= 1000, "Too many prekeys");
    let mut selected = None;
    for (index, value) in entries.iter().enumerate() {
        let candidate = pair(latin(value)?);
        if candidate.public_key.as_raw() == &pk_pub {
            ensure!(selected.is_none(), "Ambiguous one-time prekey");
            selected = Some((index, candidate));
        }
    }
    let (index, prekey) = selected.ok_or(crate::xmpp_omemo::RejectedInbound)?;
    let mut ratchet = DoubleRatchet::new_session_recipient(
        pair(private),
        peer_curve.to_vec(),
        signed.context("Signed prekey unavailable")?,
        Some(prekey),
        exchange.base_key.clone(),
        own_id.into(),
        input.peer_device.into(),
        input.peer.into(),
    )
    .map_err(crate::xmpp_omemo::reject_input)?;
    let material = Zeroizing::new(
        DoubleRatchet::decrypt_key(&mut ratchet, &exchange.raw_message_bytes)
            .map_err(crate::xmpp_omemo::reject_input)?,
    );
    ensure!(material.len() == 32, crate::xmpp_omemo::RejectedInbound);
    let combined = [input.ciphertext, &material[16..]].concat();
    let plaintext = Zeroizing::new(
        crypto::aes_gcm_decrypt(
            &combined,
            &AesGcmKey::from_slice(&material[..16]).context("Invalid payload key")?,
            &GcmNonce::from_slice(input.nonce).context("Invalid payload IV")?,
        )
        .map_err(crate::xmpp_omemo::reject_input)?,
    );
    let session = LegacySession::from_fresh_recipient(ratchet)?;
    let encode = |bytes: &[u8]| Value::String(URL_SAFE.encode(bytes));
    let associated = [vec![5], peer_curve.to_vec(), vec![5], public.to_vec()].concat();
    let metadata = [
        ("initiation", json!("PASSIVE")),
        ("key_exchange/identity_key", encode(&peer_ed)),
        ("key_exchange/ephemeral_key", encode(&exchange.base_key)),
        ("key_exchange/signed_pre_key", encode(&spk_pub)),
        (
            "key_exchange/signed_pre_key_id",
            json!(exchange.signed_pre_key_id),
        ),
        ("key_exchange/pre_key", encode(&pk_pub)),
        ("key_exchange/pre_key_id", json!(prekey_id)),
        ("associated_data", encode(&associated)),
        ("confirmed", json!(false)),
        ("double_ratchet", session.export()?),
    ];
    for (name, value) in metadata {
        put(&tx, &format!("{prefix}{name}"), &value)?;
    }
    entries.remove(index);
    put(&tx, &format!("{ROOT}/x3dh"), &state)?;
    let mut ids = load(&tx, &format!("{ROOT}/pre_key_ids"))?;
    ids.as_object_mut()
        .context("Invalid prekey index")?
        .remove(&STANDARD.encode(pk_pub));
    put(&tx, &format!("{ROOT}/pre_key_ids"), &ids)?;
    append(&tx, &format!("{ROOT}/bare_jids"), json!(input.peer))?;
    append(
        &tx,
        &format!("{ROOT}/{}/device_ids", input.peer),
        json!(input.peer_device),
    )?;
    tx.execute(
        "INSERT INTO seen(sender,id) VALUES(?1,?2)",
        params![input.peer, input.stanza_id],
    )?;
    persist(&tx, &plaintext)?;
    tx.commit()?;
    Ok(plaintext.to_vec())
}
