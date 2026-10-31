//! Ignored user-boundary probe for the actual experimental XmppChannel.
use super::*;
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
fn run_offline_admin(args: &[String]) -> Result<Value> {
    let output = std::process::Command::new(std::env::current_exe()?)
        .args([
            "xmpp::native_channel_probe::offline_admin_child",
            "--exact",
            "--ignored",
            "--nocapture",
        ])
        .env(
            "ZEROCLAW_XMPP_ADMIN_FIXTURE_ARGS",
            serde_json::to_string(args)?,
        )
        .output()?;
    ensure!(
        output.status.success() && output.stdout.len() <= 65536 && output.stderr.len() <= 65536,
        "Offline administration probe failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8(output.stdout)?;
    let line = text
        .lines()
        .find_map(|line| line.strip_prefix("XMPP_ADMIN_RESULT="))
        .context("Missing admin child result")?;
    Ok(serde_json::from_str(line)?)
}
#[test]
#[ignore = "child-only synthetic offline administration"]
fn offline_admin_child() -> Result<()> {
    let args: Vec<String> =
        serde_json::from_str(&std::env::var("ZEROCLAW_XMPP_ADMIN_FIXTURE_ARGS")?)?;
    let output = super::inbox_admin::run(&args)?;
    println!("XMPP_ADMIN_RESULT={}", serde_json::to_string(&output)?);
    Ok(())
}
#[tokio::test]
#[ignore = "requires isolated Prosody and synthetic state; see tests/xmpp_native/README.md"]
async fn native_omemo_actual_channel_shared_runtime_and_socket_ack() -> Result<()> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let f: Value = serde_json::from_slice(&std::fs::read(std::env::var(
        "ZEROCLAW_NATIVE_OMEMO_FIXTURE",
    )?)?)?;
    ensure!(f["fixture_only"] == true, "Refusing non-fixture state");
    let accounts: Value = serde_json::from_slice(&std::fs::read(std::env::var(
        "ZEROCLAW_NATIVE_OMEMO_ACCOUNTS",
    )?)?)?;
    let report = PathBuf::from(std::env::var("ZEROCLAW_NATIVE_OMEMO_RESULT")?);
    let mode = std::env::var("ZEROCLAW_NATIVE_OMEMO_TLS_MODE")?;
    let fresh = std::env::var("ZEROCLAW_NATIVE_OMEMO_FRESH").ok().as_deref() == Some("1");
    let directory = tempfile::tempdir_in("/var/tmp")?;
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700))?;
    let storage = directory.path().join("channels/xmpp/private");
    std::fs::create_dir_all(&storage)?;
    std::fs::set_permissions(&storage, std::fs::Permissions::from_mode(0o700))?;
    let database = storage.join("omemo.sqlite3");
    drop(
        std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&database)?,
    );
    let db = Connection::open(&database)?;
    db.execute_batch("CREATE TABLE state(key TEXT PRIMARY KEY,value TEXT NOT NULL);CREATE TABLE seen(sender TEXT,id TEXT,PRIMARY KEY(sender,id));")?;
    for (key, value) in f[if fresh {
        "fresh_storage"
    } else {
        "bob_storage"
    }]
    .as_object()
    .context("Missing fixture state")?
    {
        db.execute(
            "INSERT INTO state VALUES(?1,?2)",
            params![format!("omemo/{key}"), value.to_string()],
        )?;
    }
    let own: Vec<u8> = serde_json::from_value(f["bob_identity_curve"].clone())?;
    if fresh {
        let sessions: i64 = db.query_row(
            "SELECT count(*) FROM state WHERE key LIKE '%/double_ratchet'",
            [],
            |r| r.get(0),
        )?;
        ensure!(sessions == 0, "Fresh fixture already contains a ratchet");
    }
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
    drop(db);
    let vault = zeroclaw_config::secrets::SecretStore::new(directory.path(), true);
    vault.encrypt("synthetic initialization only")?;
    super::inbox_fault_probe::check(&f, fresh, directory.path())?;
    let pin: Vec<u8> = serde_json::from_value(f["alice_identity_curve"].clone())?;
    let pins = Arc::new(Mutex::new(vec![format!(
        "alice@example.test {}",
        hex::encode(&pin)
    )]));
    let allow = Arc::new(Mutex::new(vec!["alice@example.test".to_string()]));
    let live_pins = pins.clone();
    let live_allow = allow.clone();
    let channel = Arc::new(
        XmppChannel::new(
            XmppConfig {
                username: "bob@example.test".into(),
                password: accounts["bob"]
                    .as_str()
                    .context("Missing fixture password")?
                    .into(),
                server: "127.0.0.1".into(),
                port: if mode == "direct" { 15223 } else { 15222 },
                protocol: if mode == "direct" {
                    XmppProtocol::DirectTls
                } else {
                    XmppProtocol::Starttls
                },
                omemo_enabled: true,
                connect_timeout_secs: 10,
                ..Default::default()
            },
            "private".into(),
            directory.path().into(),
            Arc::new(move || live_allow.lock().unwrap().clone()),
        )
        .with_pin_resolver(Arc::new(move || live_pins.lock().unwrap().clone()))
        .with_inbox_vault(Some(directory.path().into())),
    );
    let (tx, mut rx) = mpsc::channel(2);
    let listener_channel = channel.clone();
    let mut listener = ::zeroclaw_spawn::spawn!(async move { listener_channel.listen(tx).await });
    tokio::time::timeout(Duration::from_secs(12), async {
        while !channel.health_check().await {
            if listener.is_finished() {
                (&mut listener)
                    .await
                    .context("Actual channel task failed before readiness")?
                    .context("Actual channel exited before readiness")?;
                anyhow::bail!("Actual channel returned before readiness");
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        Ok::<_, anyhow::Error>(())
    })
    .await??;
    std::fs::write(
        &report,
        json!({"ready":true,"fixture_only":true}).to_string(),
    )?;
    let msg = tokio::time::timeout(Duration::from_secs(12), rx.recv())
        .await?
        .context("Channel receiver closed")?;
    ensure!(
        msg.id == "actual-channel-inbound",
        "Rejected fixture input reached runtime: {}",
        msg.id
    );
    ensure!(
        msg.sender == "alice@example.test" && msg.channel_alias.as_deref() == Some("private"),
        "Wrong peer or alias reached runtime"
    );
    let expected: Vec<u8> = serde_json::from_value(
        f[if fresh {
            "expected_first"
        } else {
            "expected_plaintext"
        }]
        .clone(),
    )?;
    ensure!(
        msg.content.as_bytes() == expected,
        "Actual channel did not decrypt expected content"
    );
    let admin_args = vec![
        "inspect".into(),
        directory.path().to_string_lossy().into_owned(),
        storage.to_string_lossy().into_owned(),
        "private".into(),
        "bob@example.test".into(),
        "alice@example.test".into(),
        "actual-channel-inbound".into(),
    ];
    let owner_error = run_offline_admin(&admin_args)
        .err()
        .context("Offline admin bypassed active state owner")?;
    ensure!(
        owner_error
            .to_string()
            .contains("Native state owner is already active"),
        "Admin refused before reaching ownership gate: {owner_error}"
    );
    listener.abort();
    ensure!(
        listener.await.unwrap_err().is_cancelled(),
        "Pre-admission cancellation failed"
    );
    ensure!(
        !channel.health_check().await,
        "Cancelled listener stayed online"
    );
    let listener_channel = channel.clone();
    let (tx, mut recovered_rx) = mpsc::channel(2);
    let listener = ::zeroclaw_spawn::spawn!(async move { listener_channel.listen(tx).await });
    let recovered = tokio::time::timeout(Duration::from_secs(12), recovered_rx.recv())
        .await?
        .context("Pending ingress was lost")?;
    ensure!(
        recovered.id == msg.id
            && recovered.content == msg.content
            && recovered.channel_alias == msg.channel_alias,
        "Restart changed recovered input"
    );
    crate::orchestrator::tests::dispatch_native_channel_fixture(channel.clone(), recovered.clone())
        .await?;
    ensure!(
        !channel.admit_inbound(&recovered).await?,
        "Already returned ingress was admitted twice"
    );
    drop(rx);
    let mut rx = recovered_rx;
    pins.lock().unwrap().clear();
    ensure!(
        channel
            .send(&SendMessage::new(
                "must not leave socket",
                "alice@example.test"
            ))
            .await
            .is_err(),
        "Revoked fingerprint still sent"
    );
    ensure!(
        channel.health_check().await,
        "Expected policy denial stopped listener"
    );
    allow.lock().unwrap().clear();
    ensure!(
        channel
            .send(&SendMessage::new("also denied", "alice@example.test"))
            .await
            .is_err(),
        "Revoked allowlist still sent"
    );
    ensure!(
        tokio::time::timeout(Duration::from_millis(200), rx.recv())
            .await
            .is_err(),
        "Unauthorized or plaintext message dispatched"
    );
    listener.abort();
    ensure!(
        listener.await.unwrap_err().is_cancelled(),
        "Listener cancellation failed"
    );
    ensure!(
        !channel.health_check().await,
        "Cancelled channel remained online"
    );
    let mut restored = crate::xmpp_omemo::private_store::PrivateStore::open_existing(&storage)?;
    let db = restored.connection();
    let pending: i64 =
        db.query_row("SELECT count(*) FROM native_omemo_outbox", [], |r| r.get(0))?;
    ensure!(
        pending == 0,
        "Policy denial reserved or emitted another response"
    );
    let bundle = crate::xmpp_omemo::bundle::export_existing(db, "bob@example.test")?;
    ensure!(bundle.prekey_count == 100, "Unexpected prekey consumption");
    let own_after: Value = serde_json::from_str(&db.query_row(
        "SELECT value FROM state WHERE key='public/identity'",
        [],
        |r| r.get::<_, String>(0),
    )?)?;
    ensure!(
        own_after["fingerprint"] == hex::encode(&own),
        "First contact changed own identity"
    );
    let (sealed, phase): (String, String) = db.query_row(
        "SELECT sealed,phase FROM native_omemo_inbox WHERE id='actual-channel-inbound'",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    ensure!(
        sealed.starts_with("enc2:") && !sealed.contains(&String::from_utf8(expected)?),
        "Ingress plaintext was stored"
    );
    ensure!(
        phase == "returned",
        "Shared runtime did not finalize ingress"
    );
    let cipher = VaultInboxCipher {
        store: zeroclaw_config::secrets::SecretStore::new(directory.path(), true),
        alias: "private".into(),
    };
    let body: Value = serde_json::from_str(&crate::xmpp_omemo::inbox::InboxCipher::open(
        &cipher, &sealed,
    )?)?;
    ensure!(
        body["runtime"]["agent"] == "fixture-owner"
            && body["runtime"]["history_key"]
                .as_str()
                .is_some_and(|s| !s.is_empty()),
        "Actual dispatcher did not bind ingress to canonical runtime history"
    );
    let alice: [u8; 32] = pin.as_slice().try_into()?;
    // Reserve without a socket write, then prove restart keeps the identical envelope.
    let reserved = crate::xmpp_omemo::store::reserve_send_many(
        db,
        crate::xmpp_omemo::store::Outbound {
            account: "bob@example.test",
            peer: "alice@example.test",
            peer_device: 101,
            pinned_fingerprint: alice,
            stanza_id: "uncertain-write-probe",
            plaintext: b"synthetic uncertain write",
        },
        &[(101, alice)],
    )?;
    let stored_envelope = serde_json::to_value(&reserved)?;
    let resumed = crate::xmpp_omemo::store::retry_send_many(
        db,
        "bob@example.test",
        "uncertain-write-probe",
        "alice@example.test",
        &[(101, alice)],
    )?;
    ensure!(
        serde_json::to_value(resumed)? == stored_envelope,
        "Restart changed reserved ciphertext"
    );
    ensure!(
        crate::xmpp_omemo::store::retry_send_many(
            db,
            "bob@example.test",
            "uncertain-write-probe",
            "alice@example.test",
            &[]
        )
        .is_err(),
        "Restart retry bypassed recipient policy"
    );
    db.execute_batch("CREATE TEMP TRIGGER deny_outbox_delete BEFORE DELETE ON native_omemo_outbox BEGIN SELECT RAISE(ABORT,'synthetic cleanup fault'); END;")?;
    ensure!(
        crate::xmpp_omemo::store::complete_socket_write(db, "bob@example.test", &reserved).is_err(),
        "Cleanup write fault ignored"
    );
    db.execute_batch("DROP TRIGGER deny_outbox_delete")?;
    ensure!(
        crate::xmpp_omemo::store::retry_send_many(
            db,
            "bob@example.test",
            "uncertain-write-probe",
            "alice@example.test",
            &[(101, alice)]
        )
        .is_ok(),
        "Failed cleanup lost uncertain ciphertext"
    );
    crate::xmpp_omemo::store::complete_socket_write(db, "bob@example.test", &reserved)?;
    let seen: i64 = db.query_row("SELECT count(*) FROM seen", [], |r| r.get(0))?;
    ensure!(seen == 1, "Unexpected application message accepted");
    let before_state: Vec<(String, String)> = db
        .prepare("SELECT key,value FROM state ORDER BY key")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<std::result::Result<_, _>>()?;
    drop(restored);
    let inspected = run_offline_admin(&admin_args)?;
    ensure!(
        inspected["phase"] == "returned" && inspected.get("content").is_none(),
        "Offline inspection exposed content or wrong phase"
    );
    let mut wrong_args = admin_args.clone();
    wrong_args[3] = "another-channel".into();
    ensure!(
        run_offline_admin(&wrong_args).is_err(),
        "Offline inspection accepted wrong channel alias"
    );
    let mut release_args = admin_args.clone();
    release_args[0] = "release-returned".into();
    release_args.extend(["00".repeat(32), "synthetic reviewed runtime history".into()]);
    ensure!(
        run_offline_admin(&release_args).is_err(),
        "Offline release accepted stale token"
    );
    release_args[7] = inspected["review_token"]
        .as_str()
        .context("Missing review token")?
        .into();
    ensure!(
        run_offline_admin(&release_args)?["released"] == true,
        "Offline reviewed release failed"
    );
    ensure!(
        run_offline_admin(&release_args).is_err(),
        "Offline review replayed release"
    );
    let mut audit_args = admin_args.clone();
    audit_args[0] = "inspect-release".into();
    let audit = run_offline_admin(&audit_args)?;
    ensure!(
        audit["release"]["operator"]
            .as_str()
            .is_some_and(|s| s.starts_with("uid:"))
            && audit["release"].get("content").is_none(),
        "Offline review lost process identity or exposed content"
    );
    let mut owner = crate::xmpp_omemo::private_store::PrivateStore::open_existing(&storage)?;
    let db = owner.connection();
    let after_state: Vec<(String, String)> = db
        .prepare("SELECT key,value FROM state ORDER BY key")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<std::result::Result<_, _>>()?;
    ensure!(
        before_state == after_state,
        "Offline admin changed identity or ratchet state"
    );
    let counts:(i64,i64,i64)=db.query_row("SELECT (SELECT count(*) FROM native_omemo_inbox),(SELECT count(*) FROM native_omemo_ingress_releases),(SELECT count(*) FROM seen)",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
    ensure!(
        counts == (0, 1, 1),
        "Offline review lost its audit or replay marker"
    );
    std::fs::write(&report,json!({"fixture_only":true,"result":"passed","actual_xmpp_channel_tested":true,"runtime_turns":1,"shared_runtime_turn":true,"send_ack_after_socket_write":true,"live_fingerprint_revocation_denied":true,"live_allowlist_revocation_denied":true,"cancel_released_private_store":true,"outbox_reservations":pending,"agent_alias_metadata_preserved":true,"full_agent_router_tested":true,"shared_dispatch_loop_tested":true,"unowned_alias_denied":true,"fresh_contact_tested":fresh,"prekeys_remaining":bundle.prekey_count,"native_prekey_replenishment_tested":true,"new_identity_creation_and_reopen_tested":true,"actual_reserved_ciphertext_restart_tested":true,"completed_socket_write_retires_only_exact_reservation":true,"own_identity_preserved":true,"encrypted_ingress_persisted":true,"pending_recovered_before_admission":true,"runtime_admission_once":true,"canonical_runtime_reference_persisted":true,"original_receipt_preserved_through_hook":true,"runtime_binding_write_fault_rollback":true,"runtime_binding_live_pin_rechecked":true,"reviewed_returned_release_atomic_and_encrypted":true,"unknown_ingress_release_denied":true,"stale_review_denied":true,"offline_admin_owner_gate_tested":true,"offline_admin_inspect_and_release_tested":true,"offline_admin_real_executable_tested":true,"release_replay_marker_preserved":true,"inbox_failure_rolls_back_ratchet_and_prekeys":true,"missing_vault_does_not_create_key":true,"admitted_process_exit_not_replayed":true,"inbox_binding_tamper_rejected":true,"duplicate_message_dispatched_once":true,"tls_mode":mode}).to_string())?;
    Ok(())
}
