//! The ZeroClaw nominated relay: a standalone **blind forwarder**.
//!
//! A daemon behind NAT keeps a persistent *control* connection to the relay and
//! claims a `node_id`. A client connects to the relay and asks for that
//! `node_id`; the relay tells the daemon to open a *data* connection, pairs it
//! with the client, and then transparently pipes bytes between the two
//! ([`tokio::io::copy_bidirectional`]). Those bytes are the inner client<->daemon
//! mTLS session: the relay never terminates or inspects it, holds no keys, and
//! routes only on the opaque `node_id`.
//!
//! Admission control decides which daemons may register (open vs allowlist, keyed
//! on the per-daemon relay token; deny always wins). It is operational access
//! control on the rendezvous, not RPC authorization, and does not weaken the
//! blind-forwarder property.
//!
//! `zerorelay` is a standalone networking app (not daemon-path code), so bare
//! `tokio::spawn` is the right primitive here; the `zeroclaw_spawn::spawn!` rule
//! is for in-daemon tasks. Mirrors the `apps/zerocode` exemption.
#![allow(clippy::disallowed_methods)]

use anyhow::{Context, Result};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::net::tcp::OwnedReadHalf;
use tokio::sync::{Mutex, mpsc};
use zeroclaw_relay_proto::Frame;

const MAX_CONTROL_FRAME: usize = 64 * 1024;

/// Which daemons may register a rendezvous on this relay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Admission {
    /// Any daemon that passes the deny list may register.
    Open,
    /// Only daemons whose relay token is on the allow list may register.
    Allowlist,
}

/// Relay admission policy. Deny always wins.
#[derive(Debug, Clone)]
pub struct RelayConfig {
    pub registration_mode: Admission,
    /// Relay tokens allowed to register (used in `Allowlist` mode).
    pub allow: HashSet<String>,
    /// Relay tokens always rejected.
    pub deny: HashSet<String>,
}

impl Default for RelayConfig {
    fn default() -> Self {
        Self {
            registration_mode: Admission::Open,
            allow: HashSet::new(),
            deny: HashSet::new(),
        }
    }
}

struct DaemonLink {
    /// Send a `conn_id` here to push an `Open` frame to the daemon control link.
    open_tx: mpsc::Sender<u64>,
}

struct Inner {
    cfg: RelayConfig,
    daemons: Mutex<HashMap<String, DaemonLink>>,
    pending: Mutex<HashMap<u64, TcpStream>>,
    next_conn: AtomicU64,
}

/// A running relay. Cheap to clone (`Arc` inside).
#[derive(Clone)]
pub struct RelayServer {
    inner: Arc<Inner>,
}

impl RelayServer {
    pub fn new(cfg: RelayConfig) -> Self {
        Self {
            inner: Arc::new(Inner {
                cfg,
                daemons: Mutex::new(HashMap::new()),
                pending: Mutex::new(HashMap::new()),
                next_conn: AtomicU64::new(1),
            }),
        }
    }

    /// Accept connections forever from `listener`.
    pub async fn serve(self, listener: tokio::net::TcpListener) -> Result<()> {
        loop {
            let (sock, _) = listener
                .accept()
                .await
                .context("accepting relay connection")?;
            let inner = self.inner.clone();
            tokio::spawn(async move {
                let _ = handle_conn(inner, sock).await;
            });
        }
    }
}

impl Inner {
    fn admit(&self, token: &str) -> bool {
        if self.cfg.deny.contains(token) {
            return false;
        }
        match self.cfg.registration_mode {
            Admission::Open => true,
            Admission::Allowlist => self.cfg.allow.contains(token),
        }
    }
}

/// Read exactly one newline-terminated control frame without over-reading into
/// the byte stream that follows (so transparent piping starts at the right byte).
async fn read_control_frame(sock: &mut TcpStream) -> Result<Frame> {
    let mut buf = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        let n = sock
            .read(&mut byte)
            .await
            .context("reading control frame")?;
        if n == 0 {
            anyhow::bail!("connection closed before a control frame");
        }
        if byte[0] == b'\n' {
            break;
        }
        buf.push(byte[0]);
        if buf.len() > MAX_CONTROL_FRAME {
            anyhow::bail!("control frame exceeds {MAX_CONTROL_FRAME} bytes");
        }
    }
    let line = String::from_utf8(buf).context("control frame is not UTF-8")?;
    Frame::from_line(&line).context("parsing control frame")
}

async fn write_frame<W: AsyncWriteExt + Unpin>(w: &mut W, frame: &Frame) -> Result<()> {
    w.write_all(frame.to_line().as_bytes())
        .await
        .context("writing control frame")?;
    w.flush().await.ok();
    Ok(())
}

async fn handle_conn(inner: Arc<Inner>, mut sock: TcpStream) -> Result<()> {
    let frame = read_control_frame(&mut sock).await?;
    match frame {
        Frame::Register {
            node_id,
            relay_token,
        } => handle_register(inner, sock, node_id, relay_token).await,
        Frame::Connect { node_id } => handle_connect(inner, sock, node_id).await,
        Frame::Accept { conn_id, .. } => handle_accept(inner, sock, conn_id).await,
        other => {
            let _ = write_frame(
                &mut sock,
                &Frame::error("bad_first_frame", format!("unexpected {other:?}")),
            )
            .await;
            Ok(())
        }
    }
}

/// Daemon control connection: claim a node-id and pump `Open` frames to it.
async fn handle_register(
    inner: Arc<Inner>,
    mut sock: TcpStream,
    node_id: String,
    relay_token: String,
) -> Result<()> {
    if !inner.admit(&relay_token) {
        let _ = write_frame(&mut sock, &Frame::error("forbidden", "registration denied")).await;
        return Ok(());
    }

    let (open_tx, mut open_rx) = mpsc::channel::<u64>(64);
    inner
        .daemons
        .lock()
        .await
        .insert(node_id.clone(), DaemonLink { open_tx });

    write_frame(
        &mut sock,
        &Frame::Registered {
            node_id: node_id.clone(),
        },
    )
    .await?;

    let (mut read_half, mut write_half) = sock.into_split();
    loop {
        tokio::select! {
            maybe = open_rx.recv() => match maybe {
                Some(conn_id) => {
                    if write_frame(&mut write_half, &Frame::Open { conn_id }).await.is_err() {
                        break;
                    }
                }
                None => break,
            },
            // The daemon control link carries no payload; any readable event is a
            // disconnect (EOF or error). Use it to detect the daemon going away.
            _ = wait_for_close(&mut read_half) => break,
        }
    }

    inner.daemons.lock().await.remove(&node_id);
    Ok(())
}

/// Resolves when the half-connection reaches EOF or errors.
async fn wait_for_close(read_half: &mut OwnedReadHalf) {
    let mut buf = [0u8; 256];
    loop {
        match read_half.read(&mut buf).await {
            Ok(0) | Err(_) => return,
            Ok(_) => {} // unexpected payload on the control link; ignore
        }
    }
}

/// Client connection: route it to the daemon serving `node_id`.
async fn handle_connect(inner: Arc<Inner>, mut sock: TcpStream, node_id: String) -> Result<()> {
    let open_tx = {
        let daemons = inner.daemons.lock().await;
        match daemons.get(&node_id) {
            Some(link) => link.open_tx.clone(),
            None => {
                let _ = write_frame(&mut sock, &Frame::error("no_such_node", node_id)).await;
                return Ok(());
            }
        }
    };

    let conn_id = inner.next_conn.fetch_add(1, Ordering::Relaxed);
    // Tell the client the route is open before parking it; the bytes it sends now
    // buffer in the kernel until the daemon's data connection pairs and drains.
    write_frame(&mut sock, &Frame::Opened { conn_id }).await?;
    inner.pending.lock().await.insert(conn_id, sock);

    if open_tx.send(conn_id).await.is_err() {
        // Daemon vanished between lookup and notify.
        inner.pending.lock().await.remove(&conn_id);
    }
    Ok(())
}

/// Daemon data connection: pair with the waiting client and blind-pipe.
async fn handle_accept(inner: Arc<Inner>, mut daemon_data: TcpStream, conn_id: u64) -> Result<()> {
    let client = inner.pending.lock().await.remove(&conn_id);
    match client {
        Some(mut client) => {
            // Opaque, end-to-end: the relay never inspects these bytes.
            let _ = tokio::io::copy_bidirectional(&mut client, &mut daemon_data).await;
            Ok(())
        }
        None => Ok(()), // stale / unknown conn_id
    }
}
