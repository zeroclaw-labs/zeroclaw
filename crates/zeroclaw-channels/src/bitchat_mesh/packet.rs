//! BitChat binary wire format — encode/decode for Rust.
//!
//! Wire-compatible with `BitchatPacket.swift` in
//! `localPackages/BitFoundation/Sources/BitFoundation/`.
//!
//! ## Header layout (fixed, 29 bytes when recipient present, 21 without)
//!
//! ```text
//! Offset  Len  Field
//! ──────  ───  ───────────────────────────────
//!  0       1   version         (u8, always 1)
//!  1       1   packet_type     (u8)
//!  2       1   ttl             (u8, max 7)
//!  3       8   timestamp_ms    (u64 big-endian)
//!  11      1   flags           (u8)
//!  12      2   payload_length  (u16 big-endian, PKCS#7-padded size)
//!  14      8   sender_id       ([u8;8])
//!  22      1   has_recipient   (0x01 or 0x00)
//!  23      8   recipient_id    ([u8;8], only when has_recipient == 0x01)
//! ────────────────────────────────────────────
//!  …       N   payload         (PKCS#7 padded)
//!  …      64   signature       (optional Ed25519, present when flags bit 0 set)
//! ```

use anyhow::{Context, Result, bail};

// ─── UUIDs (must match BitChat Swift exactly) ────────────────────────────────

/// GATT service UUID — advertised by every BitChat device.
pub const BITCHAT_SERVICE_UUID: &str = "F47B5E2D-4A9E-4C5A-9B3F-8E1D2C3A4B5C";

/// GATT characteristic UUID — used for all read/write/notify operations.
pub const BITCHAT_CHAR_UUID: &str = "A1B2C3D4-E5F6-4A5B-8C9D-0E1F2A3B4C5D";

// ─── Constants ───────────────────────────────────────────────────────────────

/// Maximum hop count before a packet is dropped.
pub const MAX_TTL: u8 = 7;

/// Default BLE MTU / TCP fragment size.
pub const DEFAULT_MTU: usize = 512;

/// Length of a peer identifier in bytes (truncated SHA-256 of Noise pubkey).
pub const PEER_ID_LEN: usize = 8;

/// Flag bit: packet carries an Ed25519 signature.
const FLAG_SIGNED: u8 = 0x01;

// ─── PacketType ──────────────────────────────────────────────────────────────

/// Wire type discriminants matching BitChat's Swift enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PacketType {
    /// Plain text (or Noise-encrypted) message.
    Message = 0x01,
    /// Peer announce / heartbeat.
    Announce = 0x02,
    /// Delivery acknowledgement.
    Ack = 0x03,
    /// Noise XX — first handshake message (`e →`).
    NoiseHandshakeInit = 0x10,
    /// Noise XX — second handshake message (`e, ee, s, es →`).
    NoiseHandshakeResp = 0x11,
    /// AetherNet agent advertisement (JSON payload).
    AgentAdvertisement = 0x20,
}

impl TryFrom<u8> for PacketType {
    type Error = anyhow::Error;

    fn try_from(value: u8) -> Result<Self> {
        match value {
            0x01 => Ok(Self::Message),
            0x02 => Ok(Self::Announce),
            0x03 => Ok(Self::Ack),
            0x10 => Ok(Self::NoiseHandshakeInit),
            0x11 => Ok(Self::NoiseHandshakeResp),
            0x20 => Ok(Self::AgentAdvertisement),
            other => bail!("Unknown BitChat packet type: 0x{other:02x}"),
        }
    }
}

// ─── BitchatPacket ────────────────────────────────────────────────────────────

/// A single BitChat wire packet.
///
/// Created via the constructor helpers or [`decode`]. Encoded to bytes via
/// [`encode`].  The PKCS#7 padding is applied automatically on encode and
/// stripped on decode.
#[derive(Debug, Clone)]
pub struct BitchatPacket {
    pub version: u8,
    pub packet_type: PacketType,
    /// Hop count remaining. Packets with TTL 0 are not forwarded.
    pub ttl: u8,
    /// Unix milliseconds.
    pub timestamp_ms: u64,
    /// 8-byte local peer ID (truncated SHA-256 of Noise static public key).
    pub sender_id: [u8; PEER_ID_LEN],
    /// `None` = broadcast; `Some(id)` = unicast to a specific peer.
    pub recipient_id: Option<[u8; PEER_ID_LEN]>,
    /// Message payload (plaintext or Noise ciphertext).
    pub payload: Vec<u8>,
    /// Optional 64-byte Ed25519 signature (when `FLAG_SIGNED` flag is set).
    pub signature: Option<[u8; 64]>,
}

impl BitchatPacket {
    // ── Constructors ─────────────────────────────────────────────────────────

    /// Create a new [`PacketType::Message`] packet.
    pub fn new_message(
        sender_id: [u8; PEER_ID_LEN],
        recipient_id: Option<[u8; PEER_ID_LEN]>,
        payload: Vec<u8>,
    ) -> Self {
        Self {
            version: 1,
            packet_type: PacketType::Message,
            ttl: MAX_TTL,
            timestamp_ms: now_ms(),
            sender_id,
            recipient_id,
            payload,
            signature: None,
        }
    }

    /// Create a new [`PacketType::AgentAdvertisement`] packet with JSON payload.
    pub fn new_agent_advertisement(sender_id: [u8; PEER_ID_LEN], json: &str) -> Self {
        Self {
            version: 1,
            packet_type: PacketType::AgentAdvertisement,
            ttl: MAX_TTL,
            timestamp_ms: now_ms(),
            sender_id,
            recipient_id: None,
            payload: json.as_bytes().to_vec(),
            signature: None,
        }
    }

    // ── Relay ────────────────────────────────────────────────────────────────

    /// Decrements TTL and returns `true` if the packet should be relayed.
    ///
    /// A packet with TTL 0 on arrival must not be relayed further.
    pub fn try_relay(&mut self) -> bool {
        if self.ttl == 0 {
            return false;
        }
        self.ttl -= 1;
        true
    }

    // ── Helpers ──────────────────────────────────────────────────────────────

    /// Returns the payload as a UTF-8 string slice (for text messages).
    pub fn text_content(&self) -> Option<&str> {
        std::str::from_utf8(&self.payload).ok()
    }

    // ── Encode ───────────────────────────────────────────────────────────────

    /// Encodes the packet to bytes using the BitChat binary wire format.
    ///
    /// The payload is PKCS#7-padded to the next power-of-two boundary
    /// (256 / 512 / 1024 / 2048 bytes) to obscure payload length.
    pub fn encode(&self) -> Vec<u8> {
        let padded_payload = pkcs7_pad(&self.payload);
        let flags: u8 = if self.signature.is_some() { FLAG_SIGNED } else { 0 };
        let has_recipient = self.recipient_id.is_some();

        let mut buf = Vec::with_capacity(
            23 + if has_recipient { 8 } else { 0 } + padded_payload.len() + 64,
        );

        buf.push(self.version);
        buf.push(self.packet_type as u8);
        buf.push(self.ttl);
        buf.extend_from_slice(&self.timestamp_ms.to_be_bytes());
        buf.push(flags);
        buf.extend_from_slice(&(padded_payload.len() as u16).to_be_bytes());
        buf.extend_from_slice(&self.sender_id);
        buf.push(u8::from(has_recipient));

        if let Some(rid) = &self.recipient_id {
            buf.extend_from_slice(rid);
        }

        buf.extend_from_slice(&padded_payload);

        if let Some(sig) = &self.signature {
            buf.extend_from_slice(sig);
        }

        buf
    }

    // ── Decode ───────────────────────────────────────────────────────────────

    /// Decodes a BitChat packet from raw bytes.
    ///
    /// Strips PKCS#7 padding from the payload before returning.
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < 23 {
            bail!(
                "BitchatPacket: too short ({} bytes, minimum 23)",
                bytes.len()
            );
        }

        let version = bytes[0];
        let packet_type =
            PacketType::try_from(bytes[1]).context("BitchatPacket: unknown type")?;
        let ttl = bytes[2];

        let timestamp_ms = u64::from_be_bytes(
            bytes[3..11]
                .try_into()
                .context("BitchatPacket: timestamp slice")?,
        );

        let flags = bytes[11];
        let payload_length = u16::from_be_bytes(
            bytes[12..14]
                .try_into()
                .context("BitchatPacket: payload_length slice")?,
        ) as usize;

        let sender_id: [u8; PEER_ID_LEN] = bytes[14..22]
            .try_into()
            .context("BitchatPacket: sender_id slice")?;

        let has_recipient = bytes[22] != 0;
        let mut cursor = 23;

        let recipient_id = if has_recipient {
            if bytes.len() < cursor + PEER_ID_LEN {
                bail!("BitchatPacket: truncated recipient_id");
            }
            let rid: [u8; PEER_ID_LEN] = bytes[cursor..cursor + PEER_ID_LEN]
                .try_into()
                .context("BitchatPacket: recipient_id slice")?;
            cursor += PEER_ID_LEN;
            Some(rid)
        } else {
            None
        };

        if bytes.len() < cursor + payload_length {
            bail!(
                "BitchatPacket: payload underflow (need {}, have {})",
                payload_length,
                bytes.len() - cursor
            );
        }

        let raw_payload = &bytes[cursor..cursor + payload_length];
        let payload = pkcs7_unpad(raw_payload).unwrap_or_else(|| raw_payload.to_vec());
        cursor += payload_length;

        let signature = if flags & FLAG_SIGNED != 0 {
            if bytes.len() < cursor + 64 {
                bail!("BitchatPacket: signature flag set but too short");
            }
            let mut sig = [0u8; 64];
            sig.copy_from_slice(&bytes[cursor..cursor + 64]);
            Some(sig)
        } else {
            None
        };

        Ok(Self {
            version,
            packet_type,
            ttl,
            timestamp_ms,
            sender_id,
            recipient_id,
            payload,
            signature,
        })
    }
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

/// Derive an 8-byte peer ID from the first 8 bytes of a Noise static public key.
pub fn peer_id_from_noise_key(noise_pubkey: &[u8; 32]) -> [u8; PEER_ID_LEN] {
    noise_pubkey[..PEER_ID_LEN].try_into().unwrap()
}

/// PKCS#7 pad `data` to the next of 256, 512, 1024, or 2048.
fn pkcs7_pad(data: &[u8]) -> Vec<u8> {
    let target = [256usize, 512, 1024, 2048]
        .into_iter()
        .find(|&t| t >= data.len() + 1)
        .unwrap_or(data.len() + 1);
    let pad_byte = (target - data.len()) as u8;
    let mut out = data.to_vec();
    out.resize(target, pad_byte);
    out
}

/// Strip PKCS#7 padding, returning `None` if padding is invalid.
fn pkcs7_unpad(data: &[u8]) -> Option<Vec<u8>> {
    if data.is_empty() {
        return None;
    }
    let pad_len = *data.last()? as usize;
    if pad_len == 0 || pad_len > data.len() {
        return None;
    }
    if data[data.len() - pad_len..].iter().all(|&b| b == pad_len as u8) {
        Some(data[..data.len() - pad_len].to_vec())
    } else {
        None
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_message() {
        let sender = [0xDE, 0xAD, 0xBE, 0xEF, 0x01, 0x02, 0x03, 0x04];
        let recipient = [0xCA, 0xFE, 0xBA, 0xBE, 0x05, 0x06, 0x07, 0x08];
        let original =
            BitchatPacket::new_message(sender, Some(recipient), b"Hello AetherNet!".to_vec());

        let encoded = original.encode();
        let decoded = BitchatPacket::decode(&encoded).expect("decode");

        assert_eq!(decoded.sender_id, sender);
        assert_eq!(decoded.recipient_id, Some(recipient));
        assert_eq!(decoded.payload, b"Hello AetherNet!");
        assert_eq!(decoded.packet_type as u8, PacketType::Message as u8);
    }

    #[test]
    fn round_trip_broadcast() {
        let sender = [0x01; PEER_ID_LEN];
        let pkt = BitchatPacket::new_message(sender, None, b"broadcast".to_vec());
        let decoded = BitchatPacket::decode(&pkt.encode()).unwrap();
        assert!(decoded.recipient_id.is_none());
        assert_eq!(decoded.payload, b"broadcast");
    }

    #[test]
    fn try_relay_decrements_ttl() {
        let mut pkt = BitchatPacket::new_message([0; 8], None, vec![]);
        assert_eq!(pkt.ttl, MAX_TTL);
        assert!(pkt.try_relay());
        assert_eq!(pkt.ttl, MAX_TTL - 1);

        pkt.ttl = 0;
        assert!(!pkt.try_relay());
    }

    #[test]
    fn peer_id_from_noise_key_first_8_bytes() {
        let key: [u8; 32] = std::array::from_fn(|i| i as u8);
        let id = peer_id_from_noise_key(&key);
        assert_eq!(id, [0, 1, 2, 3, 4, 5, 6, 7]);
    }
}
