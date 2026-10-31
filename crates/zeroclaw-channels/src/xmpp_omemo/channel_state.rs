//! Native channel boundary for explicitly pinned legacy OMEMO sessions.
//! The caller resolves live allowlists and pins for every operation; no policy cache.
use crate::xmpp_omemo::{
    private_store::PrivateStore,
    stanza,
    store::{self, Inbound, Outbound, ReservedEnvelope},
};
use anyhow::{Context, Result, ensure};
use base64::{
    Engine,
    engine::general_purpose::{URL_SAFE, URL_SAFE_NO_PAD},
};
use rusqlite::OptionalExtension;
use serde_json::Value;

fn is_authorized_pin(peer: &str, pin: [u8; 32], policy: &[String]) -> Result<bool> {
    let peer_jid: jid::BareJid = peer.parse()?;
    ensure!(
        peer_jid.node().is_some() && !peer.contains('/'),
        "Invalid peer JID"
    );
    let mut trusted = false;
    for entry in policy {
        let mut parts = entry.split_whitespace();
        let jid: jid::BareJid = parts
            .next()
            .context("Invalid fingerprint policy")?
            .parse()?;
        let fingerprint: String = parts.collect();
        let fingerprint = fingerprint
            .strip_prefix("05")
            .filter(|_| fingerprint.len() == 66)
            .unwrap_or(&fingerprint);
        ensure!(fingerprint.len() == 64, "Invalid fingerprint policy");
        let fingerprint = hex::decode(fingerprint)?;
        if jid == peer_jid && fingerprint == pin {
            trusted = true;
        }
    }
    Ok(trusted)
}
fn authorize_pin(peer: &str, pin: [u8; 32], policy: &[String]) -> Result<()> {
    if !is_authorized_pin(peer, pin, policy)? {
        return Err(crate::xmpp_omemo::RejectedInbound.into());
    }
    Ok(())
}
pub struct ChannelState {
    owner: PrivateStore,
    account: String,
    device: u32,
    max_chars: usize,
    cipher: std::sync::Arc<dyn crate::xmpp_omemo::inbox::InboxCipher>,
}
impl ChannelState {
    /// Initialize only an absent directory or exclusively validated TLS-only
    /// schema, never when an existing encrypted identity is unreadable.
    pub fn open_or_initialize(
        path: &std::path::Path,
        account: &str,
        cipher: std::sync::Arc<dyn crate::xmpp_omemo::inbox::InboxCipher>,
        max_chars: usize,
    ) -> Result<Self> {
        ensure!(
            (1..=64000).contains(&max_chars),
            "Invalid ingress text limit"
        );
        cipher.check_existing()?;
        match std::fs::symlink_metadata(path) {
            Ok(_) => match Self::open_existing(path, account, cipher.clone(), max_chars) {
                Ok(state) => Ok(state),
                Err(original) => {
                    let mut owner = match PrivateStore::open_tls_only(path) {
                        Ok(owner) => owner,
                        Err(_) => return Err(original),
                    };
                    let mut random = [0u8; 4];
                    getrandom::fill(&mut random).map_err(|_| {
                        ::zeroclaw_log::record!(
                            ERROR,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Fail
                            ),
                            "Device identity randomness unavailable"
                        );
                        anyhow::Error::msg("Device identity randomness unavailable")
                    })?;
                    let device = (u32::from_le_bytes(random) & i32::MAX as u32).max(1);
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)?
                        .as_secs();
                    crate::xmpp_omemo::initial_identity::initialize_tls_only(
                        owner.connection(),
                        account,
                        device,
                        now,
                    )?;
                    drop(owner);
                    Self::open_existing(path, account, cipher, max_chars)
                }
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let mut owner = PrivateStore::create_new(path)?;
                let mut random = [0u8; 4];
                getrandom::fill(&mut random).map_err(|_| {
                    ::zeroclaw_log::record!(
                        ERROR,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail),
                        "Device identity randomness unavailable"
                    );
                    anyhow::Error::msg("Device identity randomness unavailable")
                })?;
                let device = (u32::from_le_bytes(random) & i32::MAX as u32).max(1);
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)?
                    .as_secs();
                crate::xmpp_omemo::initial_identity::initialize_empty(
                    owner.connection(),
                    account,
                    device,
                    now,
                )?;
                drop(owner);
                Self::open_existing(path, account, cipher, max_chars)
            }
            Err(error) => Err(error.into()),
        }
    }
    pub fn open_existing(
        path: &std::path::Path,
        account: &str,
        cipher: std::sync::Arc<dyn crate::xmpp_omemo::inbox::InboxCipher>,
        max_chars: usize,
    ) -> Result<Self> {
        ensure!(
            (1..=64000).contains(&max_chars),
            "Invalid ingress text limit"
        );
        cipher.check_existing()?;
        let mut owner = PrivateStore::open_existing(path)?;
        let tx = owner.connection().transaction()?;
        let (_, _, device) = store::identity(&tx, account)?;
        tx.commit()?;
        store::initialize_outbox(owner.connection(), account)?;
        crate::xmpp_omemo::inbox::initialize(owner.connection())?;
        Ok(Self {
            owner,
            account: account.into(),
            device,
            max_chars,
            cipher,
        })
    }
    fn pin(&mut self, peer: &str, device: u32, policy: &[String]) -> Result<[u8; 32]> {
        ensure!((1..=i32::MAX as u32).contains(&device), "Invalid device ID");
        let prefix = format!("omemo//eu.siacs.conversations.axolotl/{peer}/{device}/");
        let tx = self.owner.connection().transaction()?;
        let associated = store::load(&tx, &format!("{prefix}associated_data"))?;
        let encoded = associated
            .as_str()
            .context("Missing associated identities")?;
        let bytes = URL_SAFE
            .decode(encoded)
            .or_else(|_| URL_SAFE_NO_PAD.decode(encoded))?;
        ensure!(
            bytes.len() == 66 && bytes[0] == 5 && bytes[33] == 5,
            "Invalid associated identities"
        );
        let role = store::load(&tx, &format!("{prefix}initiation"))?;
        let remote = match role.as_str() {
            Some("ACTIVE") => &bytes[34..66],
            Some("PASSIVE") => &bytes[1..33],
            _ => anyhow::bail!("Unknown session role"),
        };
        let pin: [u8; 32] = remote.try_into()?;
        authorize_pin(peer, pin, policy)?;
        tx.commit()?;
        Ok(pin)
    }
    pub fn public_bundle(&mut self) -> Result<crate::xmpp_omemo::bundle::PublicBundle> {
        crate::xmpp_omemo::prekey_inventory::replenish(
            self.owner.connection(),
            &self.account,
            100,
        )?;
        let bundle =
            crate::xmpp_omemo::bundle::export_existing(self.owner.connection(), &self.account)?;
        ensure!(
            bundle.prekey_count >= 100,
            "Incomplete native prekey inventory"
        );
        Ok(bundle)
    }
    /// A missing ratchet never permits overwriting partial existing metadata.
    pub fn discovery_request(
        &mut self,
        peer: &str,
        xml: &str,
        policy: &[String],
    ) -> Result<Option<(u32, [u8; 32])>> {
        let selected = stanza::parse(xml, self.device).map_err(crate::xmpp_omemo::reject_input)?;
        if selected.ciphertext.is_none() {
            return Ok(None);
        }
        let prefix = format!(
            "omemo//eu.siacs.conversations.axolotl/{peer}/{}/",
            selected.sender_device
        );
        let exists: bool = self.owner.connection().query_row(
            "SELECT EXISTS(SELECT 1 FROM state WHERE key=?1)",
            [format!("{prefix}double_ratchet")],
            |r| r.get(0),
        )?;
        if exists {
            return Ok(None);
        }
        let keys: Vec<String> = self
            .owner
            .connection()
            .prepare("SELECT key FROM state")?
            .query_map([], |r| r.get(0))?
            .collect::<std::result::Result<_, _>>()?;
        ensure!(
            !keys.iter().any(|key| key.starts_with(&prefix)),
            "Partial session metadata must not be replaced"
        );
        if !selected.prekey {
            return Err(crate::xmpp_omemo::RejectedInbound.into());
        }
        let exchange =
            crate::xmpp_omemo::omemo::wire::PreKeySignalMessage::deserialize(&selected.wire)
                .ok_or(crate::xmpp_omemo::RejectedInbound)?;
        let pin: [u8; 32] = exchange
            .identity_key
            .as_slice()
            .try_into()
            .map_err(crate::xmpp_omemo::reject_input)?;
        authorize_pin(peer, pin, policy)?;
        Ok(Some((selected.sender_device, pin)))
    }
    pub fn receive_first(
        &mut self,
        peer: &str,
        id: &str,
        xml: &str,
        bundle_xml: &str,
        policy: &[String],
    ) -> Result<Option<String>> {
        let (device, pin) = self
            .discovery_request(peer, xml, policy)?
            .ok_or(crate::xmpp_omemo::RejectedInbound)?;
        let bundle = crate::xmpp_omemo::bundle::parse_pinned(bundle_xml, device, pin)
            .map_err(crate::xmpp_omemo::reject_input)?;
        let sign = bundle.signed_pre_key.signature[63] >> 7;
        let ed = curve25519_dalek::montgomery::MontgomeryPoint(pin)
            .to_edwards(sign)
            .ok_or(crate::xmpp_omemo::RejectedInbound)?
            .compress()
            .to_bytes();
        let selected = stanza::parse(xml, self.device).map_err(crate::xmpp_omemo::reject_input)?;
        let payload = selected
            .ciphertext
            .ok_or(crate::xmpp_omemo::RejectedInbound)?;
        let cipher = self.cipher.clone();
        let account = self.account.clone();
        let max_chars = self.max_chars;
        let plaintext = crate::xmpp_omemo::prekeys::receive_fresh_with(
            self.owner.connection(),
            Inbound {
                account: &self.account,
                peer,
                peer_device: device,
                pinned_fingerprint: pin,
                stanza_id: id,
                wire: &selected.wire,
                ciphertext: &payload,
                nonce: &selected.nonce,
            },
            ed,
            |tx, content| {
                let text = std::str::from_utf8(content).map_err(crate::xmpp_omemo::reject_input)?;
                ensure!(
                    !text.trim().is_empty() && text.chars().count() <= max_chars,
                    crate::xmpp_omemo::RejectedInbound
                );
                crate::xmpp_omemo::inbox::stage(
                    tx, &cipher, &account, peer, id, device, pin, content,
                )
            },
        )?;
        Ok(Some(String::from_utf8(plaintext)?))
    }
    pub fn receive_existing(
        &mut self,
        peer: &str,
        id: &str,
        xml: &str,
        policy: &[String],
    ) -> Result<Option<String>> {
        ensure!(!id.is_empty() && id.len() <= 512, "Invalid stanza ID");
        let seen: bool = self.owner.connection().query_row(
            "SELECT EXISTS(SELECT 1 FROM seen WHERE sender=?1 AND id=?2)",
            rusqlite::params![peer, id],
            |r| r.get(0),
        )?;
        if seen {
            return Ok(None);
        }
        let selected = stanza::parse(xml, self.device).map_err(crate::xmpp_omemo::reject_input)?;
        let Some(payload) = selected.ciphertext else {
            return Ok(None);
        };
        let pin = self.pin(peer, selected.sender_device, policy)?;
        let cipher = self.cipher.clone();
        let account = self.account.clone();
        let max_chars = self.max_chars;
        let plaintext = store::receive_with(
            self.owner.connection(),
            Inbound {
                account: &self.account,
                peer,
                peer_device: selected.sender_device,
                pinned_fingerprint: pin,
                stanza_id: id,
                wire: &selected.wire,
                ciphertext: &payload,
                nonce: &selected.nonce,
            },
            |tx, content| {
                let text = std::str::from_utf8(content).map_err(crate::xmpp_omemo::reject_input)?;
                ensure!(
                    !text.trim().is_empty() && text.chars().count() <= max_chars,
                    crate::xmpp_omemo::RejectedInbound
                );
                crate::xmpp_omemo::inbox::stage(
                    tx,
                    &cipher,
                    &account,
                    peer,
                    id,
                    selected.sender_device,
                    pin,
                    content,
                )
            },
        )?;
        Ok(Some(String::from_utf8(plaintext)?))
    }
    pub fn pending(&mut self, policy: &[String]) -> Result<Vec<crate::xmpp_omemo::inbox::Pending>> {
        let messages = crate::xmpp_omemo::inbox::pending(
            self.owner.connection(),
            &self.cipher,
            &self.account,
        )?;
        let mut allowed = Vec::new();
        for msg in messages {
            if msg.content.trim().is_empty() || msg.content.chars().count() > self.max_chars {
                continue;
            }
            if is_authorized_pin(&msg.peer, msg.pin, policy)? {
                ensure!(
                    self.pin(&msg.peer, msg.device, policy)? == msg.pin,
                    "Pending ingress fingerprint changed"
                );
                allowed.push(msg);
            }
        }
        Ok(allowed)
    }
    pub fn admit(&mut self, peer: &str, id: &str, policy: &[String]) -> Result<bool> {
        let phase: String = self.owner.connection().query_row(
            "SELECT phase FROM native_omemo_inbox WHERE peer=?1 AND id=?2",
            rusqlite::params![peer, id],
            |r| r.get(0),
        )?;
        if phase == "pending" {
            let message = crate::xmpp_omemo::inbox::pending_message(
                self.owner.connection(),
                &self.cipher,
                &self.account,
                peer,
                id,
            )?;
            ensure!(
                !message.content.trim().is_empty()
                    && message.content.chars().count() <= self.max_chars
                    && is_authorized_pin(peer, message.pin, policy)?
                    && self.pin(peer, message.device, policy)? == message.pin,
                "Ingress identity is no longer authorized"
            );
        }
        crate::xmpp_omemo::inbox::admit(self.owner.connection(), peer, id)
    }
    pub fn bind_runtime(
        &mut self,
        peer: &str,
        id: &str,
        agent: &str,
        history_key: &str,
        policy: &[String],
    ) -> Result<()> {
        let record = crate::xmpp_omemo::inbox::inspect(
            self.owner.connection(),
            &self.cipher,
            &self.account,
            peer,
            id,
        )?;
        ensure!(
            is_authorized_pin(peer, record.pin, policy)?
                && self.pin(peer, record.device, policy)? == record.pin,
            "Ingress identity is no longer authorized"
        );
        crate::xmpp_omemo::inbox::bind_runtime(
            self.owner.connection(),
            &self.cipher,
            &self.account,
            peer,
            id,
            crate::xmpp_omemo::inbox::RuntimeLink {
                agent: agent.into(),
                history_key: history_key.into(),
            },
        )
    }
    pub fn control_returned(&mut self, peer: &str, id: &str, control: &str) -> Result<()> {
        crate::xmpp_omemo::inbox::control_returned(
            self.owner.connection(),
            &self.cipher,
            &self.account,
            peer,
            id,
            control,
        )
    }
    pub fn returned(&mut self, peer: &str, id: &str) -> Result<()> {
        crate::xmpp_omemo::inbox::returned(self.owner.connection(), peer, id)
    }
    pub fn reserve_existing(
        &mut self,
        peer: &str,
        id: &str,
        plaintext: &[u8],
        policy: &[String],
    ) -> Result<ReservedEnvelope> {
        let key = format!("omemo//eu.siacs.conversations.axolotl/{peer}/device_ids");
        let raw: Option<String> = self
            .owner
            .connection()
            .query_row("SELECT value FROM state WHERE key=?1", [key], |r| r.get(0))
            .optional()?;
        let value: Value = serde_json::from_str(&raw.context("No existing peer device index")?)?;
        let devices = value.as_array().context("Invalid peer device index")?;
        ensure!(
            !devices.is_empty() && devices.len() <= 256,
            "Invalid peer device count"
        );
        let mut peers = Vec::new();
        for device in devices {
            let device = u32::try_from(device.as_u64().context("Invalid device index")?)?;
            let pin = self.pin(peer, device, policy)?;
            peers.push((device, pin));
        }
        store::reserve_send_many(
            self.owner.connection(),
            Outbound {
                account: &self.account,
                peer,
                peer_device: peers[0].0,
                pinned_fingerprint: peers[0].1,
                stanza_id: id,
                plaintext,
            },
            &peers,
        )
    }
    pub fn complete_socket_write(&mut self, envelope: &ReservedEnvelope) -> Result<()> {
        store::complete_socket_write(self.owner.connection(), &self.account, envelope)
    }
}

#[cfg(test)]
mod policy_tests {
    use super::is_authorized_pin;
    #[test]
    fn tls_only_upgrade_preserves_seen_and_never_replaces_identity() -> anyhow::Result<()> {
        use std::{os::unix::fs::PermissionsExt, sync::Arc};
        let root = tempfile::tempdir()?;
        let vault = crate::xmpp::vault::VaultInboxCipher {
            store: zeroclaw_config::secrets::SecretStore::new(root.path(), true),
            alias: "private".into(),
        };
        vault.store.encrypt("synthetic initialization")?;
        let cipher: Arc<dyn crate::xmpp_omemo::inbox::InboxCipher> = Arc::new(vault);
        let path = root.path().join("private");
        std::fs::create_dir(&path)?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))?;
        let db_path = path.join("omemo.sqlite3");
        let db = rusqlite::Connection::open(&db_path)?;
        db.execute_batch("CREATE TABLE seen(sender TEXT,id TEXT,PRIMARY KEY(sender,id));INSERT INTO seen VALUES('alice@example.test','old-tls-message');")?;
        std::fs::set_permissions(&db_path, std::fs::Permissions::from_mode(0o600))?;
        drop(db);
        let mut state = super::ChannelState::open_or_initialize(
            &path,
            "bob@example.test",
            cipher.clone(),
            64000,
        )?;
        let identity = state.public_bundle()?.xml;
        for control in ["stop", "gate", "event"] {
            let db = state.owner.connection();
            let tx = db.transaction()?;
            crate::xmpp_omemo::inbox::stage(
                &tx,
                &cipher,
                "bob@example.test",
                "alice@example.test",
                control,
                101,
                [1; 32],
                b"synthetic control",
            )?;
            tx.execute(
                "INSERT INTO seen VALUES('alice@example.test',?1)",
                [control],
            )?;
            tx.commit()?;
            assert!(crate::xmpp_omemo::inbox::admit(
                db,
                "alice@example.test",
                control
            )?);
            db.execute_batch("CREATE TEMP TRIGGER fail_control BEFORE UPDATE ON native_omemo_inbox BEGIN SELECT RAISE(ABORT,'synthetic control fault'); END;")?;
            assert!(
                crate::xmpp_omemo::inbox::control_returned(
                    db,
                    &cipher,
                    "bob@example.test",
                    "alice@example.test",
                    control,
                    control
                )
                .is_err()
            );
            let unknown = crate::xmpp_omemo::inbox::inspect(
                db,
                &cipher,
                "bob@example.test",
                "alice@example.test",
                control,
            )?;
            assert!(matches!(
                unknown.phase,
                crate::xmpp_omemo::inbox::Phase::Admitted
            ));
            db.execute_batch("DROP TRIGGER fail_control;")?;
            crate::xmpp_omemo::inbox::control_returned(
                db,
                &cipher,
                "bob@example.test",
                "alice@example.test",
                control,
                control,
            )?;
            let record = crate::xmpp_omemo::inbox::inspect(
                db,
                &cipher,
                "bob@example.test",
                "alice@example.test",
                control,
            )?;
            assert!(record.runtime.is_none());
            crate::xmpp_omemo::inbox::release_reviewed_returned(
                db,
                &cipher,
                "bob@example.test",
                "alice@example.test",
                control,
                crate::xmpp_omemo::inbox::ReleaseReview {
                    expected_token: record.review_token,
                    operator: "synthetic-operator",
                    evidence: "synthetic-control-review",
                },
            )?;
            let receipt = crate::xmpp_omemo::inbox::inspect_release(
                db,
                &cipher,
                "bob@example.test",
                "alice@example.test",
                control,
            )?;
            assert_eq!(receipt.control.as_deref(), Some(control));
            assert!(receipt.runtime.is_none());
        }
        let seen: i64 = state.owner.connection().query_row(
            "SELECT count(*) FROM seen WHERE id='old-tls-message'",
            [],
            |r| r.get(0),
        )?;
        assert_eq!(seen, 1);
        drop(state);
        let mut state = super::ChannelState::open_or_initialize(
            &path,
            "bob@example.test",
            cipher.clone(),
            64000,
        )?;
        assert_eq!(state.public_bundle()?.xml, identity);
        drop(state);
        assert!(
            super::ChannelState::open_or_initialize(
                &path,
                "other@example.test",
                cipher.clone(),
                64000
            )
            .is_err()
        );
        let db = rusqlite::Connection::open(&db_path)?;
        db.execute("DELETE FROM state WHERE key='account'", [])?;
        let before: i64 = db.query_row("SELECT count(*) FROM state", [], |r| r.get(0))?;
        drop(db);
        assert!(
            super::ChannelState::open_or_initialize(&path, "bob@example.test", cipher, 64000)
                .is_err()
        );
        let db = rusqlite::Connection::open(&db_path)?;
        assert_eq!(
            db.query_row("SELECT count(*) FROM state", [], |r| r.get::<_, i64>(0))?,
            before
        );
        Ok(())
    }
    #[test]
    fn pending_cap_is_transactional_and_admission_does_not_scan_other_envelopes()
    -> anyhow::Result<()> {
        use base64::Engine;
        use std::sync::Arc;
        let root = tempfile::tempdir()?;
        let vault = crate::xmpp::vault::VaultInboxCipher {
            store: zeroclaw_config::secrets::SecretStore::new(root.path(), true),
            alias: "bounded".into(),
        };
        vault.store.encrypt("synthetic initialization")?;
        let cipher: Arc<dyn crate::xmpp_omemo::inbox::InboxCipher> = Arc::new(vault);
        let mut state = super::ChannelState::open_or_initialize(
            &root.path().join("bounded"),
            "bob@example.test",
            cipher.clone(),
            64000,
        )?;
        let db = state.owner.connection();
        for n in 0..128 {
            let tx = db.transaction()?;
            crate::xmpp_omemo::inbox::stage(
                &tx,
                &cipher,
                "bob@example.test",
                "alice@example.test",
                &n.to_string(),
                101,
                [1; 32],
                b"synthetic",
            )?;
            tx.commit()?;
        }
        {
            let tx = db.transaction()?;
            assert!(
                crate::xmpp_omemo::inbox::stage(
                    &tx,
                    &cipher,
                    "bob@example.test",
                    "alice@example.test",
                    "overflow",
                    101,
                    [1; 32],
                    b"synthetic"
                )
                .is_err()
            );
        }
        assert_eq!(
            db.query_row("SELECT count(*) FROM native_omemo_inbox", [], |r| r
                .get::<_, i64>(0))?,
            128
        );
        // Simulate a valid backlog produced by the old implementation.
        db.execute(
            "UPDATE native_omemo_inbox SET phase='admitted' WHERE id='0'",
            [],
        )?;
        {
            let tx = db.transaction()?;
            crate::xmpp_omemo::inbox::stage(
                &tx,
                &cipher,
                "bob@example.test",
                "alice@example.test",
                "legacy",
                101,
                [1; 32],
                b"synthetic",
            )?;
            tx.commit()?;
        }
        db.execute(
            "UPDATE native_omemo_inbox SET phase='pending' WHERE id='0'",
            [],
        )?;
        assert_eq!(
            crate::xmpp_omemo::inbox::pending(db, &cipher, "bob@example.test")?.len(),
            129,
            "Legacy oversize backlog remains drainable after restart"
        );
        // Corrupt only the unrelated row to exercise exact-envelope admission.
        db.execute("UPDATE native_omemo_inbox SET sealed=(SELECT sealed FROM native_omemo_inbox WHERE id='0') WHERE id='legacy'", [])?;
        assert!(crate::xmpp_omemo::inbox::pending(db, &cipher, "bob@example.test").is_err());
        // The unrelated bad legacy row must not be read to authenticate a specific receipt.
        assert_eq!(
            crate::xmpp_omemo::inbox::pending_message(
                db,
                &cipher,
                "bob@example.test",
                "alice@example.test",
                "127"
            )?
            .id,
            "127"
        );
        assert!(
            state.admit("alice@example.test", "127", &[]).is_err(),
            "Revoked policy still refuses receipt"
        );
        let db = state.owner.connection();
        let mut identities = vec![5u8];
        identities.extend([2u8; 32]);
        identities.push(5);
        identities.extend([1u8; 32]);
        let prefix = "omemo//eu.siacs.conversations.axolotl/alice@example.test/101/";
        db.execute(
            "INSERT INTO state(key,value) VALUES(?1,?2)",
            rusqlite::params![
                format!("{prefix}associated_data"),
                serde_json::to_string(&super::URL_SAFE.encode(identities))?
            ],
        )?;
        db.execute(
            "INSERT INTO state(key,value) VALUES(?1,?2)",
            rusqlite::params![format!("{prefix}initiation"), "\"ACTIVE\""],
        )?;
        let policy = vec![format!("alice@example.test {}", hex::encode([1u8; 32]))];
        assert!(state.admit("alice@example.test", "127", &policy)?);
        assert!(!state.admit("alice@example.test", "127", &policy)?);
        Ok(())
    }
    #[test]
    fn configured_grouping_and_legacy_key_prefix_match_native_policy() -> anyhow::Result<()> {
        let pin = [0xabu8; 32];
        for entry in [
            format!("alice@example.test {}", hex::encode(pin)),
            format!(
                "  alice@example.test  05 {}  ",
                hex::encode_upper(pin)
                    .as_bytes()
                    .chunks(8)
                    .map(|p| std::str::from_utf8(p).unwrap())
                    .collect::<Vec<_>>()
                    .join(" ")
            ),
        ] {
            assert!(is_authorized_pin("alice@example.test", pin, &[entry])?);
        }
        assert!(!is_authorized_pin(
            "alice@example.test",
            pin,
            &[format!("mallory@example.test {}", hex::encode(pin))]
        )?);
        assert!(
            super::authorize_pin("alice@example.test", pin, &[])
                .unwrap_err()
                .is::<crate::xmpp_omemo::RejectedInbound>()
        );
        assert!(
            is_authorized_pin(
                "alice@example.test",
                pin,
                &[format!("alice@example.test/resource {}", hex::encode(pin))]
            )
            .is_err()
        );
        assert!(
            is_authorized_pin(
                "alice@example.test",
                pin,
                &["alice@example.test 05ab".into()]
            )
            .is_err()
        );
        Ok(())
    }
}
