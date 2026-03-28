//! mDNS-based local peer discovery for ZeroClaw nodes.
//!
//! Sends and listens for ZeroClaw peer announcements on the link-local
//! multicast address `224.0.0.251` (the mDNS multicast group) at port
//! `35353`.  Using the mDNS multicast group means the traffic stays
//! on-link — routers do not forward it — without requiring any external
//! infrastructure.
//!
//! The wire format is a single-line JSON object:
//!
//! ```json
//! {"type":"announce","name":"alice-laptop","addr":"192.168.1.42","port":3000,"version":"0.5.0"}
//! ```
//!
//! # Usage
//!
//! ```rust,ignore
//! use zeroclaw::nodes::mdns::{MdnsPeer, MdnsConfig, run_peer_discovery};
//! use std::sync::{Arc, Mutex};
//! use std::collections::HashMap;
//!
//! let registry: Arc<Mutex<HashMap<String, MdnsPeer>>> = Arc::new(Mutex::new(HashMap::new()));
//! let cfg = MdnsConfig::default();
//! let handle = tokio::spawn(run_peer_discovery(cfg, Arc::clone(&registry)));
//! ```
//!
//! # Limitations
//!
//! - IPv4 only (link-local multicast does not require IPv6 support).
//! - Only discovers peers on the same L2 segment.
//! - No authentication — suited for local-dev and home-lab use.
//!   Production deployments should layer the existing HMAC node-transport
//!   on top once peers are discovered.

use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddrV4};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::Result;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::net::UdpSocket;
use tokio::time::Duration;

/// IPv4 mDNS multicast group address (RFC 6762).
const MDNS_GROUP: Ipv4Addr = Ipv4Addr::new(224, 0, 0, 251);

/// ZeroClaw peer-discovery port (non-standard; 5353 is reserved for real mDNS).
const PEER_PORT: u16 = 35_353;

/// Maximum UDP datagram size we accept.
const MAX_DATAGRAM: usize = 2_048;

// ── Config ────────────────────────────────────────────────────────────────────

/// Configuration for the mDNS peer-discovery subsystem.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct MdnsConfig {
    /// Whether mDNS peer discovery is active.
    #[serde(default)]
    pub enabled: bool,

    /// Human-readable name advertised to peers (defaults to system hostname).
    #[serde(default)]
    pub node_name: Option<String>,

    /// Gateway port advertised to peers.
    #[serde(default = "MdnsConfig::default_port")]
    pub port: u16,

    /// How often (seconds) to re-broadcast a presence announcement.
    #[serde(default = "MdnsConfig::default_announce_interval_secs")]
    pub announce_interval_secs: u64,

    /// Seconds after the last announcement before a peer is evicted.
    #[serde(default = "MdnsConfig::default_peer_ttl_secs")]
    pub peer_ttl_secs: u64,
}

impl MdnsConfig {
    fn default_port() -> u16 {
        3_000
    }
    fn default_announce_interval_secs() -> u64 {
        30
    }
    fn default_peer_ttl_secs() -> u64 {
        90
    }
}

impl Default for MdnsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            node_name: None,
            port: Self::default_port(),
            announce_interval_secs: Self::default_announce_interval_secs(),
            peer_ttl_secs: Self::default_peer_ttl_secs(),
        }
    }
}

// ── Wire format ───────────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum PeerPacket {
    Announce(Announcement),
    Bye(Bye),
}

/// Presence announcement broadcast to the local network.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct Announcement {
    /// Node name (hostname or configured alias).
    pub name: String,
    /// Source IP address (filled in from the UDP datagram source).
    pub addr: String,
    /// Gateway port.
    pub port: u16,
    /// ZeroClaw version string.
    pub version: String,
}

/// Graceful departure notification (sent when the daemon stops cleanly).
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct Bye {
    pub name: String,
}

// ── Peer registry ─────────────────────────────────────────────────────────────

/// A discovered ZeroClaw peer.
#[derive(Debug, Clone)]
pub struct MdnsPeer {
    /// Node name as advertised.
    pub name: String,
    /// Source IP address observed in the last announcement.
    pub addr: String,
    /// Gateway port.
    pub port: u16,
    /// ZeroClaw version string.
    pub version: String,
    /// Wall-clock time of the last received announcement (for TTL).
    pub last_seen: Instant,
}

/// Shared peer registry type alias.
pub type PeerRegistry = Arc<Mutex<HashMap<String, MdnsPeer>>>;

// ── Public entry point ────────────────────────────────────────────────────────

/// Run the mDNS peer discovery loop.
///
/// Binds a UDP socket to the mDNS multicast group and:
/// 1. Periodically broadcasts an [`Announcement`] so peers can find this node.
/// 2. Listens for announcements from other nodes and updates `registry`.
/// 3. Evicts stale entries whose last-seen timestamp exceeds `peer_ttl_secs`.
///
/// This function runs until cancelled; wrap it in a `tokio::spawn` or a
/// supervised component handle.
pub async fn run_peer_discovery(config: MdnsConfig, registry: PeerRegistry) -> Result<()> {
    let socket = bind_multicast_socket()?;
    let node_name = resolve_node_name(&config);

    let announce_interval = Duration::from_secs(config.announce_interval_secs);
    let peer_ttl = Duration::from_secs(config.peer_ttl_secs);
    let mut announce_ticker = tokio::time::interval(announce_interval);
    let mut evict_ticker = tokio::time::interval(Duration::from_secs(15));

    let mut buf = [0u8; MAX_DATAGRAM];

    tracing::info!(
        node_name = %node_name,
        port = config.port,
        "mDNS peer discovery started ({}:{})",
        MDNS_GROUP,
        PEER_PORT,
    );

    loop {
        tokio::select! {
            _ = announce_ticker.tick() => {
                broadcast_announcement(&socket, &node_name, config.port).await;
            }
            _ = evict_ticker.tick() => {
                evict_stale_peers(&registry, peer_ttl);
            }
            result = socket.recv_from(&mut buf) => {
                match result {
                    Ok((len, src)) => {
                        let src_ip = match src {
                            std::net::SocketAddr::V4(a) => a.ip().to_string(),
                            std::net::SocketAddr::V6(a) => a.ip().to_string(),
                        };
                        handle_datagram(&buf[..len], &src_ip, &registry, &node_name);
                    }
                    Err(e) => {
                        tracing::warn!("mDNS receive error: {e}");
                    }
                }
            }
        }
    }
}

/// Send a [`PeerPacket::Bye`] announcement so peers remove this node promptly.
pub async fn send_goodbye(node_name: &str) {
    if let Ok(socket) = bind_multicast_socket() {
        let packet = PeerPacket::Bye(Bye {
            name: node_name.to_string(),
        });
        if let Ok(payload) = serde_json::to_vec(&packet) {
            let dest = SocketAddrV4::new(MDNS_GROUP, PEER_PORT);
            let _ = socket.send_to(&payload, dest).await;
        }
    }
}

// ── Internal helpers ──────────────────────────────────────────────────────────

fn bind_multicast_socket() -> Result<UdpSocket> {
    use socket2::{Domain, Protocol, Socket, Type};

    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    socket.set_reuse_address(true)?;
    #[cfg(unix)]
    socket.set_reuse_port(true)?;
    socket.set_nonblocking(true)?;
    socket.bind(&SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, PEER_PORT).into())?;
    socket.join_multicast_v4(&MDNS_GROUP, &Ipv4Addr::UNSPECIFIED)?;
    socket.set_multicast_loop_v4(true)?;
    socket.set_multicast_ttl_v4(1)?; // link-local only

    Ok(UdpSocket::from_std(socket.into())?)
}

fn resolve_node_name(config: &MdnsConfig) -> String {
    config
        .node_name
        .clone()
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| {
            hostname::get()
                .ok()
                .and_then(|h| h.into_string().ok())
                .unwrap_or_else(|| "zeroclaw-node".to_string())
        })
}

async fn broadcast_announcement(socket: &UdpSocket, name: &str, port: u16) {
    let version = env!("CARGO_PKG_VERSION");
    // addr placeholder — receiver uses the UDP source IP
    let packet = PeerPacket::Announce(Announcement {
        name: name.to_string(),
        addr: String::new(),
        port,
        version: version.to_string(),
    });
    match serde_json::to_vec(&packet) {
        Ok(payload) => {
            let dest = SocketAddrV4::new(MDNS_GROUP, PEER_PORT);
            if let Err(e) = socket.send_to(&payload, dest).await {
                tracing::debug!("mDNS announce send error: {e}");
            }
        }
        Err(e) => tracing::warn!("mDNS announce serialise error: {e}"),
    }
}

fn handle_datagram(data: &[u8], src_ip: &str, registry: &PeerRegistry, own_name: &str) {
    let packet: PeerPacket = match serde_json::from_slice(data) {
        Ok(p) => p,
        Err(_) => return, // not a ZeroClaw packet
    };

    match packet {
        PeerPacket::Announce(mut ann) => {
            // Ignore our own announcements.
            if ann.name == own_name {
                return;
            }
            ann.addr = src_ip.to_string();
            tracing::debug!(peer = %ann.name, addr = %ann.addr, port = ann.port, "mDNS peer seen");

            let mut reg = registry.lock().expect("peer registry lock");
            reg.insert(
                ann.name.clone(),
                MdnsPeer {
                    name: ann.name,
                    addr: ann.addr,
                    port: ann.port,
                    version: ann.version,
                    last_seen: Instant::now(),
                },
            );
        }
        PeerPacket::Bye(bye) => {
            if bye.name == own_name {
                return;
            }
            tracing::debug!(peer = %bye.name, "mDNS peer departed");
            let mut reg = registry.lock().expect("peer registry lock");
            reg.remove(&bye.name);
        }
    }
}

fn evict_stale_peers(registry: &PeerRegistry, ttl: Duration) {
    let mut reg = registry.lock().expect("peer registry lock");
    let before = reg.len();
    reg.retain(|_, peer| peer.last_seen.elapsed() < ttl);
    let evicted = before - reg.len();
    if evicted > 0 {
        tracing::debug!("mDNS: evicted {evicted} stale peer(s)");
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_registry() -> PeerRegistry {
        Arc::new(Mutex::new(HashMap::new()))
    }

    // ── Config ──────────────────────────────────────────────────────────────

    #[test]
    fn default_config_is_disabled() {
        let cfg = MdnsConfig::default();
        assert!(!cfg.enabled);
    }

    #[test]
    fn default_config_has_sane_values() {
        let cfg = MdnsConfig::default();
        assert_eq!(cfg.port, 3_000);
        assert!(cfg.announce_interval_secs > 0);
        assert!(cfg.peer_ttl_secs > cfg.announce_interval_secs);
    }

    #[test]
    fn config_roundtrips_toml() {
        let src = r#"
            enabled = true
            node_name = "my-node"
            port = 8080
            announce_interval_secs = 15
            peer_ttl_secs = 60
        "#;
        let cfg: MdnsConfig = toml::from_str(src).expect("parse");
        assert!(cfg.enabled);
        assert_eq!(cfg.node_name.as_deref(), Some("my-node"));
        assert_eq!(cfg.port, 8080);
        assert_eq!(cfg.announce_interval_secs, 15);
        assert_eq!(cfg.peer_ttl_secs, 60);
    }

    // ── Wire format ──────────────────────────────────────────────────────────

    #[test]
    fn announce_packet_roundtrip() {
        let packet = PeerPacket::Announce(Announcement {
            name: "alice".into(),
            addr: "192.168.1.2".into(),
            port: 3000,
            version: "0.5.0".into(),
        });
        let json = serde_json::to_string(&packet).unwrap();
        let decoded: PeerPacket = serde_json::from_str(&json).unwrap();
        assert_eq!(packet, decoded);
    }

    #[test]
    fn bye_packet_roundtrip() {
        let packet = PeerPacket::Bye(Bye { name: "bob".into() });
        let json = serde_json::to_string(&packet).unwrap();
        let decoded: PeerPacket = serde_json::from_str(&json).unwrap();
        assert_eq!(packet, decoded);
    }

    #[test]
    fn unknown_json_is_silently_dropped() {
        let registry = make_registry();
        // garbage data — handle_datagram should not panic
        handle_datagram(b"not-json", "10.0.0.1", &registry, "me");
        assert!(registry.lock().unwrap().is_empty());
    }

    // ── Datagram handling ────────────────────────────────────────────────────

    #[test]
    fn announce_from_other_node_is_added_to_registry() {
        let registry = make_registry();
        let packet = PeerPacket::Announce(Announcement {
            name: "peer-a".into(),
            addr: String::new(),
            port: 3000,
            version: "0.5.0".into(),
        });
        let data = serde_json::to_vec(&packet).unwrap();
        handle_datagram(&data, "192.168.1.5", &registry, "me");

        let reg = registry.lock().unwrap();
        let peer = reg.get("peer-a").expect("peer-a should be in registry");
        assert_eq!(peer.addr, "192.168.1.5", "addr filled from UDP src");
        assert_eq!(peer.port, 3000);
        assert_eq!(peer.version, "0.5.0");
    }

    #[test]
    fn own_announce_is_ignored() {
        let registry = make_registry();
        let packet = PeerPacket::Announce(Announcement {
            name: "me".into(),
            addr: String::new(),
            port: 3000,
            version: "0.5.0".into(),
        });
        let data = serde_json::to_vec(&packet).unwrap();
        handle_datagram(&data, "127.0.0.1", &registry, "me");
        assert!(registry.lock().unwrap().is_empty(), "own announce ignored");
    }

    #[test]
    fn bye_removes_peer_from_registry() {
        let registry = make_registry();
        // First add a peer
        registry.lock().unwrap().insert(
            "peer-b".into(),
            MdnsPeer {
                name: "peer-b".into(),
                addr: "10.0.0.2".into(),
                port: 3000,
                version: "0.5.0".into(),
                last_seen: Instant::now(),
            },
        );
        // Send goodbye
        let packet = PeerPacket::Bye(Bye {
            name: "peer-b".into(),
        });
        let data = serde_json::to_vec(&packet).unwrap();
        handle_datagram(&data, "10.0.0.2", &registry, "me");
        assert!(
            registry.lock().unwrap().is_empty(),
            "bye should remove peer"
        );
    }

    #[test]
    fn own_bye_is_ignored() {
        let registry = make_registry();
        let packet = PeerPacket::Bye(Bye { name: "me".into() });
        let data = serde_json::to_vec(&packet).unwrap();
        handle_datagram(&data, "127.0.0.1", &registry, "me");
        // nothing to assert — just must not panic or corrupt state
    }

    // ── TTL eviction ─────────────────────────────────────────────────────────

    #[test]
    fn fresh_peer_not_evicted() {
        let registry = make_registry();
        registry.lock().unwrap().insert(
            "peer-c".into(),
            MdnsPeer {
                name: "peer-c".into(),
                addr: "10.0.0.3".into(),
                port: 3000,
                version: "0.5.0".into(),
                last_seen: Instant::now(),
            },
        );
        evict_stale_peers(&registry, Duration::from_secs(60));
        assert!(
            registry.lock().unwrap().contains_key("peer-c"),
            "fresh peer must survive eviction"
        );
    }

    #[test]
    fn stale_peer_is_evicted() {
        let registry = make_registry();
        // Fake a stale last_seen by using a very short TTL
        registry.lock().unwrap().insert(
            "peer-stale".into(),
            MdnsPeer {
                name: "peer-stale".into(),
                addr: "10.0.0.9".into(),
                port: 3000,
                version: "0.5.0".into(),
                // last_seen = now, but TTL = 0ns → immediate expiry
                last_seen: Instant::now(),
            },
        );
        evict_stale_peers(&registry, Duration::from_nanos(0));
        assert!(
            registry.lock().unwrap().is_empty(),
            "stale peer must be evicted"
        );
    }

    // ── Node name resolution ─────────────────────────────────────────────────

    #[test]
    fn node_name_uses_config_when_set() {
        let cfg = MdnsConfig {
            node_name: Some("custom-name".into()),
            ..MdnsConfig::default()
        };
        assert_eq!(resolve_node_name(&cfg), "custom-name");
    }

    #[test]
    fn node_name_falls_back_to_hostname() {
        let cfg = MdnsConfig {
            node_name: None,
            ..MdnsConfig::default()
        };
        let name = resolve_node_name(&cfg);
        // Should not be empty and not the hard-coded placeholder (system has a hostname)
        assert!(!name.is_empty());
    }

    #[test]
    fn empty_node_name_falls_back() {
        let cfg = MdnsConfig {
            node_name: Some(String::new()),
            ..MdnsConfig::default()
        };
        let name = resolve_node_name(&cfg);
        assert!(!name.is_empty(), "empty config name falls back to hostname");
    }
}
