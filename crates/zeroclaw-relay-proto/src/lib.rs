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

    /// Receiver -> sender: (re)establish the absolute send window for one logical
    /// connection (credit-based flow control). The sender may have at most
    /// `credit` unacknowledged bytes in flight on this `conn_id` before it pauses,
    /// so no single conn can monopolize the multiplexed daemon link (head-of-line
    /// mitigation) or pin unbounded memory (A6). Sent once when the conn opens.
    Window { conn_id: u64, credit: u32 },
    /// Receiver -> sender: `consumed` bytes have been drained to the inner stream
    /// on this `conn_id`; the sender replenishes its send window by that amount.
    DataAck { conn_id: u64, consumed: u32 },

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

/// Default per-`conn_id` send window in bytes (four max-size DATA frames). A
/// sender may have at most this many unacknowledged bytes in flight on one
/// logical connection before it must pause for a [`Control::DataAck`]. Bounds the
/// memory and link share any single conn can take (head-of-line + A6). Both ends
/// assume this default; a [`Control::Window`] frame can change it explicitly.
pub const INITIAL_WINDOW: u32 = 4 * MAX_DATA_PAYLOAD as u32;

/// Split `payload` into `<= MAX_DATA_PAYLOAD` slices for DATA framing, so one
/// inner write can never produce an oversized frame that monopolizes the
/// multiplexed link (the relay rejects frames larger than `MAX_DATA_PAYLOAD`).
/// Yields a single (possibly empty) slice when the payload already fits.
pub fn chunk_payload(payload: &[u8]) -> impl Iterator<Item = &[u8]> {
    let mut emitted_empty = false;
    payload
        .chunks(MAX_DATA_PAYLOAD)
        .chain(std::iter::from_fn(move || {
            // `[].chunks(_)` yields nothing; preserve a zero-length DATA frame
            // (used as an inner half-close / flush signal) by emitting once.
            if payload.is_empty() && !emitted_empty {
                emitted_empty = true;
                Some(&payload[..0])
            } else {
                None
            }
        }))
}

/// Credit-based send window for one logical connection in one direction.
///
/// The sender debits the window by each DATA chunk it transmits and pauses when
/// it reaches zero; the receiver replenishes by sending [`Control::DataAck`] as it
/// drains bytes to the inner stream. [`Control::Window`] re-establishes the
/// absolute window. The relay reuses the same accountant as a blind guard: it
/// debits on forwarding DATA and credits on forwarding a `DataAck`, and treats a
/// window driven far negative as a flow-control violation to tear the conn down.
#[derive(Debug, Clone)]
pub struct ConnWindow {
    credit: i64,
}

impl ConnWindow {
    /// A window seeded with `initial` credit (use [`INITIAL_WINDOW`] by default).
    pub fn new(initial: u32) -> Self {
        Self {
            credit: i64::from(initial),
        }
    }

    /// Bytes the sender may still transmit before it must pause.
    pub fn available(&self) -> u32 {
        self.credit.clamp(0, i64::from(u32::MAX)) as u32
    }

    /// True when the sender has no credit and must pause reading its inner stream.
    pub fn is_blocked(&self) -> bool {
        self.credit <= 0
    }

    /// Debit the window for an outgoing chunk of `len` bytes.
    pub fn debit(&mut self, len: usize) {
        self.credit -= len as i64;
    }

    /// Replenish the window on a received [`Control::DataAck`] of `consumed` bytes.
    pub fn ack(&mut self, consumed: u32) {
        self.credit = self.credit.saturating_add(i64::from(consumed));
    }

    /// Re-establish the absolute window from a [`Control::Window`] grant.
    pub fn set(&mut self, credit: u32) {
        self.credit = i64::from(credit);
    }

    /// How far past zero the window has been driven. A well-behaved sender keeps
    /// this within one in-flight chunk; the relay uses a larger tolerance to
    /// detect a sender that ignores flow control entirely (A6).
    pub fn overrun(&self) -> u64 {
        if self.credit < 0 {
            (-self.credit) as u64
        } else {
            0
        }
    }
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
            Control::Window {
                conn_id: 7,
                credit: INITIAL_WINDOW,
            },
            Control::DataAck {
                conn_id: 7,
                consumed: 4096,
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

    #[test]
    fn chunk_payload_splits_at_max() {
        let big = vec![0u8; MAX_DATA_PAYLOAD * 2 + 7];
        let chunks: Vec<&[u8]> = chunk_payload(&big).collect();
        assert_eq!(chunks.len(), 3, "two full chunks + remainder");
        assert_eq!(chunks[0].len(), MAX_DATA_PAYLOAD);
        assert_eq!(chunks[1].len(), MAX_DATA_PAYLOAD);
        assert_eq!(chunks[2].len(), 7);
        assert!(chunks.iter().all(|c| c.len() <= MAX_DATA_PAYLOAD));
    }

    #[test]
    fn chunk_payload_small_and_empty() {
        // A payload that already fits yields exactly one slice.
        assert_eq!(chunk_payload(b"hi").count(), 1);
        // An empty payload still yields one (zero-length) slice, preserving an
        // explicit zero-length DATA frame as a flush/half-close signal.
        let empty: Vec<&[u8]> = chunk_payload(&[]).collect();
        assert_eq!(empty.len(), 1);
        assert!(empty[0].is_empty());
    }

    #[test]
    fn conn_window_blocks_at_zero_and_replenishes() {
        let mut w = ConnWindow::new(MAX_DATA_PAYLOAD as u32);
        assert!(!w.is_blocked());
        assert_eq!(w.available(), MAX_DATA_PAYLOAD as u32);

        w.debit(MAX_DATA_PAYLOAD); // exactly empties the window
        assert!(w.is_blocked());
        assert_eq!(w.available(), 0);
        assert_eq!(w.overrun(), 0, "an exact debit is not an overrun");

        w.ack(1024); // receiver drained some bytes
        assert!(!w.is_blocked());
        assert_eq!(w.available(), 1024);
    }

    #[test]
    fn conn_window_tracks_overrun_for_relay_guard() {
        // A sender that ignores flow control drives the window negative; the relay
        // reads `overrun()` to decide a conn is abusive and tear it down (A6).
        let mut w = ConnWindow::new(0);
        w.debit(MAX_DATA_PAYLOAD);
        assert!(w.is_blocked());
        assert_eq!(w.overrun(), MAX_DATA_PAYLOAD as u64);
    }

    #[test]
    fn conn_window_set_reestablishes_absolute() {
        let mut w = ConnWindow::new(10);
        w.debit(10);
        assert!(w.is_blocked());
        w.set(INITIAL_WINDOW);
        assert!(!w.is_blocked());
        assert_eq!(w.available(), INITIAL_WINDOW);
    }
}
