//! Client-side relay wire frames for `zeroclaw.relay.v1`.
//!
//! Keep this small and dependency-light: zerocode is an RPC/wire client and must
//! not link backend `zeroclaw-*` crates. The daemon and relay own their shared
//! protocol crate; this module mirrors only the frames the client sends or
//! receives.

use serde::{Deserialize, Serialize};

pub const SUBPROTOCOL: &str = "zeroclaw.relay.v1";
pub const MAX_CONTROL_FRAME: usize = 64 * 1024;
pub const MAX_DATA_PAYLOAD: usize = 64 * 1024;
pub const INITIAL_WINDOW: u32 = 4 * MAX_DATA_PAYLOAD as u32;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Control {
    Connect {
        node_id: String,
    },
    Enroll {
        node_id: String,
    },
    Opened {
        conn_id: u64,
    },
    Close {
        conn_id: u64,
        #[serde(default)]
        reason: String,
    },
    Window {
        conn_id: u64,
        credit: u32,
    },
    DataAck {
        conn_id: u64,
        consumed: u32,
    },
    Error {
        code: String,
        msg: String,
    },
}

impl Control {
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("relay control frame serializes")
    }

    pub fn from_json(s: &str) -> Result<Self, serde_json::Error> {
        if s.len() > MAX_CONTROL_FRAME {
            return Err(serde_json::Error::io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "relay control frame exceeds MAX_CONTROL_FRAME",
            )));
        }

        serde_json::from_str(s.trim_end_matches(['\n', '\r']))
    }
}

pub fn encode_data(conn_id: u64, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(8 + payload.len());
    out.extend_from_slice(&conn_id.to_be_bytes());
    out.extend_from_slice(payload);
    out
}

pub fn decode_data(bytes: &[u8]) -> Option<(u64, &[u8])> {
    if bytes.len() < 8 {
        return None;
    }
    let conn_id = u64::from_be_bytes(bytes[..8].try_into().expect("checked len >= 8"));
    Some((conn_id, &bytes[8..]))
}

#[derive(Debug, Clone)]
pub struct ConnWindow {
    credit: i64,
}

impl ConnWindow {
    pub fn new(initial: u32) -> Self {
        Self {
            credit: i64::from(initial),
        }
    }

    pub fn is_blocked(&self) -> bool {
        self.credit <= 0
    }

    pub fn debit(&mut self, len: usize) {
        self.credit -= len as i64;
    }

    pub fn ack(&mut self, consumed: u32) {
        self.credit = self.credit.saturating_add(i64::from(consumed));
    }

    pub fn set(&mut self, credit: u32) {
        self.credit = i64::from(credit);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_frames_round_trip() {
        let frame = Control::Connect {
            node_id: "node-a".into(),
        };
        assert_eq!(Control::from_json(&frame.to_json()).unwrap(), frame);
    }

    #[test]
    fn data_frames_round_trip() {
        let payload = b"hello";
        let frame = encode_data(42, payload);
        let (conn_id, decoded) = decode_data(&frame).unwrap();
        assert_eq!(conn_id, 42);
        assert_eq!(decoded, payload);
    }

    #[test]
    fn short_data_frame_is_rejected() {
        assert!(decode_data(&[0u8; 7]).is_none());
    }
}
