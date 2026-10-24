//! XMPP transport adapter. The embedded Slixmpp worker owns protocol/OMEMO;
//! the shared channel runtime continues to own sessions, tools and agent turns.
use anyhow::{Context, bail, ensure};
use async_trait::async_trait;
use futures_util::StreamExt;
use portable_atomic::{AtomicBool, Ordering};
use serde::Deserialize;
use serde_json::json;
use std::{collections::HashMap, path::PathBuf, process::Stdio, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncWrite, AsyncWriteExt},
    sync::{Mutex, mpsc, oneshot},
};
use tokio_util::codec::{FramedRead, LinesCodec};
use zeroclaw_api::channel::{Channel, ChannelMessage, SendMessage};
use zeroclaw_config::schema::XmppConfig;

const BRIDGE: &str = include_str!("xmpp_bridge.py");
const MAX_FRAME: usize = 512 * 1024;
type PeerResolver = Arc<dyn Fn() -> Vec<String> + Send + Sync>;

struct Outbound {
    recipient: String,
    content: String,
    result: oneshot::Sender<bool>,
}

async fn write_frames<W: AsyncWrite + Unpin>(
    mut stdin: W,
    mut frames: mpsc::Receiver<Vec<u8>>,
) -> std::io::Result<()> {
    while let Some(frame) = frames.recv().await {
        stdin.write_all(&frame).await?;
        stdin.flush().await?;
    }
    Ok(())
}

pub struct XmppChannel {
    config: XmppConfig,
    alias: String,
    storage: PathBuf,
    peers: PeerResolver,
    online: AtomicBool,
    outbound: Mutex<Option<mpsc::Sender<Outbound>>>,
}

#[derive(Deserialize)]
struct BridgeEvent {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    id: String,
    #[serde(default)]
    from: String,
    #[serde(default)]
    body: String,
    #[serde(default)]
    encrypted: bool,
    #[serde(default)]
    ok: bool,
}

impl XmppChannel {
    pub fn new(config: XmppConfig, alias: String, data_dir: PathBuf, peers: PeerResolver) -> Self {
        Self {
            config,
            storage: data_dir.join("channels").join("xmpp").join(&alias),
            alias,
            peers,
            online: AtomicBool::new(false),
            outbound: Mutex::new(None),
        }
    }

    fn allowed(&self, jid: &str) -> bool {
        // Worker normalizes JIDs with the platform's JID parser. A full JID is
        // never a separate identity/session and is not accepted as a grant.
        let Ok(account) = jid.parse::<jid::BareJid>() else {
            return false;
        };
        account.node().is_some()
            && !jid.contains('/')
            && crate::allowlist::is_user_allowed_by(&(self.peers)(), jid, |entry, user| {
                match (entry.parse::<jid::BareJid>(), user.parse::<jid::BareJid>()) {
                    (Ok(a), Ok(b)) => a == b,
                    _ => false,
                }
            })
    }

    fn inbound(&self, event: BridgeEvent) -> Option<ChannelMessage> {
        if !self.allowed(&event.from)
            || event.from.eq_ignore_ascii_case(&self.config.username)
            || event.body.trim().is_empty()
            || event.body.chars().count() > self.config.max_message_chars
            || (self.config.omemo_enabled && !event.encrypted)
        {
            return None;
        }
        Some(ChannelMessage {
            id: event.id,
            sender: event.from.clone(),
            platform_sender_id: Some(event.from.clone()),
            reply_target: event.from,
            content: event.body,
            channel: "xmpp".into(),
            channel_alias: Some(self.alias.clone()),
            timestamp: chrono::Utc::now().timestamp().max(0) as u64,
            ..Default::default()
        })
    }

    async fn run(&self, tx: mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        let interpreter =
            std::env::var_os("ZEROCLAW_XMPP_PYTHON").unwrap_or_else(|| "python3".into());
        self.run_with_interpreter(tx, interpreter).await
    }

    // Only the trusted process environment chooses the executable. Channel
    // configuration (including Web/API updates) never becomes a command path.
    async fn run_with_interpreter(
        &self,
        tx: mpsc::Sender<ChannelMessage>,
        interpreter: std::ffi::OsString,
    ) -> anyhow::Result<()> {
        ensure!(
            !self.alias.is_empty()
                && self.alias != "."
                && self.alias != ".."
                && !self.alias.contains(['/', '\\', ':']),
            "XMPP alias must be a single state-directory component"
        );
        let mut child = tokio::process::Command::new(interpreter)
            .args(["-I", "-u", "-c", BRIDGE])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .context("XMPP bridge could not start; install the documented Python requirements")?;
        let mut stdin = child
            .stdin
            .take()
            .context("XMPP bridge stdin unavailable")?;
        let stdout = child
            .stdout
            .take()
            .context("XMPP bridge stdout unavailable")?;
        // Password travels only through the private pipe, never argv/env/files/logs.
        let setup = json!({"config": self.config, "storage": self.storage});
        stdin.write_all(format!("{setup}\n").as_bytes()).await?;
        stdin.flush().await?;
        let mut frames = FramedRead::new(stdout, LinesCodec::new_with_max_length(MAX_FRAME));
        let (writes, write_rx) = mpsc::channel(32);
        // Poll writes beside reads: neither pipe may prevent the other draining.
        let writer = write_frames(stdin, write_rx);
        tokio::pin!(writer);
        let (outbound, mut rx) = mpsc::channel::<Outbound>(32);
        let mut pending: HashMap<String, oneshot::Sender<bool>> = HashMap::new();
        let deadline = tokio::time::sleep(Duration::from_secs(self.config.connect_timeout_secs));
        tokio::pin!(deadline);
        let mut sequence = 0u64;
        loop {
            tokio::select! {
                result = &mut writer => {
                    result.context("XMPP bridge write failed")?;
                    bail!("XMPP bridge writer stopped");
                }
                () = &mut deadline, if !self.online.load(Ordering::Acquire) => {
                    bail!("XMPP TLS/authentication deadline exceeded");
                }
                frame = frames.next() => {
                    let line = frame.context("XMPP bridge stopped")?.context("XMPP bridge frame exceeded limit")?;
                    let event: BridgeEvent = serde_json::from_str(&line).context("Invalid XMPP bridge event")?;
                    match event.kind.as_str() {
                        "online" => {
                            self.online.store(true, Ordering::Release);
                            *self.outbound.lock().await = Some(outbound.clone());
                        }
                        "message" => {
                            if let Some(message) = self.inbound(event)
                                && tx.send(message).await.is_err() { return Ok(()); }
                        }
                        "sent" => { if let Some(waiter) = pending.remove(&event.id) { let _ = waiter.send(event.ok); } }
                        "fatal" => bail!("XMPP bridge failed; check dependencies, TLS, credentials and device trust"),
                        _ => bail!("Unknown XMPP bridge event"),
                    }
                }
                request = rx.recv(), if pending.len() < 32 => {
                    let Some(request) = request else { return Ok(()); };
                    if request.result.is_closed() { continue; }
                    ensure!(self.online.load(Ordering::Acquire), "XMPP channel is offline");
                    if !self.allowed(&request.recipient) { let _ = request.result.send(false); continue; }
                    sequence += 1;
                    let id = sequence.to_string();
                    let frame = json!({"type":"send", "id":id, "to":request.recipient, "body":request.content});
                    writes.try_send(format!("{frame}\n").into_bytes())
                        .context("XMPP bridge write queue unavailable")?;
                    pending.insert(id, request.result);
                }
            }
        }
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
    // The bridge accepts only one-to-one chat/normal stanzas, never MUC.
    fn is_direct_message(&self, _msg: &ChannelMessage) -> bool {
        true
    }
    fn self_handle(&self) -> Option<String> {
        Some(self.config.username.clone())
    }
    async fn health_check(&self) -> bool {
        self.online.load(Ordering::Acquire)
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
            .await
            .clone()
            .context("XMPP channel is offline")?;
        let (result, ack) = oneshot::channel();
        tokio::time::timeout(Duration::from_secs(60), async {
            sender
                .send(Outbound {
                    recipient: message.recipient.clone(),
                    content: message.content.clone(),
                    result,
                })
                .await
                .context("XMPP channel stopped")?;
            ensure!(
                ack.await.context("XMPP channel stopped before sending")?,
                "XMPP send failed; no plaintext fallback was used"
            );
            Ok(())
        })
        .await
        .context("XMPP send timed out")?
    }
    async fn listen(&self, tx: mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        // Also runs on cancellation: no orphan bridge or stale online health.
        struct Reset<'a>(&'a XmppChannel);
        impl Drop for Reset<'_> {
            fn drop(&mut self) {
                self.0.online.store(false, Ordering::Release);
                if let Ok(mut sender) = self.0.outbound.try_lock() {
                    *sender = None;
                }
            }
        }
        let _reset = Reset(self);
        self.run(tx).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn channel(encrypted: bool) -> XmppChannel {
        XmppChannel::new(
            XmppConfig {
                username: "bot@example.org".into(),
                omemo_enabled: encrypted,
                ..Default::default()
            },
            "private".into(),
            PathBuf::from("/tmp/test"),
            Arc::new(|| vec!["alice@example.org".into(), "!mallory@example.org".into()]),
        )
    }
    fn event(from: &str, encrypted: bool) -> BridgeEvent {
        BridgeEvent {
            kind: "message".into(),
            id: "one".into(),
            from: from.into(),
            body: "hello".into(),
            encrypted,
            ok: false,
        }
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn full_pipes_do_not_deadlock_listener_and_reply() {
        use std::os::unix::fs::PermissionsExt;
        let folder = tempfile::tempdir().expect("fixture directory");
        let interpreter = folder.path().join("worker");
        // This protocol fixture fills stdout before reading a large send from
        // stdin. The old listener blocked writing stdin and never drained it.
        std::fs::write(&interpreter, r#"#!/usr/bin/python3
import json,sys,time
json.loads(sys.stdin.readline())
print(json.dumps({'type':'online'}),flush=True)
time.sleep(.1)
print(json.dumps({'type':'message','id':'incoming','from':'alice@example.org','body':chr(0x1f680)*64000,'encrypted':False},ensure_ascii=False),flush=True)
command=json.loads(sys.stdin.readline())
print(json.dumps({'type':'sent','id':command['id'],'ok':True}),flush=True)
time.sleep(30)
"#).expect("fixture worker");
        std::fs::set_permissions(&interpreter, std::fs::Permissions::from_mode(0o700))
            .expect("executable fixture");
        let mut adapter = channel(false);
        adapter.config.max_message_chars = 64000;
        let adapter = Arc::new(adapter);
        let (tx, mut inbound) = mpsc::channel(2);
        let listener_adapter = adapter.clone();
        let listener = ::zeroclaw_spawn::spawn!(async move {
            listener_adapter
                .run_with_interpreter(tx, interpreter.into_os_string())
                .await
        });
        let result = tokio::time::timeout(Duration::from_secs(5), async {
            while !adapter.online.load(Ordering::Acquire) {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
            let message = SendMessage::new("\u{1f680}".repeat(64000), "alice@example.org");
            let reply = adapter.send(&message);
            let receive = inbound.recv();
            let (sent, message) = tokio::join!(reply, receive);
            sent.expect("large reply acknowledged");
            assert_eq!(
                message
                    .expect("large incoming message")
                    .content
                    .chars()
                    .count(),
                64000
            );
        })
        .await;
        listener.abort();
        let _ = listener.await;
        result.expect("both pipe directions make progress");
    }

    #[test]
    fn authorization_and_required_encryption_precede_dispatch() {
        let channel = channel(true);
        assert!(channel.inbound(event("alice@example.org", false)).is_none());
        assert!(
            channel
                .inbound(event("mallory@example.org", true))
                .is_none()
        );
        assert!(
            channel
                .inbound(event("alice@example.org/phone", true))
                .is_none()
        );
        assert!(!channel.allowed("example.org"));
        let message = channel
            .inbound(event("alice@example.org", true))
            .expect("authorized encrypted message");
        assert_eq!(message.channel_alias.as_deref(), Some("private"));
        assert_eq!(message.sender, message.reply_target);
        assert!(channel.is_direct_message(&message));
    }
    #[tokio::test]
    async fn offline_or_unauthorized_send_fails_without_starting_transport() {
        let channel = channel(false);
        assert!(!channel.health_check().await);
        assert!(
            channel
                .send(&SendMessage::new("hello", "mallory@example.org"))
                .await
                .is_err()
        );
        assert!(
            channel
                .send(&SendMessage::new("hello", "alice@example.org"))
                .await
                .is_err()
        );
    }
}
