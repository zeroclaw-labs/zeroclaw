//! Bounded legacy OMEMO XML codec. Never reinterpret a modern stanza as legacy.
use crate::xmpp_omemo::store::ReservedEnvelope;
use anyhow::{Context, Result, bail, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use roxmltree::{Document, Node, ParsingOptions};
use std::collections::HashSet;
pub const LEGACY_NS: &str = "eu.siacs.conversations.axolotl";
pub struct SelectedKey {
    pub sender_device: u32,
    pub wire: Vec<u8>,
    pub prekey: bool,
    pub nonce: [u8; 12],
    pub ciphertext: Option<Vec<u8>>,
}
fn id(value: Option<&str>) -> Result<u32> {
    let value = value.context("Missing device ID")?;
    ensure!(
        !value.is_empty() && value.bytes().all(|c| c.is_ascii_digit()),
        "Invalid device ID"
    );
    let id = value.parse()?;
    ensure!((1..=i32::MAX as u32).contains(&id), "Invalid device ID");
    Ok(id)
}
fn decoded(node: Node<'_, '_>, limit: usize) -> Result<Vec<u8>> {
    ensure!(
        !node.children().any(|n| n.is_element()),
        "Nested encoded data"
    );
    let text: String = node.children().filter_map(|n| n.text()).collect();
    ensure!(
        text.len() <= limit.div_ceil(3) * 4 + 256,
        "Encoded data too large"
    );
    let clean: Vec<u8> = text.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    let bytes = STANDARD.decode(clean).context("Invalid base64 data")?;
    ensure!(bytes.len() <= limit, "Decoded data too large");
    Ok(bytes)
}
pub fn parse(xml: &str, own_device: u32) -> Result<SelectedKey> {
    ensure!(xml.len() <= 2 * 1024 * 1024, "OMEMO element too large");
    let document = Document::parse_with_options(
        xml,
        ParsingOptions {
            allow_dtd: false,
            nodes_limit: 2048,
        },
    )?;
    let root = document.root_element();
    ensure!(
        root.has_tag_name((LEGACY_NS, "encrypted")),
        "Unsupported OMEMO dialect"
    );
    let mut header = None;
    let mut payload = None;
    for node in root.children().filter(|n| n.is_element()) {
        if node.has_tag_name((LEGACY_NS, "header")) {
            ensure!(header.replace(node).is_none(), "Duplicate header");
        } else if node.has_tag_name((LEGACY_NS, "payload")) {
            ensure!(payload.replace(node).is_none(), "Duplicate payload");
        } else {
            bail!("Unexpected encrypted child");
        }
    }
    let header = header.context("Missing header")?;
    let sender = id(header.attribute("sid"))?;
    let mut nonce = None;
    let mut selected = None;
    let mut recipients = HashSet::new();
    for node in header.children().filter(|n| n.is_element()) {
        if node.has_tag_name((LEGACY_NS, "iv")) {
            ensure!(nonce.replace(decoded(node, 12)?).is_none(), "Duplicate IV");
        } else if node.has_tag_name((LEGACY_NS, "key")) {
            ensure!(recipients.len() < 256, "Too many recipient keys");
            let recipient = id(node.attribute("rid"))?;
            ensure!(recipients.insert(recipient), "Duplicate recipient key");
            let prekey = match node.attribute("prekey") {
                None | Some("false" | "0") => false,
                Some("true" | "1") => true,
                _ => bail!("Invalid prekey flag"),
            };
            let wire = decoded(node, 8192)?;
            ensure!(!wire.is_empty(), "Empty encrypted key");
            if recipient == own_device {
                selected = Some((wire, prekey));
            }
        } else {
            bail!("Unexpected header child");
        }
    }
    let nonce = nonce.context("Missing nonce")?.try_into().map_err(|_| {
        ::zeroclaw_log::record!(
            ERROR,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail),
            "Invalid nonce length"
        );
        anyhow::Error::msg("Invalid nonce length")
    })?;
    let (wire, prekey) = selected.context("No key for this device")?;
    let ciphertext = payload.map(|node| decoded(node, 1024 * 1024)).transpose()?;
    if let Some(ref bytes) = ciphertext {
        ensure!(!bytes.is_empty(), "Empty payload");
    }
    Ok(SelectedKey {
        sender_device: sender,
        wire,
        prekey,
        nonce,
        ciphertext,
    })
}
pub fn serialize(envelope: &ReservedEnvelope) -> Result<String> {
    ensure!(
        (1..=i32::MAX as u32).contains(&envelope.sender_device)
            && (1..=i32::MAX as u32).contains(&envelope.device),
        "Invalid device ID"
    );
    ensure!(
        envelope.nonce.len() == 12 && envelope.wire.len() <= 8192 && !envelope.wire.is_empty(),
        "Invalid envelope"
    );
    ensure!(
        !envelope.ciphertext.is_empty() && envelope.ciphertext.len() <= 1024 * 1024,
        "Invalid payload size"
    );
    let fallback = crate::xmpp_omemo::store::RecipientKey {
        device: envelope.device,
        fingerprint: envelope.fingerprint,
        wire: envelope.wire.clone(),
        prekey: envelope.prekey,
    };
    let recipients = if envelope.recipients.is_empty() {
        std::slice::from_ref(&fallback)
    } else {
        &envelope.recipients
    };
    ensure!(
        recipients.len() <= 256 && recipients[0] == fallback,
        "Inconsistent recipient envelope"
    );
    let mut ids = std::collections::HashSet::new();
    let mut keys = String::new();
    for r in recipients {
        ensure!(
            (1..=i32::MAX as u32).contains(&r.device)
                && ids.insert(r.device)
                && !r.wire.is_empty()
                && r.wire.len() <= 8192,
            "Invalid recipient envelope"
        );
        keys.push_str(&format!(
            "<key rid='{}' prekey='{}'>{}</key>",
            r.device,
            r.prekey,
            STANDARD.encode(&r.wire)
        ));
    }
    Ok(format!(
        "<encrypted xmlns='{LEGACY_NS}'><header sid='{}'>{keys}<iv>{}</iv></header><payload>{}</payload></encrypted>",
        envelope.sender_device,
        STANDARD.encode(&envelope.nonce),
        STANDARD.encode(&envelope.ciphertext)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn valid() -> String {
        format!(
            "<encrypted xmlns='{LEGACY_NS}'><header sid='101'><key rid='202'>AQ==</key><iv>AAAAAAAAAAAAAAAA</iv></header><payload>AQ==</payload></encrypted>"
        )
    }
    #[test]
    fn fanout_selects_each_device_and_rejects_inconsistent_keys() {
        use crate::xmpp_omemo::store::{RecipientKey, ReservedEnvelope};
        let first = RecipientKey {
            device: 101,
            fingerprint: [1; 32],
            wire: vec![1, 2],
            prekey: true,
        };
        let second = RecipientKey {
            device: 102,
            fingerprint: [2; 32],
            wire: vec![3, 4],
            prekey: false,
        };
        let mut envelope = ReservedEnvelope {
            id: "test".into(),
            peer: "alice@example.test".into(),
            device: 101,
            sender_device: 202,
            sender_fingerprint: [9; 32],
            fingerprint: first.fingerprint,
            wire: first.wire.clone(),
            ciphertext: vec![5, 6],
            nonce: vec![7; 12],
            prekey: true,
            recipients: vec![first.clone(), second.clone()],
        };
        let xml = serialize(&envelope).unwrap();
        let a = parse(&xml, 101).unwrap();
        let b = parse(&xml, 102).unwrap();
        assert_eq!(a.wire, first.wire);
        assert_eq!(b.wire, second.wire);
        assert!(a.prekey && !b.prekey);
        assert_eq!(a.ciphertext, b.ciphertext);
        assert_eq!(a.nonce, b.nonce);
        envelope.recipients[1].device = 101;
        assert!(serialize(&envelope).is_err());
        envelope.recipients[1] = second;
        envelope.recipients[0].wire.push(0);
        assert!(serialize(&envelope).is_err());
    }
    #[test]
    fn refuses_ambiguous_missing_foreign_and_malformed_fields() {
        assert!(parse(&valid(), 202).is_ok());
        for value in [
            valid().replace(
                "<key rid='202'>AQ==</key>",
                "<key rid='202'>AQ==</key><key rid='202'>Ag==</key>",
            ),
            valid().replace("sid='101'", "sid='0'"),
            valid().replace("AAAAAAAAAAAAAAAA", "AA=="),
            valid().replace(LEGACY_NS, "urn:xmpp:omemo:2"),
            valid().replace("AQ==", "$$$"),
            valid().replace("<header", "<header xmlns='urn:wrong'"),
            valid().replace("rid='202'", "rid='202' prekey='maybe'"),
            format!(
                "<!DOCTYPE encrypted [<!ENTITY key 'AQ=='>]>{}",
                valid().replace("AQ==", "&key;")
            ),
        ] {
            assert!(parse(&value, 202).is_err());
        }
        assert!(parse(&valid(), 203).is_err());
    }
    #[test]
    fn key_transport_is_explicit_and_not_a_plaintext_fallback() {
        let xml = valid().replace("<payload>AQ==</payload>", "");
        assert!(parse(&xml, 202).unwrap().ciphertext.is_none());
    }
}
