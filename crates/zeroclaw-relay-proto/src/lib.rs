//! Wire protocol `zeroclaw.relay.v1` for the ZeroClaw nominated relay.
//!
//! The relay is a **blind forwarder**: it speaks an outer WebSocket session with
//! each party (daemon and client) and pipes the *inner* client<->daemon mTLS
//! bytes between them without ever decrypting them. This crate is the single
//! shared definition of that outer protocol, consumed by `apps/zerorelay`, the
//! daemon-side bridge in `zeroclaw-runtime`, and the client in `apps/zerocode`.
//!
//! # Two message kinds on the wire
//!
//! Each outer WebSocket carries two kinds of message:
//!
//! - **Control** ([`Control`]) - a WS *Text* message holding one JSON frame.
//!   Registration, admission, connection setup/teardown, and errors.
//! - **Data** - a WS *Binary* message: an 8-byte big-endian `conn_id` followed
//!   by opaque payload bytes (the inner mTLS stream). Encoded/decoded with
//!   [`encode_data`] / [`decode_data`]. The relay never inspects the payload.
//!
//! # Connection roles
//!
//! - A **daemon** opens one persistent WS and registers a `node_id` via the
//!   signed handshake (`Hello` -> `Challenge` -> `Register` -> `Registered`).
//!   Many client connections are then multiplexed over that single WS by
//!   `conn_id` (`Open` / `Opened` / `Close` + binary `DATA`).
//! - A **client** opens a WS, names a target `node_id` with `Connect`, and once
//!   the relay replies `Opened` exchanges binary `DATA` for that one `conn_id`.
//!
//! This crate is a serde-only leaf with no runtime/crypto dependencies: the
//! `daemon_pubkey` / `sig` / `nonce` fields carry base64 text, and the relay and
//! daemon perform the Ed25519 signing/verification. This keeps the wire
//! definition drift-free and the crate trivially vendorable.

use serde::{Deserialize, Serialize};

/// The WebSocket subprotocol identifier offered/selected on every relay session.
pub const SUBPROTOCOL: &str = "zeroclaw.relay.v1";

/// Maximum control-frame size (JSON text) a peer should accept. Control frames
/// are tiny; anything larger is hostile or a bug.
pub const MAX_CONTROL_FRAME: usize = 64 * 1024;

/// Maximum inner payload carried in a single binary `DATA` message. Larger inner
/// writes are chunked across multiple `DATA` messages so one connection cannot
/// monopolize a multiplexed daemon link (head-of-line mitigation).
pub const MAX_DATA_PAYLOAD: usize = 64 * 1024;

/// A control frame, serialized as a single JSON object in a WS Text message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Control {
    /// Daemon -> relay: announce the Ed25519 registration pubkey (base64) and the
    /// `node_id` to claim. `relay_token` is an optional shared-secret gate for
    /// open/metered relays; admission is keyed primarily on the pubkey.
    Hello {
        /// Base64 (standard) of the 32-byte Ed25519 public key.
        daemon_pubkey: String,
        node_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        relay_token: Option<String>,
    },
    /// Relay -> daemon: a fresh anti-replay challenge (base64 of random bytes).
    Challenge { nonce: String },
    /// Daemon -> relay: `sig` is base64 of `sign(nonce_bytes, daemon_privkey)`,
    /// proving possession of the private key for the announced pubkey.
    Register { node_id: String, sig: String },
    /// Relay -> daemon: registration accepted; the daemon must renew (a fresh
    /// handshake) before `lease_ttl_secs` elapses or the node-id is released.
    Registered {
        node_id: String,
        lease_ttl_secs: u64,
    },

    /// Client -> relay: request a route to `node_id`.
    Connect { node_id: String },

    /// Relay -> daemon: a new client arrived; open a logical connection.
    /// `peer_hint` is optional coarse, untrusted metadata (never authoritative).
    Open {
        conn_id: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        peer_hint: Option<String>,
    },
    /// Daemon -> relay (accept an `Open`) and relay -> client (route is ready).
    Opened { conn_id: u64 },
    /// Either side: tear down one logical connection.
    Close {
        conn_id: u64,
        #[serde(default)]
        reason: String,
    },

    /// Either side: a terminal error (e.g. `forbidden`, `node_taken`, `bad_sig`,
    /// `no_such_node`, `rate_limited`, `busy`).
    Error { code: String, msg: String },
}

impl Control {
    /// Encode as a single JSON line for a WS Text message.
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("relay control frame serializes")
    }

    /// Parse one control frame from a WS Text message (trailing newline tolerated).
    pub fn from_json(s: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(s.trim_end_matches(['\n', '\r']))
    }

    /// Convenience constructor for an error frame.
    pub fn error(code: &str, msg: impl Into<String>) -> Self {
        Control::Error {
            code: code.to_string(),
            msg: msg.into(),
        }
    }
}

/// Encode a binary `DATA` message: 8-byte big-endian `conn_id` then payload.
pub fn encode_data(conn_id: u64, payload: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(8 + payload.len());
    v.extend_from_slice(&conn_id.to_be_bytes());
    v.extend_from_slice(payload);
    v
}

/// Decode a binary `DATA` message into `(conn_id, payload)`. Returns `None` if the
/// message is too short to carry a `conn_id` header.
pub fn decode_data(bytes: &[u8]) -> Option<(u64, &[u8])> {
    if bytes.len() < 8 {
        return None;
    }
    let conn_id = u64::from_be_bytes(bytes[..8].try_into().expect("checked len >= 8"));
    Some((conn_id, &bytes[8..]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_frames_round_trip() {
        let cases = [
            Control::Hello {
                daemon_pubkey: "cHVic2V5".into(),
                node_id: "n1".into(),
                relay_token: Some("tok".into()),
            },
            Control::Hello {
                daemon_pubkey: "cHVic2V5".into(),
                node_id: "n1".into(),
                relay_token: None,
            },
            Control::Challenge {
                nonce: "bm9uY2U=".into(),
            },
            Control::Register {
                node_id: "n1".into(),
                sig: "c2ln".into(),
            },
            Control::Registered {
                node_id: "n1".into(),
                lease_ttl_secs: 120,
            },
            Control::Connect {
                node_id: "n1".into(),
            },
            Control::Open {
                conn_id: 7,
                peer_hint: Some("eu".into()),
            },
            Control::Open {
                conn_id: 7,
                peer_hint: None,
            },
            Control::Opened { conn_id: 7 },
            Control::Close {
                conn_id: 7,
                reason: "client_gone".into(),
            },
            Control::error("forbidden", "registration denied"),
        ];
        for c in cases {
            let line = c.to_json();
            let back = Control::from_json(&line).expect("round-trips");
            assert_eq!(c, back, "frame did not round-trip: {line}");
        }
    }

    #[test]
    fn hello_omits_none_token() {
        let line = Control::Hello {
            daemon_pubkey: "k".into(),
            node_id: "n".into(),
            relay_token: None,
        }
        .to_json();
        assert!(
            !line.contains("relay_token"),
            "None token must be omitted: {line}"
        );
    }

    #[test]
    fn data_round_trips() {
        let payload = b"the inner mTLS ciphertext bytes";
        let framed = encode_data(0x0102_0304_0506_0708, payload);
        assert_eq!(framed.len(), 8 + payload.len());
        let (conn_id, back) = decode_data(&framed).expect("decodes");
        assert_eq!(conn_id, 0x0102_0304_0506_0708);
        assert_eq!(back, payload);
    }

    #[test]
    fn data_allows_empty_payload() {
        let framed = encode_data(42, &[]);
        let (conn_id, back) = decode_data(&framed).expect("decodes");
        assert_eq!(conn_id, 42);
        assert!(back.is_empty());
    }

    #[test]
    fn decode_data_rejects_short_header() {
        assert!(decode_data(&[]).is_none());
        assert!(decode_data(&[0u8; 7]).is_none());
        assert!(decode_data(&[0u8; 8]).is_some());
    }

    #[test]
    fn from_json_rejects_garbage_without_panicking() {
        for bad in [
            "",
            "{",
            "null",
            "{\"t\":\"nope\"}",
            "[]",
            "\u{0000}",
            "{\"t\":\"open\"}",
        ] {
            // Must return Err, never panic (parser-hostility smoke).
            let _ = Control::from_json(bad);
        }
        assert!(Control::from_json("not json").is_err());
        // Known tag but missing required field is a parse error, not a panic.
        assert!(Control::from_json("{\"t\":\"open\"}").is_err());
    }
}
