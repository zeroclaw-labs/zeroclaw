//! Wire protocol (`zeroclaw.relay.v1`) for the ZeroClaw nominated relay.
//!
//! The relay is a blind forwarder: clients reach a daemon behind NAT through it
//! while the inner client<->daemon mTLS session tunnels across unchanged. This
//! crate defines only the small set of newline-delimited JSON **control frames**
//! exchanged at the start of each connection. After the handshake the relay
//! pipes opaque bytes (the inner mTLS) and never inspects them.
//!
//! Connection kinds (each opens with exactly one control frame, then bytes):
//! - **Daemon control**: opens with [`Frame::Register`]; the relay replies
//!   [`Frame::Registered`] and thereafter pushes [`Frame::Open`] for each new
//!   client. This connection carries no payload bytes.
//! - **Client**: opens with [`Frame::Connect`]; the relay replies
//!   [`Frame::Opened`] then transparently pipes the rest (the inner mTLS).
//! - **Daemon data**: opens with [`Frame::Accept`] (in response to an `Open`);
//!   the relay pairs it with the waiting client and pipes the two together.

use serde::{Deserialize, Serialize};

/// The WebSocket subprotocol identifier (for the production WS transport).
pub const SUBPROTOCOL: &str = "zeroclaw.relay.v1";

/// A control frame. Serialized as a single line of JSON terminated by `\n`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Frame {
    /// Daemon control connection: claim a node-id to serve.
    Register {
        node_id: String,
        #[serde(default)]
        relay_token: String,
    },
    /// Relay -> daemon: registration accepted.
    Registered { node_id: String },
    /// Client connection: request a route to `node_id`.
    Connect { node_id: String },
    /// Relay -> daemon (control): a new client arrived; open a data connection.
    Open { conn_id: u64 },
    /// Relay -> client: the route is open; the rest of the stream is the daemon.
    Opened { conn_id: u64 },
    /// Daemon data connection: pair this stream with the given client.
    Accept {
        conn_id: u64,
        #[serde(default)]
        relay_token: String,
    },
    /// Either side: a terminal error (e.g. `forbidden`, `no_such_node`).
    Error { code: String, msg: String },
}

impl Frame {
    /// Encode as a single newline-terminated JSON line.
    pub fn to_line(&self) -> String {
        let mut s = serde_json::to_string(self).expect("relay frame serializes");
        s.push('\n');
        s
    }

    /// Parse one frame from a JSON line (trailing newline tolerated).
    pub fn from_line(line: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(line.trim_end_matches(['\n', '\r']))
    }

    /// Convenience constructor for an error frame.
    pub fn error(code: &str, msg: impl Into<String>) -> Self {
        Frame::Error {
            code: code.to_string(),
            msg: msg.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_round_trip() {
        let cases = [
            Frame::Register {
                node_id: "n1".into(),
                relay_token: "t".into(),
            },
            Frame::Registered {
                node_id: "n1".into(),
            },
            Frame::Connect {
                node_id: "n1".into(),
            },
            Frame::Open { conn_id: 7 },
            Frame::Opened { conn_id: 7 },
            Frame::Accept {
                conn_id: 7,
                relay_token: String::new(),
            },
            Frame::error("no_such_node", "n1"),
        ];
        for f in cases {
            let line = f.to_line();
            assert!(line.ends_with('\n'));
            assert_eq!(Frame::from_line(&line).unwrap(), f);
        }
    }

    #[test]
    fn relay_token_defaults_when_absent() {
        let f = Frame::from_line(r#"{"t":"register","node_id":"n1"}"#).unwrap();
        assert_eq!(
            f,
            Frame::Register {
                node_id: "n1".into(),
                relay_token: String::new()
            }
        );
    }
}
