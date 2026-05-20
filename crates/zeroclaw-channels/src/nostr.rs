use anyhow::{Context, Result};
use async_trait::async_trait;
use nostr_sdk::prelude::*;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use zeroclaw_api::channel::{Channel, ChannelMessage, SendMessage};

/// Protocol used by a sender, tracked so replies use the same protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NostrProtocol {
    Nip04,
    Nip17,
}

/// Nostr channel supporting NIP-04 (legacy) and NIP-17 (gift-wrapped) private messages.
/// Replies use the same protocol the sender used. Unsolicited sends default to NIP-17.
pub struct NostrChannel {
    client: Client,
    public_key: PublicKey,
    /// The alias key under `[channels.nostr.<alias>]` this handle is
    /// bound to. Used to scope peer-group writes and resolver lookups.
    alias: String,
    /// Resolves inbound external peers from canonical state at message-time.
    /// No cache (see AGENTS.md "ABSOLUTE RULE — SINGLE SOURCE OF TRUTH").
    peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
    /// Tracks last-seen protocol per sender pubkey so replies match.
    sender_protocols: Arc<RwLock<HashMap<PublicKey, NostrProtocol>>>,
}

impl NostrChannel {
    /// Create a new Nostr channel. Parses the private key, builds the
    /// client, adds relays, and connects. The client is reused for all
    /// subsequent send/listen/health_check calls.
    pub async fn new(
        private_key: &str,
        relays: Vec<String>,
        alias: impl Into<String>,
        peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
    ) -> Result<Self> {
        let keys = Keys::parse(private_key).context("Invalid Nostr private key")?;
        let public_key = keys.public_key();

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
            alias: alias.into(),
            peer_resolver,
            sender_protocols: Arc::new(RwLock::new(HashMap::new())),
        })
    }

    /// Return the alias under `[channels.nostr.<alias>]` that this
    /// channel handle is bound to.
    pub fn alias(&self) -> &str {
        &self.alias
    }

    /// Resolve allowed peers at message-time and check whether `pubkey` is
    /// authorized. Matches on the bare hex form (Nostr canonical wire
    /// representation); npub-prefixed bech32 entries in config are
    /// normalized to hex before comparison.
    fn is_pubkey_allowed(&self, pubkey: &PublicKey) -> bool {
        let peers: Vec<String> = (self.peer_resolver)()
            .into_iter()
            .map(|p| {
                if p == "*" {
                    p
                } else {
                    // Best-effort normalize: bech32 npub -> hex. Invalid
                    // entries fall through as-is and simply won't match.
                    PublicKey::parse(&p).map_or(p, |pk| pk.to_hex())
                }
            })
            .collect();
        crate::allowlist::is_user_allowed(
            &peers,
            &pubkey.to_hex(),
            crate::allowlist::Match::Sensitive,
        )
    }
}

impl ::zeroclaw_api::attribution::Attributable for NostrChannel {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Channel(::zeroclaw_api::attribution::ChannelKind::Nostr)
    }
    fn alias(&self) -> &str {
        &self.alias
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
                ::zeroclaw_log::record!(
                    DEBUG,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                    &format!(
                        "Sent NIP-17 message to {}",
                        recipient.to_bech32().unwrap_or_default()
                    )
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
                ::zeroclaw_log::record!(
                    DEBUG,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                    &format!(
                        "Sent NIP-04 message to {}",
                        recipient.to_bech32().unwrap_or_default()
                    )
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

        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
            &format!(
                "channel listening as {}",
                self.public_key.to_bech32().unwrap_or_default()
            )
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
                            if !self.is_pubkey_allowed(&event.pubkey) {
                                ::zeroclaw_log::record!(
                                    WARN,
                                    ::zeroclaw_log::Event::new(
                                        module_path!(),
                                        ::zeroclaw_log::Action::Note
                                    )
                                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                                    &format!(
                                        "Nostr: ignoring NIP-04 message from unauthorized pubkey: {}",
                                        event.pubkey.to_hex()
                                    )
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
                                    ::zeroclaw_log::record!(
                                        WARN,
                                        ::zeroclaw_log::Event::new(
                                            module_path!(),
                                            ::zeroclaw_log::Action::Note
                                        )
                                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                                        .with_attrs(
                                            ::serde_json::json!({"error": format!("{}", e)})
                                        ),
                                        "Failed to decrypt NIP-04 message"
                                    );
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
                                    if !self.is_pubkey_allowed(&sender) {
                                        ::zeroclaw_log::record!(
                                            WARN,
                                            ::zeroclaw_log::Event::new(
                                                module_path!(),
                                                ::zeroclaw_log::Action::Note
                                            )
                                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                                            &format!(
                                                "Nostr: ignoring NIP-17 message from unauthorized pubkey: {}",
                                                sender.to_hex()
                                            )
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
                                    ::zeroclaw_log::record!(
                                        WARN,
                                        ::zeroclaw_log::Event::new(
                                            module_path!(),
                                            ::zeroclaw_log::Action::Note
                                        )
                                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                                        .with_attrs(
                                            ::serde_json::json!({"error": format!("{}", e)})
                                        ),
                                        "Failed to unwrap NIP-17 gift wrap"
                                    );
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
                            channel_alias: Some(self.alias.clone()),
                            timestamp,
                            thread_ts: None,
                            interruption_scope_id: None,
                            attachments: vec![],
                        };
                        if tx.send(msg).await.is_err() {
                            ::zeroclaw_log::record!(
                                INFO,
                                ::zeroclaw_log::Event::new(
                                    module_path!(),
                                    ::zeroclaw_log::Action::Note
                                ),
                                "listener: message bus closed, stopping"
                            );
                            break;
                        }
                    }
                }
                RelayPoolNotification::Shutdown => {
                    ::zeroclaw_log::record!(
                        INFO,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                        "relay pool shut down"
                    );
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

    #[tokio::test]
    async fn empty_allowlist_denies_all() {
        let keys = Keys::generate();
        let ch = NostrChannel::new(
            &keys.secret_key().to_secret_hex(),
            vec![],
            "nostr_test_alias",
            Arc::new(Vec::new),
        )
        .await
        .unwrap();
        let pk = Keys::generate().public_key();
        assert!(!ch.is_pubkey_allowed(&pk));
    }

    #[tokio::test]
    async fn wildcard_allows_all() {
        let keys = Keys::generate();
        let ch = NostrChannel::new(
            &keys.secret_key().to_secret_hex(),
            vec![],
            "nostr_test_alias",
            Arc::new(|| vec!["*".into()]),
        )
        .await
        .unwrap();
        let pk = Keys::generate().public_key();
        assert!(ch.is_pubkey_allowed(&pk));
    }

    #[tokio::test]
    async fn specific_pubkeys_match_by_hex() {
        let k1 = Keys::generate();
        let k2 = Keys::generate();
        let k3 = Keys::generate();
        let allowed_hex = vec![k1.public_key().to_hex(), k2.public_key().to_hex()];
        let keys = Keys::generate();
        let ch = NostrChannel::new(
            &keys.secret_key().to_secret_hex(),
            vec![],
            "nostr_test_alias",
            Arc::new(move || allowed_hex.clone()),
        )
        .await
        .unwrap();
        assert!(ch.is_pubkey_allowed(&k1.public_key()));
        assert!(ch.is_pubkey_allowed(&k2.public_key()));
        assert!(!ch.is_pubkey_allowed(&k3.public_key()));
    }

    #[tokio::test]
    async fn npub_bech32_entry_matches_hex_pubkey() {
        // Resolver may return bech32 npub form; check it normalizes to hex.
        let k1 = Keys::generate();
        let npub = k1.public_key().to_bech32().unwrap();
        let keys = Keys::generate();
        let ch = NostrChannel::new(
            &keys.secret_key().to_secret_hex(),
            vec![],
            "nostr_test_alias",
            Arc::new(move || vec![npub.clone()]),
        )
        .await
        .unwrap();
        assert!(ch.is_pubkey_allowed(&k1.public_key()));
    }

    #[tokio::test]
    async fn invalid_resolver_entry_does_not_match() {
        let keys = Keys::generate();
        let ch = NostrChannel::new(
            &keys.secret_key().to_secret_hex(),
            vec![],
            "nostr_test_alias",
            Arc::new(|| vec!["not-a-valid-pubkey".into()]),
        )
        .await
        .unwrap();
        let pk = Keys::generate().public_key();
        assert!(!ch.is_pubkey_allowed(&pk));
    }

    #[tokio::test]
    async fn nostr_channel_name_is_nostr() {
        let keys = Keys::generate();
        let ch = NostrChannel::new(
            &keys.secret_key().to_secret_hex(),
            vec![],
            "nostr_test_alias",
            Arc::new(Vec::new),
        )
        .await
        .unwrap();
        assert_eq!(ch.name(), "nostr");
    }

    #[tokio::test]
    async fn nostr_channel_stores_parsed_keys() {
        let keys = Keys::generate();
        let ch = NostrChannel::new(
            &keys.secret_key().to_secret_hex(),
            vec![],
            "nostr_test_alias",
            Arc::new(Vec::new),
        )
        .await
        .unwrap();
        assert_eq!(ch.public_key, keys.public_key());
    }

    #[tokio::test]
    async fn new_rejects_invalid_key() {
        let result = NostrChannel::new(
            "not-a-valid-key",
            vec![],
            "nostr_test_alias",
            Arc::new(Vec::new),
        )
        .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn health_check_false_with_no_relays() {
        let keys = Keys::generate();
        let ch = NostrChannel::new(
            &keys.secret_key().to_secret_hex(),
            vec![],
            "nostr_test_alias",
            Arc::new(Vec::new),
        )
        .await
        .unwrap();
        assert!(!ch.health_check().await);
    }

    #[tokio::test]
    async fn default_protocol_is_nip17() {
        let keys = Keys::generate();
        let ch = NostrChannel::new(
            &keys.secret_key().to_secret_hex(),
            vec![],
            "nostr_test_alias",
            Arc::new(Vec::new),
        )
        .await
        .unwrap();
        let map = ch.sender_protocols.read().await;
        let pk = Keys::generate().public_key();
        assert_eq!(map.get(&pk), None);
    }

    #[tokio::test]
    async fn sender_protocol_tracks_updates() {
        let keys = Keys::generate();
        let ch = NostrChannel::new(
            &keys.secret_key().to_secret_hex(),
            vec![],
            "nostr_test_alias",
            Arc::new(Vec::new),
        )
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
