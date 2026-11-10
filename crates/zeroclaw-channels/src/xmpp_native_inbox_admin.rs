//! Supported offline operator recovery. Never registered as an agent tool.
use super::vault::VaultInboxCipher;
use crate::xmpp_omemo::{
    inbox::{self, InboxCipher, Phase},
    private_store::PrivateStore,
};
use anyhow::{Context, Result, ensure};
use rusqlite::OptionalExtension;
use serde_json::{Value, json};
use std::{os::unix::fs::MetadataExt, path::Path, sync::Arc};
pub fn run(args: &[String]) -> Result<Value> {
    ensure!(
        matches!(
            args.first().map(String::as_str),
            Some(
                "inspect"
                    | "inspect-release"
                    | "release-returned"
                    | "inspect-session"
                    | "reset-session"
            )
        ),
        "Unknown offline ingress operation"
    );
    let operation = &args[0];
    ensure!(
        args.len()
            == if matches!(operation.as_str(), "release-returned" | "reset-session") {
                9
            } else {
                7
            },
        "Invalid offline ingress arguments"
    );
    let root = Path::new(&args[1]);
    let state = Path::new(&args[2]);
    let alias = &args[3];
    let account = &args[4];
    let peer = &args[5];
    let id = &args[6];
    ensure!(
        !alias.is_empty()
            && alias.len() <= 64
            && alias
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'),
        "Invalid channel alias"
    );
    ensure!(
        root.is_absolute() && root.canonicalize()? == root,
        "Canonical absolute vault root required"
    );
    let uid = std::fs::metadata("/proc/self")?.uid();
    let meta = std::fs::symlink_metadata(root)?;
    ensure!(
        meta.is_dir() && meta.uid() == uid && meta.mode() & 0o777 == 0o700,
        "Vault root must be private and owned by current user"
    );
    let meta = std::fs::symlink_metadata(root.join(".secret_key"))?;
    ensure!(
        meta.is_file()
            && !meta.file_type().is_symlink()
            && meta.nlink() == 1
            && meta.uid() == uid
            && meta.mode() & 0o777 == 0o600,
        "Existing private vault key required"
    );
    let cipher: Arc<dyn InboxCipher> = Arc::new(VaultInboxCipher {
        store: zeroclaw_config::secrets::SecretStore::new(root, true),
        alias: alias.clone(),
    });
    cipher.check_existing()?;
    let mut owner = PrivateStore::open_existing(state)?;
    let db = owner.connection();
    // Inspection must not silently initialize an empty or legacy-only inbox.
    let tables: i64=db.query_row("SELECT count(*) FROM sqlite_master WHERE type='table' AND name IN ('native_omemo_inbox','native_omemo_ingress_releases')",[],|r|r.get(0))?;
    ensure!(tables == 2, "Existing native ingress tables required");
    if matches!(operation.as_str(), "inspect-session" | "reset-session") {
        let device: u32 = id.parse()?;
        let review = if operation == "reset-session" {
            Some((args[7].as_str(), args[8].as_str()))
        } else {
            None
        };
        return recover_session(
            db,
            account,
            peer,
            device,
            review,
            &format!("uid:{uid}"),
            &cipher,
        );
    }
    if operation == "inspect-release" {
        let receipt = inbox::inspect_release(db, &cipher, account, peer, id)?;
        return Ok(json!({"operation":operation,"release":receipt}));
    }
    if operation == "inspect" {
        let record = inbox::inspect(db, &cipher, account, peer, id)?;
        let phase = match record.phase {
            Phase::Pending => "pending",
            Phase::Admitted => "admitted",
            Phase::Returned => "returned",
        };
        return Ok(
            json!({"operation":operation,"phase":phase,"device":record.device,"fingerprint":hex::encode(record.pin),"runtime":record.runtime,"control":record.control,"review_token":hex::encode(record.review_token)}),
        );
    }
    let token: [u8; 32] = hex::decode(&args[7])?.try_into().map_err(|_| {
        ::zeroclaw_log::record!(
            ERROR,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail),
            "Invalid review token length"
        );
        anyhow::Error::msg("Invalid review token length")
    })?;
    let operator = format!("uid:{uid}");
    inbox::release_reviewed_returned(
        db,
        &cipher,
        account,
        peer,
        id,
        inbox::ReleaseReview {
            expected_token: token,
            operator: &operator,
            evidence: &args[8],
        },
    )?;
    Ok(json!({"operation":operation,"released":true,"replayed":false}))
}

// Offline recovery deliberately requires an operator review instead of accepting
// a replacement ratchet from an unauthenticated network packet.
struct SessionReview {
    prefix: String,
    fingerprint: [u8; 32],
    own: [u8; 32],
    own_device: u32,
    token: [u8; 32],
    rows: u64,
    unreturned: u64,
    outbox: u64,
}
fn inspect_session(
    tx: &rusqlite::Transaction<'_>,
    account: &str,
    peer: &str,
    device: u32,
) -> Result<SessionReview> {
    use crate::xmpp_omemo::store;
    use base64::{
        Engine,
        engine::general_purpose::{URL_SAFE, URL_SAFE_NO_PAD},
    };
    use sha2::{Digest, Sha256};
    use zeroize::Zeroizing;
    for jid in [account, peer] {
        let canonical: crate::xmpp_omemo::jid::BareJid = jid.try_into()?;
        ensure!(
            canonical.as_str() == jid,
            "Canonical account and peer required"
        );
    }
    ensure!(
        (1..=i32::MAX as u32).contains(&device),
        "Invalid peer device"
    );
    let (_, own, own_device) = store::identity(tx, account)?;
    let prefix = format!("omemo//eu.siacs.conversations.axolotl/{peer}/{device}/");
    let associated = store::load(tx, &format!("{prefix}associated_data"))?;
    let encoded = associated.as_str().context("Missing session identities")?;
    let bytes = URL_SAFE
        .decode(encoded)
        .or_else(|_| URL_SAFE_NO_PAD.decode(encoded))?;
    ensure!(
        bytes.len() == 66 && bytes[0] == 5 && bytes[33] == 5,
        "Invalid session identities"
    );
    let role = store::load(tx, &format!("{prefix}initiation"))?;
    let pin: [u8; 32] = match role.as_str() {
        Some("ACTIVE") => bytes[34..66].try_into()?,
        Some("PASSIVE") => bytes[1..33].try_into()?,
        _ => anyhow::bail!("Invalid session role"),
    };
    store::session_identity(tx, &prefix, own, pin)?;
    store::load(tx, &format!("{prefix}double_ratchet"))?;
    let unreturned: u64 = tx.query_row(
        "SELECT count(*) FROM native_omemo_inbox WHERE peer=?1 AND phase!='returned'",
        [peer],
        |r| r.get(0),
    )?;
    let outbox: u64 = tx.query_row("SELECT count(*) FROM native_omemo_outbox", [], |r| r.get(0))?;
    let mut digest = Sha256::new();
    digest.update(b"zeroclaw-offline-session-reset-v2");
    // The reviewed operation also changes this peer's shared device index.
    // Bind it to the token so a later inspection cannot overwrite unseen changes.
    let index: Option<String> = tx
        .query_row(
            "SELECT value FROM state WHERE key=?1",
            [format!(
                "omemo//eu.siacs.conversations.axolotl/{peer}/device_ids"
            )],
            |row| row.get(0),
        )
        .optional()?;
    digest.update([u8::from(index.is_some())]);
    if let Some(index) = index {
        digest.update((index.len() as u64).to_be_bytes());
        digest.update(index.as_bytes());
    }
    for value in [
        account.as_bytes(),
        peer.as_bytes(),
        &device.to_be_bytes(),
        &own,
        &pin,
    ] {
        digest.update((value.len() as u64).to_be_bytes());
        digest.update(value);
    }
    let mut statement =
        tx.prepare("SELECT key,value FROM state WHERE substr(key,1,length(?1))=?1 ORDER BY key")?;
    let mut cursor = statement.query([&prefix])?;
    let mut rows = 0;
    while let Some(row) = cursor.next()? {
        let key: String = row.get(0)?;
        let value = Zeroizing::new(row.get::<_, String>(1)?);
        for value in [key.as_bytes(), value.as_bytes()] {
            digest.update((value.len() as u64).to_be_bytes());
            digest.update(value);
        }
        rows += 1;
    }
    digest.update(own_device.to_be_bytes());
    digest.update(unreturned.to_be_bytes());
    digest.update(outbox.to_be_bytes());
    Ok(SessionReview {
        prefix,
        fingerprint: pin,
        own,
        own_device,
        token: digest.finalize().into(),
        rows,
        unreturned,
        outbox,
    })
}
fn recover_session(
    db: &mut rusqlite::Connection,
    account: &str,
    peer: &str,
    device: u32,
    review: Option<(&str, &str)>,
    operator: &str,
    cipher: &Arc<dyn InboxCipher>,
) -> Result<Value> {
    let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let snapshot = inspect_session(&tx, account, peer, device)?;
    if let Some((expected, evidence)) = review {
        ensure!(
            !evidence.trim().is_empty()
                && evidence.len() <= 2048
                && !evidence.chars().any(char::is_control),
            "Bounded operator evidence required"
        );
        let token = hex::decode(expected)?;
        ensure!(
            token.as_slice() == snapshot.token,
            "Session changed since inspection"
        );
        ensure!(
            snapshot.unreturned == 0 && snapshot.outbox == 0,
            "Settle pending or uncertain ingress and all outbox reservations before resetting a session"
        );
        let deleted = tx.execute(
            "DELETE FROM state WHERE substr(key,1,length(?1))=?1",
            [&snapshot.prefix],
        )?;
        ensure!(
            deleted as u64 == snapshot.rows,
            "Session changed during recovery"
        );
        // Session rows and the canonical recipient index must change atomically.
        // Otherwise another device's reply tries to load this deleted session.
        let index_key = format!("omemo//eu.siacs.conversations.axolotl/{peer}/device_ids");
        let index: Option<String> = tx
            .query_row(
                "SELECT value FROM state WHERE key=?1",
                [&index_key],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(index) = index {
            let mut ids: Vec<u32> = serde_json::from_str(&index)?;
            ensure!(
                ids.len() <= 256 && ids.iter().all(|id| (1..=i32::MAX as u32).contains(id)),
                "Invalid peer device index"
            );
            ids.retain(|id| *id != device);
            tx.execute(
                "UPDATE state SET value=?1 WHERE key=?2",
                rusqlite::params![serde_json::to_string(&ids)?, index_key],
            )?;
        }
        let receipt = json!({"account":account,"peer":peer,"device":device,"fingerprint":hex::encode(snapshot.fingerprint),"own":hex::encode(snapshot.own),"own_device":snapshot.own_device,"review_token":expected,"operator":operator,"evidence":evidence});
        let receipt = zeroize::Zeroizing::new(receipt.to_string());
        let sealed = cipher.seal(&receipt)?;
        tx.execute(
            "INSERT INTO state(key,value) VALUES(?1,?2)",
            rusqlite::params![
                format!("native_omemo_session_reset/{expected}"),
                serde_json::to_string(&sealed)?
            ],
        )?;
        tx.commit()?;
        return Ok(
            json!({"operation":"reset-session","reset":true,"identity_unchanged":true,"receipts_preserved":true,"requires_pinned_peer_handshake":true}),
        );
    }
    tx.commit()?;
    Ok(
        json!({"operation":"inspect-session","device":device,"fingerprint":hex::encode(snapshot.fingerprint),"review_token":hex::encode(snapshot.token),"session_rows":snapshot.rows,"unreturned_peer_records":snapshot.unreturned,"pending_outbox":snapshot.outbox}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xmpp_omemo::channel_state::ChannelState;
    use std::os::unix::fs::PermissionsExt;
    #[test]
    fn reviewed_session_reset_preserves_identity_replay_and_uncertain_work() -> Result<()> {
        use crate::xmpp_omemo::{
            omemo::wire::{PreKeySignalMessage, SignalMessage},
            store,
        };
        use base64::{
            Engine,
            engine::general_purpose::{STANDARD, URL_SAFE},
        };
        let root = tempfile::tempdir()?;
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700))?;
        let vault = zeroclaw_config::secrets::SecretStore::new(root.path(), true);
        vault.encrypt("synthetic fixture")?;
        let cipher: Arc<dyn InboxCipher> = Arc::new(VaultInboxCipher {
            store: vault,
            alias: "private".into(),
        });
        let state = root.path().join("state");
        let mut channel =
            ChannelState::open_or_initialize(&state, "bob@example.test", cipher.clone(), 64000)?;
        let own_bundle = channel.public_bundle()?;
        drop(channel);
        let mut owner = PrivateStore::open_existing(&state)?;
        let db = owner.connection();
        let tx = db.transaction()?;
        let (_, own, device) = store::identity(&tx, "bob@example.test")?;
        let peer_ed = curve25519_dalek::constants::ED25519_BASEPOINT_POINT
            * curve25519_dalek::scalar::Scalar::from(7u64);
        let peer_pin = peer_ed.to_montgomery().to_bytes();
        let prefix = "omemo//eu.siacs.conversations.axolotl/alice@example.test/101/";
        let associated = [&[5u8][..], &peer_pin, &[5u8][..], &own].concat();
        for (key, value) in [
            ("associated_data", json!(URL_SAFE.encode(associated))),
            ("initiation", json!("PASSIVE")),
            (
                "key_exchange/identity_key",
                json!(URL_SAFE.encode(peer_ed.compress().to_bytes())),
            ),
            (
                "key_exchange/ephemeral_key",
                json!(URL_SAFE.encode([3; 32])),
            ),
            ("key_exchange/signed_pre_key_id", json!(1)),
            ("key_exchange/pre_key_id", json!(1)),
            ("double_ratchet", json!({"synthetic_stale_session":true})),
        ] {
            tx.execute(
                "INSERT INTO state(key,value) VALUES(?1,?2)",
                rusqlite::params![format!("{prefix}{key}"), value.to_string()],
            )?;
        }
        for id in ["old-returned", "uncertain"] {
            inbox::stage(
                &tx,
                &cipher,
                "bob@example.test",
                "alice@example.test",
                id,
                101,
                peer_pin,
                b"synthetic",
            )?;
            tx.execute(
                "INSERT INTO seen(sender,id) VALUES('alice@example.test',?1)",
                [id],
            )?;
        }
        tx.execute(
            "INSERT INTO state(key,value) VALUES(?1,'{}')",
            ["omemo//eu.siacs.conversations.axolotl/alice@example.test/1010/double_ratchet"],
        )?;
        let index_key = "omemo//eu.siacs.conversations.axolotl/alice@example.test/device_ids";
        tx.execute(
            "INSERT INTO state(key,value) VALUES(?1,'[101,1010]')",
            [index_key],
        )?;
        // A second device has a usable established ratchet; exercise the actual
        // outbound path after resetting device101 rather than only inspecting JSON.
        let latin = |byte: u8| std::iter::repeat_n(char::from(byte), 32).collect::<String>();
        let ratchet = json!({"version":"1.0.0","diffie_hellman_ratchet":{
            "version":"1.0.0","own_ratchet_priv":latin(3),"other_ratchet_pub":latin(4),
            "root_chain":{"version":"1.0.0","length":2,"key":latin(5)},
            "symmetric_key_ratchet":{"version":"1.0.0","previous_sending_chain_length":0,
                "sending_chain":{"version":"1.0.0","length":0,"key":latin(6)},
                "receiving_chain":{"version":"1.0.0","length":1,"key":latin(7)}}},
            "skipped_message_keys":[]});
        for (key, value) in [
            (
                "associated_data",
                store::load(&tx, &format!("{prefix}associated_data"))?,
            ),
            ("initiation", json!("PASSIVE")),
            (
                "key_exchange/identity_key",
                json!(URL_SAFE.encode(peer_ed.compress().to_bytes())),
            ),
            ("confirmed", json!(true)),
            ("double_ratchet", ratchet),
        ] {
            tx.execute(
                "INSERT OR REPLACE INTO state(key,value) VALUES(?1,?2)",
                rusqlite::params![
                    format!("omemo//eu.siacs.conversations.axolotl/alice@example.test/1010/{key}"),
                    value.to_string()
                ],
            )?;
        }
        tx.commit()?;
        for id in ["old-returned", "uncertain"] {
            assert!(inbox::admit(db, "alice@example.test", id)?);
        }
        inbox::control_returned(
            db,
            &cipher,
            "bob@example.test",
            "alice@example.test",
            "old-returned",
            "unowned_route",
        )?;
        let own_before: String = db.query_row(
            "SELECT value FROM state WHERE key='public/identity'",
            [],
            |r| r.get(0),
        )?;
        let mut args = vec![
            "inspect-session".into(),
            root.path().display().to_string(),
            state.display().to_string(),
            "private".into(),
            "bob@example.test".into(),
            "alice@example.test".into(),
            "101".into(),
        ];
        assert!(
            run(&args).is_err(),
            "live owner must exclude offline recovery"
        );
        drop(owner);
        let inspect = run(&args)?;
        let mut wrong_account = args.clone();
        wrong_account[4] = "other@example.test".into();
        assert!(
            run(&wrong_account).is_err(),
            "review must bind the owning account"
        );
        assert_eq!(inspect["unreturned_peer_records"], 1);
        let reset_args = |view: &Value| {
            let mut reset = args.clone();
            reset[0] = "reset-session".into();
            reset.push(view["review_token"].as_str().unwrap().into());
            reset.push("peer reset confirmed by operator".into());
            reset
        };
        assert!(run(&reset_args(&inspect)).is_err());
        let mut owner = PrivateStore::open_existing(&state)?;
        let db = owner.connection();
        assert_eq!(
            inbox::inspect(
                db,
                &cipher,
                "bob@example.test",
                "alice@example.test",
                "uncertain"
            )?
            .phase,
            Phase::Admitted
        );
        inbox::control_returned(
            db,
            &cipher,
            "bob@example.test",
            "alice@example.test",
            "uncertain",
            "unowned_route",
        )?;
        db.execute(
            "INSERT INTO native_omemo_outbox(id,envelope) VALUES('uncertain-send','{}')",
            [],
        )?;
        drop(owner);
        let inspect = run(&args)?;
        assert_eq!(inspect["pending_outbox"], 1);
        assert!(
            run(&reset_args(&inspect)).is_err(),
            "uncertain send reservation stays protected"
        );
        let mut owner = PrivateStore::open_existing(&state)?;
        let db = owner.connection();
        assert_eq!(
            db.query_row(
                "SELECT envelope FROM native_omemo_outbox WHERE id='uncertain-send'",
                [],
                |r| r.get::<_, String>(0)
            )?,
            "{}"
        );
        db.execute(
            "DELETE FROM native_omemo_outbox WHERE id='uncertain-send'",
            [],
        )?; // fixture reconciles its own synthetic send
        drop(owner);
        let inspect = run(&args)?;
        let mut wrong = reset_args(&inspect);
        wrong[7] = "00".repeat(32);
        assert!(run(&wrong).is_err());
        let mut owner = PrivateStore::open_existing(&state)?;
        owner.connection().execute(
            "UPDATE state SET value='{}' WHERE key=?1",
            [format!("{prefix}double_ratchet")],
        )?;
        drop(owner);
        assert!(
            run(&reset_args(&inspect)).is_err(),
            "stale review must not reset changed session state"
        );
        let inspect = run(&args)?;
        let mut owner = PrivateStore::open_existing(&state)?;
        owner.connection().execute(
            "UPDATE state SET value='[1010,101]' WHERE key=?1",
            [index_key],
        )?;
        drop(owner);
        assert!(
            run(&reset_args(&inspect)).is_err(),
            "index changes invalidate the reviewed reset"
        );
        let inspect = run(&args)?;
        assert_eq!(run(&reset_args(&inspect))?["reset"], true);
        let mut owner = PrivateStore::open_existing(&state)?;
        let db = owner.connection();
        assert_eq!(store::load(&db.transaction()?, index_key)?, json!([1010]));
        assert_eq!(
            db.query_row(
                "SELECT value FROM state WHERE key='public/identity'",
                [],
                |r| r.get::<_, String>(0)
            )?,
            own_before
        );
        assert_eq!(
            db.query_row("SELECT count(*) FROM seen", [], |r| r.get::<_, u64>(0))?,
            2
        );
        assert_eq!(
            db.query_row("SELECT count(*) FROM native_omemo_inbox", [], |r| r
                .get::<_, u64>(0))?,
            2
        );
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM state WHERE substr(key,1,length(?1))=?1",
                [prefix],
                |r| r.get::<_, u64>(0)
            )?,
            0
        );
        assert_eq!(
            inbox::inspect(
                db,
                &cipher,
                "bob@example.test",
                "alice@example.test",
                "old-returned"
            )?
            .phase,
            Phase::Returned
        );
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM state WHERE key=?1",
                ["omemo//eu.siacs.conversations.axolotl/alice@example.test/1010/double_ratchet"],
                |r| r.get::<_, u64>(0)
            )?,
            1,
            "adjacent device must survive"
        );
        let sealed: String = db.query_row(
            "SELECT value FROM state WHERE key=?1",
            [format!(
                "native_omemo_session_reset/{}",
                inspect["review_token"].as_str().unwrap()
            )],
            |r| r.get(0),
        )?;
        let sealed: String = serde_json::from_str(&sealed)?;
        let receipt: Value = serde_json::from_str(&cipher.open(&sealed)?)?;
        assert_eq!(receipt["account"], "bob@example.test");
        assert_eq!(receipt["peer"], "alice@example.test");
        assert_eq!(receipt["fingerprint"], hex::encode(peer_pin));
        assert_eq!(receipt["own"], hex::encode(own));
        assert_eq!(receipt["own_device"], device);
        drop(owner);
        let mut channel = ChannelState::open_existing(&state, "bob@example.test", cipher, 64000)?;
        assert_eq!(channel.public_bundle()?.device, own_bundle.device);
        let policy = vec![format!("alice@example.test {}", hex::encode(peer_pin))];
        channel.reserve_existing(
            "alice@example.test",
            "reply-after-reset",
            b"still reachable",
            &policy,
        )?;
        let message = SignalMessage {
            ratchet_key: vec![4; 32],
            counter: 0,
            previous_counter: 0,
            ciphertext: vec![0; 16],
            mac: vec![],
        };
        let inner = message.serialize(&[9; 32]);
        let exchange = PreKeySignalMessage {
            registration_id: 101,
            pre_key_id: Some(1),
            signed_pre_key_id: 1,
            base_key: vec![8; 32],
            identity_key: peer_pin.to_vec(),
            message,
            raw_message_bytes: inner.clone(),
        };
        let xml = format!(
            "<encrypted xmlns='eu.siacs.conversations.axolotl'><header sid='101'><key rid='{device}' prekey='true'>{}</key><iv>{}</iv></header><payload>{}</payload></encrypted>",
            STANDARD.encode(exchange.serialize_with_inner_bytes(&inner)),
            STANDARD.encode([0; 12]),
            STANDARD.encode([0; 16])
        );
        assert_eq!(
            channel.discovery_request("alice@example.test", &xml, &policy)?,
            Some((101, peer_pin)),
            "new pinned handshake is possible after reviewed reset"
        );
        assert!(
            channel
                .discovery_request("alice@example.test", &xml, &[])
                .is_err(),
            "reset grants no identity trust"
        );
        assert!(
            channel
                .receive_first(
                    "alice@example.test",
                    "forged",
                    &xml,
                    &own_bundle.xml,
                    &policy
                )
                .is_err(),
            "normal bundle authentication still applies"
        );
        assert!(
            channel
                .receive_existing("alice@example.test", "old-returned", "invalid XML", &policy)?
                .is_none(),
            "old replay marker remains authoritative"
        );
        args[4] = "other@example.test".into();
        assert!(run(&args).is_err());
        Ok(())
    }

    #[test]
    fn native_inbox_operator_release_restores_capacity_without_replay() -> Result<()> {
        let root = tempfile::tempdir()?;
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700))?;
        let vault = zeroclaw_config::secrets::SecretStore::new(root.path(), true);
        vault.encrypt("synthetic fixture bootstrap")?;
        let cipher: Arc<dyn InboxCipher> = Arc::new(VaultInboxCipher {
            store: vault,
            alias: "private".into(),
        });
        let state = root.path().join("state");
        drop(ChannelState::open_or_initialize(
            &state,
            "bob@example.test",
            cipher.clone(),
            64000,
        )?);
        let mut owner = PrivateStore::open_existing(&state)?;
        let db = owner.connection();
        for index in 0..1024 {
            let id = format!("fixture-{index}");
            let tx = db.transaction()?;
            inbox::stage(
                &tx,
                &cipher,
                "bob@example.test",
                "alice@example.test",
                &id,
                101,
                [7; 32],
                b"synthetic",
            )?;
            tx.execute(
                "INSERT INTO seen(sender,id) VALUES(?1,?2)",
                rusqlite::params!["alice@example.test", &id],
            )?;
            tx.commit()?;
            ensure!(
                inbox::admit(db, "alice@example.test", &id)?,
                "Fixture admission failed"
            );
            inbox::control_returned(
                db,
                &cipher,
                "bob@example.test",
                "alice@example.test",
                &id,
                "unowned_route",
            )?;
        }
        let tx = db.transaction()?;
        ensure!(
            inbox::stage(
                &tx,
                &cipher,
                "bob@example.test",
                "alice@example.test",
                "next",
                101,
                [7; 32],
                b"next"
            )
            .is_err(),
            "Cap ignored"
        );
        tx.rollback()?;
        let args = vec![
            "inspect".into(),
            root.path().display().to_string(),
            state.display().to_string(),
            "private".into(),
            "bob@example.test".into(),
            "alice@example.test".into(),
            "fixture-0".into(),
        ];
        ensure!(run(&args).is_err(), "Operator crossed live exclusive owner");
        drop(owner);
        let inspected = run(&args)?;
        ensure!(
            inspected["phase"] == "returned" && inspected.get("content").is_none(),
            "Unsafe review output"
        );
        let token = inspected["review_token"]
            .as_str()
            .context("Review token missing")?;
        let mut release = args.clone();
        release[0] = "release-returned".into();
        release.push(token.into());
        release.push("synthetic-review-receipt".into());
        ensure!(
            run(&release)?["released"] == true,
            "Production release failed"
        );
        ensure!(run(&release).is_err(), "Repeat release recreated state");
        let mut owner = PrivateStore::open_existing(&state)?;
        let db = owner.connection();
        let tx = db.transaction()?;
        ensure!(
            inbox::stage(
                &tx,
                &cipher,
                "bob@example.test",
                "alice@example.test",
                "fixture-0",
                101,
                [7; 32],
                b"replay"
            )
            .is_err(),
            "Released original replayed"
        );
        inbox::stage(
            &tx,
            &cipher,
            "bob@example.test",
            "alice@example.test",
            "next",
            101,
            [7; 32],
            b"next",
        )?;
        tx.commit()?;
        let count: i64 =
            db.query_row("SELECT count(*) FROM native_omemo_inbox", [], |r| r.get(0))?;
        ensure!(count == 1024, "New ingress did not regain one slot");
        Ok(())
    }
}
