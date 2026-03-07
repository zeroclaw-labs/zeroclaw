use crate::channels::traits::{Channel, ChannelMessage, SendMessage};
use anyhow::{Context, Result};
use async_trait::async_trait;
use nostr_sdk::prelude::*;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Protocol used by a sender, tracked so replies use the same protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NostrProtocol {
    Nip04,
    Nip17,
}

/// Whether to allow all senders (wildcard) or only specific public keys.
#[derive(Debug, Clone)]
enum AllowList {
    /// "*" — accept messages from any pubkey.
    Any,
    /// Accept only from these specific pubkeys.
    Set(Vec<PublicKey>),
}

impl AllowList {
    /// Parse the raw config strings into a typed allow list.
    /// Empty list means deny-all. A single `"*"` means allow-all.
    fn parse(raw: &[String]) -> Result<Self> {
        if raw.is_empty() {
            return Ok(Self::Set(Vec::new())); // deny-all
        }
        if raw.iter().any(|p| p == "*") {
            return Ok(Self::Any);
        }
        let mut keys = Vec::with_capacity(raw.len());
        for s in raw {
            keys.push(PublicKey::parse(s).with_context(|| format!("Invalid allowed pubkey: {s}"))?);
        }
        Ok(Self::Set(keys))
    }

    fn is_allowed(&self, pubkey: &PublicKey) -> bool {
        match self {
            Self::Any => true,
            Self::Set(keys) => keys.iter().any(|k| k == pubkey),
        }
    }
}

/// Nostr channel supporting NIP-04 (legacy) and NIP-17 (gift-wrapped) private messages.
/// Replies use the same protocol the sender used. Unsolicited sends default to NIP-17.
pub struct NostrChannel {
    client: Client,
    public_key: PublicKey,
    allowed: AllowList,
    /// Tracks last-seen protocol per sender pubkey so replies match.
    sender_protocols: Arc<RwLock<HashMap<PublicKey, NostrProtocol>>>,
}

impl NostrChannel {
    /// Create a new Nostr channel. Parses keys and allowed pubkeys, builds the
    /// client, adds relays, and connects. The client is reused for all
    /// subsequent send/listen/health_check calls.
    pub async fn new(
        private_key: &str,
        relays: Vec<String>,
        allowed_pubkeys: &[String],
    ) -> Result<Self> {
        let keys = Keys::parse(private_key).context("Invalid Nostr private key")?;
        let public_key = keys.public_key();
        let allowed = AllowList::parse(allowed_pubkeys)?;

        let client = Client::builder().signer(keys).build();
        for relay in &relays {
            client
                .add_relay(relay.as_str())
                .await
                .with_context(|| format!("Failed to add relay: {relay}"))?;
        }
        client.connect().await;

        Ok(Self {
            client,
            public_key,
            allowed,
            sender_protocols: Arc::new(RwLock::new(HashMap::new())),
        })
    }
}

#[async_trait]
impl Channel for NostrChannel {
    fn name(&self) -> &str {
        "nostr"
    }

    async fn send(&self, message: &SendMessage) -> Result<()> {
        let recipient =
            PublicKey::parse(&message.recipient).context("Invalid recipient Nostr public key")?;

        // Look up which protocol this recipient last used; default to NIP-17
        let protocol = {
            let map = self.sender_protocols.read().await;
            map.get(&recipient).copied().unwrap_or(NostrProtocol::Nip17)
        };

        match protocol {
            NostrProtocol::Nip17 => {
                // NIP-17: gift-wrapped private message
                self.client
                    .send_private_msg(recipient, &message.content, None)
                    .await
                    .context("Failed to send NIP-17 message")?;
                tracing::debug!(
                    "Sent NIP-17 message to {}",
                    recipient.to_bech32().unwrap_or_default()
                );
            }
            NostrProtocol::Nip04 => {
                // NIP-04: legacy encrypted DM (kind 4)
                let signer = self.client.signer().await.context("No signer on client")?;
                let encrypted = signer
                    .nip04_encrypt(&recipient, &message.content)
                    .await
                    .context("NIP-04 encryption failed")?;
                let builder = EventBuilder::new(Kind::EncryptedDirectMessage, encrypted)
                    .tag(Tag::public_key(recipient));
                self.client
                    .send_event_builder(builder)
                    .await
                    .context("Failed to send NIP-04 message")?;
                tracing::debug!(
                    "Sent NIP-04 message to {}",
                    recipient.to_bech32().unwrap_or_default()
                );
            }
        }

        Ok(())
    }

    async fn listen(&self, tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> Result<()> {
        let listen_start = Timestamp::now();

        // Subscribe to both NIP-04 (kind 4) and NIP-17/gift-wrap (kind 1059).
        // Use limit(10) for relay compatibility; events from before listen_start
        // are skipped below using the real message timestamp (rumor.created_at
        // for NIP-17, since the outer gift-wrap timestamp is jittered).
        let filter = Filter::new()
            .pubkey(self.public_key)
            .kinds(vec![Kind::EncryptedDirectMessage, Kind::GiftWrap])
            .limit(10);

        self.client
            .subscribe(filter, None)
            .await
            .context("Failed to subscribe to Nostr events")?;

        tracing::info!(
            "Nostr channel listening as {}",
            self.public_key.to_bech32().unwrap_or_default()
        );

        let sender_protocols = Arc::clone(&self.sender_protocols);
        let signer = self.client.signer().await.context("No signer on client")?;

        loop {
            let notification = self
                .client
                .notifications()
                .recv()
                .await
                .context("Notification channel closed")?;

            match notification {
                RelayPoolNotification::Event { event, .. } => {
                    let result = match event.kind {
                        Kind::EncryptedDirectMessage => {
                            // NIP-04: created_at is the real timestamp (no jitter)
                            if event.created_at < listen_start {
                                continue;
                            }
                            if !self.allowed.is_allowed(&event.pubkey) {
                                tracing::warn!(
                                    "Nostr: ignoring NIP-04 message from unauthorized pubkey: {}",
                                    event.pubkey.to_hex()
                                );
                                continue;
                            }
                            match signer.nip04_decrypt(&event.pubkey, &event.content).await {
                                Ok(content) => {
                                    let sender = event.pubkey;
                                    sender_protocols
                                        .write()
                                        .await
                                        .insert(sender, NostrProtocol::Nip04);
                                    Some((
                                        event.id.to_hex(),
                                        sender.to_hex(),
                                        content,
                                        event.created_at.as_secs(),
                                    ))
                                }
                                Err(e) => {
                                    tracing::warn!("Failed to decrypt NIP-04 message: {e}");
                                    None
                                }
                            }
                        }
                        Kind::GiftWrap => {
                            // NIP-17: unwrap first, then check the rumor's created_at
                            // (the outer gift-wrap timestamp is jittered for privacy)
                            match self.client.unwrap_gift_wrap(&event).await {
                                Ok(unwrapped) => {
                                    let rumor = unwrapped.rumor;
                                    if rumor.created_at < listen_start {
                                        continue;
                                    }
                                    let sender = rumor.pubkey;
                                    if !self.allowed.is_allowed(&sender) {
                                        tracing::warn!(
                                            "Nostr: ignoring NIP-17 message from unauthorized pubkey: {}",
                                            sender.to_hex()
                                        );
                                        continue;
                                    }
                                    sender_protocols
                                        .write()
                                        .await
                                        .insert(sender, NostrProtocol::Nip17);
                                    Some((
                                        event.id.to_hex(),
                                        sender.to_hex(),
                                        rumor.content.clone(),
                                        rumor.created_at.as_secs(),
                                    ))
                                }
                                Err(e) => {
                                    tracing::warn!("Failed to unwrap NIP-17 gift wrap: {e}");
                                    None
                                }
                            }
                        }
                        _ => None,
                    };

                    if let Some((id, sender_hex, content, timestamp)) = result {
                        let msg = ChannelMessage {
                            id,
                            sender: sender_hex.clone(),
                            reply_target: sender_hex,
                            content,
                            channel: "nostr".to_string(),
                            timestamp,
                            thread_ts: None,
                        };
                        if tx.send(msg).await.is_err() {
                            tracing::info!("Nostr listener: message bus closed, stopping");
                            break;
                        }
                    }
                }
                RelayPoolNotification::Shutdown => {
                    tracing::info!("Nostr relay pool shut down");
                    break;
                }
                RelayPoolNotification::Message { .. } => {}
            }
        }

        Ok(())
    }

    async fn health_check(&self) -> bool {
        self.client
            .relays()
            .await
            .values()
            .any(|r| r.is_connected())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allow_list_empty_denies_all() {
        let al = AllowList::parse(&[]).unwrap();
        let pk = Keys::generate().public_key();
        assert!(!al.is_allowed(&pk));
    }

    #[test]
    fn allow_list_wildcard_allows_all() {
        let al = AllowList::parse(&["*".to_string()]).unwrap();
        let pk = Keys::generate().public_key();
        assert!(al.is_allowed(&pk));
    }

    #[test]
    fn allow_list_specific_pubkeys() {
        let k1 = Keys::generate();
        let k2 = Keys::generate();
        let k3 = Keys::generate();
        let al = AllowList::parse(&[k1.public_key().to_hex(), k2.public_key().to_hex()]).unwrap();
        assert!(al.is_allowed(&k1.public_key()));
        assert!(al.is_allowed(&k2.public_key()));
        assert!(!al.is_allowed(&k3.public_key()));
    }

    #[test]
    fn allow_list_rejects_invalid_key() {
        let result = AllowList::parse(&["not-a-valid-pubkey".to_string()]);
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn nostr_channel_name_is_nostr() {
        let keys = Keys::generate();
        let ch = NostrChannel::new(&keys.secret_key().to_secret_hex(), vec![], &[])
            .await
            .unwrap();
        assert_eq!(ch.name(), "nostr");
    }

    #[tokio::test]
    async fn nostr_channel_stores_parsed_keys() {
        let keys = Keys::generate();
        let ch = NostrChannel::new(&keys.secret_key().to_secret_hex(), vec![], &[])
            .await
            .unwrap();
        assert_eq!(ch.public_key, keys.public_key());
    }

    #[tokio::test]
    async fn new_rejects_invalid_key() {
        let result = NostrChannel::new("not-a-valid-key", vec![], &[]).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn new_rejects_invalid_allowed_pubkey() {
        let keys = Keys::generate();
        let result = NostrChannel::new(
            &keys.secret_key().to_secret_hex(),
            vec![],
            &["bad-pubkey".to_string()],
        )
        .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn health_check_false_with_no_relays() {
        let keys = Keys::generate();
        let ch = NostrChannel::new(&keys.secret_key().to_secret_hex(), vec![], &[])
            .await
            .unwrap();
        assert!(!ch.health_check().await);
    }

    #[tokio::test]
    async fn default_protocol_is_nip17() {
        let keys = Keys::generate();
        let ch = NostrChannel::new(&keys.secret_key().to_secret_hex(), vec![], &[])
            .await
            .unwrap();
        let map = ch.sender_protocols.read().await;
        let pk = Keys::generate().public_key();
        assert_eq!(map.get(&pk), None);
    }

    #[tokio::test]
    async fn sender_protocol_tracks_updates() {
        let keys = Keys::generate();
        let ch = NostrChannel::new(&keys.secret_key().to_secret_hex(), vec![], &[])
            .await
            .unwrap();
        let pk = Keys::generate().public_key();
        {
            let mut map = ch.sender_protocols.write().await;
            map.insert(pk, NostrProtocol::Nip04);
        }
        {
            let map = ch.sender_protocols.read().await;
            assert_eq!(map.get(&pk), Some(&NostrProtocol::Nip04));
        }
        {
            let mut map = ch.sender_protocols.write().await;
            map.insert(pk, NostrProtocol::Nip17);
        }
        {
            let map = ch.sender_protocols.read().await;
            assert_eq!(map.get(&pk), Some(&NostrProtocol::Nip17));
        }
    }
}
