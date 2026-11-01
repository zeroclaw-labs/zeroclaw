//! Codec for the Python DoubleRatchet JSON state. No implicit identity reset.
use crate::xmpp_omemo::omemo::{
    keys::{ChainKey, MessageKey, PublicKey, RootKey, Secret},
    protocol::{KeyPair, RatchetState},
};
use anyhow::{Context, Result, ensure};
use serde_json::Value;
use std::collections::{HashMap, HashSet};

fn key(value: &Value) -> Result<[u8; 32]> {
    let text = value.as_str().context("Missing legacy key")?;
    let bytes: Vec<u8> = text
        .chars()
        .map(|c| u8::try_from(u32::from(c)))
        .collect::<std::result::Result<_, _>>()
        .context("Invalid legacy byte encoding")?;
    bytes.try_into().map_err(|_| {
        ::zeroclaw_log::record!(
            ERROR,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail),
            "Invalid legacy key length"
        );
        anyhow::Error::msg("Invalid legacy key length")
    })
}
fn text(bytes: &[u8]) -> Value {
    Value::String(bytes.iter().map(|b| char::from(*b)).collect())
}
fn counter(value: &Value) -> Result<u32> {
    u32::try_from(value.as_u64().context("Invalid legacy counter")?)
        .context("Legacy counter overflow")
}
fn version(value: &Value) -> Result<()> {
    ensure!(
        value["version"].as_str() == Some("1.0.0"),
        "Unsupported legacy state version"
    );
    Ok(())
}
fn pair(private: [u8; 32]) -> KeyPair {
    let public = x25519_dalek::PublicKey::from(&x25519_dalek::StaticSecret::from(private));
    KeyPair {
        public_key: PublicKey::new(*public.as_bytes()),
        private_key: Secret::new(private),
    }
}

pub type PreparedPayload = (zeroize::Zeroizing<Vec<u8>>, Vec<u8>, Vec<u8>);

pub struct LegacySession {
    pub state: RatchetState,
    template: Value,
}
fn wipe_json(value: &mut Value) {
    use zeroize::Zeroize;
    match value {
        Value::String(s) => s.zeroize(),
        Value::Array(items) => items.iter_mut().for_each(wipe_json),
        Value::Object(items) => items.values_mut().for_each(wipe_json),
        _ => {}
    }
}
impl Drop for LegacySession {
    fn drop(&mut self) {
        wipe_json(&mut self.template);
    }
}
impl LegacySession {
    /// Build the canonical JSON representation after one authenticated passive X3DH receive.
    pub fn from_fresh_recipient(state: RatchetState) -> Result<Self> {
        ensure!(
            !state.is_initiator && state.receive_message_number > 0,
            "Fresh recipient has not authenticated a message"
        );
        let template = serde_json::json!({"version":"1.0.0","diffie_hellman_ratchet":{
            "version":"1.0.0","own_ratchet_priv":text(state.ratchet_key_pair.private_key.expose_secret()),
            "other_ratchet_pub":text(&state.remote_ratchet_key),
            "root_chain":{"version":"1.0.0","length":2,"key":text(state.root_key.expose_secret())},
            "symmetric_key_ratchet":{"version":"1.0.0","previous_sending_chain_length":null,
                "sending_chain":{"version":"1.0.0","length":state.send_message_number,"key":text(state.send_chain_key.expose_secret())},
                "receiving_chain":{"version":"1.0.0","length":state.receive_message_number,"key":text(state.receive_chain_key.expose_secret())}}},
            "skipped_message_keys":[]});
        Ok(Self { state, template })
    }
    /// Encrypt once; the same authenticated material can be wrapped for each device.
    pub fn prepare_payload(plaintext: &[u8]) -> Result<PreparedPayload> {
        use crate::xmpp_omemo::omemo::{
            crypto,
            keys::{AesGcmKey, GcmNonce},
        };
        use zeroize::Zeroizing;
        ensure!(
            !plaintext.is_empty() && plaintext.len() <= 1024 * 1024,
            "Invalid payload size"
        );
        let key = Zeroizing::new(crypto::generate_aes_key()?);
        let nonce = crypto::generate_gcm_iv()?;
        let combined = crypto::aes_gcm_encrypt(
            plaintext,
            &AesGcmKey::from_slice(&key).context("Invalid generated key")?,
            &GcmNonce::from_slice(&nonce).context("Invalid generated nonce")?,
        )?;
        let split = combined
            .len()
            .checked_sub(16)
            .context("Invalid authenticated payload")?;
        Ok((
            Zeroizing::new([key.as_slice(), &combined[split..]].concat()),
            combined[..split].to_vec(),
            nonce,
        ))
    }
    /// Advance only this device's send chain, after validating shared key material.
    pub fn wrap_payload_material(&mut self, material: &[u8]) -> Result<Vec<u8>> {
        ensure!(material.len() == 32, "Invalid legacy payload key material");
        let mut candidate = self.state.clone();
        let wire = crate::xmpp_omemo::omemo::protocol::DoubleRatchet::encrypt_key(
            &mut candidate,
            material,
        )?;
        self.state = candidate;
        Ok(wire)
    }

    /// Commit the receiving ratchet only after the entire payload authenticates.
    /// The caller must still persist the exported state and replay marker atomically.
    pub fn decrypt_payload(
        &mut self,
        wire: &[u8],
        ciphertext: &[u8],
        iv: &[u8],
    ) -> Result<Vec<u8>> {
        use crate::xmpp_omemo::omemo::{
            crypto,
            keys::{AesGcmKey, GcmNonce},
            protocol::DoubleRatchet,
        };
        use zeroize::Zeroizing;
        let nonce = GcmNonce::from_slice(iv).context("Invalid payload nonce")?;
        ensure!(ciphertext.len() <= 1024 * 1024, "Payload exceeds limit");
        let mut candidate = self.state.clone();
        let material = Zeroizing::new(DoubleRatchet::decrypt_key(&mut candidate, wire)?);
        ensure!(material.len() == 32, "Invalid legacy payload key material");
        let key = AesGcmKey::from_slice(&material[..16]).context("Invalid payload key")?;
        let mut combined = ciphertext.to_vec();
        combined.extend_from_slice(&material[16..]);
        let plaintext = crypto::aes_gcm_decrypt(&combined, &key, &nonce)?;
        self.state = candidate;
        Ok(plaintext)
    }
    pub fn import(
        template: Value,
        own_private: [u8; 32],
        own_public: [u8; 32],
        remote_public: [u8; 32],
        own_id: u32,
        remote_id: u32,
        peer: String,
    ) -> Result<Self> {
        ensure!(
            (1..=i32::MAX as u32).contains(&own_id) && (1..=i32::MAX as u32).contains(&remote_id),
            "Invalid legacy device ID"
        );
        version(&template)?;
        let dh = &template["diffie_hellman_ratchet"];
        let sym = &dh["symmetric_key_ratchet"];
        for section in [dh, &dh["root_chain"], sym, &sym["sending_chain"]] {
            version(section)?;
        }
        let waiting_for_reply = sym["receiving_chain"].is_null();
        if !waiting_for_reply {
            version(&sym["receiving_chain"])?;
        }
        let entries = template["skipped_message_keys"]
            .as_array()
            .context("Missing skipped keys")?;
        ensure!(
            entries.len() <= 1000,
            "Unsupported legacy skipped-key capacity"
        );
        let mut skipped = HashMap::new();
        for entry in entries {
            let id = (
                key(&entry["ratchet_pub"])?.to_vec(),
                counter(&entry["index"])?,
            );
            ensure!(
                skipped
                    .insert(id, MessageKey(Secret::new(key(&entry["message_key"])?)))
                    .is_none(),
                "Duplicate legacy skipped key"
            );
        }
        let identity = pair(own_private);
        ensure!(
            identity.public_key.as_raw() == &own_public,
            "Legacy identity does not match its fingerprint"
        );
        let state = RatchetState {
            initialized: true,
            is_initiator: waiting_for_reply,
            remote_identity_key: remote_public.to_vec(),
            local_identity_key_pair: identity,
            root_key: RootKey(Secret::new(key(&dh["root_chain"]["key"])?)),
            send_chain_key: ChainKey::new(Secret::new(key(&sym["sending_chain"]["key"])?)),
            receive_chain_key: ChainKey::new(Secret::new(if waiting_for_reply {
                [0; 32]
            } else {
                key(&sym["receiving_chain"]["key"])?
            })),
            ratchet_key_pair: pair(key(&dh["own_ratchet_priv"])?),
            remote_ratchet_key: key(&dh["other_ratchet_pub"])?.to_vec(),
            prev_remote_ratchet_key: vec![],
            send_message_number: counter(&sym["sending_chain"]["length"])?,
            receive_message_number: if waiting_for_reply {
                0
            } else {
                counter(&sym["receiving_chain"]["length"])?
            },
            prev_receive_message_number: 0,
            prev_send_message_number: if sym["previous_sending_chain_length"].is_null() {
                0
            } else {
                counter(&sym["previous_sending_chain_length"])?
            },
            skipped_message_keys: skipped,
            local_device_id: own_id.into(),
            remote_device_id: remote_id.into(),
            remote_jid: peer,
            establishing_base_key: None,
        };
        Ok(Self { state, template })
    }
    pub fn export(&self) -> Result<Value> {
        ensure!(
            self.state.skipped_message_keys.len() <= 3000,
            "Skipped-key capacity exceeded"
        );
        let mut seen = HashSet::new();
        let mut entries = Vec::new();
        for entry in self.template["skipped_message_keys"]
            .as_array()
            .context("Missing skipped keys")?
        {
            let id = (
                key(&entry["ratchet_pub"])?.to_vec(),
                counter(&entry["index"])?,
            );
            if let Some(value) = self.state.skipped_message_keys.get(&id) {
                let mut kept = entry.clone();
                kept["message_key"] = text(value.expose_secret());
                entries.push(kept);
                seen.insert(id);
            }
        }
        let mut added: Vec<_> = self
            .state
            .skipped_message_keys
            .iter()
            .filter(|(id, _)| !seen.contains(*id))
            .collect();
        added.sort_by_key(|((ratchet, index), _)| {
            (
                if *ratchet == self.state.prev_remote_ratchet_key {
                    0
                } else {
                    1
                },
                *index,
            )
        });
        for ((ratchet, index), value) in added {
            ensure!(
                *ratchet == self.state.prev_remote_ratchet_key
                    || *ratchet == self.state.remote_ratchet_key,
                "Unknown new skipped-key epoch"
            );
            entries.push(serde_json::json!({"ratchet_pub":text(ratchet),"index":index,"message_key":text(value.expose_secret())}));
        }
        if entries.len() > 1000 {
            entries.drain(..entries.len() - 1000);
        }
        let mut result = self.template.clone();
        result["skipped_message_keys"] = Value::Array(entries);
        let dh = &mut result["diffie_hellman_ratchet"];
        let changed = key(&dh["own_ratchet_priv"])?
            != *self.state.ratchet_key_pair.private_key.expose_secret();
        if changed {
            let steps = dh["root_chain"]["length"]
                .as_u64()
                .context("Missing root chain counter")?;
            dh["root_chain"]["length"] = steps
                .checked_add(2)
                .context("Root chain counter overflow")?
                .into();
        }
        dh["root_chain"]["key"] = text(self.state.root_key.expose_secret());
        dh["own_ratchet_priv"] = text(self.state.ratchet_key_pair.private_key.expose_secret());
        dh["other_ratchet_pub"] = text(&self.state.remote_ratchet_key);
        let sym = &mut dh["symmetric_key_ratchet"];
        sym["sending_chain"]["key"] = text(self.state.send_chain_key.expose_secret());
        sym["sending_chain"]["length"] = self.state.send_message_number.into();
        if changed || !sym["receiving_chain"].is_null() {
            if sym["receiving_chain"].is_null() {
                sym["receiving_chain"] = serde_json::json!({"version":"1.0.0"});
            }
            sym["receiving_chain"]["key"] = text(self.state.receive_chain_key.expose_secret());
            sym["receiving_chain"]["length"] = self.state.receive_message_number.into();
        }
        if changed || !sym["previous_sending_chain_length"].is_null() {
            sym["previous_sending_chain_length"] = self.state.prev_send_message_number.into();
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn legacy_bytes_are_latin1_not_utf8() {
        let bytes: [u8; 32] = std::array::from_fn(|i| 224 + i as u8);
        assert_eq!(key(&text(&bytes)).unwrap(), bytes);
        assert!(key(&Value::String("x".repeat(31))).is_err());
        assert!(key(&Value::String("\u{1f680}".repeat(32))).is_err());
    }
}
