//! Native XMPP transport. The shared runtime owns turns and reconnection.
use anyhow::{Context, bail, ensure};
use async_trait::async_trait;
use futures_util::{Sink, SinkExt, Stream, StreamExt};
use rusqlite::{Connection, OpenFlags, params};
use std::{
    borrow::Cow,
    fs,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};
use std::{
    pin::Pin,
    task::{Context as TaskContext, Poll},
};
use tokio::io::{AsyncBufRead, AsyncWrite};
use tokio::sync::{mpsc, oneshot};
use tokio_xmpp::{
    connect::{DirectTlsServerConnector, DnsConfig, ServerConnector, StartTlsServerConnector},
    parsers::{
        bind::{BindQuery, BindResponse},
        iq::Iq,
        message::{Id, Lang, Message, MessageType},
        ns,
        presence::{Presence, Type as PresenceType},
        stanza::Stanza,
    },
    xmlstream::{ReadError, StreamHeader, Timeouts, XmlStream},
};
use zeroclaw_api::channel::{Channel, ChannelMessage, SendMessage};
use zeroclaw_config::schema::{XmppConfig, XmppProtocol};

// XmlStream serializes each borrowed stanza synchronously in start_send.
// Adapt ownership at that boundary so split I/O does not retain a borrowed stanza.
struct OwnedStanzaStream<Io>(XmlStream<Io, Stanza>);
impl<Io: AsyncBufRead + AsyncWrite + Unpin> Stream for OwnedStanzaStream<Io> {
    type Item = <XmlStream<Io, Stanza> as Stream>::Item;
    fn poll_next(self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.get_mut().0).poll_next(cx)
    }
}
impl<Io: AsyncBufRead + AsyncWrite + Unpin> Sink<Stanza> for OwnedStanzaStream<Io> {
    type Error = std::io::Error;
    fn poll_ready(self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<Result<(), Self::Error>> {
        <XmlStream<Io, Stanza> as Sink<&Stanza>>::poll_ready(Pin::new(&mut self.get_mut().0), cx)
    }
    fn start_send(self: Pin<&mut Self>, stanza: Stanza) -> Result<(), Self::Error> {
        <XmlStream<Io, Stanza> as Sink<&Stanza>>::start_send(
            Pin::new(&mut self.get_mut().0),
            &stanza,
        )
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<Result<(), Self::Error>> {
        <XmlStream<Io, Stanza> as Sink<&Stanza>>::poll_flush(Pin::new(&mut self.get_mut().0), cx)
    }
    fn poll_close(self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<Result<(), Self::Error>> {
        <XmlStream<Io, Stanza> as Sink<&Stanza>>::poll_close(Pin::new(&mut self.get_mut().0), cx)
    }
}

#[cfg(target_os = "linux")]
type CryptoOwner = Arc<Mutex<crate::xmpp_omemo::channel_state::ChannelState>>;
#[cfg(target_os = "linux")]
#[path = "xmpp_native_inbox_vault.rs"]
pub(crate) mod vault;
#[cfg(target_os = "linux")]
use vault::VaultInboxCipher;

// A queued request and its socket write share the caller's absolute deadline.
// Cancelling an in-progress write ends the listener, dropping the socket; its
// durable reservation remains uncertain and must not be encrypted a second time.
async fn write_before_deadline<S>(
    sink: &mut S,
    stanza: Stanza,
    result: &mut oneshot::Sender<Result<(), String>>,
    deadline: tokio::time::Instant,
) -> anyhow::Result<()>
where
    S: Sink<Stanza> + Unpin,
    S::Error: std::fmt::Display,
{
    ensure!(
        !result.is_closed() && tokio::time::Instant::now() < deadline,
        "XMPP send expired before socket write"
    );
    tokio::select! {
        biased;
        _ = result.closed() => bail!("XMPP send cancelled during socket write; outcome unknown"),
        sent = tokio::time::timeout_at(deadline, sink.send(stanza)) => {
            sent.context("XMPP socket write deadline exceeded; outcome unknown")?.map_err(|error|{ ::zeroclaw_log::record!(ERROR, ::zeroclaw_log::Event::new(module_path!(),::zeroclaw_log::Action::Fail), "XMPP socket write failed: {error}"); anyhow::Error::msg(format!("XMPP socket write failed: {error}")) })
        }
    }
}

type PinResolver = Arc<dyn Fn() -> Vec<String> + Send + Sync>;
type PeerResolver = Arc<dyn Fn() -> Vec<String> + Send + Sync>;
enum Outbound {
    Message {
        recipient: String,
        content: String,
        result: oneshot::Sender<Result<(), String>>,
        deadline: tokio::time::Instant,
    },
    Control(Box<Stanza>),
}
pub struct XmppChannel {
    config: XmppConfig,
    alias: String,
    storage: PathBuf,
    peers: PeerResolver,
    pins: Option<PinResolver>,
    inbox_vault: Option<PathBuf>,
    #[cfg(target_os = "linux")]
    encrypted: Mutex<Option<CryptoOwner>>,
    // Presence of this sender is the connection's single online-state fact.
    outbound: Mutex<Option<mpsc::Sender<Outbound>>>,
}
impl XmppChannel {
    pub fn new(config: XmppConfig, alias: String, data_dir: PathBuf, peers: PeerResolver) -> Self {
        Self {
            config,
            storage: data_dir.join("channels").join("xmpp").join(&alias),
            alias,
            peers,
            pins: None,
            inbox_vault: None,
            #[cfg(target_os = "linux")]
            encrypted: Mutex::new(None),
            outbound: Mutex::new(None),
        }
    }
    pub fn with_pin_resolver(mut self, pins: PinResolver) -> Self {
        self.pins = Some(pins);
        self
    }
    pub fn with_inbox_vault(mut self, root: Option<PathBuf>) -> Self {
        self.inbox_vault = root;
        self
    }
    fn enqueue_ping_reply(
        &self,
        from: Option<jid::Jid>,
        id: String,
        requests: &mpsc::Sender<Outbound>,
    ) -> anyhow::Result<()> {
        let own = self.config.username.parse::<jid::BareJid>()?;
        let authorized = from.as_ref().is_none_or(|sender| {
            let bare = sender.to_bare();
            bare == own
                || (bare.node().is_none() && bare.domain() == own.domain())
                || self.allowed(&bare.to_string())
        });
        if authorized {
            let response = Outbound::Control(Box::new(Stanza::Iq(Iq::Result {
                from: None,
                to: from,
                id,
                payload: None,
            })));
            if requests.try_send(response).is_err() {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                    "XMPP ping response dropped: control queue unavailable"
                );
            }
        }
        Ok(())
    }
    fn allowed(&self, account: &str) -> bool {
        let Ok(jid) = account.parse::<jid::BareJid>() else {
            return false;
        };
        jid.node().is_some()
            && !account.contains('/')
            && crate::allowlist::is_user_allowed_by(&(self.peers)(), account, |entry, user| match (
                entry.parse::<jid::BareJid>(),
                user.parse::<jid::BareJid>(),
            ) {
                (Ok(a), Ok(b)) => a == b,
                _ => false,
            })
    }
    fn validate(&self) -> anyhow::Result<jid::BareJid> {
        ensure!(
            !self.alias.is_empty()
                && self.alias != "."
                && self.alias != ".."
                && !self.alias.contains(['/', '\\', ':']),
            "Invalid XMPP state alias"
        );
        if self.config.omemo_enabled {
            ensure!(
                self.inbox_vault.is_some(),
                "Native OMEMO requires the existing inbox vault"
            );
            ensure!(
                self.pins.is_some(),
                "Native OMEMO requires a live fingerprint resolver"
            );
            #[cfg(not(target_os = "linux"))]
            bail!("Native OMEMO private state is currently supported only on Linux");
        }
        let jid = self
            .config
            .username
            .parse::<jid::BareJid>()
            .context("Invalid XMPP account JID")?;
        ensure!(
            jid.node().is_some() && !self.config.username.contains('/'),
            "XMPP requires a bare account JID"
        );
        ensure!(
            !self.config.password.is_empty()
                && !self.config.server.is_empty()
                && self.config.port != 0
                && !self.config.server.contains(['/', '\\'])
                && !self.config.server.chars().any(char::is_whitespace),
            "Invalid XMPP connection configuration"
        );
        ensure!(
            (1..=64000).contains(&self.config.max_message_chars)
                && (1..=120).contains(&self.config.connect_timeout_secs),
            "Invalid XMPP limits"
        );
        ensure!(
            !self.config.resource.is_empty()
                && !self.config.resource.contains('/')
                && !self.config.resource.chars().any(char::is_control),
            "Invalid XMPP resource"
        );
        Ok(jid)
    }
    fn seen_database(&self) -> anyhow::Result<Connection> {
        #[cfg(unix)]
        use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        builder.mode(0o700);
        let xmpp = self.storage.parent().context("Invalid XMPP state path")?;
        let channels = xmpp.parent().context("Invalid XMPP state path")?;
        for dir in [channels, xmpp, self.storage.as_path()] {
            if let Ok(meta) = fs::symlink_metadata(dir) {
                ensure!(
                    meta.is_dir() && !meta.file_type().is_symlink(),
                    "Unsafe XMPP state directory"
                );
            } else {
                builder.create(dir)?;
            }
        }
        #[cfg(unix)]
        fs::set_permissions(&self.storage, fs::Permissions::from_mode(0o700))?;
        // Reuse deduplication only; encrypted identity/session rows stay untouched.
        let path = self.storage.join("omemo.sqlite3");
        let mut options = fs::OpenOptions::new();
        options.read(true).write(true).create(true);
        #[cfg(unix)]
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
        let file = options.open(&path)?;
        ensure!(file.metadata()?.is_file(), "Unsafe XMPP state database");
        #[cfg(unix)]
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
        let db = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        db.busy_timeout(Duration::from_millis(250))?;
        db.execute_batch(
            "CREATE TABLE IF NOT EXISTS seen(sender TEXT, id TEXT, PRIMARY KEY(sender,id));",
        )?;
        Ok(db)
    }
    fn inbound(&self, message: Message, own: &jid::BareJid) -> Option<ChannelMessage> {
        if !matches!(message.type_, MessageType::Chat | MessageType::Normal) {
            return None;
        }
        let sender = message.from?.to_bare();
        let account = sender.to_string();
        if sender == *own || !self.allowed(&account) {
            return None;
        }
        if message.payloads.iter().any(|p| {
            matches!(
                p.ns().as_str(),
                "urn:xmpp:delay"
                    | "urn:xmpp:carbons:2"
                    | "urn:xmpp:forward:0"
                    | "eu.siacs.conversations.axolotl"
                    | "urn:xmpp:omemo:2"
            )
        }) {
            return None;
        }
        let body = message
            .bodies
            .get("")
            .or_else(|| message.bodies.values().next())?
            .clone();
        if body.trim().is_empty() || body.chars().count() > self.config.max_message_chars {
            return None;
        }
        Some(ChannelMessage {
            id: message.id.map(|id| id.0).unwrap_or_default(),
            sender: account.clone(),
            platform_sender_id: Some(account.clone()),
            reply_target: account,
            content: body,
            channel: "xmpp".into(),
            channel_alias: Some(self.alias.clone()),
            timestamp: chrono::Utc::now().timestamp().max(0) as u64,
            ..Default::default()
        })
    }
    #[cfg(target_os = "linux")]
    async fn encrypted_inbound<R>(
        &self,
        message: Message,
        own: &jid::BareJid,
        state: &Arc<Mutex<crate::xmpp_omemo::channel_state::ChannelState>>,
        source: &mut R,
        requests: &mpsc::Sender<Outbound>,
        pending: &mut std::collections::VecDeque<Stanza>,
    ) -> anyhow::Result<Option<ChannelMessage>>
    where
        R: Stream<Item = Result<Stanza, ReadError>> + Unpin,
    {
        if !matches!(message.type_, MessageType::Chat | MessageType::Normal) {
            return Ok(None);
        }
        let Some(sender) = message.from.as_ref().map(|jid| jid.to_bare()) else {
            return Ok(None);
        };
        let account = sender.to_string();
        if sender == *own || !self.allowed(&account) {
            return Ok(None);
        }
        if message.payloads.iter().any(|p| {
            matches!(
                p.ns().as_str(),
                "urn:xmpp:delay" | "urn:xmpp:carbons:2" | "urn:xmpp:forward:0" | "urn:xmpp:omemo:2"
            )
        }) {
            return Ok(None);
        }
        let encrypted: Vec<_> = message
            .payloads
            .iter()
            .filter(|p| p.is("encrypted", crate::xmpp_omemo::stanza::LEGACY_NS))
            .collect();
        if encrypted.len() != 1 {
            return Ok(None);
        }
        let Some(id) = message
            .id
            .as_ref()
            .map(|id| id.0.clone())
            .filter(|id| !id.is_empty() && id.len() <= 512)
        else {
            return Ok(None);
        };
        let mut bytes = Vec::new();
        encrypted[0].write_to(&mut bytes)?;
        let pins = self
            .pins
            .as_ref()
            .context("Missing live fingerprint resolver")?;
        let xml = std::str::from_utf8(&bytes)?;
        let missing = state
            .lock()
            .map_err(|_| {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail),
                    "Encrypted state lock poisoned"
                );
                anyhow::Error::msg("Encrypted state lock poisoned")
            })?
            .discovery_request(&account, xml, &pins())?;
        let content = if let Some((device, _)) = missing {
            let bundle =
                discovery::peer_bundle(source, requests, pending, own, &sender, device).await?;
            if !self.allowed(&account) {
                return Ok(None);
            }
            let content = state
                .lock()
                .map_err(|_| {
                    ::zeroclaw_log::record!(
                        ERROR,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail),
                        "Encrypted state lock poisoned"
                    );
                    anyhow::Error::msg("Encrypted state lock poisoned")
                })?
                .receive_first(&account, &id, xml, &bundle, &pins())?;
            discovery::refresh(source, requests, pending, own, state).await?;
            content
        } else {
            state
                .lock()
                .map_err(|_| {
                    ::zeroclaw_log::record!(
                        ERROR,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail),
                        "Encrypted state lock poisoned"
                    );
                    anyhow::Error::msg("Encrypted state lock poisoned")
                })?
                .receive_existing(&account, &id, xml, &pins())?
        };
        let Some(content) = content else {
            return Ok(None);
        };
        if content.trim().is_empty() || content.chars().count() > self.config.max_message_chars {
            return Ok(None);
        }
        if !self.allowed(&account) {
            return Ok(None);
        }
        Ok(Some(ChannelMessage {
            id,
            sender: account.clone(),
            platform_sender_id: Some(account.clone()),
            reply_target: account,
            content,
            channel: "xmpp".into(),
            channel_alias: Some(self.alias.clone()),
            timestamp: chrono::Utc::now().timestamp().max(0) as u64,
            ..Default::default()
        }))
    }
    async fn connect<C: ServerConnector>(
        &self,
        connector: C,
        own: &jid::BareJid,
    ) -> anyhow::Result<XmlStream<C::Stream, Stanza>> {
        let jid: jid::Jid = own.clone().into();
        let (pending, binding) = connector
            .connect(&jid, ns::JABBER_CLIENT, Timeouts::default())
            .await?;
        let (mut features, stream) = pending.recv_features().await?;
        features
            .sasl_mechanisms
            .retain(|m| matches!(m.as_str(), "SCRAM-SHA-256" | "SCRAM-SHA-1" | "PLAIN"));
        ensure!(
            !features.sasl_mechanisms.is_empty(),
            "No supported authenticated XMPP SASL mechanism"
        );
        let node = own.node().context("XMPP account has no node")?;
        let credentials = sasl::common::Credentials::default()
            .with_username(node.as_str())
            .with_password(self.config.password.clone())
            .with_channel_binding(binding);
        let stream =
            tokio_xmpp::client_login(stream, features.sasl_mechanisms, credentials).await?;
        let stream = stream
            .send_header(StreamHeader {
                to: Some(Cow::Borrowed(own.domain().as_str())),
                from: None,
                id: None,
            })
            .await?;
        let (_, mut stream) = stream.recv_features::<Stanza>().await?;
        let id = uuid::Uuid::new_v4().to_string();
        stream
            .send(&Stanza::Iq(Iq::from_set(
                id.clone(),
                BindQuery::new(Some(self.config.resource.clone())),
            )))
            .await?;
        loop {
            match stream
                .next()
                .await
                .context("XMPP disconnected during resource binding")??
            {
                Stanza::Iq(Iq::Result {
                    id: reply,
                    payload: Some(payload),
                    ..
                }) if reply == id => {
                    let bound =
                        BindResponse::try_from(payload).context("Invalid XMPP resource binding")?;
                    ensure!(
                        bound.jid.to_bare() == *own,
                        "XMPP bound a different account"
                    );
                    break;
                }
                Stanza::Iq(Iq::Error { id: reply, .. }) if reply == id => {
                    bail!("XMPP resource binding rejected")
                }
                _ => {}
            }
        }
        stream
            .send(&Stanza::Presence(Presence::new(PresenceType::None)))
            .await?;
        Ok(stream)
    }
    async fn run(&self, tx: mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        let own = self.validate()?;
        if tokio_xmpp::rustls::crypto::CryptoProvider::get_default().is_none() {
            let _ = tokio_xmpp::rustls::crypto::ring::default_provider().install_default();
        }
        let dns = DnsConfig::no_srv(&self.config.server, self.config.port);
        let stream = tokio::time::timeout(
            Duration::from_secs(self.config.connect_timeout_secs),
            async {
                match self.config.protocol {
                    XmppProtocol::DirectTls => {
                        self.connect(DirectTlsServerConnector::from(dns), &own)
                            .await
                    }
                    XmppProtocol::Starttls => {
                        self.connect(StartTlsServerConnector::from(dns), &own).await
                    }
                }
            },
        )
        .await
        .context("XMPP TLS/authentication deadline exceeded")??;
        #[cfg(target_os = "linux")]
        let crypto = if self.config.omemo_enabled {
            Some(Arc::new(Mutex::new(
                crate::xmpp_omemo::channel_state::ChannelState::open_or_initialize(
                    &self.storage,
                    &self.config.username,
                    Arc::new(VaultInboxCipher {
                        store: zeroclaw_config::secrets::SecretStore::new(
                            self.inbox_vault.as_ref().context("Missing inbox vault")?,
                            true,
                        ),
                        alias: self.alias.clone(),
                    }),
                    self.config.max_message_chars,
                )?,
            )))
        } else {
            None
        };
        #[cfg(target_os = "linux")]
        let receive_crypto = crypto.clone();
        #[cfg(target_os = "linux")]
        {
            *self.encrypted.lock().map_err(|_| {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail),
                    "Encrypted state lock poisoned"
                );
                anyhow::Error::msg("Encrypted state lock poisoned")
            })? = crypto.clone();
        }
        let db = if self.config.omemo_enabled {
            None
        } else {
            Some(self.seen_database()?)
        };
        let (requests, mut rx) = mpsc::channel(32);
        // Split protocol I/O, not a mutex around a Client. Full writes cannot stop reads.
        let (mut sink, mut source) = OwnedStanzaStream(stream).split();
        let writer = async {
            while let Some(request) = rx.recv().await {
                match request {
                    Outbound::Control(stanza) => sink.send(*stanza).await?,
                    Outbound::Message {
                        recipient,
                        content,
                        mut result,
                        deadline,
                    } => {
                        if result.is_closed() || tokio::time::Instant::now() >= deadline {
                            continue;
                        }
                        if !self.allowed(&recipient) {
                            let _ = result.send(Err("XMPP recipient is not authorized".into()));
                            continue;
                        }
                        let mut message = Message::new(Some(recipient.parse::<jid::Jid>()?));
                        message.type_ = MessageType::Chat;
                        message.id = Some(Id(uuid::Uuid::new_v4().to_string()));
                        #[cfg(target_os = "linux")]
                        let mut reservation = None;
                        let prepared = (|| -> anyhow::Result<()> {
                            #[cfg(target_os = "linux")]
                            if let Some(state) = &crypto {
                                let resolve = self
                                    .pins
                                    .as_ref()
                                    .context("Missing live fingerprint resolver")?;
                                let policy = resolve();
                                let envelope = state
                                    .lock()
                                    .map_err(|_| {
                                        ::zeroclaw_log::record!(
                                            ERROR,
                                            ::zeroclaw_log::Event::new(
                                                module_path!(),
                                                ::zeroclaw_log::Action::Fail
                                            ),
                                            "Encrypted state lock poisoned"
                                        );
                                        anyhow::Error::msg("Encrypted state lock poisoned")
                                    })?
                                    .reserve_existing(
                                        &recipient,
                                        &message.id.as_ref().context("Missing stanza ID")?.0,
                                        content.as_bytes(),
                                        &policy,
                                    )?;
                                message.payloads.push(
                                    crate::xmpp_omemo::stanza::serialize(&envelope)?.parse()?,
                                );
                                ensure!(
                                    self.allowed(&recipient) && resolve() == policy,
                                    "XMPP authorization changed before socket write"
                                );
                                reservation = Some(envelope);
                            } else {
                                message.bodies.insert(Lang::default(), content);
                            }
                            #[cfg(not(target_os = "linux"))]
                            message.bodies.insert(Lang::default(), content);
                            Ok(())
                        })();
                        if let Err(error) = prepared {
                            let _ = result.send(Err(error.to_string()));
                            continue;
                        }
                        let sent = write_before_deadline(
                            &mut sink,
                            Stanza::Message(message),
                            &mut result,
                            deadline,
                        )
                        .await;
                        if let Err(error) = sent {
                            let _ = result.send(Err(error.to_string()));
                            return Err(error);
                        }
                        #[cfg(target_os = "linux")]
                        if let (Some(state), Some(envelope)) = (&crypto, reservation)
                            && let Err(error) = state
                                .lock()
                                .map_err(|_| {
                                    ::zeroclaw_log::record!(
                                        ERROR,
                                        ::zeroclaw_log::Event::new(
                                            module_path!(),
                                            ::zeroclaw_log::Action::Fail
                                        ),
                                        "Encrypted state lock poisoned"
                                    );
                                    anyhow::Error::msg("Encrypted state lock poisoned")
                                })?
                                .complete_socket_write(&envelope)
                        {
                            let _ = result.send(Err(error.to_string()));
                            return Err(error);
                        }
                        let _ = result.send(Ok(()));
                    }
                }
            }
            Ok::<(), anyhow::Error>(())
        };
        let reader = async move {
            let mut pending = std::collections::VecDeque::new();
            #[cfg(target_os = "linux")]
            if let Some(state) = &receive_crypto {
                discovery::announce(&mut source, &requests, &mut pending, &own, state).await?;
            }
            *self.outbound.lock().map_err(|_| {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail),
                    "XMPP state lock poisoned"
                );
                anyhow::Error::msg("XMPP state lock poisoned")
            })? = Some(requests.clone());
            #[cfg(target_os = "linux")]
            if let Some(state) = &receive_crypto {
                let pins = self.pins.as_ref().context("Missing fingerprint resolver")?;
                let recovered = state
                    .lock()
                    .map_err(|_| {
                        ::zeroclaw_log::record!(
                            ERROR,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Fail
                            ),
                            "Encrypted state lock poisoned"
                        );
                        anyhow::Error::msg("Encrypted state lock poisoned")
                    })?
                    .pending(&pins())?;
                for mut message in recovered {
                    if !self.allowed(&message.peer) {
                        continue;
                    }
                    let sender = std::mem::take(&mut message.peer);
                    tx.send(ChannelMessage {
                        id: std::mem::take(&mut message.id),
                        sender: sender.clone(),
                        platform_sender_id: Some(sender.clone()),
                        reply_target: sender,
                        content: std::mem::take(&mut message.content),
                        channel: "xmpp".into(),
                        channel_alias: Some(self.alias.clone()),
                        timestamp: chrono::Utc::now().timestamp().max(0) as u64,
                        ..Default::default()
                    })
                    .await
                    .context("Recovered ingress receiver closed")?;
                }
            }
            loop {
                let event = if let Some(stanza) = pending.pop_front() {
                    Some(Ok(stanza))
                } else {
                    source.next().await
                };
                let Some(event) = event else {
                    break;
                };
                let stanza = match event {
                    Ok(stanza) => stanza,
                    Err(ReadError::SoftTimeout) => {
                        requests
                            .try_send(Outbound::Control(Box::new(Stanza::Iq(Iq::from_get(
                                uuid::Uuid::new_v4().to_string(),
                                tokio_xmpp::parsers::ping::Ping,
                            )))))
                            .context("XMPP keepalive queue full")?;
                        continue;
                    }
                    Err(error) => bail!("XMPP stream read failed: {error}"),
                };
                match stanza {
                    Stanza::Message(message) => {
                        #[cfg(target_os = "linux")]
                        let message = if let Some(state) = &receive_crypto {
                            match self
                                .encrypted_inbound(
                                    message,
                                    &own,
                                    state,
                                    &mut source,
                                    &requests,
                                    &mut pending,
                                )
                                .await
                            {
                                Ok(message) => message,
                                Err(error) if error.is::<crate::xmpp_omemo::RejectedInbound>() => {
                                    None
                                }
                                Err(error) => return Err(error),
                            }
                        } else {
                            self.inbound(message, &own)
                        };
                        #[cfg(not(target_os = "linux"))]
                        let message = self.inbound(message, &own);
                        if let Some(message) = message {
                            if let Some(db) = &db
                                && !message.id.is_empty()
                                && db.query_row(
                                    "SELECT EXISTS(SELECT 1 FROM seen WHERE sender=?1 AND id=?2)",
                                    params![message.sender, message.id],
                                    |r| r.get::<_, bool>(0),
                                )?
                            {
                                continue;
                            }
                            let identity = (message.sender.clone(), message.id.clone());
                            tx.send(message)
                                .await
                                .context("XMPP runtime receiver closed")?;
                            if let Some(db) = &db
                                && !identity.1.is_empty()
                            {
                                db.execute(
                                    "INSERT OR IGNORE INTO seen(sender,id) VALUES(?1,?2)",
                                    params![identity.0, identity.1],
                                )?;
                                db.execute("DELETE FROM seen WHERE rowid NOT IN (SELECT rowid FROM seen ORDER BY rowid DESC LIMIT 8192)",[])?;
                            }
                        }
                    }
                    Stanza::Iq(Iq::Get {
                        from, id, payload, ..
                    }) if payload.is("ping", "urn:xmpp:ping") => {
                        self.enqueue_ping_reply(from, id, &requests)?;
                    }
                    _ => {}
                }
            }
            bail!("XMPP stream closed")
        };
        // Both futures belong to this listener; cancellation drops the socket and writes.
        tokio::select! { result = writer => result, result = reader => result }
    }
}
impl zeroclaw_api::attribution::Attributable for XmppChannel {
    fn role(&self) -> zeroclaw_api::attribution::Role {
        zeroclaw_api::attribution::Role::Channel(zeroclaw_api::attribution::ChannelKind::Xmpp)
    }
    fn alias(&self) -> &str {
        &self.alias
    }
}
#[async_trait]
impl Channel for XmppChannel {
    fn name(&self) -> &str {
        "xmpp"
    }
    fn is_direct_message(&self, _: &ChannelMessage) -> bool {
        true
    }
    fn self_handle(&self) -> Option<String> {
        Some(self.config.username.clone())
    }
    async fn health_check(&self) -> bool {
        self.outbound.lock().map(|s| s.is_some()).unwrap_or(false)
    }
    async fn listen(&self, tx: mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        struct Reset<'a>(&'a XmppChannel);
        impl Drop for Reset<'_> {
            fn drop(&mut self) {
                if let Ok(mut sender) = self.0.outbound.lock() {
                    *sender = None;
                }
                #[cfg(target_os = "linux")]
                if let Ok(mut state) = self.0.encrypted.lock() {
                    *state = None;
                }
            }
        }
        let _reset = Reset(self);
        self.run(tx).await
    }
    async fn admit_inbound(&self, message: &ChannelMessage) -> anyhow::Result<bool> {
        if !self.config.omemo_enabled {
            return Ok(true);
        }
        ensure!(
            self.allowed(&message.sender),
            "Ingress sender is no longer authorized"
        );
        #[cfg(target_os = "linux")]
        {
            let state = self
                .encrypted
                .lock()
                .map_err(|_| {
                    ::zeroclaw_log::record!(
                        ERROR,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail),
                        "Encrypted state lock poisoned"
                    );
                    anyhow::Error::msg("Encrypted state lock poisoned")
                })?
                .clone()
                .context("Encrypted channel offline")?;
            let pins = self.pins.as_ref().context("Missing fingerprint resolver")?;
            let result = state
                .lock()
                .map_err(|_| {
                    ::zeroclaw_log::record!(
                        ERROR,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail),
                        "Encrypted state lock poisoned"
                    );
                    anyhow::Error::msg("Encrypted state lock poisoned")
                })?
                .admit(&message.sender, &message.id, &pins());
            return result;
        }
        #[cfg(not(target_os = "linux"))]
        bail!("Encrypted ingress unavailable on this platform")
    }
    async fn inbound_runtime_bound(
        &self,
        message: &ChannelMessage,
        agent: &str,
        history_key: &str,
    ) -> anyhow::Result<()> {
        #[cfg(not(target_os = "linux"))]
        let _ = (agent, history_key);
        if !self.config.omemo_enabled {
            return Ok(());
        }
        ensure!(
            self.allowed(&message.sender),
            "Ingress sender is no longer authorized"
        );
        #[cfg(target_os = "linux")]
        {
            let state = self
                .encrypted
                .lock()
                .map_err(|_| {
                    ::zeroclaw_log::record!(
                        ERROR,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail),
                        "Encrypted state lock poisoned"
                    );
                    anyhow::Error::msg("Encrypted state lock poisoned")
                })?
                .clone()
                .context("Encrypted channel offline")?;
            let pins = self.pins.as_ref().context("Missing fingerprint resolver")?;
            let result = state
                .lock()
                .map_err(|_| {
                    ::zeroclaw_log::record!(
                        ERROR,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail),
                        "Encrypted state lock poisoned"
                    );
                    anyhow::Error::msg("Encrypted state lock poisoned")
                })?
                .bind_runtime(&message.sender, &message.id, agent, history_key, &pins());
            return result;
        }
        #[cfg(not(target_os = "linux"))]
        bail!("Encrypted ingress unavailable on this platform")
    }
    async fn inbound_returned(&self, message: &ChannelMessage) -> anyhow::Result<()> {
        #[cfg(not(target_os = "linux"))]
        let _ = message;
        if !self.config.omemo_enabled {
            return Ok(());
        }
        #[cfg(target_os = "linux")]
        {
            let state = self
                .encrypted
                .lock()
                .map_err(|_| {
                    ::zeroclaw_log::record!(
                        ERROR,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail),
                        "Encrypted state lock poisoned"
                    );
                    anyhow::Error::msg("Encrypted state lock poisoned")
                })?
                .clone()
                .context("Encrypted channel offline")?;
            let result = state
                .lock()
                .map_err(|_| {
                    ::zeroclaw_log::record!(
                        ERROR,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail),
                        "Encrypted state lock poisoned"
                    );
                    anyhow::Error::msg("Encrypted state lock poisoned")
                })?
                .returned(&message.sender, &message.id);
            return result;
        }
        #[cfg(not(target_os = "linux"))]
        bail!("Encrypted ingress unavailable on this platform")
    }
    fn requires_individual_ingress(&self) -> bool {
        self.config.omemo_enabled
    }
    async fn inbound_control_returned(
        &self,
        message: &ChannelMessage,
        control: &str,
    ) -> anyhow::Result<()> {
        #[cfg(not(target_os = "linux"))]
        let _ = (message, control);
        if !self.config.omemo_enabled {
            return Ok(());
        }
        #[cfg(target_os = "linux")]
        {
            let state = self
                .encrypted
                .lock()
                .map_err(|_| {
                    ::zeroclaw_log::record!(
                        ERROR,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail),
                        "Encrypted state lock poisoned"
                    );
                    anyhow::Error::msg("Encrypted state lock poisoned")
                })?
                .clone()
                .context("Encrypted channel offline")?;
            state
                .lock()
                .map_err(|_| {
                    ::zeroclaw_log::record!(
                        ERROR,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail),
                        "Encrypted state lock poisoned"
                    );
                    anyhow::Error::msg("Encrypted state lock poisoned")
                })?
                .control_returned(&message.sender, &message.id, control)
        }
        #[cfg(not(target_os = "linux"))]
        bail!("Encrypted ingress unavailable on this platform")
    }
    async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
        ensure!(
            self.allowed(&message.recipient),
            "XMPP recipient is not authorized"
        );
        ensure!(
            message.attachments.is_empty(),
            "XMPP attachments are not supported"
        );
        ensure!(
            message.content.chars().count() <= self.config.max_message_chars,
            "XMPP reply exceeds message limit"
        );
        let sender = self
            .outbound
            .lock()
            .map_err(|_| {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail),
                    "XMPP state lock poisoned"
                );
                anyhow::Error::msg("XMPP state lock poisoned")
            })?
            .clone()
            .context("XMPP channel is offline")?;
        let (result, ack) = oneshot::channel();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
        tokio::time::timeout_at(deadline, async {
            sender
                .send(Outbound::Message {
                    recipient: message.recipient.clone(),
                    content: message.content.clone(),
                    result,
                    deadline,
                })
                .await
                .context("XMPP channel stopped")?;
            ack.await
                .context("XMPP send outcome unknown; verify before retrying")?
                .map_err(anyhow::Error::msg)?;
            Ok(())
        })
        .await
        .context("XMPP send deadline exceeded; outcome unknown, verify before retrying")?
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn expired_and_cancelled_sends_never_reach_socket() -> anyhow::Result<()> {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let writes = Arc::new(AtomicUsize::new(0));
        let counted = writes.clone();
        let mut sink = Box::pin(futures_util::sink::unfold((), move |(), _: Stanza| {
            counted.fetch_add(1, Ordering::SeqCst);
            async { Ok::<_, std::io::Error>(()) }
        }));
        let (mut result, _ack) = oneshot::channel();
        let stanza = Stanza::Message(Message::new(None));
        assert!(
            write_before_deadline(&mut sink, stanza, &mut result, tokio::time::Instant::now())
                .await
                .is_err()
        );
        let (mut result, ack) = oneshot::channel();
        drop(ack);
        assert!(
            write_before_deadline(
                &mut sink,
                Stanza::Message(Message::new(None)),
                &mut result,
                tokio::time::Instant::now() + Duration::from_secs(1)
            )
            .await
            .is_err()
        );
        assert_eq!(writes.load(Ordering::SeqCst), 0);
        Ok(())
    }

    #[tokio::test]
    async fn blocked_socket_write_stops_when_caller_cancels() -> anyhow::Result<()> {
        use std::sync::atomic::{AtomicBool, Ordering};
        struct WriteGuard(Arc<AtomicBool>);
        impl Drop for WriteGuard {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }
        let dropped = Arc::new(AtomicBool::new(false));
        let watched = dropped.clone();
        let mut sink = Box::pin(futures_util::sink::unfold((), move |(), _: Stanza| {
            let guard = WriteGuard(watched.clone());
            async move {
                let _guard = guard;
                std::future::pending::<Result<(), std::io::Error>>().await
            }
        }));
        let (mut result, ack) = oneshot::channel();
        let cancel = ::zeroclaw_spawn::spawn!(async move {
            tokio::time::sleep(Duration::from_millis(10)).await;
            drop(ack);
        });
        assert!(
            write_before_deadline(
                &mut sink,
                Stanza::Message(Message::new(None)),
                &mut result,
                tokio::time::Instant::now() + Duration::from_secs(1)
            )
            .await
            .is_err()
        );
        cancel.await?;
        // The writer owns the transport: its error path drops the sink/listener.
        drop(sink);
        assert!(dropped.load(Ordering::SeqCst));
        Ok(())
    }

    fn adapter(data: PathBuf) -> XmppChannel {
        XmppChannel::new(
            XmppConfig {
                username: "bot@example.org".into(),
                server: "xmpp.example.org".into(),
                password: "fixture-only".into(),
                ..Default::default()
            },
            "private".into(),
            data,
            Arc::new(|| vec!["alice@example.org".into(), "!mallory@example.org".into()]),
        )
    }
    #[test]
    fn xmpp_ping_denies_untrusted_peers_and_drops_backpressure_without_failure()
    -> anyhow::Result<()> {
        let adapter = adapter(PathBuf::new());
        let (tx, mut rx) = mpsc::channel(32);
        adapter.enqueue_ping_reply(
            Some("mallory@example.org/device".parse()?),
            "denied".into(),
            &tx,
        )?;
        adapter.enqueue_ping_reply(Some("foreign.test".parse()?), "foreign-server".into(), &tx)?;
        assert!(rx.try_recv().is_err());
        for sender in [
            None,
            Some("example.org"),
            Some("bot@example.org/device"),
            Some("alice@example.org/device"),
        ] {
            adapter.enqueue_ping_reply(
                sender.map(str::parse).transpose()?,
                "keepalive".into(),
                &tx,
            )?;
            assert!(rx.try_recv().is_ok());
        }
        for index in 0..64 {
            adapter.enqueue_ping_reply(
                Some("alice@example.org/device".parse()?),
                format!("burst-{index}"),
                &tx,
            )?;
        }
        let mut replies = 0;
        while rx.try_recv().is_ok() {
            replies += 1;
        }
        assert_eq!(replies, 32);
        drop(rx);
        adapter.enqueue_ping_reply(None, "closed-queue".into(), &tx)?;
        Ok(())
    }
    fn message(sender: &str, body: &str) -> Message {
        let mut message = Message::new(None);
        message.from = Some(sender.parse().unwrap());
        message.type_ = MessageType::Chat;
        message.bodies.insert(Lang::default(), body.into());
        message
    }
    #[test]
    fn native_admission_normalizes_resources_and_denies_other_peers() {
        let c = adapter(PathBuf::new());
        let own = c.validate().unwrap();
        let m = c
            .inbound(message("alice@example.org/phone", "hello"), &own)
            .unwrap();
        assert_eq!(m.sender, "alice@example.org");
        assert_eq!(m.channel_alias.as_deref(), Some("private"));
        assert!(c.is_direct_message(&m));
        assert!(
            c.inbound(message("mallory@example.org", "hello"), &own)
                .is_none()
        );
        assert!(
            c.inbound(message("bot@example.org/other", "hello"), &own)
                .is_none()
        );
        assert!(!c.allowed("alice@example.org/phone"));
    }
    #[tokio::test]
    async fn encrypted_config_refuses_start_before_touching_identity_state() {
        let dir = tempfile::tempdir().unwrap();
        let mut c = adapter(dir.path().into());
        c.config.omemo_enabled = true;
        let (tx, _) = mpsc::channel(1);
        assert!(
            c.listen(tx)
                .await
                .unwrap_err()
                .to_string()
                .contains("OMEMO")
        );
        assert!(!c.health_check().await);
        assert!(!c.storage.exists());
    }
    #[test]
    fn native_admission_rejects_archives_encrypted_fallbacks_and_oversize() {
        let c = adapter(PathBuf::new());
        let own = c.validate().unwrap();
        for namespace in [
            "urn:xmpp:delay",
            "urn:xmpp:forward:0",
            "urn:xmpp:carbons:2",
            "eu.siacs.conversations.axolotl",
            "urn:xmpp:omemo:2",
        ] {
            let mut m = message("alice@example.org", "fallback");
            m.payloads
                .push(tokio_xmpp::minidom::Element::builder("data", namespace).build());
            assert!(c.inbound(m, &own).is_none());
        }
        assert!(
            c.inbound(message("alice@example.org", &"x".repeat(16001)), &own)
                .is_none()
        );
        let mut m = message("alice@example.org", "group");
        m.type_ = MessageType::Groupchat;
        assert!(c.inbound(m, &own).is_none());
    }
    #[test]
    fn dedup_state_does_not_replace_encrypted_identity_rows() {
        let dir = tempfile::tempdir().unwrap();
        let c = adapter(dir.path().into());
        let db = c.seen_database().unwrap();
        db.execute_batch("CREATE TABLE state(key TEXT PRIMARY KEY,value TEXT NOT NULL); INSERT INTO state VALUES('identity','fixture-identity');").unwrap();
        drop(db);
        let db = c.seen_database().unwrap();
        assert_eq!(
            db.query_row("SELECT value FROM state WHERE key='identity'", [], |r| {
                r.get::<_, String>(0)
            })
            .unwrap(),
            "fixture-identity"
        );
    }
    #[tokio::test]
    #[ignore = "requires isolated Prosody accounts and SSL_CERT_FILE with the fixture CA"]
    async fn native_tls_listener_roundtrip_and_authorization() {
        let fixture = std::env::var("ZEROCLAW_XMPP_TEST_ACCOUNTS").unwrap();
        let accounts: std::collections::HashMap<String, String> =
            serde_json::from_str(&std::fs::read_to_string(fixture).unwrap()).unwrap();
        for (protocol, port) in [
            (XmppProtocol::DirectTls, 15223),
            (XmppProtocol::Starttls, 15222),
        ] {
            let data = tempfile::tempdir().unwrap();
            let channel = |user: &str, peer: &str| {
                let peer = format!("{peer}@example.test");
                Arc::new(XmppChannel::new(
                    XmppConfig {
                        username: format!("{user}@example.test"),
                        password: accounts[user].clone(),
                        server: "127.0.0.1".into(),
                        port,
                        protocol,
                        connect_timeout_secs: 10,
                        ..Default::default()
                    },
                    user.into(),
                    data.path().into(),
                    Arc::new(move || vec![peer.clone()]),
                ))
            };
            let bot = channel("bot", "alice");
            let alice = channel("alice", "bot");
            let mallory = channel("mallory", "bot");
            let (bot_tx, mut bot_rx) = mpsc::channel(1);
            let (alice_tx, mut alice_rx) = mpsc::channel(1);
            let (mallory_tx, _mallory_rx) = mpsc::channel(1);
            let start = |c: Arc<XmppChannel>, tx| {
                ::zeroclaw_spawn::spawn!(async move {
                    let result = c.listen(tx).await;
                    if let Err(error) = &result {
                        eprintln!("fixture listener: {error:#}");
                    }
                    result
                })
            };
            let tasks = [
                start(bot.clone(), bot_tx),
                start(alice.clone(), alice_tx),
                start(mallory.clone(), mallory_tx),
            ];
            tokio::time::timeout(Duration::from_secs(15), async {
                loop {
                    for task in &tasks {
                        assert!(!task.is_finished(), "fixture listener exited before online");
                    }
                    if bot.health_check().await
                        && alice.health_check().await
                        && mallory.health_check().await
                    {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .await
            .unwrap();
            mallory
                .send(&SendMessage::new("unauthorized", "bot@example.test"))
                .await
                .unwrap();
            alice
                .send(&SendMessage::new("authorized", "bot@example.test"))
                .await
                .unwrap();
            let received = tokio::time::timeout(Duration::from_secs(5), bot_rx.recv())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(received.content, "authorized");
            assert_eq!(received.sender, "alice@example.test");
            bot.send(&SendMessage::new("reply", "alice@example.test"))
                .await
                .unwrap();
            let reply = tokio::time::timeout(Duration::from_secs(5), alice_rx.recv())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(reply.content, "reply");
            assert!(
                tokio::time::timeout(Duration::from_millis(200), bot_rx.recv())
                    .await
                    .is_err()
            );
            for task in tasks {
                task.abort();
                assert!(task.await.unwrap_err().is_cancelled());
            }
            assert!(!bot.health_check().await);
            assert!(!alice.health_check().await);
            assert!(
                bot.send(&SendMessage::new("offline", "alice@example.test"))
                    .await
                    .is_err()
            );
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
#[path = "xmpp_native_channel_probe.rs"]
mod native_channel_probe;

#[cfg(target_os = "linux")]
#[path = "xmpp_native_discovery.rs"]
mod discovery;

#[cfg(all(test, target_os = "linux"))]
#[path = "xmpp_inbox_fault_probe.rs"]
mod inbox_fault_probe;

#[cfg(target_os = "linux")]
#[path = "xmpp_native_inbox_admin.rs"]
pub mod inbox_admin;
