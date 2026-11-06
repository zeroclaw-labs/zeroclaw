//! Transactional established-session adapter for the existing compatibility schema.
//! File ownership/locking and live peer authorization are the channel's responsibility.
//! Session updates and send reservations share the canonical private database.
use crate::xmpp_omemo::legacy::LegacySession;
use anyhow::{Context, Result, ensure};
use base64::{
    Engine,
    engine::general_purpose::{URL_SAFE, URL_SAFE_NO_PAD},
};
use curve25519_dalek::edwards::CompressedEdwardsY;
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde_json::Value;
use sha2::{Digest, Sha512};
use zeroize::Zeroizing;

pub struct Inbound<'a> {
    pub account: &'a str,
    pub peer: &'a str,
    pub peer_device: u32,
    /// Obtained from current operator policy, never from a stanza/bundle.
    pub pinned_fingerprint: [u8; 32],
    pub stanza_id: &'a str,
    pub wire: &'a [u8],
    pub ciphertext: &'a [u8],
    pub nonce: &'a [u8],
}
pub(crate) fn load(tx: &Transaction<'_>, key: &str) -> Result<Value> {
    let raw: String = tx
        .query_row("SELECT value FROM state WHERE key=?1", [key], |row| {
            row.get(0)
        })
        .context("Required OMEMO state is missing")?;
    let raw = Zeroizing::new(raw);
    serde_json::from_str(&raw).context("Invalid stored OMEMO JSON")
}
pub(crate) fn decode(value: &Value) -> Result<[u8; 32]> {
    let encoded = value.as_str().context("Invalid encoded key")?;
    let bytes = Zeroizing::new(
        URL_SAFE
            .decode(encoded)
            .or_else(|_| URL_SAFE_NO_PAD.decode(encoded))
            .context("Invalid key encoding")?,
    );
    bytes.as_slice().try_into().context("Invalid key size")
}
pub(crate) fn identity(tx: &Transaction<'_>, account: &str) -> Result<([u8; 32], [u8; 32], u32)> {
    ensure!(load(tx, "account")? == account, "State account mismatch");
    let public = load(tx, "public/identity")?;
    ensure!(public["jid"] == account, "Identity account mismatch");
    let id = u32::try_from(public["device_id"].as_u64().context("Missing device ID")?)?;
    ensure!((1..=i32::MAX as u32).contains(&id), "Invalid device ID");
    let encoded = public["fingerprint"]
        .as_str()
        .context("Missing fingerprint")?;
    let mut fingerprint = hex::decode(encoded.split_whitespace().collect::<String>())?;
    if fingerprint.len() == 33 && fingerprint[0] == 5 {
        fingerprint.remove(0);
    }
    let fingerprint: [u8; 32] = fingerprint.try_into().map_err(|_| {
        ::zeroclaw_log::record!(
            ERROR,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail),
            "Invalid fingerprint size"
        );
        anyhow::Error::msg("Invalid fingerprint size")
    })?;
    let mut private = decode(&load(tx, "omemo//ikp/key")?)?;
    match load(tx, "omemo//ikp/is_seed")?.as_bool() {
        Some(true) => {
            let digest = Zeroizing::new(Sha512::digest(private).to_vec());
            private.copy_from_slice(&digest[..32]);
            private[0] &= 248;
            private[31] &= 127;
            private[31] |= 64;
        }
        Some(false) => {}
        None => anyhow::bail!("Invalid identity representation"),
    }
    let computed = x25519_dalek::PublicKey::from(&x25519_dalek::StaticSecret::from(private));
    ensure!(
        computed.as_bytes() == &fingerprint,
        "Stored identity fingerprint mismatch"
    );
    Ok((private, fingerprint, id))
}

fn session_identity(
    tx: &Transaction<'_>,
    prefix: &str,
    own: [u8; 32],
    pin: [u8; 32],
) -> Result<[u8; 32]> {
    let initiation = load(tx, &format!("{prefix}initiation"))?;
    let associated = load(tx, &format!("{prefix}associated_data"))?;
    let encoded = associated
        .as_str()
        .context("Missing session identity binding")?;
    let associated = URL_SAFE
        .decode(encoded)
        .or_else(|_| URL_SAFE_NO_PAD.decode(encoded))?;
    ensure!(
        associated.len() == 66 && associated[0] == 5 && associated[33] == 5,
        "Invalid legacy associated identities"
    );
    let first: [u8; 32] = associated[1..33].try_into()?;
    let second: [u8; 32] = associated[34..66].try_into()?;
    let ed = decode(&load(tx, &format!("{prefix}key_exchange/identity_key"))?)?;
    let point = CompressedEdwardsY(ed)
        .decompress()
        .context("Invalid exchange identity point")?;
    ensure!(
        !point.is_small_order() && point.is_torsion_free(),
        "Invalid exchange identity subgroup"
    );
    let header_identity = point.to_montgomery().to_bytes();
    let remote = match initiation.as_str() {
        Some("ACTIVE") => {
            ensure!(
                first == own && header_identity == own,
                "Active session identity mismatch"
            );
            second
        }
        Some("PASSIVE") => {
            ensure!(
                second == own && header_identity == first,
                "Passive session identity mismatch"
            );
            first
        }
        _ => anyhow::bail!("Invalid session initiation"),
    };
    ensure!(remote == pin, "Peer fingerprint is not pinned");
    Ok(remote)
}

pub fn receive_with(
    conn: &mut Connection,
    input: Inbound<'_>,
    persist: impl FnOnce(&Transaction<'_>, &[u8]) -> Result<()>,
) -> Result<Vec<u8>> {
    ensure!(
        !input.stanza_id.is_empty() && input.stanza_id.len() <= 512,
        "Invalid stanza ID"
    );
    ensure!(
        (1..=i32::MAX as u32).contains(&input.peer_device),
        "Invalid peer device"
    );
    ensure!(
        !input.peer.contains('/') && input.peer.len() <= 1023 && input.peer.contains('@'),
        "Invalid peer JID"
    );
    ensure!(input.wire.len() <= 8192, "Encrypted key exceeds limit");
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let (private, public, own_id) = identity(&tx, input.account)?;
    let prefix = format!(
        "omemo//eu.siacs.conversations.axolotl/{}/{}/",
        input.peer, input.peer_device
    );
    let remote = session_identity(&tx, &prefix, public, input.pinned_fingerprint)?;
    if let Some(exchange) =
        crate::xmpp_omemo::omemo::wire::PreKeySignalMessage::deserialize(input.wire)
    {
        ensure!(
            exchange.identity_key == remote
                && exchange.base_key
                    == decode(&load(&tx, &format!("{prefix}key_exchange/ephemeral_key"))?)?,
            crate::xmpp_omemo::RejectedInbound
        );
        ensure!(
            load(&tx, &format!("{prefix}key_exchange/signed_pre_key_id"))?.as_u64()
                == Some(u64::from(exchange.signed_pre_key_id)),
            crate::xmpp_omemo::RejectedInbound
        );
        ensure!(
            load(&tx, &format!("{prefix}key_exchange/pre_key_id"))?.as_u64()
                == exchange.pre_key_id.map(u64::from),
            crate::xmpp_omemo::RejectedInbound
        );
    }
    let seen: Option<i64> = tx
        .query_row(
            "SELECT 1 FROM seen WHERE sender=?1 AND id=?2",
            params![input.peer, input.stanza_id],
            |row| row.get(0),
        )
        .optional()?;
    ensure!(seen.is_none(), "Repeated stanza");
    let ratchet_key = format!("{prefix}double_ratchet");
    let mut session = LegacySession::import(
        load(&tx, &ratchet_key)?,
        private,
        public,
        remote,
        own_id,
        input.peer_device,
        input.peer.into(),
    )?;
    let plaintext = Zeroizing::new(
        session
            .decrypt_payload(input.wire, input.ciphertext, input.nonce)
            .map_err(crate::xmpp_omemo::reject_input)?,
    );
    let state = Zeroizing::new(serde_json::to_string(&session.export()?)?);
    ensure!(
        tx.execute(
            "UPDATE state SET value=?1 WHERE key=?2",
            params![state.as_str(), ratchet_key]
        )? == 1,
        "Session disappeared"
    );
    tx.execute(
        "INSERT INTO seen(sender,id) VALUES(?1,?2)",
        params![input.peer, input.stanza_id],
    )?;
    tx.execute(
        "UPDATE state SET value='true' WHERE key=?1",
        [format!("{prefix}confirmed")],
    )?;
    persist(&tx, &plaintext)?;
    tx.commit()?;
    Ok(plaintext.to_vec())
}

#[derive(Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct RecipientKey {
    pub device: u32,
    pub fingerprint: [u8; 32],
    pub wire: Vec<u8>,
    pub prekey: bool,
}
#[derive(Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct ReservedEnvelope {
    pub id: String,
    pub peer: String,
    pub device: u32,
    pub sender_device: u32,
    pub sender_fingerprint: [u8; 32],
    pub fingerprint: [u8; 32],
    pub wire: Vec<u8>,
    pub ciphertext: Vec<u8>,
    pub nonce: Vec<u8>,
    #[serde(default)]
    pub prekey: bool,
    #[serde(default)]
    pub recipients: Vec<RecipientKey>,
}
pub struct Outbound<'a> {
    pub account: &'a str,
    pub peer: &'a str,
    pub peer_device: u32,
    pub pinned_fingerprint: [u8; 32],
    pub stanza_id: &'a str,
    pub plaintext: &'a [u8],
}

/// Initialize native send reservations alongside the existing legacy state.
/// The compatibility backend ignores this additional table.
pub fn initialize_outbox(conn: &mut Connection, account: &str) -> Result<()> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    identity(&tx, account)?;
    tx.execute_batch("CREATE TABLE IF NOT EXISTS native_omemo_outbox(id TEXT PRIMARY KEY, envelope TEXT NOT NULL);")?;
    tx.commit()?;
    Ok(())
}

/// Reserve every device ratchet and one shared payload atomically.
pub fn reserve_send_many(
    conn: &mut Connection,
    input: Outbound<'_>,
    peers: &[(u32, [u8; 32])],
) -> Result<ReservedEnvelope> {
    ensure!(
        !peers.is_empty() && peers.len() <= 256,
        "Invalid recipient count"
    );
    ensure!(
        peers[0] == (input.peer_device, input.pinned_fingerprint),
        "Primary recipient mismatch"
    );
    let mut ids = std::collections::HashSet::new();
    for (device, _) in peers {
        ensure!(
            (1..=i32::MAX as u32).contains(device) && ids.insert(*device),
            "Invalid or duplicate recipient"
        );
    }
    ensure!(
        !input.stanza_id.is_empty() && input.stanza_id.len() <= 512,
        "Invalid stanza ID"
    );
    ensure!(
        (1..=i32::MAX as u32).contains(&input.peer_device),
        "Invalid peer device"
    );
    ensure!(
        !input.peer.contains('/') && input.peer.len() <= 1023 && input.peer.contains('@'),
        "Invalid peer JID"
    );
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let (private, public, own_id) = identity(&tx, input.account)?;
    let exists: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM native_omemo_outbox WHERE id=?1)",
        [input.stanza_id],
        |r| r.get(0),
    )?;
    ensure!(
        !exists,
        "Stanza already reserved; retry the stored envelope"
    );
    let pending: i64 =
        tx.query_row("SELECT count(*) FROM native_omemo_outbox", [], |r| r.get(0))?;
    ensure!(
        pending < 128,
        "Outbox is full; confirmed receipts are required before pruning"
    );
    let (material, ciphertext, nonce) = LegacySession::prepare_payload(input.plaintext)?;
    let mut recipients = Vec::new();
    for &(peer_device, pin) in peers {
        let prefix = format!(
            "omemo//eu.siacs.conversations.axolotl/{}/{}/",
            input.peer, peer_device
        );
        let remote = session_identity(&tx, &prefix, public, pin)?;
        let ratchet_key = format!("{prefix}double_ratchet");
        let mut session = LegacySession::import(
            load(&tx, &ratchet_key)?,
            private,
            public,
            remote,
            own_id,
            peer_device,
            input.peer.into(),
        )?;
        let mut wire = session.wrap_payload_material(&material)?;
        let initiation = load(&tx, &format!("{prefix}initiation"))?;
        let confirmed = load(&tx, &format!("{prefix}confirmed"))?;
        let confirmed = confirmed
            .as_bool()
            .context("Invalid session confirmation")?;
        ensure!(
            matches!(initiation.as_str(), Some("ACTIVE" | "PASSIVE")),
            "Invalid session initiation"
        );
        let prekey = initiation == "ACTIVE" && !confirmed;
        if prekey {
            use crate::xmpp_omemo::omemo::wire::{PreKeySignalMessage, SignalMessage};
            let exchange = PreKeySignalMessage {
                registration_id: own_id,
                pre_key_id: Some(u32::try_from(
                    load(&tx, &format!("{prefix}key_exchange/pre_key_id"))?
                        .as_u64()
                        .context("Missing prekey ID")?,
                )?),
                signed_pre_key_id: u32::try_from(
                    load(&tx, &format!("{prefix}key_exchange/signed_pre_key_id"))?
                        .as_u64()
                        .context("Missing signed prekey ID")?,
                )?,
                base_key: decode(&load(&tx, &format!("{prefix}key_exchange/ephemeral_key"))?)?
                    .to_vec(),
                identity_key: public.to_vec(),
                message: SignalMessage::deserialize(&wire).context("Invalid generated wire")?,
                raw_message_bytes: wire.clone(),
            };
            wire = exchange.serialize_with_inner_bytes(&wire);
        }
        let state = Zeroizing::new(serde_json::to_string(&session.export()?)?);
        ensure!(
            tx.execute(
                "UPDATE state SET value=?1 WHERE key=?2",
                params![state.as_str(), ratchet_key]
            )? == 1,
            "Session disappeared"
        );
        recipients.push(RecipientKey {
            device: peer_device,
            fingerprint: remote,
            wire,
            prekey,
        });
    }
    let first = &recipients[0];
    let envelope = ReservedEnvelope {
        id: input.stanza_id.into(),
        peer: input.peer.into(),
        device: input.peer_device,
        sender_device: own_id,
        sender_fingerprint: public,
        fingerprint: first.fingerprint,
        wire: first.wire.clone(),
        ciphertext,
        nonce,
        prekey: first.prekey,
        recipients,
    };
    tx.execute(
        "INSERT INTO native_omemo_outbox(id,envelope) VALUES(?1,?2)",
        params![input.stanza_id, serde_json::to_string(&envelope)?],
    )?;
    tx.commit()?;
    Ok(envelope)
}

/// Retry only if the complete current recipient policy still matches the reservation.
#[cfg(test)]
pub fn retry_send_many(
    conn: &mut Connection,
    account: &str,
    id: &str,
    peer: &str,
    pins: &[(u32, [u8; 32])],
) -> Result<ReservedEnvelope> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Deferred)?;
    let (_, public, device) = identity(&tx, account)?;
    let raw: String = tx.query_row(
        "SELECT envelope FROM native_omemo_outbox WHERE id=?1",
        [id],
        |r| r.get(0),
    )?;
    let envelope: ReservedEnvelope = serde_json::from_str(&raw)?;

    ensure!(
        envelope.id == id
            && envelope.peer == peer
            && envelope.sender_device == device
            && envelope.sender_fingerprint == public,
        "Outbox binding mismatch"
    );
    let expected: Vec<_> = if envelope.recipients.is_empty() {
        vec![(envelope.device, envelope.fingerprint)]
    } else {
        envelope
            .recipients
            .iter()
            .map(|r| (r.device, r.fingerprint))
            .collect()
    };
    ensure!(
        pins == expected.as_slice() || (expected.len() == 1 && pins == [(0, expected[0].1)]),
        "Outbox recipient policy changed"
    );
    crate::xmpp_omemo::stanza::serialize(&envelope)?;
    tx.commit()?;
    Ok(envelope)
}

/// Retire only the exact reservation after the transport completed its socket write.
/// This is not a delivery/read receipt. Failed or cancelled writes keep their reservation.
pub fn complete_socket_write(
    conn: &mut Connection,
    account: &str,
    envelope: &ReservedEnvelope,
) -> Result<()> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let (_, public, device) = identity(&tx, account)?;
    ensure!(
        envelope.sender_device == device && envelope.sender_fingerprint == public,
        "Written envelope identity mismatch"
    );
    let raw: String = tx.query_row(
        "SELECT envelope FROM native_omemo_outbox WHERE id=?1",
        [&envelope.id],
        |r| r.get(0),
    )?;
    ensure!(
        serde_json::from_str::<Value>(&raw)? == serde_json::to_value(envelope)?,
        "Written envelope reservation mismatch"
    );
    ensure!(
        tx.execute(
            "DELETE FROM native_omemo_outbox WHERE id=?1 AND envelope=?2",
            params![envelope.id, raw]
        )? == 1,
        "Written reservation disappeared"
    );
    tx.commit()?;
    Ok(())
}
