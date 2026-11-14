//! Durable native ingress facts, not a replacement for runtime tool/turn history.
use crate::xmpp_omemo::store::identity;
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, Transaction, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use zeroize::{Zeroize, Zeroizing};
pub trait InboxCipher: Send + Sync {
    fn check_existing(&self) -> Result<()>;
    fn seal(&self, plaintext: &str) -> Result<String>;
    fn open(&self, ciphertext: &str) -> Result<String>;
}
/// Reference to the canonical runtime history, never a copy of tool outcomes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeLink {
    pub agent: String,
    pub history_key: String,
}
#[derive(Serialize, Deserialize)]
struct Body {
    account: String,
    peer: String,
    id: String,
    device: u32,
    pin: [u8; 32],
    own: [u8; 32],
    own_device: u32,
    content: String,
    #[serde(default)]
    runtime: Option<RuntimeLink>,
    #[serde(default)]
    control: Option<String>,
}
impl Drop for Body {
    fn drop(&mut self) {
        self.content.zeroize();
    }
}
pub struct Pending {
    pub peer: String,
    pub id: String,
    pub device: u32,
    pub pin: [u8; 32],
    pub content: String,
}
impl Drop for Pending {
    fn drop(&mut self) {
        self.content.zeroize();
    }
}
pub fn initialize(conn: &mut Connection) -> Result<()> {
    conn.execute_batch("CREATE TABLE IF NOT EXISTS native_omemo_inbox(peer TEXT NOT NULL,id TEXT NOT NULL,sealed TEXT NOT NULL,phase TEXT NOT NULL CHECK(phase IN ('pending','admitted','returned')),PRIMARY KEY(peer,id));")?;
    let columns: Vec<(String, String, bool, i64)> = conn
        .prepare("PRAGMA table_info(native_omemo_inbox)")?
        .query_map([], |r| Ok((r.get(1)?, r.get(2)?, r.get(3)?, r.get(5)?)))?
        .collect::<std::result::Result<_, _>>()?;
    ensure!(
        columns
            == vec![
                ("peer".into(), "TEXT".into(), true, 1),
                ("id".into(), "TEXT".into(), true, 2),
                ("sealed".into(), "TEXT".into(), true, 0),
                ("phase".into(), "TEXT".into(), true, 0),
            ],
        "Incompatible encrypted inbox schema"
    );
    let invalid: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM native_omemo_inbox WHERE typeof(peer)!='text' OR typeof(id)!='text' OR typeof(sealed)!='text' OR typeof(phase)!='text' OR phase NOT IN ('pending','admitted','returned') OR substr(sealed,1,5)!='enc2:')",
        [], |r| r.get(0),
    )?;
    ensure!(!invalid, "Invalid encrypted inbox record");
    initialize_releases(conn)?;
    Ok(())
}
fn initialize_releases(conn: &Connection) -> Result<()> {
    conn.execute_batch("CREATE TABLE IF NOT EXISTS native_omemo_ingress_releases(peer TEXT NOT NULL,id TEXT NOT NULL,sealed TEXT NOT NULL,PRIMARY KEY(peer,id));")?;
    let columns: Vec<(String, String, bool, i64)> = conn
        .prepare("PRAGMA table_info(native_omemo_ingress_releases)")?
        .query_map([], |r| Ok((r.get(1)?, r.get(2)?, r.get(3)?, r.get(5)?)))?
        .collect::<std::result::Result<_, _>>()?;
    ensure!(
        columns
            == vec![
                ("peer".into(), "TEXT".into(), true, 1),
                ("id".into(), "TEXT".into(), true, 2),
                ("sealed".into(), "TEXT".into(), true, 0)
            ],
        "Incompatible ingress release schema"
    );
    let invalid: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM native_omemo_ingress_releases WHERE typeof(sealed)!='text' OR substr(sealed,1,5)!='enc2:')",[],|r|r.get(0))?;
    ensure!(!invalid, "Invalid encrypted ingress release");
    Ok(())
}
pub fn stage(
    tx: &Transaction<'_>,
    cipher: &Arc<dyn InboxCipher>,
    account: &str,
    peer: &str,
    id: &str,
    device: u32,
    pin: [u8; 32],
    content: &[u8],
) -> Result<()> {
    let released: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM native_omemo_ingress_releases WHERE peer=?1 AND id=?2)",
        params![peer, id],
        |r| r.get(0),
    )?;
    ensure!(!released, "Reviewed ingress must not be staged again");
    let count: i64 = tx.query_row("SELECT count(*) FROM native_omemo_inbox", [], |r| r.get(0))?;
    ensure!(count < 1024, "Encrypted inbox retention limit reached");
    let pending_count: i64 = tx.query_row(
        "SELECT count(*) FROM native_omemo_inbox WHERE phase='pending'",
        [],
        |r| r.get(0),
    )?;
    ensure!(pending_count < 128, "Encrypted inbox pending limit reached");
    let (_, own, own_device) = identity(tx, account)?;
    let body = Body {
        account: account.into(),
        peer: peer.into(),
        id: id.into(),
        device,
        pin,
        own,
        own_device,
        content: std::str::from_utf8(content)?.into(),
        runtime: None,
        control: None,
    };
    let serialized = Zeroizing::new(serde_json::to_string(&body)?);
    let sealed = cipher.seal(&serialized)?;
    ensure!(
        sealed.starts_with("enc2:"),
        "Inbox encryption must use the current vault envelope"
    );
    tx.execute(
        "INSERT INTO native_omemo_inbox(peer,id,sealed,phase) VALUES(?1,?2,?3,'pending')",
        params![peer, id, sealed],
    )?;
    Ok(())
}
pub fn pending(
    conn: &mut Connection,
    cipher: &Arc<dyn InboxCipher>,
    account: &str,
) -> Result<Vec<Pending>> {
    let tx = conn.transaction()?;
    let (_, own, own_device) = identity(&tx, account)?;
    let rows:Vec<(String,String,String)>=tx.prepare("SELECT peer,id,sealed FROM native_omemo_inbox WHERE phase='pending' ORDER BY rowid LIMIT 1025")?.query_map([],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?.collect::<std::result::Result<_,_>>()?;
    // Drain legacy backlogs without making a previously oversized queue unrecoverable.
    ensure!(
        rows.len() <= 1024,
        "Encrypted inbox retention limit exceeded"
    );
    let mut result = Vec::new();
    for (peer, id, sealed) in rows {
        let mut body = decode_body(cipher, account, &peer, &id, own, own_device, &sealed)?;
        result.push(Pending {
            peer,
            id,
            device: body.device,
            pin: body.pin,
            content: std::mem::take(&mut body.content),
        });
    }
    tx.commit()?;
    Ok(result)
}
/// Authenticate exactly one pending envelope; unrelated backlog cannot block admission.
pub fn pending_message(
    conn: &mut Connection,
    cipher: &Arc<dyn InboxCipher>,
    account: &str,
    peer: &str,
    id: &str,
) -> Result<Pending> {
    let tx = conn.transaction()?;
    let (_, own, own_device) = identity(&tx, account)?;
    let sealed: String = tx.query_row(
        "SELECT sealed FROM native_omemo_inbox WHERE peer=?1 AND id=?2 AND phase='pending'",
        params![peer, id],
        |r| r.get(0),
    )?;
    let mut body = decode_body(cipher, account, peer, id, own, own_device, &sealed)?;
    let result = Pending {
        peer: peer.into(),
        id: id.into(),
        device: body.device,
        pin: body.pin,
        content: std::mem::take(&mut body.content),
    };
    tx.commit()?;
    Ok(result)
}
pub fn admit(conn: &mut Connection, peer: &str, id: &str) -> Result<bool> {
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let phase: String = tx
        .query_row(
            "SELECT phase FROM native_omemo_inbox WHERE peer=?1 AND id=?2",
            params![peer, id],
            |r| r.get(0),
        )
        .context("No durable ingress record")?;
    if phase != "pending" {
        ensure!(
            matches!(phase.as_str(), "admitted" | "returned"),
            "Invalid inbox phase"
        );
        return Ok(false);
    }
    ensure!(tx.execute("UPDATE native_omemo_inbox SET phase='admitted' WHERE peer=?1 AND id=?2 AND phase='pending'",params![peer,id])?==1,"Ingress admission raced");
    tx.commit()?;
    Ok(true)
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Pending,
    Admitted,
    Returned,
}
/// Authenticated metadata for reconciliation. Message content is never returned.
#[derive(Debug)]
pub struct Record {
    pub phase: Phase,
    pub device: u32,
    pub pin: [u8; 32],
    pub runtime: Option<RuntimeLink>,
    pub control: Option<String>,
    pub review_token: [u8; 32],
}
pub fn inspect(
    conn: &mut Connection,
    cipher: &Arc<dyn InboxCipher>,
    account: &str,
    peer: &str,
    id: &str,
) -> Result<Record> {
    let tx = conn.transaction()?;
    let (_, own, own_device) = identity(&tx, account)?;
    let (sealed, phase): (String, String) = tx.query_row(
        "SELECT sealed,phase FROM native_omemo_inbox WHERE peer=?1 AND id=?2",
        params![peer, id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let phase = match phase.as_str() {
        "pending" => Phase::Pending,
        "admitted" => Phase::Admitted,
        "returned" => Phase::Returned,
        _ => anyhow::bail!("Invalid inbox phase"),
    };
    let mut body = decode_body(cipher, account, peer, id, own, own_device, &sealed)?;
    let record = Record {
        phase,
        device: body.device,
        pin: body.pin,
        runtime: body.runtime.take(),
        control: body.control.take(),
        review_token: review_token(peer, id, &phase, &sealed),
    };
    tx.commit()?;
    Ok(record)
}
fn review_token(peer: &str, id: &str, phase: &Phase, sealed: &str) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(b"native-omemo-ingress-review-v1");
    for part in [
        peer.as_bytes(),
        id.as_bytes(),
        match phase {
            Phase::Pending => b"pending",
            Phase::Admitted => b"admitted",
            Phase::Returned => b"returned",
        },
        sealed.as_bytes(),
    ] {
        hash.update((part.len() as u64).to_be_bytes());
        hash.update(part);
    }
    hash.finalize().into()
}
/// A new operator-review fact, not a tool outcome or runtime completion claim.
#[derive(Debug, Serialize, Deserialize)]
pub struct ReleaseReceipt {
    domain: String,
    account: String,
    peer: String,
    id: String,
    own: [u8; 32],
    own_device: u32,
    pub runtime: Option<RuntimeLink>,
    #[serde(default)]
    pub control: Option<String>,
    pub operator: String,
    pub evidence: String,
    pub source_token: [u8; 32],
    pub released_at: u64,
}
pub struct ReleaseReview<'a> {
    pub expected_token: [u8; 32],
    pub operator: &'a str,
    pub evidence: &'a str,
}
/// Explicit review can release only returned processing, never pending or unknown.
/// This primitive is deliberately absent from the agent/channel tool interface.
pub fn release_reviewed_returned(
    conn: &mut Connection,
    cipher: &Arc<dyn InboxCipher>,
    account: &str,
    peer: &str,
    id: &str,
    review: ReleaseReview<'_>,
) -> Result<()> {
    ensure!(
        !review.operator.trim().is_empty()
            && review.operator.len() <= 256
            && !review.evidence.trim().is_empty()
            && review.evidence.len() <= 4096,
        "Explicit bounded operator and evidence references required"
    );
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let (_, own, own_device) = identity(&tx, account)?;
    let (sealed, phase): (String, String) = tx.query_row(
        "SELECT sealed,phase FROM native_omemo_inbox WHERE peer=?1 AND id=?2",
        params![peer, id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    ensure!(
        phase == "returned",
        "Only reviewed returned ingress may be released"
    );
    let token = review_token(peer, id, &Phase::Returned, &sealed);
    ensure!(token == review.expected_token, "Ingress review is stale");
    let mut body = decode_body(cipher, account, peer, id, own, own_device, &sealed)?;
    let runtime = body.runtime.take();
    let control = body.control.take();
    ensure!(
        runtime.is_some() != control.is_some(),
        "Ingress lacks an unambiguous processing reference"
    );
    let seen: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM seen WHERE sender=?1 AND id=?2)",
        params![peer, id],
        |r| r.get(0),
    )?;
    ensure!(
        seen,
        "Cannot release ingress without its canonical replay marker"
    );
    let receipt = ReleaseReceipt {
        domain: "native-omemo-ingress-release-v1".into(),
        account: account.into(),
        peer: peer.into(),
        id: id.into(),
        own,
        own_device,
        runtime,
        control,
        operator: review.operator.into(),
        evidence: review.evidence.into(),
        source_token: token,
        released_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs(),
    };
    let serialized = Zeroizing::new(serde_json::to_string(&receipt)?);
    let sealed = cipher.seal(&serialized)?;
    ensure!(
        sealed.starts_with("enc2:"),
        "Release receipt must use the current vault envelope"
    );
    tx.execute(
        "INSERT INTO native_omemo_ingress_releases(peer,id,sealed) VALUES(?1,?2,?3)",
        params![peer, id, sealed],
    )?;
    ensure!(
        tx.execute(
            "DELETE FROM native_omemo_inbox WHERE peer=?1 AND id=?2 AND phase='returned'",
            params![peer, id]
        )? == 1,
        "Ingress release raced"
    );
    tx.commit()?;
    Ok(())
}
pub fn inspect_release(
    conn: &mut Connection,
    cipher: &Arc<dyn InboxCipher>,
    account: &str,
    peer: &str,
    id: &str,
) -> Result<ReleaseReceipt> {
    let tx = conn.transaction()?;
    let (_, own, own_device) = identity(&tx, account)?;
    let sealed: String = tx.query_row(
        "SELECT sealed FROM native_omemo_ingress_releases WHERE peer=?1 AND id=?2",
        params![peer, id],
        |r| r.get(0),
    )?;
    ensure!(
        sealed.starts_with("enc2:"),
        "Refusing plaintext ingress release"
    );
    let plaintext = Zeroizing::new(cipher.open(&sealed)?);
    let receipt: ReleaseReceipt = serde_json::from_str(&plaintext)?;
    ensure!(
        receipt.domain == "native-omemo-ingress-release-v1"
            && receipt.account == account
            && receipt.peer == peer
            && receipt.id == id
            && receipt.own == own
            && receipt.own_device == own_device,
        "Ingress release account/message/identity binding mismatch"
    );
    tx.commit()?;
    Ok(receipt)
}
fn decode_body(
    cipher: &Arc<dyn InboxCipher>,
    account: &str,
    peer: &str,
    id: &str,
    own: [u8; 32],
    own_device: u32,
    sealed: &str,
) -> Result<Body> {
    ensure!(
        sealed.starts_with("enc2:"),
        "Refusing plaintext inbox record"
    );
    let plaintext = Zeroizing::new(cipher.open(sealed)?);
    let body: Body = serde_json::from_str(&plaintext)?;
    ensure!(
        body.account == account
            && body.peer == peer
            && body.id == id
            && body.own == own
            && body.own_device == own_device,
        "Inbox account/message/identity binding mismatch"
    );
    Ok(body)
}
/// Bind an admitted ingress to the runtime-selected history before model/tools.
/// A different second binding is refused: it cannot silently redirect recovery.
pub fn bind_runtime(
    conn: &mut Connection,
    cipher: &Arc<dyn InboxCipher>,
    account: &str,
    peer: &str,
    id: &str,
    link: RuntimeLink,
) -> Result<()> {
    ensure!(
        !link.agent.is_empty()
            && link.agent.len() <= 256
            && !link.history_key.is_empty()
            && link.history_key.len() <= 8192,
        "Invalid canonical runtime reference"
    );
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let (_, own, own_device) = identity(&tx, account)?;
    let (sealed, phase): (String, String) = tx.query_row(
        "SELECT sealed,phase FROM native_omemo_inbox WHERE peer=?1 AND id=?2",
        params![peer, id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    ensure!(
        phase == "admitted",
        "Runtime binding requires admitted ingress"
    );
    let mut body = decode_body(cipher, account, peer, id, own, own_device, &sealed)?;
    ensure!(
        body.control.is_none(),
        "Ingress already bound to control processing"
    );
    if let Some(existing) = &body.runtime {
        ensure!(existing == &link, "Canonical runtime reference changed");
        return Ok(());
    }
    body.runtime = Some(link);
    let serialized = Zeroizing::new(serde_json::to_string(&body)?);
    let sealed = cipher.seal(&serialized)?;
    ensure!(
        sealed.starts_with("enc2:"),
        "Inbox encryption must use the current vault envelope"
    );
    ensure!(
        tx.execute(
            "UPDATE native_omemo_inbox SET sealed=?1 WHERE peer=?2 AND id=?3 AND phase='admitted'",
            params![sealed, peer, id]
        )? == 1,
        "Ingress binding raced"
    );
    tx.commit()?;
    Ok(())
}
pub fn returned(conn: &mut Connection, peer: &str, id: &str) -> Result<()> {
    ensure!(conn.execute("UPDATE native_omemo_inbox SET phase='returned' WHERE peer=?1 AND id=?2 AND phase='admitted'",params![peer,id])?==1,"No admitted ingress to finalize");
    Ok(())
}
/// Commit the fact that a dispatcher control path returned, without inventing
/// an agent or claiming an agent history exists. Unknown paths remain admitted.
pub fn control_returned(
    conn: &mut Connection,
    cipher: &Arc<dyn InboxCipher>,
    account: &str,
    peer: &str,
    id: &str,
    control: &str,
) -> Result<()> {
    ensure!(
        matches!(
            control,
            "stop"
                | "gate"
                | "event"
                | "unowned_route"
                | "pending_budget"
                | "agent_retired"
                | "hook_cancelled"
                | "lane_refused"
                | "queued_cancelled"
        ),
        "Unknown dispatcher control path"
    );
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let (_, own, own_device) = identity(&tx, account)?;
    let sealed: String = tx.query_row(
        "SELECT sealed FROM native_omemo_inbox WHERE peer=?1 AND id=?2 AND phase='admitted'",
        params![peer, id],
        |r| r.get(0),
    )?;
    let mut body = decode_body(cipher, account, peer, id, own, own_device, &sealed)?;
    ensure!(
        body.runtime.is_none() && body.control.is_none(),
        "Ingress already bound"
    );
    body.control = Some(control.into());
    let serialized = Zeroizing::new(serde_json::to_string(&body)?);
    let sealed = cipher.seal(&serialized)?;
    ensure!(
        sealed.starts_with("enc2:"),
        "Control receipt must be encrypted"
    );
    ensure!(tx.execute("UPDATE native_omemo_inbox SET sealed=?1,phase='returned' WHERE peer=?2 AND id=?3 AND phase='admitted'",params![sealed,peer,id])?==1,"Control return raced");
    tx.commit()?;
    Ok(())
}

#[cfg(test)]
mod schema_tests {
    use super::*;
    struct SyntheticCipher(String);
    impl InboxCipher for SyntheticCipher {
        fn check_existing(&self) -> Result<()> {
            Ok(())
        }
        fn seal(&self, _: &str) -> Result<String> {
            anyhow::bail!("Read-only test cipher")
        }
        fn open(&self, _: &str) -> Result<String> {
            Ok(self.0.clone())
        }
    }
    #[test]
    fn unlinked_envelopes_remain_readable_and_binding_is_checked() -> Result<()> {
        let body = Body {
            account: "bob@example.test".into(),
            peer: "alice@example.test".into(),
            id: "synthetic-id".into(),
            device: 101,
            pin: [1; 32],
            own: [2; 32],
            own_device: 202,
            content: "synthetic content".into(),
            runtime: None,
            control: None,
        };
        let mut json = serde_json::to_value(&body)?;
        json.as_object_mut()
            .context("Expected object")?
            .remove("runtime");
        let cipher: Arc<dyn InboxCipher> = Arc::new(SyntheticCipher(json.to_string()));
        let decoded = decode_body(
            &cipher,
            "bob@example.test",
            "alice@example.test",
            "synthetic-id",
            [2; 32],
            202,
            "enc2:synthetic",
        )?;
        assert!(decoded.runtime.is_none());
        assert!(
            decode_body(
                &cipher,
                "bob@example.test",
                "mallory@example.test",
                "synthetic-id",
                [2; 32],
                202,
                "enc2:synthetic"
            )
            .is_err()
        );
        assert!(
            decode_body(
                &cipher,
                "bob@example.test",
                "alice@example.test",
                "synthetic-id",
                [3; 32],
                202,
                "enc2:synthetic"
            )
            .is_err()
        );
        assert!(
            decode_body(
                &cipher,
                "bob@example.test",
                "alice@example.test",
                "synthetic-id",
                [2; 32],
                202,
                "plaintext"
            )
            .is_err()
        );
        Ok(())
    }
    #[test]
    fn review_token_changes_with_phase_and_ciphertext() {
        let a = review_token("peer", "id", &Phase::Admitted, "enc2:a");
        assert_ne!(a, review_token("peer", "id", &Phase::Returned, "enc2:a"));
        assert_ne!(a, review_token("peer", "id", &Phase::Admitted, "enc2:b"));
        assert_ne!(a, review_token("other", "id", &Phase::Admitted, "enc2:a"));
    }
    #[test]
    fn inbox_rejects_release_table_without_canonical_primary_key() -> Result<()> {
        let mut conn = Connection::open_in_memory()?;
        conn.execute_batch("CREATE TABLE native_omemo_ingress_releases(peer TEXT NOT NULL,id TEXT NOT NULL,sealed TEXT NOT NULL)")?;
        assert!(
            initialize(&mut conn)
                .unwrap_err()
                .to_string()
                .contains("release schema")
        );
        Ok(())
    }
    #[test]
    fn inbox_reopen_accepts_schema_and_preserves_unknown_outcomes() -> Result<()> {
        let mut conn = Connection::open_in_memory()?;
        initialize(&mut conn)?;
        conn.execute(
            "INSERT INTO native_omemo_inbox VALUES('peer','id','enc2:synthetic','admitted')",
            [],
        )?;
        initialize(&mut conn)?;
        assert!(!admit(&mut conn, "peer", "id")?);
        Ok(())
    }
    #[test]
    fn inbox_rejects_missing_primary_key_before_admission() -> Result<()> {
        let mut conn = Connection::open_in_memory()?;
        conn.execute_batch("CREATE TABLE native_omemo_inbox(peer TEXT NOT NULL,id TEXT NOT NULL,sealed TEXT NOT NULL,phase TEXT NOT NULL)")?;
        assert!(
            initialize(&mut conn)
                .unwrap_err()
                .to_string()
                .contains("schema")
        );
        Ok(())
    }
    #[test]
    fn inbox_rejects_corrupt_phase_and_plaintext_on_open() -> Result<()> {
        for (sealed, phase) in [("plaintext", "pending"), ("enc2:synthetic", "unknown")] {
            let mut conn = Connection::open_in_memory()?;
            initialize(&mut conn)?;
            conn.execute_batch("PRAGMA ignore_check_constraints=ON")?;
            conn.execute(
                "INSERT INTO native_omemo_inbox VALUES('peer','id',?1,?2)",
                params![sealed, phase],
            )?;
            assert!(
                initialize(&mut conn)
                    .unwrap_err()
                    .to_string()
                    .contains("record")
            );
            let count: i64 =
                conn.query_row("SELECT count(*) FROM native_omemo_inbox", [], |r| r.get(0))?;
            assert_eq!(count, 1, "Opening must not delete uncertain ingress");
        }
        Ok(())
    }
}
