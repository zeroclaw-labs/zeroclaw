//! Synthetic receive-transaction and vault-failure tests, never a network writer.
use super::*;
use crate::xmpp_omemo::inbox::InboxCipher;
use crate::xmpp_omemo::{inbox, prekeys, store};
use serde_json::{Value, json};
fn snapshot(db: &Connection) -> anyhow::Result<Vec<(String, String)>> {
    Ok(db
        .prepare("SELECT key,value FROM state ORDER BY key")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<std::result::Result<_, _>>()?)
}
pub(super) fn check(f: &Value, fresh: bool, root: &std::path::Path) -> anyhow::Result<()> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    // A first install may initialize only an absent directory; existing or
    // damaged state must fail closed and retain its original identity.
    let new_dir = root.join("first-native-identity");
    let new_cipher: Arc<dyn InboxCipher> = Arc::new(VaultInboxCipher {
        store: zeroclaw_config::secrets::SecretStore::new(root, true),
        alias: "private".into(),
    });
    let mut first = crate::xmpp_omemo::channel_state::ChannelState::open_or_initialize(
        &new_dir,
        "new-bob@example.test",
        new_cipher.clone(),
        64000,
    )?;
    let first_bundle = first.public_bundle()?;
    ensure!(
        first_bundle.prekey_count == 100,
        "New native identity did not provision prekeys"
    );
    ensure!(
        crate::xmpp_omemo::channel_state::ChannelState::open_or_initialize(
            &new_dir,
            "new-bob@example.test",
            new_cipher.clone(),
            64000
        )
        .is_err(),
        "Concurrent opener replaced new identity"
    );
    drop(first);
    ensure!(
        crate::xmpp_omemo::channel_state::ChannelState::open_or_initialize(
            &new_dir,
            "wrong@example.test",
            new_cipher.clone(),
            64000
        )
        .is_err(),
        "Wrong account replaced identity"
    );
    let mut reopened = crate::xmpp_omemo::channel_state::ChannelState::open_or_initialize(
        &new_dir,
        "new-bob@example.test",
        new_cipher.clone(),
        64000,
    )?;
    ensure!(
        reopened.public_bundle()?.xml == first_bundle.xml,
        "Reopen regenerated identity, signed prekey or one-time keys"
    );
    drop(reopened);
    let partial = root.join("partial-native-identity");
    std::fs::create_dir(&partial)?;
    std::fs::set_permissions(&partial, std::fs::Permissions::from_mode(0o700))?;
    ensure!(
        crate::xmpp_omemo::channel_state::ChannelState::open_or_initialize(
            &partial,
            "new-bob@example.test",
            new_cipher,
            64000
        )
        .is_err()
            && !partial.join("omemo.sqlite3").exists(),
        "Partial state was silently initialized"
    );
    let state_dir = root.join("inbox-fault-state");
    std::fs::create_dir(&state_dir)?;
    std::fs::set_permissions(&state_dir, std::fs::Permissions::from_mode(0o700))?;
    let database = state_dir.join("omemo.sqlite3");
    drop(
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&database)?,
    );
    let mut db = Connection::open(&database)?;
    db.execute_batch("CREATE TABLE state(key TEXT PRIMARY KEY,value TEXT NOT NULL); CREATE TABLE seen(sender TEXT,id TEXT,PRIMARY KEY(sender,id));")?;
    for (key, value) in f[if fresh {
        "fresh_storage"
    } else {
        "bob_storage"
    }]
    .as_object()
    .context("Missing synthetic state")?
    {
        db.execute(
            "INSERT INTO state VALUES(?1,?2)",
            params![format!("omemo/{key}"), value.to_string()],
        )?;
    }
    let own: Vec<u8> = serde_json::from_value(f["bob_identity_curve"].clone())?;
    db.execute(
        "INSERT INTO state VALUES('account',?1)",
        [json!("bob@example.test").to_string()],
    )?;
    db.execute(
        "INSERT INTO state VALUES('public/identity',?1)",
        [
            json!({"jid":"bob@example.test","device_id":202,"fingerprint":hex::encode(&own)})
                .to_string(),
        ],
    )?;
    inbox::initialize(&mut db)?;
    let cipher: Arc<dyn InboxCipher> = Arc::new(VaultInboxCipher {
        store: zeroclaw_config::secrets::SecretStore::new(root, true),
        alias: "private".into(),
    });
    let wire: Vec<u8> =
        serde_json::from_value(f[if fresh { "fresh_wire" } else { "next_wire" }].clone())?;
    let payload: Vec<u8> =
        serde_json::from_value(f[if fresh { "first_payload" } else { "payload" }].clone())?;
    let nonce: Vec<u8> = serde_json::from_value(f[if fresh { "first_iv" } else { "iv" }].clone())?;
    let pin: [u8; 32] = serde_json::from_value(f["alice_identity_curve"].clone())?;
    let receive = |db: &mut Connection| -> anyhow::Result<Vec<u8>> {
        let request = store::Inbound {
            account: "bob@example.test",
            peer: "alice@example.test",
            peer_device: 101,
            pinned_fingerprint: pin,
            stanza_id: "fault-incoming",
            wire: &wire,
            ciphertext: &payload,
            nonce: &nonce,
        };
        let persist = |tx: &rusqlite::Transaction<'_>, content: &[u8]| {
            inbox::stage(
                tx,
                &cipher,
                "bob@example.test",
                "alice@example.test",
                "fault-incoming",
                101,
                pin,
                content,
            )
        };
        if fresh {
            prekeys::receive_fresh_with(
                db,
                request,
                serde_json::from_value(f["alice_identity_ed"].clone())?,
                persist,
            )
        } else {
            store::receive_with(db, request, persist)
        }
    };
    let untouched = snapshot(&db)?;
    drop(db);
    let mut limited = crate::xmpp_omemo::channel_state::ChannelState::open_existing(
        &state_dir,
        "bob@example.test",
        cipher.clone(),
        1,
    )?;
    let rejected = if fresh {
        limited.receive_first(
            "alice@example.test",
            "oversize-input",
            f["first_xml"].as_str().context("Missing XML")?,
            f["alice_bundle_xml"].as_str().context("Missing bundle")?,
            &[format!("alice@example.test {}", hex::encode(pin))],
        )
    } else {
        limited.receive_existing(
            "alice@example.test",
            "oversize-input",
            f["next_xml"].as_str().context("Missing XML")?,
            &[format!("alice@example.test {}", hex::encode(pin))],
        )
    };
    ensure!(rejected.is_err(), "Oversized input reached durable inbox");
    drop(limited);
    let mut db = Connection::open(&database)?;
    ensure!(
        snapshot(&db)? == untouched,
        "Oversized input advanced ratchet or prekeys"
    );
    let before = snapshot(&db)?;
    let key_path = root.join(".secret_key");
    let saved = root.join(".secret_key.synthetic-test-held");
    let original = std::fs::read(&key_path)?;
    std::fs::rename(&key_path, &saved)?;
    let missing = receive(&mut db);
    let recreated = key_path.exists();
    std::fs::rename(&saved, &key_path)?;
    ensure!(
        !recreated && missing.is_err(),
        "Missing vault was silently recreated or accepted"
    );
    ensure!(
        std::fs::read(&key_path)? == original && snapshot(&db)? == before,
        "Missing vault changed identity or ratchet"
    );
    db.execute_batch("CREATE TEMP TRIGGER fail_inbox BEFORE INSERT ON native_omemo_inbox BEGIN SELECT RAISE(ABORT,'synthetic inbox persistence fault'); END;")?;
    let error = receive(&mut db)
        .err()
        .context("Inbox persistence failure ignored")?;
    ensure!(
        error
            .to_string()
            .contains("synthetic inbox persistence fault"),
        "Fault probe failed before inbox write"
    );
    ensure!(
        snapshot(&db)? == before,
        "Inbox failure advanced ratchet or consumed prekey"
    );
    let counts: (i64, i64) = db.query_row(
        "SELECT (SELECT count(*) FROM seen),(SELECT count(*) FROM native_omemo_inbox)",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    ensure!(
        counts == (0, 0),
        "Failed ingress left a replay marker or journal row"
    );
    db.execute_batch("DROP TRIGGER fail_inbox;")?;
    receive(&mut db)?;
    let pending = inbox::pending(&mut db, &cipher, "bob@example.test")?;
    ensure!(pending.len() == 1, "Pending encrypted input missing");
    db.execute(
        "UPDATE native_omemo_inbox SET peer='mallory@example.test'",
        [],
    )?;
    ensure!(
        inbox::pending(&mut db, &cipher, "bob@example.test").is_err(),
        "Swapped inbox binding accepted"
    );
    db.execute(
        "UPDATE native_omemo_inbox SET peer='alice@example.test'",
        [],
    )?;
    let wrong: Arc<dyn InboxCipher> = Arc::new(VaultInboxCipher {
        store: zeroclaw_config::secrets::SecretStore::new(root, true),
        alias: "another-channel".into(),
    });
    ensure!(
        inbox::pending(&mut db, &wrong, "bob@example.test").is_err(),
        "Different channel alias decrypted ingress"
    );
    ensure!(
        inbox::bind_runtime(
            &mut db,
            &cipher,
            "bob@example.test",
            "alice@example.test",
            "fault-incoming",
            inbox::RuntimeLink {
                agent: "fixture-owner".into(),
                history_key: "synthetic-history".into()
            }
        )
        .is_err(),
        "Unadmitted ingress was linked to execution"
    );
    drop(db);
    let child = std::process::Command::new(std::env::current_exe()?)
        .arg("native_omemo_inbox_process_claim_probe")
        .arg("--ignored")
        .arg("--nocapture")
        .env("ZEROCLAW_NATIVE_OMEMO_INBOX_CHILD_DIR", &state_dir)
        .env("ZEROCLAW_NATIVE_OMEMO_INBOX_CHILD_VAULT", root)
        .output()?;
    ensure!(
        child.status.code() == Some(73),
        "Claiming child did not stop at the planned crash boundary"
    );
    let mut owner = crate::xmpp_omemo::private_store::PrivateStore::open_existing(&state_dir)?;
    let db = owner.connection();
    ensure!(
        !inbox::admit(db, "alice@example.test", "fault-incoming")?,
        "Admitted outcome-unknown input replayed after process death"
    );
    ensure!(
        inbox::pending(db, &cipher, "bob@example.test")?.is_empty(),
        "Admitted input was automatically requeued after process death"
    );
    let record = inbox::inspect(
        db,
        &cipher,
        "bob@example.test",
        "alice@example.test",
        "fault-incoming",
    )?;
    ensure!(
        record.phase == inbox::Phase::Admitted
            && record
                .runtime
                .as_ref()
                .is_some_and(|link| link.history_key == "synthetic-history"),
        "Authenticated inspection lost unknown outcome or runtime reference"
    );
    ensure!(
        inbox::inspect(
            db,
            &wrong,
            "bob@example.test",
            "alice@example.test",
            "fault-incoming"
        )
        .is_err(),
        "Inspection accepted another channel vault alias"
    );
    let sealed: String = db.query_row("SELECT sealed FROM native_omemo_inbox", [], |r| r.get(0))?;
    let body: Value = serde_json::from_str(&cipher.open(&sealed)?)?;
    ensure!(
        body["runtime"]["history_key"] == "synthetic-history"
            && body["runtime"]["agent"] == "fixture-owner",
        "Canonical runtime reference lost across process exit"
    );
    let link = inbox::RuntimeLink {
        agent: "fixture-owner".into(),
        history_key: "synthetic-history".into(),
    };
    inbox::bind_runtime(
        db,
        &cipher,
        "bob@example.test",
        "alice@example.test",
        "fault-incoming",
        link.clone(),
    )?;
    ensure!(
        inbox::bind_runtime(
            db,
            &cipher,
            "bob@example.test",
            "alice@example.test",
            "fault-incoming",
            inbox::RuntimeLink {
                history_key: "other-history".into(),
                ..link.clone()
            }
        )
        .is_err(),
        "Runtime reference silently changed"
    );
    ensure!(
        inbox::bind_runtime(
            db,
            &wrong,
            "bob@example.test",
            "alice@example.test",
            "fault-incoming",
            link
        )
        .is_err(),
        "Runtime binding accepted another channel vault alias"
    );
    let after: String = db.query_row("SELECT sealed FROM native_omemo_inbox", [], |r| r.get(0))?;
    ensure!(
        sealed == after,
        "Idempotence or rejected reference changed ciphertext"
    );
    let refused = inbox::release_reviewed_returned(
        db,
        &cipher,
        "bob@example.test",
        "alice@example.test",
        "fault-incoming",
        inbox::ReleaseReview {
            expected_token: record.review_token,
            operator: "synthetic-operator",
            evidence: "synthetic-review",
        },
    );
    ensure!(refused.is_err(), "Unknown admitted outcome was released");
    inbox::returned(db, "alice@example.test", "fault-incoming")?;
    ensure!(
        !inbox::admit(db, "alice@example.test", "fault-incoming")?,
        "Returned input replayed"
    );
    let current = inbox::inspect(
        db,
        &cipher,
        "bob@example.test",
        "alice@example.test",
        "fault-incoming",
    )?;
    let release = |db: &mut Connection, token| {
        inbox::release_reviewed_returned(
            db,
            &cipher,
            "bob@example.test",
            "alice@example.test",
            "fault-incoming",
            inbox::ReleaseReview {
                expected_token: token,
                operator: "synthetic-operator",
                evidence: "synthetic-review",
            },
        )
    };
    ensure!(
        release(db, record.review_token).is_err(),
        "Stale review crossed a phase change"
    );
    let before_state = snapshot(db)?;
    for (name, sql) in [
        (
            "audit",
            "CREATE TEMP TRIGGER fail_release BEFORE INSERT ON native_omemo_ingress_releases BEGIN SELECT RAISE(ABORT,'synthetic release audit fault'); END;",
        ),
        (
            "delete",
            "CREATE TEMP TRIGGER fail_release BEFORE DELETE ON native_omemo_inbox BEGIN SELECT RAISE(ABORT,'synthetic release delete fault'); END;",
        ),
    ] {
        db.execute_batch(sql)?;
        let error = release(db, current.review_token)
            .err()
            .context("Injected release failure ignored")?;
        ensure!(
            error
                .to_string()
                .contains(&format!("synthetic release {name} fault")),
            "Release failed before planned boundary"
        );
        let counts: (i64,i64) = db.query_row("SELECT (SELECT count(*) FROM native_omemo_inbox),(SELECT count(*) FROM native_omemo_ingress_releases)",[],|r|Ok((r.get(0)?,r.get(1)?)))?;
        ensure!(
            counts == (1, 0),
            "Failed release split audit and pending state"
        );
        db.execute_batch("DROP TRIGGER fail_release")?;
    }
    db.execute(
        "DELETE FROM seen WHERE sender='alice@example.test' AND id='fault-incoming'",
        [],
    )?;
    ensure!(
        release(db, current.review_token).is_err(),
        "Released without canonical replay marker"
    );
    db.execute(
        "INSERT INTO seen VALUES('alice@example.test','fault-incoming')",
        [],
    )?;
    ensure!(
        inbox::release_reviewed_returned(
            db,
            &wrong,
            "bob@example.test",
            "alice@example.test",
            "fault-incoming",
            inbox::ReleaseReview {
                expected_token: current.review_token,
                operator: "synthetic-operator",
                evidence: "synthetic-review"
            }
        )
        .is_err(),
        "Wrong vault alias released input"
    );
    release(db, current.review_token)?;
    ensure!(
        release(db, current.review_token).is_err(),
        "Released input was recreated on repeat"
    );
    ensure!(
        snapshot(db)? == before_state,
        "Review changed identity, ratchet or prekeys"
    );
    let receipt = inbox::inspect_release(
        db,
        &cipher,
        "bob@example.test",
        "alice@example.test",
        "fault-incoming",
    )?;
    ensure!(
        receipt
            .runtime
            .as_ref()
            .context("Missing runtime reference")?
            .history_key
            == "synthetic-history"
            && receipt.operator == "synthetic-operator"
            && receipt.source_token == current.review_token,
        "Release lost its review provenance"
    );
    ensure!(
        inbox::inspect_release(
            db,
            &wrong,
            "bob@example.test",
            "alice@example.test",
            "fault-incoming"
        )
        .is_err(),
        "Wrong alias inspected release"
    );
    let audit: String = db.query_row(
        "SELECT sealed FROM native_omemo_ingress_releases",
        [],
        |r| r.get(0),
    )?;
    let audit_body: Value = serde_json::from_str(&cipher.open(&audit)?)?;
    ensure!(
        audit.starts_with("enc2:") && audit_body.get("content").is_none(),
        "Release retained message content"
    );
    db.execute(
        "UPDATE native_omemo_ingress_releases SET peer='mallory@example.test'",
        [],
    )?;
    ensure!(
        inbox::inspect_release(
            db,
            &cipher,
            "bob@example.test",
            "mallory@example.test",
            "fault-incoming"
        )
        .is_err(),
        "Swapped release row accepted"
    );
    db.execute(
        "UPDATE native_omemo_ingress_releases SET peer='alice@example.test'",
        [],
    )?;
    let tx = db.transaction()?;
    ensure!(
        inbox::stage(
            &tx,
            &cipher,
            "bob@example.test",
            "alice@example.test",
            "fault-incoming",
            101,
            pin,
            b"synthetic retry"
        )
        .is_err(),
        "Released input was staged again"
    );
    tx.rollback()?;
    ensure!(
        inbox::pending(db, &cipher, "bob@example.test")?.is_empty(),
        "Released input requeued"
    );
    let counts: (i64,i64,i64) = db.query_row("SELECT (SELECT count(*) FROM native_omemo_inbox),(SELECT count(*) FROM native_omemo_ingress_releases),(SELECT count(*) FROM seen)",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
    ensure!(
        counts == (0, 1, 1),
        "Release did not retain audit and canonical replay marker"
    );
    Ok(())
}

#[test]
#[ignore = "child-process crash probe; invoked only by the synthetic inbox test"]
fn native_omemo_inbox_process_claim_probe() -> anyhow::Result<()> {
    let directory = PathBuf::from(std::env::var("ZEROCLAW_NATIVE_OMEMO_INBOX_CHILD_DIR")?);
    let vault = PathBuf::from(std::env::var("ZEROCLAW_NATIVE_OMEMO_INBOX_CHILD_VAULT")?);
    let cipher: Arc<dyn InboxCipher> = Arc::new(VaultInboxCipher {
        store: zeroclaw_config::secrets::SecretStore::new(&vault, true),
        alias: "private".into(),
    });
    let mut owner = crate::xmpp_omemo::private_store::PrivateStore::open_existing(&directory)?;
    ensure!(
        inbox::pending(owner.connection(), &cipher, "bob@example.test")?.len() == 1,
        "Child lost pending ingress"
    );
    ensure!(
        inbox::admit(owner.connection(), "alice@example.test", "fault-incoming")?,
        "Child did not durably admit ingress"
    );
    let db = owner.connection();
    let link = inbox::RuntimeLink {
        agent: "fixture-owner".into(),
        history_key: "synthetic-history".into(),
    };
    db.execute_batch("CREATE TEMP TRIGGER fail_binding BEFORE UPDATE OF sealed ON native_omemo_inbox BEGIN SELECT RAISE(ABORT,'synthetic runtime binding fault'); END;")?;
    let error = inbox::bind_runtime(
        db,
        &cipher,
        "bob@example.test",
        "alice@example.test",
        "fault-incoming",
        link.clone(),
    )
    .err()
    .context("Runtime binding persistence failure ignored")?;
    ensure!(
        error
            .to_string()
            .contains("synthetic runtime binding fault"),
        "Failure happened before runtime binding write"
    );
    let sealed: String = db.query_row("SELECT sealed FROM native_omemo_inbox", [], |r| r.get(0))?;
    let body: Value = serde_json::from_str(&cipher.open(&sealed)?)?;
    ensure!(
        body["runtime"].is_null(),
        "Failed binding was partially committed"
    );
    db.execute_batch("DROP TRIGGER fail_binding")?;
    inbox::bind_runtime(
        db,
        &cipher,
        "bob@example.test",
        "alice@example.test",
        "fault-incoming",
        link,
    )?;
    let record = inbox::inspect(
        db,
        &cipher,
        "bob@example.test",
        "alice@example.test",
        "fault-incoming",
    )?;
    let policy = vec![format!("alice@example.test {}", hex::encode(record.pin))];
    drop(owner);
    let mut state = crate::xmpp_omemo::channel_state::ChannelState::open_existing(
        &directory,
        "bob@example.test",
        cipher.clone(),
        64000,
    )?;
    ensure!(
        state
            .bind_runtime(
                "alice@example.test",
                "fault-incoming",
                "fixture-owner",
                "synthetic-history",
                &[]
            )
            .is_err(),
        "Runtime binding accepted a revoked live fingerprint"
    );
    state.bind_runtime(
        "alice@example.test",
        "fault-incoming",
        "fixture-owner",
        "synthetic-history",
        &policy,
    )?;
    // Deliberately skip all Rust/SQLite drop paths after a committed admission and link.
    std::process::exit(73);
}
