//! BitChat-compatible BLE mesh channel for ZeroClaw.
//!
//! Implements ZeroClaw's [`Channel`] trait on top of the BitChat binary protocol,
//! enabling agents to communicate over Bluetooth LE mesh networks — fully offline,
//! no internet required, with up to 7-hop multi-hop relay.
//!
//! ## WiFi Direct bridge
//!
//! The [`wifi_direct_tx`] function exposes the internal packet sender so the
//! ZeroClaw gateway can inject packets received from the Android
//! `WiFiDirectManager` via `POST /channels/bitchat_mesh/ingest`.
//! Injected packets are processed identically to BLE packets — Noise
//! decryption, multi-hop relay, and message delivery to the orchestrator.
//!
//! ## Usage (TOML config)
//!
//! ```toml
//! [channels_config.bitchat_mesh]
//! enabled = true
//! geohash = "dr5rs"        # neighborhood-level discovery
//! allowed_peers = ["*"]    # or list specific peer fingerprints
//! ```

pub mod ble_service;
pub mod noise_session;
pub mod packet;

use anyhow::{Context, Result};
use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::{OnceLock, OnceCell, broadcast, mpsc};
use zeroclaw_api::channel::{Channel, ChannelMessage, SendMessage};

use self::ble_service::{BleService, RawPacket};
use self::noise_session::NoiseSessionManager;
use self::packet::{BitchatPacket, PacketType};
use crate::agent_discovery::AgentAdvertisement;

// ─── Global WiFi Direct injection bus ────────────────────────────────────────
//
// This OnceLock is populated when BitChatMeshChannel is first created.
// The gateway's /channels/bitchat_mesh/ingest handler calls
// `inject_wifi_direct_packet()` to push WiFi Direct bytes into the same
// receive loop as BLE packets.

static WIFI_DIRECT_TX: OnceLock<broadcast::Sender<RawPacket>> = OnceLock::new();

/// Inject a raw BitChat packet received over WiFi Direct into the mesh channel.
///
/// Called by the ZeroClaw gateway handler `POST /channels/bitchat_mesh/ingest`.
/// Returns `false` if the BitChat mesh channel has not been initialized yet.
pub fn inject_wifi_direct_packet(from_peer_ip: String, bytes: Vec<u8>) -> bool {
    let Some(tx) = WIFI_DIRECT_TX.get() else {
        return false;
    };
    let raw = RawPacket {
        from_peripheral_id: from_peer_ip,
        bytes,
    };
    // Ignore send errors — a lagged receiver just catches up later.
    let _ = tx.send(raw);
    true
}

// ─── AllowedPeers ─────────────────────────────────────────────────────────────

/// Peer fingerprint allow-list — either `"*"` (allow all) or a set of hex peer-ID prefixes.
#[derive(Debug, Clone)]
enum AllowedPeers {
    Any,
    Set(Vec<String>),
}

impl AllowedPeers {
    fn parse(raw: &[String]) -> Self {
        if raw.is_empty() || raw.iter().any(|s| s == "*") {
            Self::Any
        } else {
            Self::Set(raw.to_vec())
        }
    }

    fn is_allowed(&self, peer_id_hex: &str) -> bool {
        match self {
            Self::Any => true,
            Self::Set(ids) => ids.iter().any(|id| peer_id_hex.starts_with(id.as_str())),
        }
    }
}

// ─── BitChatMeshChannel ───────────────────────────────────────────────────────

/// ZeroClaw channel backed by the BitChat BLE mesh protocol.
///
/// Implements both the incoming (`listen`) and outgoing (`send`) sides of
/// the ZeroClaw channel contract. The underlying BLE service is initialised
/// lazily on the first call to [`Channel::listen`] or [`Channel::send`],
/// allowing [`BitChatMeshChannel::new`] to be a synchronous constructor so
/// it can be called from [`build_channel_by_id`] in the orchestrator.
///
/// WiFi Direct packets are injected via the process-global [`WIFI_DIRECT_TX`] bus.
pub struct BitChatMeshChannel {
    /// Lazily-initialised BLE service (populated on first use).
    ble: OnceCell<Arc<BleService>>,
    allowed: AllowedPeers,
    /// Optional geohash for cross-advertising with Nostr agents.
    geohash: Option<String>,
}

impl BitChatMeshChannel {
    /// Create a new BitChat mesh channel (synchronous).
    ///
    /// BLE initialisation is deferred to the first call to [`Channel::listen`]
    /// or [`Channel::send`] — both of which are async — so this constructor is
    /// safe to call from synchronous orchestrator code.
    pub fn new(allowed_peers: &[String], geohash: Option<String>) -> Self {
        Self {
            ble: OnceCell::new(),
            allowed: AllowedPeers::parse(allowed_peers),
            geohash,
        }
    }

    /// Lazily initialise and return the BLE service.
    ///
    /// Idempotent — subsequent calls return the cached instance.
    async fn ble(&self) -> Result<&Arc<BleService>> {
        self.ble
            .get_or_try_init(|| async {
                let (noise_mgr, noise_pubkey) = NoiseSessionManager::generate()
                    .context("BitChatMesh: Noise keypair generation")?;
                let noise = Arc::new(noise_mgr);
                let local_peer_id = packet::peer_id_from_noise_key(
                    noise_pubkey[..32]
                        .try_into()
                        .context("BitChatMesh: pubkey too short")?,
                );

                let ble = BleService::new(Arc::clone(&noise), local_peer_id)
                    .await
                    .context("BitChatMesh: BleService init")?;

                // Register the WiFi Direct injection bus (idempotent — first one wins).
                let _ = WIFI_DIRECT_TX.set(ble.packet_tx.clone());

                tracing::info!(
                    "BitChatMesh: initialised with peer-id {:02x?}",
                    &local_peer_id
                );

                Ok::<Arc<BleService>, anyhow::Error>(Arc::new(ble))
            })
            .await
    }
}

#[async_trait]
impl Channel for BitChatMeshChannel {
    fn name(&self) -> &str {
        "bitchat-mesh"
    }

    /// Send a message to a specific peer or broadcast to the mesh.
    ///
    /// `message.recipient` is interpreted as a hex peer-ID or `"*"` / `""` for broadcast.
    async fn send(&self, message: &SendMessage) -> Result<()> {
        let ble = self.ble().await?;
        let payload = message.content.as_bytes().to_vec();

        if message.recipient == "*" || message.recipient.is_empty() {
            let pkt = BitchatPacket::new_message(ble.local_peer_id, None, payload);
            ble.broadcast(pkt).await
        } else {
            let peer_id_bytes = hex_to_peer_id(&message.recipient)?;
            let pkt = BitchatPacket::new_message(
                ble.local_peer_id,
                Some(peer_id_bytes),
                payload,
            );
            ble.send_to(peer_id_bytes, pkt).await
        }
    }

    /// Start the BLE scan loop and forward decoded messages to the orchestrator.
    ///
    /// Also handles:
    /// - Noise handshake packets (transparent, no agent involvement)
    /// - Agent advertisement packets (forwarded as `__agent_discovery__` messages)
    /// - Multi-hop relay (packets with TTL > 0 are re-broadcast)
    /// - WiFi Direct packets injected via [`inject_wifi_direct_packet`]
    async fn listen(&self, tx: mpsc::Sender<ChannelMessage>) -> Result<()> {
        let ble_ref = self.ble().await?;
        let ble_run = Arc::clone(ble_ref);

        // Spawn the BLE scan/connect loop as a background task
        tokio::spawn(async move {
            if let Err(e) = ble_run.run().await {
                tracing::error!("BitChatMesh BLE service exited: {e}");
            }
        });

        tracing::info!("BitChatMesh: listening on BLE mesh (+ WiFi Direct bridge)");

        let mut rx = ble_ref.subscribe();
        let noise = Arc::clone(&ble_ref.noise);
        let ble = Arc::clone(ble_ref);
        let local_peer_id = ble_ref.local_peer_id;

        loop {
            let raw: RawPacket = match rx.recv().await {
                Ok(r) => r,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!("BitChatMesh: dropped {n} packets (lagged)");
                    continue;
                }
                Err(_) => {
                    tracing::info!("BitChatMesh: broadcast channel closed");
                    break;
                }
            };

            // Decode the raw bytes
            let mut packet = match BitchatPacket::decode(&raw.bytes) {
                Ok(p) => p,
                Err(e) => {
                    tracing::debug!("BitChatMesh: decode error (likely fragment): {e}");
                    continue;
                }
            };

            // Ignore our own transmissions echoed back
            if packet.sender_id == local_peer_id {
                continue;
            }

            let sender_hex = format!("{:016x}", u64::from_be_bytes(packet.sender_id));

            // Check allow-list
            if !self.allowed.is_allowed(&sender_hex) {
                tracing::debug!("BitChatMesh: ignoring packet from {sender_hex} (not allowed)");
                continue;
            }

            // Handle Noise handshake packets
            match packet.packet_type {
                PacketType::NoiseHandshakeInit => {
                    match noise.receive_handshake_init(packet.sender_id, &packet.payload).await {
                        Ok(resp) => {
                            let resp_pkt = BitchatPacket {
                                version: 1,
                                packet_type: PacketType::NoiseHandshakeResp,
                                ttl: 1,
                                timestamp_ms: now_ms(),
                                sender_id: local_peer_id,
                                recipient_id: Some(packet.sender_id),
                                payload: resp,
                                signature: None,
                            };
                            ble.send_to(packet.sender_id, resp_pkt).await.ok();
                        }
                        Err(e) => tracing::warn!("BitChatMesh: handshake init failed: {e}"),
                    }
                    continue;
                }
                PacketType::NoiseHandshakeResp => {
                    match noise.receive_handshake_response(packet.sender_id, &packet.payload).await {
                        Ok(final_msg) => {
                            let final_pkt = BitchatPacket {
                                version: 1,
                                packet_type: PacketType::NoiseHandshakeInit,
                                ttl: 1,
                                timestamp_ms: now_ms(),
                                sender_id: local_peer_id,
                                recipient_id: Some(packet.sender_id),
                                payload: final_msg,
                                signature: None,
                            };
                            ble.send_to(packet.sender_id, final_pkt).await.ok();
                        }
                        Err(e) => tracing::warn!("BitChatMesh: handshake response failed: {e}"),
                    }
                    continue;
                }
                _ => {}
            }

            // Attempt Noise decryption if session established
            let decrypted_payload = if noise.is_established(packet.sender_id).await {
                match noise.decrypt(packet.sender_id, &packet.payload).await {
                    Ok(plain) => plain,
                    Err(e) => {
                        tracing::warn!("BitChatMesh: decrypt from {sender_hex} failed: {e}");
                        packet.payload.clone()
                    }
                }
            } else {
                packet.payload.clone()
            };

            // Multi-hop relay: re-broadcast if TTL allows and not directed to us
            let directed_to_us = packet
                .recipient_id
                .map_or(false, |rid| rid == local_peer_id);
            if !directed_to_us && packet.try_relay() {
                let relay_pkt = BitchatPacket {
                    payload: packet.payload.clone(),
                    ..packet.clone()
                };
                ble.broadcast(relay_pkt).await.ok();
            }

            // Build a ChannelMessage for the orchestrator
            let content = match packet.packet_type {
                PacketType::AgentAdvertisement => {
                    if let Ok(text) = std::str::from_utf8(&decrypted_payload) {
                        if let Some(ad) = AgentAdvertisement::from_json(text) {
                            tracing::info!(
                                "BitChatMesh: discovered agent '{}' on geohash #{} (online: {})",
                                ad.name,
                                ad.geohash,
                                ad.is_online,
                            );
                        }
                        format!("__agent_discovery__ {text}")
                    } else {
                        continue;
                    }
                }
                PacketType::Message => {
                    match std::str::from_utf8(&decrypted_payload) {
                        Ok(s) => s.to_string(),
                        Err(_) => {
                            tracing::debug!("BitChatMesh: non-UTF-8 message from {sender_hex}");
                            continue;
                        }
                    }
                }
                _ => continue,
            };

            let msg = ChannelMessage {
                id: format!("{sender_hex}_{}", packet.timestamp_ms),
                sender: sender_hex.clone(),
                reply_target: sender_hex,
                content,
                channel: "bitchat-mesh".to_string(),
                timestamp: packet.timestamp_ms / 1000,
                thread_ts: None,
                interruption_scope_id: None,
                attachments: vec![],
            };

            if tx.send(msg).await.is_err() {
                tracing::info!("BitChatMesh: orchestrator channel closed, stopping");
                break;
            }
        }

        Ok(())
    }

    async fn health_check(&self) -> bool {
        match self.ble().await {
            Ok(ble) => ble.connected_peer_count().await > 0,
            Err(_) => false,
        }
    }
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

fn hex_to_peer_id(hex: &str) -> Result<[u8; 8]> {
    let trimmed = hex.trim_start_matches("0x");
    let padded = format!("{:0>16}", trimmed);
    let n = u64::from_str_radix(&padded, 16)
        .with_context(|| format!("Invalid peer-ID hex: {hex}"))?;
    Ok(n.to_be_bytes())
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_to_peer_id_roundtrip() {
        let id = [0xDE, 0xAD, 0xBE, 0xEF, 0x01, 0x02, 0x03, 0x04];
        let hex = format!("{:016x}", u64::from_be_bytes(id));
        assert_eq!(hex_to_peer_id(&hex).unwrap(), id);
    }

    #[test]
    fn allowed_peers_wildcard() {
        let ap = AllowedPeers::parse(&["*".to_string()]);
        assert!(ap.is_allowed("deadbeef01020304"));
    }

    #[test]
    fn allowed_peers_specific() {
        let ap = AllowedPeers::parse(&["deadbeef".to_string()]);
        assert!(ap.is_allowed("deadbeef01020304"));
        assert!(!ap.is_allowed("cafebabe01020304"));
    }
}
