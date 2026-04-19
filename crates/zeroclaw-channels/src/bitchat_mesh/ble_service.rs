//! BLE mesh service for BitChat-compatible peer communication.
//!
//! Uses `btleplug` for cross-platform Bluetooth LE (Linux/macOS/Windows/Android).
//! Simultaneously acts as a **central** (scanner/connector) and a **peripheral**
//! (advertiser) — matching BitChat's dual-role BLE architecture.
//!
//! ## WiFi Direct injection
//!
//! The same [`broadcast::Sender<RawPacket>`] used for BLE packets is accessible
//! via [`BleService::packet_tx`] so the ZeroClaw gateway's
//! `/channels/bitchat_mesh/ingest` endpoint can inject WiFi Direct packets
//! directly into the same receive loop — zero code duplication.

use anyhow::{Context, Result};
use btleplug::api::{
    Central, CentralEvent, Manager as _, Peripheral as _, ScanFilter, WriteType,
};
use btleplug::platform::{Manager, Peripheral};
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::{Mutex, broadcast};
use tokio::time::{Duration, sleep};
use uuid::Uuid;

use super::packet::{
    BITCHAT_CHAR_UUID, BITCHAT_SERVICE_UUID, BitchatPacket, DEFAULT_MTU, PacketType,
};
use super::noise_session::NoiseSessionManager;

/// Broadcast channel capacity for incoming raw packets.
const INCOMING_CAPACITY: usize = 256;
/// How often to re-scan for new BLE peers.
const RESCAN_INTERVAL_SECS: u64 = 30;
/// Max fragment size for chunking large packets over BLE.
const FRAGMENT_SIZE: usize = DEFAULT_MTU;

/// An incoming raw BLE (or WiFi Direct) packet before Noise decryption.
#[derive(Debug, Clone)]
pub struct RawPacket {
    /// Source identifier — either a BLE peripheral ID or a WiFi Direct peer IP.
    pub from_peripheral_id: String,
    /// Raw encoded bytes (may be a fragment for BLE).
    pub bytes: Vec<u8>,
}

/// Manages BLE scanning, connection, characteristic subscriptions,
/// and raw packet delivery/relay.
pub struct BleService {
    /// btleplug manager (platform-specific BLE adapter).
    manager: Manager,
    /// UUIDs of connected peripherals → their characteristic handle.
    peers: Arc<Mutex<HashMap<String, Peripheral>>>,
    /// Broadcast sender for incoming packets.
    ///
    /// **Also exposed as [`packet_tx`] for WiFi Direct injection from the gateway.**
    pub packet_tx: broadcast::Sender<RawPacket>,
    /// Noise session manager (shared with BitChatMeshChannel).
    pub noise: Arc<NoiseSessionManager>,
    /// Our own peer-ID (derived from Noise static pubkey).
    pub local_peer_id: [u8; 8],
}

impl BleService {
    /// Create a new BleService.
    pub async fn new(
        noise: Arc<NoiseSessionManager>,
        local_peer_id: [u8; 8],
    ) -> Result<Self> {
        let manager = Manager::new().await.context("BLE: failed to create manager")?;
        let (packet_tx, _) = broadcast::channel(INCOMING_CAPACITY);

        Ok(Self {
            manager,
            peers: Arc::new(Mutex::new(HashMap::new())),
            packet_tx,
            noise,
            local_peer_id,
        })
    }

    /// Subscribe to incoming raw packets from the BLE mesh (or WiFi Direct bridge).
    pub fn subscribe(&self) -> broadcast::Receiver<RawPacket> {
        self.packet_tx.subscribe()
    }

    /// Number of currently connected BLE peers.
    pub async fn connected_peer_count(&self) -> usize {
        self.peers.lock().await.len()
    }

    /// Start the BLE scan loop. Runs indefinitely — call in a dedicated Tokio task.
    pub async fn run(&self) -> Result<()> {
        let adapters = self
            .manager
            .adapters()
            .await
            .context("BLE: no adapters found")?;

        let adapter = adapters.into_iter().next().context("BLE: no adapter")?;
        tracing::info!("BLE: using adapter {:?}", adapter.adapter_info().await);

        let filter = ScanFilter {
            services: vec![Uuid::from_str(BITCHAT_SERVICE_UUID).expect("valid UUID")],
        };
        adapter.start_scan(filter).await.context("BLE: scan failed")?;
        tracing::info!("BLE: scanning for BitChat peers (service {BITCHAT_SERVICE_UUID})");

        let mut events = adapter.events().await.context("BLE: cannot subscribe to events")?;
        let char_uuid = Uuid::from_str(BITCHAT_CHAR_UUID).expect("valid UUID");

        loop {
            tokio::select! {
                Some(event) = async { events.next().await } => {
                    match event {
                        CentralEvent::DeviceDiscovered(id) => {
                            let peers = Arc::clone(&self.peers);
                            let tx = self.packet_tx.clone();
                            let noise = Arc::clone(&self.noise);
                            let local_peer_id = self.local_peer_id;

                            if let Ok(peripheral) = adapter.peripheral(&id).await {
                                tokio::spawn(async move {
                                    if let Err(e) = connect_and_subscribe(
                                        peripheral, char_uuid, peers, tx, noise, local_peer_id,
                                    ).await {
                                        tracing::warn!("BLE: connect/subscribe failed: {e}");
                                    }
                                });
                            }
                        }
                        CentralEvent::DeviceDisconnected(id) => {
                            let id_str = id.to_string();
                            self.peers.lock().await.remove(&id_str);
                            tracing::info!("BLE: peer disconnected: {id_str}");
                        }
                        _ => {}
                    }
                }
                _ = sleep(Duration::from_secs(RESCAN_INTERVAL_SECS)) => {
                    tracing::debug!("BLE: periodic rescan");
                    let _ = adapter.start_scan(ScanFilter {
                        services: vec![Uuid::from_str(BITCHAT_SERVICE_UUID).expect("valid UUID")],
                    }).await;
                }
            }
        }
    }

    /// Broadcast a packet to all connected BLE peers.
    ///
    /// Encrypts payload with Noise if a session is established.
    pub async fn broadcast(&self, packet: BitchatPacket) -> Result<()> {
        let encoded = packet.encode();
        let peers = self.peers.lock().await;

        if peers.is_empty() {
            tracing::debug!("BLE: no peers connected, packet dropped");
            return Ok(());
        }

        for (peer_id_str, peripheral) in peers.iter() {
            let peer_id_bytes = parse_peer_id_str(peer_id_str);
            let payload_bytes = if let Some(pid) = peer_id_bytes {
                if self.noise.is_established(pid).await {
                    match self.noise.encrypt(pid, &encoded).await {
                        Ok(enc) => enc,
                        Err(e) => {
                            tracing::warn!("BLE: encrypt for {peer_id_str} failed: {e}");
                            encoded.clone()
                        }
                    }
                } else {
                    encoded.clone()
                }
            } else {
                encoded.clone()
            };

            let char_uuid = Uuid::from_str(BITCHAT_CHAR_UUID).expect("valid UUID");
            let characteristics = peripheral.characteristics();
            let Some(char_handle) = characteristics.iter().find(|c| c.uuid == char_uuid) else {
                tracing::warn!("BLE: characteristic not found on {peer_id_str}");
                continue;
            };

            for chunk in payload_bytes.chunks(FRAGMENT_SIZE) {
                if let Err(e) = peripheral
                    .write(char_handle, chunk, WriteType::WithoutResponse)
                    .await
                {
                    tracing::warn!("BLE: write to {peer_id_str} failed: {e}");
                }
            }
        }

        Ok(())
    }

    /// Send a directed packet to a specific peer by peer-ID.
    ///
    /// Falls through to broadcast if the peer is not directly connected.
    pub async fn send_to(&self, peer_id: [u8; 8], packet: BitchatPacket) -> Result<()> {
        let peer_id_str = format!(
            "{:02x}",
            peer_id.iter().fold(0u64, |acc, b| (acc << 8) | *b as u64)
        );
        let peers = self.peers.lock().await;

        let target = peers
            .iter()
            .find(|(id, _)| id.as_str() == peer_id_str || id.ends_with(&peer_id_str));

        if let Some((_, peripheral)) = target {
            let encoded = packet.encode();
            let payload = if self.noise.is_established(peer_id).await {
                self.noise.encrypt(peer_id, &encoded).await.unwrap_or(encoded)
            } else {
                encoded
            };

            let char_uuid = Uuid::from_str(BITCHAT_CHAR_UUID).expect("valid UUID");
            let characteristics = peripheral.characteristics();
            if let Some(ch) = characteristics.iter().find(|c| c.uuid == char_uuid) {
                for chunk in payload.chunks(FRAGMENT_SIZE) {
                    peripheral.write(ch, chunk, WriteType::WithoutResponse).await.ok();
                }
            }
        } else {
            drop(peers);
            self.broadcast(packet).await?;
        }

        Ok(())
    }
}

// ─── Internal helpers ──────────────────────────────────────────────────────────

async fn connect_and_subscribe(
    peripheral: Peripheral,
    char_uuid: Uuid,
    peers: Arc<Mutex<HashMap<String, Peripheral>>>,
    tx: broadcast::Sender<RawPacket>,
    noise: Arc<NoiseSessionManager>,
    local_peer_id: [u8; 8],
) -> Result<()> {
    let id = peripheral.id().to_string();

    if peers.lock().await.contains_key(&id) {
        return Ok(());
    }

    peripheral.connect().await.context("BLE: connect failed")?;
    peripheral.discover_services().await.context("BLE: discover services failed")?;

    let characteristics = peripheral.characteristics();
    let char_handle = characteristics
        .iter()
        .find(|c| c.uuid == char_uuid)
        .with_context(|| format!("BLE: characteristic {char_uuid} not found on {id}"))?
        .clone();

    peripheral.subscribe(&char_handle).await.context("BLE: subscribe failed")?;
    tracing::info!("BLE: connected and subscribed to peer {id}");
    peers.lock().await.insert(id.clone(), peripheral.clone());

    // Initiate Noise handshake
    if let Some(peer_bytes) = parse_peer_id_str(&id) {
        if !noise.has_session(peer_bytes).await {
            match noise.initiate_handshake(peer_bytes).await {
                Ok(handshake_msg) => {
                    let hs_pkt = BitchatPacket {
                        version: 1,
                        packet_type: PacketType::NoiseHandshakeInit,
                        ttl: 1,
                        timestamp_ms: now_ms(),
                        sender_id: local_peer_id,
                        recipient_id: Some(peer_bytes),
                        payload: handshake_msg,
                        signature: None,
                    };
                    let encoded = hs_pkt.encode();
                    for chunk in encoded.chunks(DEFAULT_MTU) {
                        peripheral.write(&char_handle, chunk, WriteType::WithoutResponse).await.ok();
                    }
                    tracing::debug!("BLE: sent Noise handshake init to {id}");
                }
                Err(e) => tracing::warn!("BLE: handshake init failed: {e}"),
            }
        }
    }

    let mut notif_stream = peripheral
        .notifications()
        .await
        .context("BLE: notification stream unavailable")?;

    while let Some(notification) = notif_stream.next().await {
        if notification.uuid == char_uuid {
            let raw = RawPacket {
                from_peripheral_id: id.clone(),
                bytes: notification.value,
            };
            let _ = tx.send(raw);
        }
    }

    tracing::info!("BLE: notification stream ended for peer {id}");
    peers.lock().await.remove(&id);
    Ok(())
}

fn parse_peer_id_str(id_str: &str) -> Option<[u8; 8]> {
    let hash = id_str
        .bytes()
        .fold(0u64, |acc, b| acc.wrapping_mul(31).wrapping_add(b as u64));
    Some(hash.to_be_bytes())
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

use futures::StreamExt as _;
