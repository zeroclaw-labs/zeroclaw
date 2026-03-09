use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr, UdpSocket};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::collective::PeerState;

const MAX_DATAGRAM_SIZE: usize = 1400;
const MULTICAST_ADDR: Ipv4Addr = Ipv4Addr::new(239, 255, 42, 1);

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PeerMessage {
    Discovery { node_id: String, port: u16 },
    State { peer_state: PeerState },
    Heartbeat { node_id: String },
}

pub struct PeerTransport {
    socket: Option<UdpSocket>,
    discovery_port: u16,
    local_node_id: String,
    known_peers: HashMap<String, (SocketAddr, DateTime<Utc>)>,
    stale_threshold_secs: i64,
}

impl PeerTransport {
    pub fn new(node_id: String, port: u16) -> Self {
        let bind_addr: SocketAddr = ([0, 0, 0, 0], port).into();
        let socket = match UdpSocket::bind(bind_addr) {
            Ok(s) => {
                if let Err(e) = s.set_nonblocking(true) {
                    tracing::warn!("failed to set udp socket non-blocking: {e}");
                    None
                } else {
                    if let Err(e) = s.join_multicast_v4(&MULTICAST_ADDR, &Ipv4Addr::UNSPECIFIED) {
                        tracing::warn!("failed to join multicast group: {e}");
                    }
                    Some(s)
                }
            }
            Err(e) => {
                tracing::warn!("peer transport bind on port {port} failed: {e}");
                None
            }
        };

        Self {
            socket,
            discovery_port: port,
            local_node_id: node_id,
            known_peers: HashMap::new(),
            stale_threshold_secs: 300,
        }
    }

    pub fn broadcast_discovery(&self) {
        let Some(ref socket) = self.socket else {
            return;
        };
        let msg = PeerMessage::Discovery {
            node_id: self.local_node_id.clone(),
            port: self.discovery_port,
        };
        let Ok(data) = serde_json::to_vec(&msg) else {
            return;
        };
        if data.len() > MAX_DATAGRAM_SIZE {
            tracing::warn!("discovery message exceeds max datagram size");
            return;
        }
        let target: SocketAddr = (MULTICAST_ADDR, self.discovery_port).into();
        let _ = socket.send_to(&data, target);
    }

    pub fn send_state(&self, peer_addr: SocketAddr, state: &PeerState) {
        let Some(ref socket) = self.socket else {
            return;
        };
        let msg = PeerMessage::State {
            peer_state: state.clone(),
        };
        let Ok(data) = serde_json::to_vec(&msg) else {
            return;
        };
        if data.len() > MAX_DATAGRAM_SIZE {
            tracing::warn!("state message exceeds max datagram size, dropping");
            return;
        }
        let _ = socket.send_to(&data, peer_addr);
    }

    pub fn send_heartbeat(&self) {
        let Some(ref socket) = self.socket else {
            return;
        };
        let msg = PeerMessage::Heartbeat {
            node_id: self.local_node_id.clone(),
        };
        let Ok(data) = serde_json::to_vec(&msg) else {
            return;
        };
        for (addr, _) in self.known_peers.values() {
            let _ = socket.send_to(&data, addr);
        }
    }

    pub fn poll_incoming(&mut self) -> Vec<PeerMessage> {
        let Some(ref socket) = self.socket else {
            return Vec::new();
        };
        let mut buf = [0u8; MAX_DATAGRAM_SIZE];
        let mut messages = Vec::new();

        loop {
            match socket.recv_from(&mut buf) {
                Ok((len, src_addr)) => {
                    let Ok(msg) = serde_json::from_slice::<PeerMessage>(&buf[..len]) else {
                        continue;
                    };
                    match &msg {
                        PeerMessage::Discovery { node_id, port } => {
                            if *node_id != self.local_node_id {
                                let peer_addr: SocketAddr = (src_addr.ip(), *port).into();
                                self.known_peers
                                    .insert(node_id.clone(), (peer_addr, Utc::now()));
                            }
                        }
                        PeerMessage::State { peer_state } => {
                            if peer_state.node_id != self.local_node_id {
                                self.known_peers
                                    .insert(peer_state.node_id.clone(), (src_addr, Utc::now()));
                            }
                        }
                        PeerMessage::Heartbeat { node_id } => {
                            if *node_id != self.local_node_id {
                                self.known_peers
                                    .entry(node_id.clone())
                                    .and_modify(|(addr, ts)| {
                                        *addr = src_addr;
                                        *ts = Utc::now();
                                    })
                                    .or_insert((src_addr, Utc::now()));
                            }
                        }
                    }
                    messages.push(msg);
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(_) => break,
            }
        }

        messages
    }

    pub fn known_peer_addrs(&self) -> Vec<(String, SocketAddr)> {
        self.known_peers
            .iter()
            .map(|(id, (addr, _))| (id.clone(), *addr))
            .collect()
    }

    pub fn prune_stale(&mut self) {
        let cutoff = Utc::now() - chrono::Duration::seconds(self.stale_threshold_secs);
        self.known_peers.retain(|_, (_, ts)| *ts > cutoff);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consciousness::traits::PhenomenalState;

    #[test]
    fn peer_transport_new_binds_socket() {
        let transport = PeerTransport::new("test_node".to_string(), 0);
        assert_eq!(transport.local_node_id, "test_node");
    }

    #[test]
    fn serialize_deserialize_peer_message() {
        let messages = vec![
            PeerMessage::Discovery {
                node_id: "node_a".to_string(),
                port: 9870,
            },
            PeerMessage::State {
                peer_state: PeerState {
                    node_id: "node_b".to_string(),
                    phenomenal: PhenomenalState {
                        attention: 0.7,
                        arousal: 0.5,
                        valence: 0.1,
                        ..Default::default()
                    },
                    coherence: 0.9,
                    tick_count: 42,
                    last_seen: Utc::now(),
                },
            },
            PeerMessage::Heartbeat {
                node_id: "node_c".to_string(),
            },
        ];

        for msg in &messages {
            let data = serde_json::to_vec(msg).expect("serialize");
            assert!(data.len() < MAX_DATAGRAM_SIZE);
            let decoded: PeerMessage = serde_json::from_slice(&data).expect("deserialize");
            match (&msg, &decoded) {
                (
                    PeerMessage::Discovery {
                        node_id: a,
                        port: pa,
                    },
                    PeerMessage::Discovery {
                        node_id: b,
                        port: pb,
                    },
                ) => {
                    assert_eq!(a, b);
                    assert_eq!(pa, pb);
                }
                (PeerMessage::State { peer_state: a }, PeerMessage::State { peer_state: b }) => {
                    assert_eq!(a.node_id, b.node_id);
                    assert!((a.coherence - b.coherence).abs() < f64::EPSILON);
                }
                (PeerMessage::Heartbeat { node_id: a }, PeerMessage::Heartbeat { node_id: b }) => {
                    assert_eq!(a, b);
                }
                _ => panic!("mismatched message types"),
            }
        }
    }

    #[test]
    fn multicast_address_is_admin_scoped() {
        assert_eq!(MULTICAST_ADDR, Ipv4Addr::new(239, 255, 42, 1));
        assert!(MULTICAST_ADDR.is_multicast());
    }

    #[test]
    fn prune_removes_stale_peers() {
        let mut transport = PeerTransport::new("local".to_string(), 0);
        let stale_time = Utc::now() - chrono::Duration::seconds(600);
        let addr: SocketAddr = ([127, 0, 0, 1], 9870).into();
        transport
            .known_peers
            .insert("stale_peer".to_string(), (addr, stale_time));
        transport
            .known_peers
            .insert("fresh_peer".to_string(), (addr, Utc::now()));

        assert_eq!(transport.known_peers.len(), 2);
        transport.prune_stale();
        assert_eq!(transport.known_peers.len(), 1);
        assert!(transport.known_peers.contains_key("fresh_peer"));
    }
}
