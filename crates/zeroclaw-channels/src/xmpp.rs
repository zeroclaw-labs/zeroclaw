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

const OUTBOUND_TIMEOUT: Duration = Duration::from_secs(60);
const TLS_SEEN_LIMIT: i64 = 8192;

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
// Prefer challenge-response authentication even if the server advertises
// PLAIN first. PLAIN remains available for TLS-only servers without SCRAM.
fn prefer_authenticated_mechanisms(
    mechanisms: &mut std::collections::BTreeSet<String>,
    binding: sasl::common::ChannelBinding,
) -> sasl::common::ChannelBinding {
    use sasl::common::ChannelBinding;
    let can_bind = matches!(
        binding,
        ChannelBinding::TlsUnique(_) | ChannelBinding::TlsExporter(_)
    );
    let preferred = [
        "SCRAM-SHA-256-PLUS",
        "SCRAM-SHA-1-PLUS",
        "SCRAM-SHA-256",
        "SCRAM-SHA-1",
        "PLAIN",
    ]
    .into_iter()
    .find(|mechanism| {
        (can_bind || !mechanism.ends_with("-PLUS")) && mechanisms.contains(*mechanism)
    });
    mechanisms.retain(|mechanism| Some(mechanism.as_str()) == preferred);
    if can_bind && !preferred.is_some_and(|mechanism| mechanism.ends_with("-PLUS")) {
        // Tell SCRAM that binding is supported locally but not advertised by
        // this server. Otherwise sasl names the mechanism *-PLUS even though
        // the server only offered the non-PLUS variant.
        ChannelBinding::Unsupported
    } else {
        binding
    }
}

impl XmppChannel {
    pub fn new(
        mut config: XmppConfig,
        alias: String,
        data_dir: PathBuf,
        peers: PeerResolver,
    ) -> Self {
        // JID parsing normalizes domains; connection and durable identity must
        // use the same account before any private store can be created.
        if let Ok(account) = config.username.parse::<jid::BareJid>() {
            config.username = account.to_string();
        }
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
    fn ping_sender_allowed(&self, from: Option<&jid::Jid>) -> anyhow::Result<bool> {
        let own = self.config.username.parse::<jid::BareJid>()?;
        Ok(from.is_none_or(|sender| {
            let bare = sender.to_bare();
            bare == own
                || (bare.node().is_none() && bare.domain() == own.domain())
                || self.allowed(&bare.to_string())
        }))
    }
    fn enqueue_ping_reply(
        &self,
        from: Option<jid::Jid>,
        id: String,
        requests: &mpsc::Sender<Outbound>,
    ) -> anyhow::Result<()> {
        if self.ping_sender_allowed(from.as_ref())? {
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
        let mut db = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        db.busy_timeout(Duration::from_millis(250))?;
        let schema = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        schema.execute_batch(
            "CREATE TABLE IF NOT EXISTS seen(sender TEXT, id TEXT, PRIMARY KEY(sender,id));",
        )?;
        let classified: bool = schema.query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info('seen') WHERE name='tls_only')",
            [],
            |row| row.get(0),
        )?;
        if !classified {
            // Legacy rows may be encrypted receipts, so only new TLS writes opt into eviction.
            schema.execute_batch(
                "ALTER TABLE seen ADD COLUMN tls_only INTEGER NOT NULL DEFAULT 0 CHECK(tls_only IN (0,1));",
            )?;
        }
        schema.execute_batch(
            "CREATE INDEX IF NOT EXISTS seen_tls_only ON seen(tls_only) WHERE tls_only=1;",
        )?;
        schema.commit()?;
        Ok(db)
    }
    fn remember_tls_message(db: &Connection, sender: &str, id: &str) -> anyhow::Result<()> {
        db.execute(
            "INSERT OR IGNORE INTO seen(sender,id,tls_only) VALUES(?1,?2,1)",
            params![sender, id],
        )?;
        db.execute(
            "DELETE FROM seen WHERE tls_only=1 AND rowid <= (
                SELECT rowid FROM seen WHERE tls_only=1 ORDER BY rowid DESC LIMIT 1 OFFSET ?1
            )",
            [TLS_SEEN_LIMIT],
        )?;
        Ok(())
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
                discovery::peer_bundle(self, source, requests, pending, own, &sender, device)
                    .await?;
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
            discovery::refresh(self, source, requests, pending, own, state).await?;
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
        let binding = prefer_authenticated_mechanisms(&mut features.sasl_mechanisms, binding);
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
    // The channel owns durable receipts; the listener owns only socket I/O.
    // Reconnect reuses this exclusive store rather than opening another owner.
    #[cfg(target_os = "linux")]
    fn crypto_owner(&self) -> anyhow::Result<Option<CryptoOwner>> {
        if !self.config.omemo_enabled {
            return Ok(None);
        }
        self.validate()?;
        let mut owner = self
            .encrypted
            .lock()
            .map_err(|_| anyhow::Error::msg("Encrypted state lock poisoned"))?;
        if owner.is_none() {
            *owner = Some(Arc::new(Mutex::new(
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
            )));
        }
        Ok(owner.clone())
    }
    async fn write_outbound<S>(
        &self,
        mut sink: S,
        mut rx: mpsc::Receiver<Outbound>,
        #[cfg(target_os = "linux")] crypto: Option<CryptoOwner>,
    ) -> anyhow::Result<()>
    where
        S: Sink<Stanza> + Unpin,
        S::Error: std::fmt::Display,
    {
        while let Some(request) = rx.recv().await {
            match request {
                Outbound::Control(stanza) => {
                    // Inbound traffic must not keep a blocked writer healthy indefinitely.
                    tokio::time::timeout(OUTBOUND_TIMEOUT, sink.send(*stanza))
                        .await
                        .context("XMPP control write deadline exceeded; reconnect required")?
                        .map_err(|error| {
                            ::zeroclaw_log::record!(
                                ERROR,
                                ::zeroclaw_log::Event::new(
                                    module_path!(),
                                    ::zeroclaw_log::Action::Fail
                                ),
                                "XMPP control write failed: {error}"
                            );
                            anyhow::Error::msg(format!("XMPP control write failed: {error}"))
                        })?;
                }
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
                            message
                                .payloads
                                .push(crate::xmpp_omemo::stanza::serialize(&envelope)?.parse()?);
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
        let crypto = self.crypto_owner()?;
        #[cfg(target_os = "linux")]
        let receive_crypto = crypto.clone();
        let db = if self.config.omemo_enabled {
            None
        } else {
            Some(self.seen_database()?)
        };
        let (requests, rx) = mpsc::channel(32);
        // Split protocol I/O, not a mutex around a Client. Full writes cannot stop reads.
        let (sink, mut source) = OwnedStanzaStream(stream).split();
        let writer = self.write_outbound(
            sink,
            rx,
            #[cfg(target_os = "linux")]
            crypto,
        );
        let reader = async move {
            let mut pending = std::collections::VecDeque::new();
            #[cfg(target_os = "linux")]
            if let Some(state) = &receive_crypto {
                discovery::announce(self, &mut source, &requests, &mut pending, &own, state)
                    .await?;
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
                                Self::remember_tls_message(db, &identity.0, &identity.1)?;
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
        let deadline = tokio::time::Instant::now() + OUTBOUND_TIMEOUT;
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

    struct StalledControlSink {
        writes: Arc<std::sync::atomic::AtomicUsize>,
        dropped: Arc<std::sync::atomic::AtomicBool>,
    }
    impl Sink<Stanza> for StalledControlSink {
        type Error = std::io::Error;
        fn poll_ready(
            self: Pin<&mut Self>,
            _: &mut TaskContext<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }
        fn start_send(self: Pin<&mut Self>, _: Stanza) -> Result<(), Self::Error> {
            self.writes
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }
        fn poll_flush(
            self: Pin<&mut Self>,
            _: &mut TaskContext<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Pending
        }
        fn poll_close(
            self: Pin<&mut Self>,
            _: &mut TaskContext<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }
    }
    impl Drop for StalledControlSink {
        fn drop(&mut self) {
            self.dropped
                .store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }
    fn writer_test_channel() -> XmppChannel {
        XmppChannel::new(
            XmppConfig {
                username: "bot@example.test".into(),
                ..Default::default()
            },
            "test".into(),
            PathBuf::new(),
            Arc::new(|| vec!["alice@example.test".into()]),
        )
    }
    fn control_probe() -> Outbound {
        Outbound::Control(Box::new(Stanza::Iq(Iq::from_get(
            "probe".to_owned(),
            tokio_xmpp::parsers::ping::Ping,
        ))))
    }
    #[tokio::test(start_paused = true)]
    async fn blocked_control_write_drops_writer_and_queued_replies_at_deadline()
    -> anyhow::Result<()> {
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        let writes = Arc::new(AtomicUsize::new(0));
        let dropped = Arc::new(AtomicBool::new(false));
        let sink = StalledControlSink {
            writes: writes.clone(),
            dropped: dropped.clone(),
        };
        let channel = writer_test_channel();
        let (tx, rx) = mpsc::channel(2);
        tx.send(control_probe()).await?;
        let (result, mut ack) = oneshot::channel();
        tx.send(Outbound::Message {
            recipient: "alice@example.test".into(),
            content: "queued".into(),
            result,
            deadline: tokio::time::Instant::now() + Duration::from_secs(120),
        })
        .await?;
        drop(tx);
        let writer = ::zeroclaw_spawn::spawn!(async move {
            channel
                .write_outbound(
                    sink,
                    rx,
                    #[cfg(target_os = "linux")]
                    None,
                )
                .await
        });
        tokio::task::yield_now().await;
        assert_eq!(writes.load(Ordering::SeqCst), 1);
        tokio::time::advance(Duration::from_secs(59)).await;
        assert!(!writer.is_finished());
        assert!(matches!(
            ack.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
        tokio::time::advance(Duration::from_secs(1)).await;
        tokio::task::yield_now().await;
        assert!(
            writer.is_finished(),
            "blocked control write must terminate the writer at 60 seconds"
        );
        assert!(
            writer
                .await?
                .unwrap_err()
                .to_string()
                .contains("control write deadline exceeded")
        );
        assert!(
            dropped.load(Ordering::SeqCst),
            "connection writer must be dropped on timeout"
        );
        assert!(
            ack.await.is_err(),
            "queued replies must be released when the writer fails"
        );
        assert_eq!(
            writes.load(Ordering::SeqCst),
            1,
            "queued replies must not be written later"
        );
        Ok(())
    }
    #[tokio::test]
    async fn successful_control_write_leaves_writer_usable() -> anyhow::Result<()> {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let channel = writer_test_channel();
        let (tx, rx) = mpsc::channel(2);
        tx.send(control_probe()).await?;
        let (result, ack) = oneshot::channel();
        tx.send(Outbound::Message {
            recipient: "alice@example.test".into(),
            content: "queued".into(),
            result,
            deadline: tokio::time::Instant::now() + Duration::from_secs(60),
        })
        .await?;
        drop(tx);
        let writes = Arc::new(AtomicUsize::new(0));
        let counted = writes.clone();
        let sink = Box::pin(futures_util::sink::unfold((), move |(), _: Stanza| {
            counted.fetch_add(1, Ordering::SeqCst);
            async { Ok::<_, std::io::Error>(()) }
        }));
        channel
            .write_outbound(
                sink,
                rx,
                #[cfg(target_os = "linux")]
                None,
            )
            .await?;
        assert!(ack.await?.is_ok());
        assert_eq!(writes.load(Ordering::SeqCst), 2);
        Ok(())
    }
    #[tokio::test]
    async fn failed_control_write_releases_queued_replies() -> anyhow::Result<()> {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let channel = writer_test_channel();
        let (tx, rx) = mpsc::channel(2);
        tx.send(control_probe()).await?;
        let (result, ack) = oneshot::channel();
        tx.send(Outbound::Message {
            recipient: "alice@example.test".into(),
            content: "queued".into(),
            result,
            deadline: tokio::time::Instant::now() + Duration::from_secs(60),
        })
        .await?;
        drop(tx);
        let writes = Arc::new(AtomicUsize::new(0));
        let counted = writes.clone();
        let sink = Box::pin(futures_util::sink::unfold((), move |(), _: Stanza| {
            counted.fetch_add(1, Ordering::SeqCst);
            async {
                Err::<(), _>(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "synthetic closed socket",
                ))
            }
        }));
        let error = channel
            .write_outbound(
                sink,
                rx,
                #[cfg(target_os = "linux")]
                None,
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("control write failed"));
        assert!(ack.await.is_err());
        assert_eq!(writes.load(Ordering::SeqCst), 1);
        Ok(())
    }
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn disconnected_listener_keeps_runtime_and_control_receipts_releasable()
    -> anyhow::Result<()> {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir_in("/var/tmp")?;
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700))?;
        let vault = zeroclaw_config::secrets::SecretStore::new(directory.path(), true);
        vault.encrypt("synthetic initialization")?;
        let channel = XmppChannel::new(
            XmppConfig {
                username: "bob@EXAMPLE.TEST".into(),
                password: "synthetic".into(),
                server: "127.0.0.1".into(),
                port: 9,
                omemo_enabled: true,
                connect_timeout_secs: 1,
                ..Default::default()
            },
            "private".into(),
            directory.path().into(),
            Arc::new(|| vec!["alice@example.test".into()]),
        )
        .with_pin_resolver(Arc::new(Vec::new))
        .with_inbox_vault(Some(directory.path().into()));
        assert_eq!(channel.config.username, "bob@example.test");
        let owner = channel.crypto_owner()?.unwrap();
        for id in ["runtime", "queued", "interrupted"] {
            owner
                .lock()
                .unwrap()
                .stage_admitted_fixture(id, id == "runtime")?;
        }
        let (tx, _rx) = mpsc::channel(1);
        assert!(channel.listen(tx).await.is_err());
        assert!(!channel.health_check().await);
        let message = |id: &str| ChannelMessage {
            id: id.into(),
            sender: "alice@example.test".into(),
            content: "synthetic input".into(),
            channel: "xmpp".into(),
            ..Default::default()
        };
        channel.inbound_returned(&message("runtime")).await?;
        channel
            .inbound_control_returned(&message("queued"), "queued_cancelled")
            .await?;
        assert!(Arc::ptr_eq(&owner, &channel.crypto_owner()?.unwrap()));
        assert!(
            crate::xmpp_omemo::private_store::PrivateStore::open_existing(&channel.storage)
                .is_err()
        );
        let storage = channel.storage.clone();
        drop(owner);
        drop(channel);
        for id in ["runtime", "queued", "interrupted"] {
            let args = vec![
                "inspect".into(),
                directory.path().to_string_lossy().into_owned(),
                storage.to_string_lossy().into_owned(),
                "private".into(),
                "bob@example.test".into(),
                "alice@example.test".into(),
                id.into(),
            ];
            let record = super::inbox_admin::run(&args)?;
            assert_eq!(
                record["phase"],
                if id == "interrupted" {
                    "admitted"
                } else {
                    "returned"
                }
            );
            let mut release = args;
            release[0] = "release-returned".into();
            release.extend([
                record["review_token"].as_str().unwrap().into(),
                "synthetic reviewed completion".into(),
            ]);
            let result = super::inbox_admin::run(&release);
            if id == "interrupted" {
                assert!(result.is_err());
            } else {
                assert_eq!(result?["released"], true);
            }
        }
        Ok(())
    }

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
    #[cfg(target_os = "linux")]
    #[test]
    fn tls_cache_eviction_preserves_encrypted_replay_and_reviewed_release() -> anyhow::Result<()> {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir_in("/var/tmp")?;
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700))?;
        let channel = adapter(root.path().into());
        let vault = zeroclaw_config::secrets::SecretStore::new(root.path(), true);
        vault.encrypt("synthetic initialization")?;
        let cipher = Arc::new(VaultInboxCipher {
            store: vault,
            alias: channel.alias.clone(),
        });
        let mut state = crate::xmpp_omemo::channel_state::ChannelState::open_or_initialize(
            &channel.storage,
            &channel.config.username,
            cipher,
            64000,
        )?;
        let peer = "alice@example.test";
        let id = "old-encrypted";
        state.stage_admitted_fixture(id, false)?;
        state.control_returned(peer, id, "queued_cancelled")?;
        drop(state);

        // Open the legacy encrypted store in TLS-only mode and exceed the cache limit.
        let db = channel.seen_database()?;
        db.execute(
            "WITH RECURSIVE ids(n) AS (VALUES(0) UNION ALL SELECT n+1 FROM ids WHERE n+1 < ?1)
             INSERT INTO seen(sender,id,tls_only) SELECT ?2,'tls-'||n,1 FROM ids",
            params![TLS_SEEN_LIMIT, peer],
        )?;
        XmppChannel::remember_tls_message(&db, peer, "new-tls")?;
        let tls_count: i64 =
            db.query_row("SELECT count(*) FROM seen WHERE tls_only=1", [], |r| {
                r.get(0)
            })?;
        assert_eq!(tls_count, TLS_SEEN_LIMIT);
        assert!(!db.query_row(
            "SELECT EXISTS(SELECT 1 FROM seen WHERE sender=?1 AND id='tls-0')",
            [peer],
            |r| r.get::<_, bool>(0)
        )?);
        drop(db);

        let mut args = vec![
            "inspect".into(),
            root.path().to_string_lossy().into_owned(),
            channel.storage.to_string_lossy().into_owned(),
            channel.alias.clone(),
            channel.config.username.clone(),
            peer.into(),
            id.into(),
        ];
        let review = inbox_admin::run(&args)?;
        assert_eq!(review["phase"], "returned");
        args[0] = "release-returned".into();
        args.extend([
            review["review_token"].as_str().unwrap().into(),
            "synthetic reviewed completion".into(),
        ]);
        assert_eq!(inbox_admin::run(&args)?["released"], true);
        let db = channel.seen_database()?;
        XmppChannel::remember_tls_message(&db, peer, "another-tls")?;
        assert!(db.query_row(
            "SELECT EXISTS(SELECT 1 FROM seen WHERE sender=?1 AND id=?2 AND tls_only=0)",
            params![peer, id],
            |r| r.get::<_, bool>(0)
        )?);
        Ok(())
    }
    #[cfg(target_os = "linux")]
    #[test]
    fn classified_tls_store_upgrades_to_omemo_without_losing_seen_ids() -> anyhow::Result<()> {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir_in("/var/tmp")?;
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700))?;
        let channel = adapter(root.path().into());
        let db = channel.seen_database()?;
        XmppChannel::remember_tls_message(&db, "alice@example.test", "tls-first")?;
        drop(db);
        let vault = zeroclaw_config::secrets::SecretStore::new(root.path(), true);
        vault.encrypt("synthetic initialization")?;
        let cipher = Arc::new(VaultInboxCipher {
            store: vault,
            alias: channel.alias.clone(),
        });
        let mut state = crate::xmpp_omemo::channel_state::ChannelState::open_or_initialize(
            &channel.storage,
            &channel.config.username,
            cipher,
            64000,
        )?;
        assert!(!state.public_bundle()?.xml.is_empty());
        drop(state);
        let db = channel.seen_database()?;
        assert!(db.query_row(
            "SELECT EXISTS(SELECT 1 FROM seen WHERE id='tls-first' AND tls_only=1)",
            [],
            |r| r.get::<_, bool>(0)
        )?);
        Ok(())
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

#[cfg(test)]
mod sasl_preference_tests {
    use super::prefer_authenticated_mechanisms;
    #[test]
    fn advertised_scram_matches_the_actual_client_mechanism() {
        use sasl::{
            client::{Mechanism, mechanisms::Scram},
            common::{ChannelBinding, Credentials, scram::Sha256},
        };
        for plus in [false, true] {
            let mut mechanisms = ["PLAIN", "SCRAM-SHA-256"]
                .into_iter()
                .map(str::to_owned)
                .collect::<std::collections::BTreeSet<_>>();
            if plus {
                mechanisms.insert("SCRAM-SHA-256-PLUS".into());
            }
            let binding = prefer_authenticated_mechanisms(
                &mut mechanisms,
                ChannelBinding::TlsExporter(vec![7; 32]),
            );
            let client = Scram::<Sha256>::from_credentials(
                Credentials::default()
                    .with_username("synthetic-user")
                    .with_password("synthetic-password")
                    .with_channel_binding(binding),
            )
            .unwrap();
            assert!(mechanisms.contains(client.name()));
            assert_eq!(client.name().ends_with("-PLUS"), plus);
            assert!(!mechanisms.contains("PLAIN"));
        }
    }
    #[test]
    fn server_order_cannot_downgrade_scram_to_plain() {
        let mut mechanisms = ["PLAIN", "SCRAM-SHA-1", "SCRAM-SHA-256"]
            .into_iter()
            .map(str::to_owned)
            .collect();
        prefer_authenticated_mechanisms(&mut mechanisms, sasl::common::ChannelBinding::None);
        assert_eq!(mechanisms, ["SCRAM-SHA-256".to_owned()].into());
    }
    #[test]
    fn tls_plain_only_servers_remain_supported() {
        let mut mechanisms = ["ANONYMOUS", "PLAIN"]
            .into_iter()
            .map(str::to_owned)
            .collect();
        prefer_authenticated_mechanisms(&mut mechanisms, sasl::common::ChannelBinding::None);
        assert_eq!(mechanisms, ["PLAIN".to_owned()].into());
    }
}
