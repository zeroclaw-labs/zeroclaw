// src/omemo/wire.rs
//! Signal Protocol wire format serialization/deserialization
//!
//! Implements the binary format used by libsignal for SignalMessage and
//! PreKeySignalMessage, enabling interoperability with Conversations, Dino,
//! and other OMEMO clients.
//!
//! Wire formats (protobuf-like, varint-encoded):
//!
//! SignalMessage (serialized):
//!   version_byte (0x33) || protobuf(SignalMessage) || mac[0..8]
//!
//! PreKeySignalMessage (serialized):
//!   version_byte (0x33) || protobuf(PreKeySignalMessage)

/// Current Signal protocol version byte: current=3, max=3 → 0x33
const VERSION_BYTE: u8 = 0x33;

/// Strip 0x05 prefix from a public key if present (for internal 32-byte representation)
fn strip_key_prefix(key: Vec<u8>) -> Vec<u8> {
    if key.len() == 33 && key[0] == 0x05 {
        key[1..].to_vec()
    } else {
        key
    }
}

/// MAC length appended to SignalMessage (8 bytes)
pub(crate) const MAC_LENGTH: usize = 8;

/// An 8-byte truncated HMAC-SHA256 MAC.  Can only be constructed from exactly
/// MAC_LENGTH bytes, so `verify_mac` never receives a wrong-length slice.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mac([u8; MAC_LENGTH]);

impl Mac {
    /// Construct from a slice.  Returns `None` if the length is not MAC_LENGTH.
    pub fn from_bytes(b: &[u8]) -> Option<Self> {
        <[u8; MAC_LENGTH]>::try_from(b).ok().map(Mac)
    }
}

/// Proof that a `SignalMessage`'s MAC was verified against the derived mac_key.
/// No public constructor — only produced by `verify_and_authenticate`.
/// The ratchet decrypt step takes this type so decryption cannot precede MAC check.
pub(super) struct AuthenticatedSignalMessage(pub(super) SignalMessage);

/// MAC verification failed.
#[derive(Debug)]
pub(super) struct MacFailure;

/// Verify the Signal-protocol MAC and, if correct, return an authenticated wrapper.
/// `mac_key`, `mac_input`, and `msg.mac` must all be consistent with one call
/// site's construction of the MAC input (identity keys ‖ message bytes).
pub(super) fn verify_and_authenticate(
    mac_key: &[u8],
    mac_input: &[u8],
    msg: SignalMessage,
) -> Result<AuthenticatedSignalMessage, MacFailure> {
    let expected = match Mac::from_bytes(&msg.mac) {
        Some(m) => m,
        None => return Err(MacFailure),
    };
    if verify_mac(mac_key, mac_input, &expected) {
        Ok(AuthenticatedSignalMessage(msg))
    } else {
        Err(MacFailure)
    }
}

// Protobuf field tags (field_number << 3 | wire_type)
// Wire type 0 = varint, 2 = length-delimited
// Tag byte layout: [field_number (high 5 bits)] [wire_type (low 3 bits)]
mod signal_message_tags {
    /// Field 1: Ratchet public key (32-byte Curve25519 key, encoded with 0x05 prefix)
    pub const RATCHET_KEY: u8 = (1 << 3) | 2; // field 1, length-delimited
    /// Field 2: Ratchet counter (monotonically increasing message number)
    pub const COUNTER: u8 = 2 << 3; // field 2, varint
    /// Field 3: Previous ratchet counter (0 for first message in chain)
    pub const PREV_COUNTER: u8 = 3 << 3; // field 3, varint
    /// Field 4: AES-GCM encrypted ciphertext (typically 48 bytes)
    pub const CIPHERTEXT: u8 = (4 << 3) | 2; // field 4, length-delimited
}

mod prekey_message_tags {
    /// Field 1: One-time pre-key ID (optional, only present for first message)
    pub const PRE_KEY_ID: u8 = 1 << 3; // field 1, varint
    /// Field 2: ECDH base key (32-byte Curve25519 public key, encoded with 0x05 prefix)
    pub const BASE_KEY: u8 = (2 << 3) | 2; // field 2, length-delimited
    /// Field 3: Sender's identity key (32-byte Curve25519 public key, encoded with 0x05 prefix)
    pub const IDENTITY_KEY: u8 = (3 << 3) | 2; // field 3, length-delimited
    /// Field 4: Inner SignalMessage (pre-serialized with its own MAC)
    pub const MESSAGE: u8 = (4 << 3) | 2; // field 4, length-delimited
    /// Field 5: Device registration ID (random 16-bit value for key ratcheting)
    pub const REGISTRATION_ID: u8 = 5 << 3; // field 5, varint
    /// Field 6: Signed pre-key ID (used for initial key exchange)
    pub const SIGNED_PRE_KEY_ID: u8 = 6 << 3; // field 6, varint
}

/// A decoded SignalMessage
#[derive(Debug, Clone)]
pub struct SignalMessage {
    pub ratchet_key: Vec<u8>,
    pub counter: u32,
    pub previous_counter: u32,
    pub ciphertext: Vec<u8>,
    pub mac: Vec<u8>,
}

/// A decoded PreKeySignalMessage
#[derive(Debug, Clone)]
pub struct PreKeySignalMessage {
    pub registration_id: u32,
    pub pre_key_id: Option<u32>,
    pub signed_pre_key_id: u32,
    pub base_key: Vec<u8>,
    pub identity_key: Vec<u8>,
    pub message: SignalMessage,
    /// Raw bytes of the inner SignalMessage (for MAC verification and re-embedding)
    pub raw_message_bytes: Vec<u8>,
}

// --- Protobuf encoding helpers ---

fn encode_varint(value: u32) -> Vec<u8> {
    let mut buf = Vec::new();
    let mut v = value;
    loop {
        if v < 0x80 {
            buf.push(v as u8);
            break;
        }
        buf.push((v as u8 & 0x7F) | 0x80);
        v >>= 7;
    }
    buf
}

fn encode_field_varint(tag: u8, value: u32) -> Vec<u8> {
    let mut buf = vec![tag];
    buf.extend(encode_varint(value));
    buf
}

fn encode_field_bytes(tag: u8, data: &[u8]) -> Vec<u8> {
    let mut buf = vec![tag];
    buf.extend(encode_varint(data.len() as u32));
    buf.extend_from_slice(data);
    buf
}

// --- Protobuf decoding helpers ---

fn decode_varint(data: &[u8], offset: &mut usize) -> Option<u32> {
    let mut result: u32 = 0;
    let mut shift = 0;
    loop {
        if *offset >= data.len() {
            return None;
        }
        let byte = data[*offset];
        *offset += 1;
        if shift == 28 && byte & 0xf0 != 0 {
            return None;
        }
        result |= ((byte & 0x7F) as u32) << shift;
        if byte & 0x80 == 0 {
            break;
        }
        shift += 7;
        if shift >= 35 {
            return None; // overflow protection
        }
    }
    Some(result)
}

fn decode_bytes<'a>(data: &'a [u8], offset: &mut usize) -> Option<&'a [u8]> {
    let len = decode_varint(data, offset)? as usize;
    let end = offset.checked_add(len)?;
    if end > data.len() {
        return None;
    }
    let result = &data[*offset..*offset + len];
    *offset += len;
    Some(result)
}

// --- SignalMessage encoding/decoding ---

impl SignalMessage {
    /// Serialize a SignalMessage to the Signal wire format.
    /// Format: version_byte || protobuf || mac[0..8]
    ///
    /// The MAC is computed externally (using the sending/receiving chain MAC key).
    /// For OMEMO key transport, we use a truncated HMAC of the serialized protobuf.
    #[cfg(test)]
    pub fn serialize(&self, mac_key: &[u8]) -> Vec<u8> {
        let proto = self.encode_proto();
        let mut buf = Vec::with_capacity(1 + proto.len() + MAC_LENGTH);
        buf.push(VERSION_BYTE);
        buf.extend_from_slice(&proto);

        // Compute MAC over version + proto
        let mac = compute_mac(mac_key, &buf);
        buf.extend_from_slice(&mac[..MAC_LENGTH]);
        buf
    }

    /// Serialize with MAC including identity keys per Signal spec.
    /// MAC = HMAC-SHA256(mac_key, sender_identity || receiver_identity || version || protobuf)[..8]
    /// Identity keys are encoded with 0x05 prefix (33 bytes) per libsignal convention.
    pub fn serialize_with_identity(
        &self,
        mac_key: &[u8],
        sender_identity: &[u8],
        receiver_identity: &[u8],
    ) -> Vec<u8> {
        let proto = self.encode_proto();
        let mut buf = Vec::with_capacity(1 + proto.len() + MAC_LENGTH);
        buf.push(VERSION_BYTE);
        buf.extend_from_slice(&proto);

        // MAC input: sender_identity(33) || receiver_identity(33) || version || protobuf
        let sender_prefixed =
            crate::xmpp_omemo::omemo::crypto::encode_public_key_with_prefix(sender_identity);
        let receiver_prefixed =
            crate::xmpp_omemo::omemo::crypto::encode_public_key_with_prefix(receiver_identity);
        let mut mac_input =
            Vec::with_capacity(sender_prefixed.len() + receiver_prefixed.len() + buf.len());
        mac_input.extend_from_slice(&sender_prefixed);
        mac_input.extend_from_slice(&receiver_prefixed);
        mac_input.extend_from_slice(&buf);

        let mac = compute_mac(mac_key, &mac_input);
        buf.extend_from_slice(&mac[..MAC_LENGTH]);
        buf
    }

    fn encode_proto(&self) -> Vec<u8> {
        let mut proto = Vec::new();
        if !self.ratchet_key.is_empty() {
            // Encode ratchet_key with 0x05 prefix for libsignal interop
            let prefixed_key =
                crate::xmpp_omemo::omemo::crypto::encode_public_key_with_prefix(&self.ratchet_key);
            proto.extend(encode_field_bytes(
                signal_message_tags::RATCHET_KEY,
                &prefixed_key,
            ));
        }
        proto.extend(encode_field_varint(
            signal_message_tags::COUNTER,
            self.counter,
        ));
        if self.previous_counter > 0 {
            proto.extend(encode_field_varint(
                signal_message_tags::PREV_COUNTER,
                self.previous_counter,
            ));
        }
        if !self.ciphertext.is_empty() {
            proto.extend(encode_field_bytes(
                signal_message_tags::CIPHERTEXT,
                &self.ciphertext,
            ));
        }
        proto
    }

    /// Deserialize a SignalMessage from wire format.
    /// Returns None if the format is invalid.
    pub fn deserialize(data: &[u8]) -> Option<Self> {
        if data.len() < 1 + MAC_LENGTH || data.len() > 8192 {
            ::zeroclaw_log::record!(
                DEBUG,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Validate),
                format!("SignalMessage too short: {} bytes", data.len())
            );
            return None;
        }

        let version = data[0];
        if version != VERSION_BYTE {
            ::zeroclaw_log::record!(
                DEBUG,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Validate),
                format!("SignalMessage unexpected version byte: 0x{:02x}", version)
            );
            return None;
        }

        // Strip version byte and MAC
        let proto_end = data.len() - MAC_LENGTH;
        let proto = &data[1..proto_end];
        let mac = data[proto_end..].to_vec();

        let mut msg = Self::decode_proto(proto)?;
        msg.mac = mac;
        Some(msg)
    }

    fn decode_proto(data: &[u8]) -> Option<Self> {
        let mut offset = 0;
        let mut ratchet_key = Vec::new();
        let mut counter = 0u32;
        let mut previous_counter = 0u32;
        let mut ciphertext = Vec::new();
        let mut seen = 0u8;

        while offset < data.len() {
            let tag = data[offset];
            offset += 1;

            if matches!(
                tag,
                signal_message_tags::RATCHET_KEY
                    | signal_message_tags::COUNTER
                    | signal_message_tags::PREV_COUNTER
                    | signal_message_tags::CIPHERTEXT
            ) {
                let flag = 1u8 << (tag >> 3);
                if seen & flag != 0 {
                    return None;
                }
                seen |= flag;
            }

            match tag {
                signal_message_tags::RATCHET_KEY => {
                    ratchet_key = decode_bytes(data, &mut offset)?.to_vec();
                }
                signal_message_tags::COUNTER => {
                    counter = decode_varint(data, &mut offset)?;
                }
                signal_message_tags::PREV_COUNTER => {
                    previous_counter = decode_varint(data, &mut offset)?;
                }
                signal_message_tags::CIPHERTEXT => {
                    ciphertext = decode_bytes(data, &mut offset)?.to_vec();
                }
                _ => {
                    // Skip unknown fields
                    let wire_type = tag & 0x07;
                    match wire_type {
                        0 => {
                            decode_varint(data, &mut offset)?;
                        } // varint
                        1 => {
                            // 64-bit: skip 8 bytes
                            if offset + 8 > data.len() {
                                return None;
                            }
                            offset += 8;
                        }
                        2 => {
                            decode_bytes(data, &mut offset)?;
                        } // length-delimited
                        5 => {
                            // 32-bit (fixed32): skip 4 bytes
                            if offset + 4 > data.len() {
                                return None;
                            }
                            offset += 4;
                        }
                        _ => {
                            ::zeroclaw_log::record!(
                                WARN,
                                ::zeroclaw_log::Event::new(
                                    module_path!(),
                                    ::zeroclaw_log::Action::Validate
                                ),
                                format!("Unknown wire type {wire_type} in SignalMessage")
                            );
                            return None;
                        }
                    }
                }
            }
        }

        if seen & 0b10110 != 0b10110
            || ratchet_key.len() != 33
            || ratchet_key[0] != 5
            || ciphertext.is_empty()
            || ciphertext.len() % 16 != 0
        {
            return None;
        }
        Some(SignalMessage {
            ratchet_key: strip_key_prefix(ratchet_key),
            counter,
            previous_counter,
            ciphertext,
            mac: Vec::new(),
        })
    }
}

// --- PreKeySignalMessage encoding/decoding ---

impl PreKeySignalMessage {
    /// Serialize a PreKeySignalMessage to the Signal wire format.
    /// Format: version_byte || protobuf
    /// The inner SignalMessage is embedded as raw pre-serialized bytes (with its own MAC).
    pub fn serialize_with_inner_bytes(&self, inner_signal_msg_bytes: &[u8]) -> Vec<u8> {
        let mut proto = Vec::new();
        proto.extend(encode_field_varint(
            prekey_message_tags::REGISTRATION_ID,
            self.registration_id,
        ));
        if let Some(pre_key_id) = self.pre_key_id {
            proto.extend(encode_field_varint(
                prekey_message_tags::PRE_KEY_ID,
                pre_key_id,
            ));
        }
        proto.extend(encode_field_varint(
            prekey_message_tags::SIGNED_PRE_KEY_ID,
            self.signed_pre_key_id,
        ));
        // Encode base_key and identity_key with 0x05 prefix for libsignal interop
        let prefixed_base_key =
            crate::xmpp_omemo::omemo::crypto::encode_public_key_with_prefix(&self.base_key);
        proto.extend(encode_field_bytes(
            prekey_message_tags::BASE_KEY,
            &prefixed_base_key,
        ));
        let prefixed_identity_key =
            crate::xmpp_omemo::omemo::crypto::encode_public_key_with_prefix(&self.identity_key);
        proto.extend(encode_field_bytes(
            prekey_message_tags::IDENTITY_KEY,
            &prefixed_identity_key,
        ));
        proto.extend(encode_field_bytes(
            prekey_message_tags::MESSAGE,
            inner_signal_msg_bytes,
        ));

        let mut buf = Vec::with_capacity(1 + proto.len());
        buf.push(VERSION_BYTE);
        buf.extend(proto);
        buf
    }

    /// Legacy serialize that re-serializes inner message (kept for compatibility)
    #[cfg(test)]
    pub fn serialize(&self, mac_key: &[u8]) -> Vec<u8> {
        let inner_serialized = self.message.serialize(mac_key);
        self.serialize_with_inner_bytes(&inner_serialized)
    }

    /// Deserialize a PreKeySignalMessage from wire format.
    pub fn deserialize(data: &[u8]) -> Option<Self> {
        if data.len() < 2 || data.len() > 8192 {
            ::zeroclaw_log::record!(
                DEBUG,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Validate),
                format!("PreKeySignalMessage too short: {} bytes", data.len())
            );
            return None;
        }

        let version = data[0];
        if version != VERSION_BYTE {
            ::zeroclaw_log::record!(
                DEBUG,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Validate),
                format!(
                    "PreKeySignalMessage unexpected version byte: 0x{:02x}",
                    version
                )
            );
            return None;
        }

        let proto = &data[1..];
        Self::decode_proto(proto)
    }

    fn decode_proto(data: &[u8]) -> Option<Self> {
        let mut offset = 0;
        let mut registration_id = 0u32;
        let mut pre_key_id: Option<u32> = None;
        let mut signed_pre_key_id = 0u32;
        let mut base_key = Vec::new();
        let mut identity_key = Vec::new();
        let mut message_bytes = Vec::new();
        let mut seen = 0u8;

        while offset < data.len() {
            let tag = data[offset];
            offset += 1;

            if matches!(
                tag,
                prekey_message_tags::REGISTRATION_ID
                    | prekey_message_tags::PRE_KEY_ID
                    | prekey_message_tags::SIGNED_PRE_KEY_ID
                    | prekey_message_tags::BASE_KEY
                    | prekey_message_tags::IDENTITY_KEY
                    | prekey_message_tags::MESSAGE
            ) {
                let flag = 1u8 << (tag >> 3);
                if seen & flag != 0 {
                    return None;
                }
                seen |= flag;
            }

            match tag {
                prekey_message_tags::REGISTRATION_ID => {
                    registration_id = decode_varint(data, &mut offset)?;
                }
                prekey_message_tags::PRE_KEY_ID => {
                    pre_key_id = Some(decode_varint(data, &mut offset)?);
                }
                prekey_message_tags::SIGNED_PRE_KEY_ID => {
                    signed_pre_key_id = decode_varint(data, &mut offset)?;
                }
                prekey_message_tags::BASE_KEY => {
                    base_key = decode_bytes(data, &mut offset)?.to_vec();
                }
                prekey_message_tags::IDENTITY_KEY => {
                    identity_key = decode_bytes(data, &mut offset)?.to_vec();
                }
                prekey_message_tags::MESSAGE => {
                    message_bytes = decode_bytes(data, &mut offset)?.to_vec();
                }
                _ => {
                    let wire_type = tag & 0x07;
                    match wire_type {
                        0 => {
                            decode_varint(data, &mut offset)?;
                        }
                        1 => {
                            // 64-bit: skip 8 bytes
                            if offset + 8 > data.len() {
                                return None;
                            }
                            offset += 8;
                        }
                        2 => {
                            decode_bytes(data, &mut offset)?;
                        }
                        5 => {
                            // 32-bit (fixed32): skip 4 bytes
                            if offset + 4 > data.len() {
                                return None;
                            }
                            offset += 4;
                        }
                        _ => {
                            ::zeroclaw_log::record!(
                                WARN,
                                ::zeroclaw_log::Event::new(
                                    module_path!(),
                                    ::zeroclaw_log::Action::Validate
                                ),
                                format!("Unknown wire type {wire_type} in PreKeySignalMessage")
                            );
                            return None;
                        }
                    }
                }
            }
        }

        if message_bytes.is_empty()
            || seen & 0b1011100 != 0b1011100
            || base_key.len() != 33
            || base_key[0] != 5
            || identity_key.len() != 33
            || identity_key[0] != 5
        {
            ::zeroclaw_log::record!(
                DEBUG,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Validate),
                "PreKeySignalMessage missing inner message"
            );
            return None;
        }

        let message = SignalMessage::deserialize(&message_bytes)?;

        Some(PreKeySignalMessage {
            registration_id,
            pre_key_id,
            signed_pre_key_id,
            base_key: strip_key_prefix(base_key),
            identity_key: strip_key_prefix(identity_key),
            message,
            raw_message_bytes: message_bytes,
        })
    }
}

/// Compute an HMAC-SHA256 and return the full 32-byte MAC
fn compute_mac(key: &[u8], data: &[u8]) -> Vec<u8> {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;

    let mut mac = <Hmac<Sha256>>::new_from_slice(key).expect("HMAC key length is always valid");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

/// Verify an 8-byte truncated HMAC-SHA256.  The `expected` type guarantees the
/// slice is exactly MAC_LENGTH bytes — no runtime length check needed.
pub fn verify_mac(key: &[u8], data: &[u8], expected: &Mac) -> bool {
    let computed = compute_mac(key, data);
    let mut result = 0u8;
    for (a, b) in computed[..MAC_LENGTH].iter().zip(expected.0.iter()) {
        result |= a ^ b;
    }
    result == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_signal_message_roundtrip() {
        let msg = SignalMessage {
            ratchet_key: vec![1u8; 32],
            counter: 42,
            previous_counter: 10,
            ciphertext: vec![0xAB; 48],
            mac: Vec::new(),
        };

        let mac_key = vec![0x55u8; 32];
        let serialized = msg.serialize(&mac_key);
        let deserialized = SignalMessage::deserialize(&serialized).unwrap();

        assert_eq!(deserialized.ratchet_key, msg.ratchet_key);
        assert_eq!(deserialized.counter, msg.counter);
        assert_eq!(deserialized.previous_counter, msg.previous_counter);
        assert_eq!(deserialized.ciphertext, msg.ciphertext);
        assert_eq!(deserialized.mac.len(), MAC_LENGTH);
    }

    #[test]
    fn test_prekey_message_roundtrip() {
        let inner = SignalMessage {
            ratchet_key: vec![2u8; 32],
            counter: 0,
            previous_counter: 0,
            ciphertext: vec![0xCD; 48],
            mac: Vec::new(),
        };

        let prekey_msg = PreKeySignalMessage {
            registration_id: 12345,
            pre_key_id: Some(7),
            signed_pre_key_id: 1,
            base_key: vec![3u8; 32],
            identity_key: vec![4u8; 32],
            message: inner,
            raw_message_bytes: vec![],
        };

        let mac_key = vec![0x66u8; 32];
        let serialized = prekey_msg.serialize(&mac_key);
        let deserialized = PreKeySignalMessage::deserialize(&serialized).unwrap();

        assert_eq!(deserialized.registration_id, prekey_msg.registration_id);
        assert_eq!(deserialized.pre_key_id, prekey_msg.pre_key_id);
        assert_eq!(deserialized.signed_pre_key_id, prekey_msg.signed_pre_key_id);
        assert_eq!(deserialized.base_key, prekey_msg.base_key);
        assert_eq!(deserialized.identity_key, prekey_msg.identity_key);
        assert_eq!(deserialized.message.ratchet_key, vec![2u8; 32]);
        assert_eq!(deserialized.message.counter, 0);
        assert_eq!(deserialized.message.ciphertext, vec![0xCD; 48]);
    }

    #[test]
    fn test_varint_encoding() {
        // Single byte
        assert_eq!(encode_varint(0), vec![0]);
        assert_eq!(encode_varint(1), vec![1]);
        assert_eq!(encode_varint(127), vec![127]);
        // Two bytes
        assert_eq!(encode_varint(128), vec![0x80, 0x01]);
        assert_eq!(encode_varint(300), vec![0xAC, 0x02]);
    }

    #[test]
    fn test_verify_mac_empty_mac_rejected() {
        // Mac::from_bytes rejects wrong lengths at construction — an empty MAC
        // can never reach verify_mac, making the C3 bug unconstructible.
        assert!(
            Mac::from_bytes(&[]).is_none(),
            "empty slice must not produce a Mac"
        );
        assert!(
            Mac::from_bytes(&[0u8; 1]).is_none(),
            "1-byte slice must not produce a Mac"
        );
        assert!(
            Mac::from_bytes(&[0u8; MAC_LENGTH]).is_some(),
            "correct length must succeed"
        );
    }

    #[test]
    fn test_verify_mac_length_enforcement() {
        let key = vec![0x22u8; 32];
        let data = b"some data";

        let full_mac = compute_mac(&key, data);

        // 32-byte full HMAC-SHA256 must not construct a Mac (MAC_LENGTH is 8).
        assert!(Mac::from_bytes(&full_mac).is_none());

        // Correct 8-byte truncated MAC must be accepted.
        let mac = Mac::from_bytes(&full_mac[..MAC_LENGTH]).unwrap();
        assert!(verify_mac(&key, data, &mac));

        // Tampered MAC must be rejected.
        let mut tampered_bytes = full_mac[..MAC_LENGTH].to_vec();
        tampered_bytes[0] ^= 0xFF;
        let tampered = Mac::from_bytes(&tampered_bytes).unwrap();
        assert!(!verify_mac(&key, data, &tampered));
    }
}

#[cfg(test)]
mod boundary_tests {
    use super::*;
    #[test]
    fn rejects_unknown_versions_duplicate_and_missing_fields() {
        let signal = SignalMessage {
            ratchet_key: vec![7; 32],
            counter: 0,
            previous_counter: 0,
            ciphertext: vec![8; 48],
            mac: vec![],
        };
        let valid = signal.serialize(&[9; 32]);
        assert!(SignalMessage::deserialize(&valid).is_some());
        let mut unknown = valid.clone();
        unknown[0] = 0x34;
        assert!(SignalMessage::deserialize(&unknown).is_none());
        let mut duplicate = valid.clone();
        let pos = duplicate.len() - MAC_LENGTH;
        duplicate.splice(pos..pos, [0x10, 0]);
        assert!(SignalMessage::deserialize(&duplicate).is_none());
        let missing = [
            vec![0x33],
            encode_field_bytes(
                signal_message_tags::RATCHET_KEY,
                &[vec![5], vec![7; 32]].concat(),
            ),
            vec![0; MAC_LENGTH],
        ]
        .concat();
        assert!(SignalMessage::deserialize(&missing).is_none());
        let prekey = PreKeySignalMessage {
            registration_id: 1,
            pre_key_id: Some(3),
            signed_pre_key_id: 4,
            base_key: vec![6; 32],
            identity_key: vec![7; 32],
            message: signal,
            raw_message_bytes: valid.clone(),
        };
        let mut bytes = prekey.serialize_with_inner_bytes(&valid);
        assert!(PreKeySignalMessage::deserialize(&bytes).is_some());
        bytes[0] = 0x34;
        assert!(PreKeySignalMessage::deserialize(&bytes).is_none());
        bytes[0] = 0x33;
        bytes.extend_from_slice(&[0x30, 4]);
        assert!(PreKeySignalMessage::deserialize(&bytes).is_none());
    }
    #[test]
    fn rejects_overflowing_and_truncated_varints() {
        for bytes in [
            vec![0xff, 0xff, 0xff, 0xff, 0x7f],
            vec![0x80; 5],
            vec![0x80],
        ] {
            assert!(decode_varint(&bytes, &mut 0).is_none());
        }
        assert_eq!(
            decode_varint(&[0xff, 0xff, 0xff, 0xff, 0x0f], &mut 0),
            Some(u32::MAX)
        );
        assert!(decode_bytes(&[0xff, 0xff, 0xff, 0xff, 0x0f], &mut 0).is_none());
    }
    #[test]
    fn malformed_wire_inputs_do_not_panic() {
        use rand::{Rng, SeedableRng};
        let mut rng = rand::rngs::StdRng::seed_from_u64(0x384);
        for length in 0..512 {
            for _ in 0..16 {
                let mut bytes = vec![0; length];
                rng.fill_bytes(&mut bytes);
                if length > 0 {
                    bytes[0] = 0x33;
                }
                assert!(
                    std::panic::catch_unwind(|| {
                        let _ = SignalMessage::deserialize(&bytes);
                        let _ = PreKeySignalMessage::deserialize(&bytes);
                    })
                    .is_ok()
                );
            }
        }
    }
}
