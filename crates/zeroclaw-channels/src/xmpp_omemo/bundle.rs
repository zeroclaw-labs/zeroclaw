//! Existing legacy identity bundle export. No identity generation or automatic trust.
use crate::xmpp_omemo::{
    omemo::protocol::X3DHProtocol,
    store::{identity, load},
};
use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use curve25519_dalek::{constants::ED25519_BASEPOINT_POINT, scalar::Scalar};
use rusqlite::{Connection, TransactionBehavior};
use serde_json::Value;
use zeroize::Zeroizing;
const ROOT: &str = "omemo//eu.siacs.conversations.axolotl";
fn latin(v: &Value, size: usize) -> Result<Zeroizing<Vec<u8>>> {
    let text = v.as_str().context("Missing key bytes")?;
    let bytes: Vec<u8> = text
        .chars()
        .map(|c| u8::try_from(u32::from(c)))
        .collect::<std::result::Result<_, _>>()?;
    ensure!(bytes.len() == size, "Invalid stored key size");
    Ok(Zeroizing::new(bytes))
}
fn identifier(map: &Value, key: &[u8]) -> Result<u32> {
    let value = map
        .get(STANDARD.encode(key))
        .and_then(Value::as_u64)
        .context("Missing stable prekey ID")?;
    let id = u32::try_from(value)?;
    ensure!((1..=i32::MAX as u32).contains(&id), "Invalid prekey ID");
    Ok(id)
}
fn curve(bytes: &[u8]) -> Result<[u8; 32]> {
    let raw: [u8; 32] = bytes.try_into()?;
    Ok(*x25519_dalek::PublicKey::from(&x25519_dalek::StaticSecret::from(raw)).as_bytes())
}
fn prefixed(key: &[u8]) -> String {
    STANDARD.encode([&[5u8], key].concat())
}
pub struct PublicBundle {
    pub device: u32,
    pub xml: String,
    pub prekey_count: usize,
}
/// Bundle bound to an operator-supplied fingerprint, with verified signed prekey.
pub fn parse_pinned(
    xml: &str,
    device: u32,
    pin: [u8; 32],
) -> Result<crate::xmpp_omemo::omemo::protocol::VerifiedDeviceIdentity> {
    use crate::xmpp_omemo::omemo::protocol::{DeviceIdentity, PreKeyBundle, SignedPreKeyBundle};
    use roxmltree::{Document, Node, ParsingOptions};
    const NS: &str = "eu.siacs.conversations.axolotl";
    ensure!(
        (1..=i32::MAX as u32).contains(&device) && xml.len() <= 256 * 1024,
        "Invalid bundle size/device"
    );
    let doc = Document::parse_with_options(
        xml,
        ParsingOptions {
            allow_dtd: false,
            nodes_limit: 4096,
        },
    )?;
    let root = doc.root_element();
    ensure!(
        root.has_tag_name((NS, "bundle")),
        "Unsupported bundle namespace"
    );
    fn one<'a, 'i>(parent: Node<'a, 'i>, name: &str) -> Result<Node<'a, 'i>> {
        let matches: Vec<_> = parent
            .children()
            .filter(|n| n.is_element() && n.has_tag_name((NS, name)))
            .collect();
        ensure!(matches.len() == 1, "Missing or duplicate bundle field");
        Ok(matches[0])
    }
    fn decode(node: Node<'_, '_>, size: usize) -> Result<Vec<u8>> {
        ensure!(
            !node.children().any(|n| n.is_element()),
            "Nested bundle key"
        );
        let text: String = node.children().filter_map(|n| n.text()).collect();
        ensure!(text.len() <= 512, "Bundle field too large");
        let clean: Vec<_> = text.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
        let bytes = STANDARD.decode(clean)?;
        ensure!(bytes.len() == size, "Wrong bundle key size");
        Ok(bytes)
    }
    fn public(node: Node<'_, '_>) -> Result<Vec<u8>> {
        let bytes = decode(node, 33)?;
        ensure!(bytes[0] == 5, "Wrong legacy key encoding");
        Ok(bytes[1..].to_vec())
    }
    fn key_id(node: Node<'_, '_>, attribute: &str) -> Result<u32> {
        let text = node.attribute(attribute).context("Missing prekey ID")?;
        ensure!(
            !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit()),
            "Invalid prekey ID"
        );
        let id = text.parse()?;
        ensure!((1..=i32::MAX as u32).contains(&id), "Invalid prekey ID");
        Ok(id)
    }
    ensure!(
        root.children().filter(|n| n.is_element()).count() == 4,
        "Unexpected bundle fields"
    );
    let identity = public(one(root, "identityKey")?)?;
    ensure!(identity == pin, "Bundle fingerprint is not pinned");
    let signed = one(root, "signedPreKeyPublic")?;
    let signature = decode(one(root, "signedPreKeySignature")?, 64)?;
    let keys = one(root, "prekeys")?;
    let mut prekeys = Vec::new();
    let mut ids = std::collections::HashSet::new();
    let mut pubs = std::collections::HashSet::new();
    for node in keys.children().filter(|n| n.is_element()) {
        ensure!(
            node.has_tag_name((NS, "preKeyPublic")) && prekeys.len() < 1000,
            "Unexpected or excessive prekeys"
        );
        let id = key_id(node, "preKeyId")?;
        let key = public(node)?;
        ensure!(
            ids.insert(id) && pubs.insert(key.clone()),
            "Duplicate prekey ID or key"
        );
        prekeys.push(PreKeyBundle {
            id,
            public_key: key,
        });
    }
    ensure!(!prekeys.is_empty(), "Bundle has no one-time prekeys");
    DeviceIdentity {
        id: device.into(),
        identity_key: identity,
        signed_pre_key: SignedPreKeyBundle {
            id: key_id(signed, "signedPreKeyId")?,
            public_key: public(signed)?,
            signature,
        },
        pre_keys: prekeys,
    }
    .verify()
    .map_err(Into::into)
}
pub fn export_existing(conn: &mut Connection, account: &str) -> Result<PublicBundle> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Deferred)?;
    let (private, public, device) = identity(&tx, account)?;
    let private = Zeroizing::new(private);
    let state = load(&tx, &format!("{ROOT}/x3dh"))?;
    ensure!(
        state["version"] == "1.0.0" && state["identity_key"]["version"] == "1.0.0",
        "Unsupported X3DH schema"
    );
    let stored = latin(&state["identity_key"]["secret"], 32)?;
    let mut secret = Zeroizing::new([0u8; 32]);
    secret.copy_from_slice(&stored);
    match state["identity_key"]["secret_type"].as_str() {
        Some("SEED") => {
            use sha2::{Digest, Sha512};
            let digest = Zeroizing::new(Sha512::digest(*secret).to_vec());
            secret.copy_from_slice(&digest[..32]);
        }
        Some("PRIV") => {}
        _ => anyhow::bail!("Unsupported X3DH identity format"),
    }
    ensure!(
        curve(secret.as_slice())? == public,
        "X3DH identity mismatch"
    );
    let signed = &state["signed_pre_key"];
    ensure!(
        signed["version"] == "1.0.0",
        "Unsupported signed prekey schema"
    );
    let spk = curve(&latin(&signed["priv"], 32)?)?;
    let mut signature = latin(&signed["sig"], 64)?;
    // Ed25519 public sign bit is retained by the legacy 64-byte signature wire format.
    let mut clamped = *private;
    clamped[0] &= 248;
    clamped[31] &= 127;
    clamped[31] |= 64;
    let scalar = Zeroizing::new(Scalar::from_bytes_mod_order(clamped));
    let ed = (*scalar * ED25519_BASEPOINT_POINT).compress().to_bytes();
    signature[63] |= ed[31] & 128;
    X3DHProtocol::verify_pre_key(&public, &spk, &signature)?;
    let signed_id = identifier(&load(&tx, &format!("{ROOT}/signed_pre_key_ids"))?, &spk)?;
    let ids = load(&tx, &format!("{ROOT}/pre_key_ids"))?;
    let entries = state["pre_keys"].as_array().context("Missing prekeys")?;
    ensure!(
        !entries.is_empty() && entries.len() <= 1000,
        "Invalid prekey inventory"
    );
    let mut keys = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for entry in entries {
        let key = curve(&latin(entry, 32)?)?;
        let id = identifier(&ids, &key)?;
        ensure!(seen.insert(id), "Duplicate prekey ID");
        keys.push((id, key));
    }
    keys.sort_by_key(|(id, _)| *id);
    let mut xml = format!(
        "<bundle xmlns='eu.siacs.conversations.axolotl'><signedPreKeyPublic signedPreKeyId='{signed_id}'>{}</signedPreKeyPublic><signedPreKeySignature>{}</signedPreKeySignature><identityKey>{}</identityKey><prekeys>",
        prefixed(&spk),
        STANDARD.encode(signature.as_slice()),
        prefixed(&public)
    );
    for (id, key) in &keys {
        xml.push_str(&format!(
            "<preKeyPublic preKeyId='{id}'>{}</preKeyPublic>",
            prefixed(key)
        ));
    }
    xml.push_str("</prekeys></bundle>");
    tx.commit()?;
    Ok(PublicBundle {
        device,
        xml,
        prekey_count: keys.len(),
    })
}
