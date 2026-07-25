//! LAN-local multicast peer discovery for ZeroClaw gateways.
//!
//! This is a discovery hint surface only. Multicast packets are unauthenticated
//! and never grant access to `/ws/nodes`, A2A, or any gateway API. Peers still
//! have to satisfy the existing pairing/token/auth boundary before they can
//! connect or invoke anything.

use anyhow::Result;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    net::{Ipv4Addr, SocketAddr, SocketAddrV4},
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::net::UdpSocket;
use tokio::sync::watch;
use tokio::time::MissedTickBehavior;
use uuid::Uuid;
use zeroclaw_config::schema::MdnsConfig;

// Proprietary ZeroClaw discovery uses an administratively scoped multicast group
// rather than the IANA-reserved mDNS group at 224.0.0.251.
const LAN_DISCOVERY_GROUP: Ipv4Addr = Ipv4Addr::new(239, 255, 42, 17);
const DISCOVERY_PORT: u16 = 35_353;
const MAX_DATAGRAM: usize = 2_048;
const MAX_INSTANCE_ID_LEN: usize = 128;
const MAX_NODE_NAME_LEN: usize = 128;
const MAX_VERSION_LEN: usize = 128;
const RECEIVE_ERROR_BACKOFF: Duration = Duration::from_millis(250);
const RECEIVE_ERROR_LOG_INTERVAL: Duration = Duration::from_secs(30);

/// Runtime gateway endpoint advertised over LAN discovery.
///
/// The port and path prefix come from the already-bound gateway listener, not
/// from `[nodes.mdns]`, so the discovery config does not duplicate gateway
/// listen state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MdnsAdvertisedGateway {
    port: u16,
    path_prefix: Option<String>,
}

impl MdnsAdvertisedGateway {
    pub fn new(port: u16, path_prefix: Option<&str>) -> Self {
        Self {
            port,
            path_prefix: normalize_path_prefix(path_prefix),
        }
    }

    pub fn announcement(&self, name: &str, version: &str) -> Announcement {
        Announcement {
            id: String::new(),
            name: name.to_string(),
            addr: String::new(),
            port: self.port,
            version: version.to_string(),
            path_prefix: self.path_prefix.clone(),
        }
    }
}

/// Return whether the actual bound gateway address can be advertised to LAN
/// peers. Use this after bind fallback so discovery follows the real listener.
pub fn is_advertisable_gateway_addr(addr: &SocketAddr) -> bool {
    !addr.ip().is_loopback()
}

/// A ZeroClaw LAN discovery packet.
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PeerPacket {
    Announce(Announcement),
    Bye(Bye),
}

/// Presence announcement broadcast to the local network.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Announcement {
    /// Runtime-generated instance identifier for this gateway process. This is
    /// the peer identity; `name` is display-only and may collide.
    pub id: String,
    /// Node name as configured or resolved locally.
    pub name: String,
    /// Source IP address. Senders leave this empty; receivers fill it from the
    /// UDP source address so a peer cannot spoof the reachable IP in JSON.
    pub addr: String,
    /// Runtime gateway port.
    pub port: u16,
    /// ZeroClaw version string.
    pub version: String,
    /// Runtime gateway path prefix, when configured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path_prefix: Option<String>,
}

/// Graceful departure notification.
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Bye {
    pub id: String,
}

/// A discovered LAN peer.
#[derive(Debug, Clone)]
pub struct MdnsPeer {
    pub name: String,
    pub addr: String,
    pub port: u16,
    pub version: String,
    pub path_prefix: Option<String>,
    pub last_seen: Instant,
}

impl MdnsPeer {
    pub fn base_url(&self) -> String {
        format_peer_base_url(&self.addr, self.port, self.path_prefix.as_deref())
    }
}

/// Authenticated API/status snapshot of a discovered LAN peer.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct MdnsPeerSnapshot {
    pub id: String,
    pub name: String,
    pub addr: String,
    pub port: u16,
    pub version: String,
    pub path_prefix: Option<String>,
    pub base_url: String,
}

/// In-memory peer registry populated from LAN discovery announcements.
#[derive(Clone)]
pub struct MdnsPeerRegistry {
    peers: Arc<Mutex<HashMap<String, MdnsPeer>>>,
    max_peers: Arc<dyn Fn() -> usize + Send + Sync>,
}

impl Default for MdnsPeerRegistry {
    fn default() -> Self {
        Self::new(|| MdnsConfig::default().max_peers)
    }
}

impl MdnsPeerRegistry {
    pub fn new(max_peers: impl Fn() -> usize + Send + Sync + 'static) -> Self {
        Self {
            peers: Arc::new(Mutex::new(HashMap::new())),
            max_peers: Arc::new(max_peers),
        }
    }

    /// Insert or refresh a peer. New identities are rejected at capacity, but
    /// known identities may refresh without increasing registry cardinality.
    pub fn insert(&self, id: String, peer: MdnsPeer) {
        let mut peers = self.peers.lock();
        if peers.len() >= (self.max_peers)() && !peers.contains_key(&id) {
            return;
        }
        peers.insert(id, peer);
    }

    pub fn remove(&self, id: &str) {
        self.peers.lock().remove(id);
    }

    pub fn snapshots(&self) -> Vec<MdnsPeerSnapshot> {
        let mut peers: Vec<_> = self
            .peers
            .lock()
            .iter()
            .map(|(id, peer)| MdnsPeerSnapshot {
                id: id.clone(),
                name: peer.name.clone(),
                addr: peer.addr.clone(),
                port: peer.port,
                version: peer.version.clone(),
                path_prefix: peer.path_prefix.clone(),
                base_url: peer.base_url(),
            })
            .collect();
        peers.sort_by(|a, b| a.id.cmp(&b.id));
        peers
    }
}

/// Run the LAN discovery loop until cancelled.
pub async fn run_peer_discovery(
    config: MdnsConfig,
    advertised: MdnsAdvertisedGateway,
    registry: MdnsPeerRegistry,
    mut shutdown_rx: watch::Receiver<bool>,
) -> Result<()> {
    if !config.enabled {
        return Ok(());
    }

    let socket = bind_multicast_socket()?;
    let instance_id = Uuid::new_v4().to_string();
    let node_name = resolve_node_name(&config);
    let announce_interval = Duration::from_secs(config.announce_interval_secs);
    let peer_ttl = Duration::from_secs(config.peer_ttl_secs);
    let mut announce_ticker = tokio::time::interval(announce_interval);
    announce_ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut evict_ticker = tokio::time::interval(Duration::from_secs(15));
    evict_ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut buf = [0u8; MAX_DATAGRAM];
    let mut last_receive_error_log = None;

    ::zeroclaw_log::record!(
        INFO,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(
            ::serde_json::json!({
                "node_name": node_name,
                "instance_id": instance_id,
                "discovery_port": DISCOVERY_PORT,
            })
        ),
        "LAN peer discovery started"
    );

    loop {
        tokio::select! {
            _ = announce_ticker.tick() => {
                broadcast_announcement(&socket, &advertised, &instance_id, &node_name).await;
            }
            _ = evict_ticker.tick() => {
                evict_stale_peers(&registry, peer_ttl);
            }
            _ = shutdown_rx.changed() => {
                broadcast_bye(&socket, &instance_id).await;
                return Ok(());
            }
            result = socket.recv_from(&mut buf) => {
                match result {
                    Ok((len, src)) => {
                        let src_ip = src.ip().to_string();
                        handle_datagram(&buf[..len], &src_ip, &registry, &instance_id);
                    }
                    Err(err) => {
                        if should_log_receive_error(&mut last_receive_error_log, Instant::now()) {
                            ::zeroclaw_log::record!(
                                WARN,
                                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                                    .with_attrs(::serde_json::json!({"error": format!("{err}")})),
                                "LAN peer discovery receive failed; retrying with backoff"
                            );
                        }
                        if receive_error_backoff_or_shutdown(&mut shutdown_rx).await {
                            broadcast_bye(&socket, &instance_id).await;
                            return Ok(());
                        }
                    }
                }
            }
        }
    }
}

pub fn handle_datagram(data: &[u8], src_ip: &str, registry: &MdnsPeerRegistry, own_id: &str) {
    let packet: PeerPacket = match serde_json::from_slice(data) {
        Ok(packet) => packet,
        Err(_) => return,
    };

    match packet {
        PeerPacket::Announce(mut ann) => {
            if ann.id == own_id || !is_valid_announcement(&ann) {
                return;
            }
            ann.addr = src_ip.to_string();
            registry.insert(
                ann.id,
                MdnsPeer {
                    name: ann.name,
                    addr: ann.addr,
                    port: ann.port,
                    version: ann.version,
                    path_prefix: normalize_path_prefix(ann.path_prefix.as_deref()),
                    last_seen: Instant::now(),
                },
            );
        }
        PeerPacket::Bye(bye) => {
            if bye.id != own_id && is_valid_instance_id(&bye.id) {
                registry.remove(&bye.id);
            }
        }
    }
}

pub fn evict_stale_peers(registry: &MdnsPeerRegistry, ttl: Duration) {
    registry
        .peers
        .lock()
        .retain(|_, peer| peer.last_seen.elapsed() < ttl);
}

async fn broadcast_announcement(
    socket: &UdpSocket,
    advertised: &MdnsAdvertisedGateway,
    id: &str,
    name: &str,
) {
    let mut announcement = advertised.announcement(name, env!("CARGO_PKG_VERSION"));
    announcement.id = id.to_string();
    let packet = PeerPacket::Announce(announcement);
    match serde_json::to_vec(&packet) {
        Ok(payload) => {
            let dest = SocketAddrV4::new(LAN_DISCOVERY_GROUP, DISCOVERY_PORT);
            if let Err(err) = socket.send_to(&payload, dest).await {
                ::zeroclaw_log::record!(
                    DEBUG,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"error": format!("{err}")})),
                    "LAN peer discovery announcement send failed"
                );
            }
        }
        Err(err) => {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"error": format!("{err}")})),
                "LAN peer discovery announcement serialization failed"
            );
        }
    }
}

async fn broadcast_bye(socket: &UdpSocket, id: &str) {
    let packet = PeerPacket::Bye(Bye { id: id.to_string() });
    match serde_json::to_vec(&packet) {
        Ok(payload) => {
            let dest = SocketAddrV4::new(LAN_DISCOVERY_GROUP, DISCOVERY_PORT);
            if let Err(err) = socket.send_to(&payload, dest).await {
                ::zeroclaw_log::record!(
                    DEBUG,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"error": format!("{err}")})),
                    "LAN peer discovery goodbye send failed"
                );
            }
        }
        Err(err) => {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"error": format!("{err}")})),
                "LAN peer discovery goodbye serialization failed"
            );
        }
    }
}

fn bind_multicast_socket() -> Result<UdpSocket> {
    use socket2::{Domain, Protocol, Socket, Type};

    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    socket.set_reuse_address(true)?;
    #[cfg(unix)]
    socket.set_reuse_port(true)?;
    socket.set_nonblocking(true)?;
    socket.bind(&SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, DISCOVERY_PORT).into())?;
    socket.join_multicast_v4(&LAN_DISCOVERY_GROUP, &Ipv4Addr::UNSPECIFIED)?;
    socket.set_multicast_loop_v4(true)?;
    socket.set_multicast_ttl_v4(1)?;

    Ok(UdpSocket::from_std(socket.into())?)
}

fn resolve_node_name(config: &MdnsConfig) -> String {
    config
        .node_name
        .clone()
        .filter(|name| !name.trim().is_empty())
        .or_else(|| std::env::var("HOSTNAME").ok())
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| "zeroclaw-node".to_string())
}

fn normalize_path_prefix(prefix: Option<&str>) -> Option<String> {
    prefix
        .map(str::trim)
        .filter(|prefix| !prefix.is_empty())
        .map(|prefix| prefix.trim_end_matches('/').to_string())
}

fn is_valid_instance_id(id: &str) -> bool {
    let id = id.trim();
    !id.is_empty() && id.len() <= MAX_INSTANCE_ID_LEN
}

fn is_valid_announcement(ann: &Announcement) -> bool {
    if !is_valid_instance_id(&ann.id)
        || ann.name.trim().is_empty()
        || ann.name.len() > MAX_NODE_NAME_LEN
        || ann.port == 0
        || ann.version.len() > MAX_VERSION_LEN
    {
        return false;
    }

    match ann.path_prefix.as_deref().map(str::trim) {
        None | Some("") => true,
        Some(prefix) => prefix.starts_with('/') && !prefix.contains('?') && !prefix.contains('#'),
    }
}

async fn receive_error_backoff_or_shutdown(shutdown_rx: &mut watch::Receiver<bool>) -> bool {
    tokio::select! {
        _ = tokio::time::sleep(RECEIVE_ERROR_BACKOFF) => false,
        _ = shutdown_rx.changed() => true,
    }
}

fn should_log_receive_error(last_log: &mut Option<Instant>, now: Instant) -> bool {
    let should_log = match *last_log {
        Some(last) => now.saturating_duration_since(last) >= RECEIVE_ERROR_LOG_INTERVAL,
        None => true,
    };
    if should_log {
        *last_log = Some(now);
    }
    should_log
}

fn format_peer_base_url(addr: &str, port: u16, path_prefix: Option<&str>) -> String {
    match normalize_path_prefix(path_prefix) {
        Some(prefix) => format!("http://{addr}:{port}{prefix}"),
        None => format!("http://{addr}:{port}"),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        LAN_DISCOVERY_GROUP, RECEIVE_ERROR_LOG_INTERVAL, receive_error_backoff_or_shutdown,
        should_log_receive_error,
    };
    use std::net::Ipv4Addr;
    use std::time::{Duration, Instant};
    use tokio::sync::watch;

    #[test]
    fn proprietary_multicast_group_is_not_reserved_mdns() {
        assert_ne!(LAN_DISCOVERY_GROUP, Ipv4Addr::new(224, 0, 0, 251));
        assert_eq!(LAN_DISCOVERY_GROUP.octets()[0], 239);
    }

    #[tokio::test]
    async fn receive_error_backoff_delays_retry() {
        let (_shutdown_tx, mut shutdown_rx) = watch::channel(false);

        let result = tokio::time::timeout(
            Duration::from_millis(20),
            receive_error_backoff_or_shutdown(&mut shutdown_rx),
        )
        .await;

        assert!(result.is_err(), "retry returned before the backoff elapsed");
    }

    #[tokio::test]
    async fn receive_error_backoff_yields_to_shutdown() {
        let (shutdown_tx, mut shutdown_rx) = watch::channel(false);
        shutdown_tx.send(true).unwrap();

        let result = tokio::time::timeout(
            Duration::from_millis(20),
            receive_error_backoff_or_shutdown(&mut shutdown_rx),
        )
        .await;

        assert!(result.unwrap());
    }

    #[test]
    fn receive_error_logs_are_rate_limited() {
        let now = Instant::now();
        let mut last_log = None;

        assert!(should_log_receive_error(&mut last_log, now));
        assert!(!should_log_receive_error(
            &mut last_log,
            now + Duration::from_secs(1)
        ));
        assert!(should_log_receive_error(
            &mut last_log,
            now + RECEIVE_ERROR_LOG_INTERVAL
        ));
    }
}
