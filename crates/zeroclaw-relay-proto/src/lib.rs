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

/// `Open.peer_hint` value for the narrow daemon enrollment route.
pub const PEER_HINT_ENROLL: &str = "enroll";

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
    /// Client -> relay: request a route to the daemon's enrollment endpoint for
    /// `node_id`. The pairing code, CSR, and enrollment response are carried as
    /// opaque DATA on the resulting logical connection; the relay only asks the
    /// daemon bridge for its narrow enrollment loopback target.
    Enroll { node_id: String },

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

/// A simple monotonic token-bucket rate limiter (std only, no background timer).
///
/// Refills continuously at `refill_per_sec` up to `capacity`; each admitted event
/// costs one token. Used to cap abusive rates (A6): connection handshakes per
/// source IP at the relay, and per-node `OPEN` handshakes at the daemon bridge so
/// a flood cannot force unbounded loopback mTLS handshakes. Cheap and lock-guarded
/// by the caller; not payload-aware (it counts events, never bytes).
#[derive(Debug, Clone)]
pub struct TokenBucket {
    capacity: f64,
    tokens: f64,
    refill_per_sec: f64,
    last_refill: std::time::Instant,
}

impl TokenBucket {
    /// A bucket that starts full with room for `capacity` burst events, refilling
    /// at `refill_per_sec`. A zero/negative rate makes a fixed `capacity`-event
    /// allowance that never refills.
    pub fn new(capacity: u32, refill_per_sec: f64) -> Self {
        Self::new_at(capacity, refill_per_sec, std::time::Instant::now())
    }

    /// Construct with an explicit start instant (deterministic tests).
    pub fn new_at(capacity: u32, refill_per_sec: f64, now: std::time::Instant) -> Self {
        let capacity = f64::from(capacity);
        Self {
            capacity,
            tokens: capacity,
            refill_per_sec: refill_per_sec.max(0.0),
            last_refill: now,
        }
    }

    /// Try to admit one event now, debiting a token. Returns false when the bucket
    /// is empty (the caller should reject / rate-limit).
    pub fn try_take(&mut self) -> bool {
        self.try_take_at(std::time::Instant::now())
    }

    /// Try to admit one event at `now` (deterministic tests).
    pub fn try_take_at(&mut self, now: std::time::Instant) -> bool {
        let elapsed = now
            .saturating_duration_since(self.last_refill)
            .as_secs_f64();
        self.last_refill = now;
        self.tokens = (self.tokens + elapsed * self.refill_per_sec).min(self.capacity);
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }

    /// True when the bucket is at full capacity (no recent activity) - the relay
    /// uses this to prune idle per-source entries so the tracking map stays bounded.
    pub fn is_full_at(&self, now: std::time::Instant) -> bool {
        let elapsed = now
            .saturating_duration_since(self.last_refill)
            .as_secs_f64();
        (self.tokens + elapsed * self.refill_per_sec) >= self.capacity
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
            Control::Enroll {
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

    #[test]
    fn token_bucket_allows_burst_then_blocks() {
        let t0 = std::time::Instant::now();
        let mut b = TokenBucket::new_at(3, 1.0, t0); // 3 burst, 1/sec
        assert!(b.try_take_at(t0));
        assert!(b.try_take_at(t0));
        assert!(b.try_take_at(t0));
        assert!(!b.try_take_at(t0), "4th in the same instant is rejected");
    }

    #[test]
    fn token_bucket_refills_over_time() {
        let t0 = std::time::Instant::now();
        let mut b = TokenBucket::new_at(2, 10.0, t0); // 10 tokens/sec
        assert!(b.try_take_at(t0));
        assert!(b.try_take_at(t0));
        assert!(!b.try_take_at(t0));
        // 0.2s later -> ~2 tokens refilled (capped at capacity 2).
        let later = t0 + std::time::Duration::from_millis(200);
        assert!(b.try_take_at(later));
        assert!(b.try_take_at(later));
        assert!(!b.try_take_at(later));
    }

    #[test]
    fn token_bucket_zero_rate_is_a_fixed_allowance() {
        let t0 = std::time::Instant::now();
        let mut b = TokenBucket::new_at(2, 0.0, t0);
        assert!(b.try_take_at(t0));
        assert!(b.try_take_at(t0));
        // Never refills, even much later.
        let later = t0 + std::time::Duration::from_secs(3600);
        assert!(!b.try_take_at(later));
    }

    #[test]
    fn token_bucket_full_marks_idle_for_pruning() {
        let t0 = std::time::Instant::now();
        let mut b = TokenBucket::new_at(4, 4.0, t0);
        assert!(b.try_take_at(t0));
        assert!(!b.is_full_at(t0), "just used a token, not full");
        // After enough time it refills to full -> prunable.
        let later = t0 + std::time::Duration::from_secs(2);
        assert!(b.is_full_at(later));
    }
}
