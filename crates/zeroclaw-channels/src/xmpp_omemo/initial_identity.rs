//! Atomic initialization only for a genuinely new legacy database.
use anyhow::{Result, ensure};
use base64::{
    Engine,
    engine::general_purpose::{STANDARD, URL_SAFE},
};
use rusqlite::{Connection, TransactionBehavior, params};
use serde_json::{Value, json};
use zeroize::Zeroizing;
const ROOT: &str = "omemo//eu.siacs.conversations.axolotl";
/// Creates the identity fact, never an inferred or replacement identity.
/// Existing rows of any kind refuse initialization, including partial state.
pub fn initialize_empty(conn: &mut Connection, account: &str, device: u32, now: u64) -> Result<()> {
    initialize(conn, account, device, now, false)
}
/// Called only after the exclusive exact-schema TLS-only ownership gate.
pub fn initialize_tls_only(
    conn: &mut Connection,
    account: &str,
    device: u32,
    now: u64,
) -> Result<()> {
    initialize(conn, account, device, now, true)
}
fn initialize(
    conn: &mut Connection,
    account: &str,
    device: u32,
    now: u64,
    tls_only: bool,
) -> Result<()> {
    let jid: crate::xmpp_omemo::jid::BareJid = account.try_into()?;
    ensure!(
        jid.as_str() == account && (1..=i32::MAX as u32).contains(&device),
        "Canonical account and valid device required"
    );
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if tls_only {
        tx.execute_batch("CREATE TABLE state(key TEXT PRIMARY KEY,value TEXT NOT NULL);")?;
    }
    let foreign: i64 = tx.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type='table' AND name NOT IN ('state','seen')",
        [],
        |r| r.get(0),
    )?;
    ensure!(
        foreign == 0,
        "Refusing identity initialization over existing native records"
    );
    let count: i64 = tx.query_row(
        if tls_only {
            "SELECT count(*) FROM state"
        } else {
            "SELECT (SELECT count(*) FROM state)+(SELECT count(*) FROM seen)"
        },
        [],
        |r| r.get(0),
    )?;
    ensure!(
        count == 0,
        "Refusing to replace existing or partial OMEMO state"
    );
    let mut own = Zeroizing::new([0u8; 32]);
    let mut signed = Zeroizing::new([0u8; 32]);
    getrandom::fill(own.as_mut()).map_err(|_| {
        ::zeroclaw_log::record!(
            ERROR,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail),
            "Identity randomness unavailable"
        );
        anyhow::Error::msg("Identity randomness unavailable")
    })?;
    getrandom::fill(signed.as_mut()).map_err(|_| {
        ::zeroclaw_log::record!(
            ERROR,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail),
            "Signed prekey randomness unavailable"
        );
        anyhow::Error::msg("Signed prekey randomness unavailable")
    })?;
    own[0] &= 248;
    own[31] &= 127;
    own[31] |= 64;
    let public = *x25519_dalek::PublicKey::from(&x25519_dalek::StaticSecret::from(*own)).as_bytes();
    let spk = *x25519_dalek::PublicKey::from(&x25519_dalek::StaticSecret::from(*signed)).as_bytes();
    // Legacy signing authenticates the 0x05-prefixed public key. Store the
    // Edwards signature; the bundle exporter restores the Montgomery sign bit.
    let mut signature =
        crate::xmpp_omemo::omemo::protocol::X3DHProtocol::sign_pre_key(own.as_slice(), &spk)?;
    signature[63] &= 0x7f;
    let text = |raw: &[u8]| -> String { raw.iter().map(|b| char::from(*b)).collect() };
    let x3dh = json!({"version":"1.0.0","identity_key":{"version":"1.0.0","secret":text(own.as_slice()),"secret_type":"PRIV"},
  "signed_pre_key":{"version":"1.0.0","priv":text(signed.as_slice()),"sig":text(&signature),"timestamp":now},"old_signed_pre_key":null,"pre_keys":[]});
    let rows: [(String, Value); 8] = [
        ("account".into(), json!(account)),
        (
            "public/identity".into(),
            json!({"jid":account,"device_id":device,"fingerprint":hex::encode(public)}),
        ),
        (
            "omemo//ikp/key".into(),
            json!(URL_SAFE.encode(own.as_slice())),
        ),
        ("omemo//ikp/is_seed".into(), json!(false)),
        (format!("{ROOT}/x3dh"), x3dh),
        (
            format!("{ROOT}/signed_pre_key_ids"),
            json!({(STANDARD.encode(spk)):1}),
        ),
        (format!("{ROOT}/pre_key_ids"), json!({})),
        (format!("{ROOT}/pre_key_id_counter"), json!(0)),
    ];
    for (key, value) in rows {
        let encoded = Zeroizing::new(serde_json::to_string(&value)?);
        tx.execute(
            "INSERT INTO state(key,value) VALUES(?1,?2)",
            params![key, encoded.as_str()],
        )?;
    }
    crate::xmpp_omemo::store::identity(&tx, account)?;
    tx.commit()?;
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    fn empty() -> Result<Connection> {
        let db = Connection::open_in_memory()?;
        db.execute_batch("CREATE TABLE state(key TEXT PRIMARY KEY,value TEXT NOT NULL);CREATE TABLE seen(sender TEXT,id TEXT,PRIMARY KEY(sender,id));")?;
        Ok(db)
    }
    #[test]
    fn initialization_is_atomic_and_never_replaces_partial_or_existing_state() -> Result<()> {
        let mut db = empty()?;
        db.execute_batch("CREATE TEMP TRIGGER fail_identity BEFORE INSERT ON state WHEN NEW.key='public/identity' BEGIN SELECT RAISE(ABORT,'synthetic identity fault'); END;")?;
        assert!(
            initialize_empty(&mut db, "bob@example.test", 202, 1)
                .unwrap_err()
                .to_string()
                .contains("synthetic identity fault")
        );
        let count: i64 = db.query_row("SELECT count(*) FROM state", [], |r| r.get(0))?;
        assert_eq!(count, 0);
        db.execute_batch("DROP TRIGGER fail_identity")?;
        initialize_empty(&mut db, "bob@example.test", 202, 1)?;
        assert!(initialize_empty(&mut db, "bob@example.test", 202, 1).is_err());
        let mut partial = empty()?;
        partial.execute(
            "INSERT INTO state VALUES('account','\"bob@example.test\"')",
            [],
        )?;
        assert!(initialize_empty(&mut partial, "bob@example.test", 202, 1).is_err());
        Ok(())
    }
}
