//! Per-node dashboard hosts: the relay serves an allowlisted daemon's web
//! dashboard at `https://<label>.<domain>/` (`[dashboard]`, off by default).
//!
//! This is the one route on the relay that is NOT a blind splice. The relay
//! ends the browser's TLS (with the operator's wildcard certificate for
//! `*.<domain>`, chosen by the TLS server name) and forwards the plaintext
//! HTTP to the daemon over its relay link, as a route with the
//! [`PEER_HINT_GATEWAY`] hint and the browser's address. The daemon hands it to
//! its gateway in process, which serves it as that browser and builds every
//! URL from the dashboard's own origin. So this relay's operator can read and
//! change that dashboard traffic - which is why the tier is opt-in on both
//! ends, limited to daemons the operator lists, and warns at startup.
//!
//! Routing is by the TLS server name alone: the relay never parses the HTTP it
//! forwards (the gateway refuses a `Host` that does not match, with 421), and
//! the listener offers browsers HTTP/1.1 only, so a browser never reuses one
//! connection for another dashboard host.
//!
//! The label comes from the daemon's registration key
//! ([`zeroclaw_relay_proto::dashboard_label_from_digest`]), which this relay
//! verified at `Register`, so only that key's holder is ever served under it,
//! and it does not change when the node-id rotates. A rotation's grace window
//! briefly registers two node-ids for one key; the newest registration wins.

use crate::enroll_route::{DaemonLink, OpenError, open_route_on};
use crate::{Inner, REFUSAL_WRITE_BUDGET};
use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::time::{Duration, Instant};
use zeroclaw_relay_proto::{PEER_HINT_GATEWAY, is_dashboard_label};

/// How long the relay waits for a browser's request head before answering a
/// connection it will not route. Only refusals read anything: a routed
/// connection is forwarded unread.
const REFUSAL_READ_BUDGET: Duration = Duration::from_secs(5);

/// Refusals the dashboard tier answers at once (see [`DashboardTier::refuse`]).
///
/// A dashboard connection gives back its pre-admission handshake permit as
/// soon as it is classified, so browser traffic can never shed the daemons
/// and native clients that pool protects. A refusal then holds a TLS stream,
/// a read buffer and a task for up to its read and write budgets, and the
/// browser it answers holds at most an address slot: one over its address's
/// cap holds none. This pool is what bounds them across addresses. Past it, a
/// refusal is dropped unanswered. Routed connections never take from it:
/// their per-address and per-daemon caps bound them.
pub(crate) const MAX_DASHBOARD_REFUSALS: usize = 64;

/// The operator's `[dashboard]` settings.
#[derive(Debug, Clone)]
pub struct DashboardConfig {
    /// Dashboards are served at `https://<label>.<domain>/`.
    pub domain: String,
    /// Daemon pubkey fingerprints (sha256 hex) whose dashboards this relay
    /// serves. Never empty: [`check_dashboard_startup`] refuses that.
    pub allow: HashSet<String>,
    /// Concurrent dashboard connections to one daemon. Separate from
    /// `max_conns_per_node`, which they also count against, so browsers can
    /// never take every slot its native clients use.
    pub max_conns_per_node: usize,
    /// Concurrent dashboard connections from one browser address.
    pub max_conns_per_ip: usize,
}

/// Refuse a `[dashboard]` the relay must not run.
///
/// The allowlist is required in every admission mode. In open mode in
/// particular, any daemon can register, and without it every one of them
/// would get a trusted HTTPS host under this relay's domain - free hosting
/// for whatever its gateway serves, under the operator's name.
pub fn check_dashboard_startup(cfg: &DashboardConfig) -> anyhow::Result<()> {
    if !is_dns_name(&cfg.domain) {
        anyhow::bail!(
            "refusing to start: [dashboard] domain must be a lower-case DNS name such as \
             relay.example.com (dashboards are served at https://<label>.<domain>/), got {:?}",
            cfg.domain
        );
    }
    if cfg.allow.is_empty() {
        anyhow::bail!(
            "refusing to start: [dashboard] is enabled but [dashboard] allow is empty. List the \
             registration-key fingerprints of the daemons whose dashboards this relay may serve; \
             without the list, an open relay would serve a dashboard host to any daemon that \
             registers."
        );
    }
    if cfg.max_conns_per_node == 0 || cfg.max_conns_per_ip == 0 {
        anyhow::bail!("refusing to start: [dashboard] connection caps must be at least 1");
    }
    Ok(())
}

/// Whether `name` is a lower-case DNS name of at least two labels, the last
/// not numeric.
pub fn is_dns_name(name: &str) -> bool {
    let labels: Vec<&str> = name.split('.').collect();
    name.len() <= 253
        && labels.len() >= 2
        && !labels
            .last()
            .is_some_and(|tld| tld.bytes().all(|b| b.is_ascii_digit()))
        && labels.iter().all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        })
}

/// What a TLS server name under the dashboard domain asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DashboardHost {
    /// `<label>.<domain>` with a well-formed label.
    Label(String),
    /// Some other name under the domain: answered with "no dashboard here",
    /// never handed to the relay plane.
    Unknown,
}

/// The running tier: its settings and the per-address connection counts.
pub(crate) struct DashboardTier {
    cfg: DashboardConfig,
    per_ip: std::sync::Mutex<HashMap<IpAddr, usize>>,
    refusals: tokio::sync::Semaphore,
}

impl DashboardTier {
    pub(crate) fn new(cfg: DashboardConfig) -> Self {
        Self {
            cfg,
            per_ip: std::sync::Mutex::new(HashMap::new()),
            refusals: tokio::sync::Semaphore::new(MAX_DASHBOARD_REFUSALS),
        }
    }

    /// Whether the daemon with pubkey fingerprint `fpr` may be served.
    pub(crate) fn allows(&self, fpr: &str) -> bool {
        self.cfg.allow.contains(fpr)
    }

    pub(crate) fn max_conns_per_node(&self) -> usize {
        self.cfg.max_conns_per_node
    }

    /// Classify a TLS server name: `None` when it is not under the dashboard
    /// domain (the connection takes the relay's normal path).
    pub(crate) fn classify(&self, server_name: &str) -> Option<DashboardHost> {
        let name = server_name.to_ascii_lowercase();
        let label = name.strip_suffix(&self.cfg.domain)?.strip_suffix('.')?;
        Some(if is_dashboard_label(label) {
            DashboardHost::Label(label.to_string())
        } else {
            DashboardHost::Unknown
        })
    }

    /// Answer a connection the relay will not route, under the refusal pool.
    /// With the pool spent the browser gets no answer: dropping the stream
    /// closes it.
    async fn refuse<S>(&self, browser: &mut S, status: u16, message: &str)
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        let Ok(_refusal) = self.refusals.try_acquire() else {
            return;
        };
        refuse(browser, status, message).await;
    }

    /// Take one of `ip`'s connection slots, or `None` when it has none left.
    fn take_ip_slot(self: &Arc<Self>, ip: IpAddr) -> Option<IpSlot> {
        let mut per_ip = self
            .per_ip
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let live = per_ip.entry(ip).or_insert(0);
        if *live >= self.cfg.max_conns_per_ip {
            return None;
        }
        *live += 1;
        Some(IpSlot {
            tier: Arc::clone(self),
            ip,
        })
    }
}

/// One browser address's connection slot; freed on drop.
struct IpSlot {
    tier: Arc<DashboardTier>,
    ip: IpAddr,
}

impl Drop for IpSlot {
    fn drop(&mut self) {
        let mut per_ip = self
            .tier
            .per_ip
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(live) = per_ip.get_mut(&self.ip) {
            *live -= 1;
            if *live == 0 {
                per_ip.remove(&self.ip);
            }
        }
    }
}

/// One of a daemon's dashboard connection slots; freed on drop.
struct NodeSlot(Arc<AtomicUsize>);

impl NodeSlot {
    fn take(live: Arc<AtomicUsize>, max: usize) -> Option<Self> {
        if live.fetch_add(1, Ordering::AcqRel) >= max {
            live.fetch_sub(1, Ordering::AcqRel);
            return None;
        }
        Some(Self(live))
    }
}

impl Drop for NodeSlot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Serve one browser connection for a dashboard host, until either side
/// ends or it idles past the relay's `idle_timeout`.
pub(crate) async fn serve<S>(
    inner: Arc<Inner>,
    mut browser: S,
    host: DashboardHost,
    peer: SocketAddr,
) where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    let Some(tier) = inner.dashboard.clone() else {
        return;
    };
    let Some(_ip_slot) = tier.take_ip_slot(peer.ip()) else {
        tier.refuse(
            &mut browser,
            429,
            "Too many connections from this address. Try again shortly.",
        )
        .await;
        return;
    };
    let DashboardHost::Label(label) = host else {
        tier.refuse(&mut browser, 404, "There is no dashboard at this address.")
            .await;
        return;
    };
    // The newest registration under this label: during a node-id rotation's
    // grace window the same key holds two node-ids for a while. Its link is
    // taken under the same lock as the match, and the route is opened on that
    // link, not looked up again by node-id: the node-id is the daemon's own
    // choice, and once this daemon goes away another key may register it.
    let target = {
        let daemons = inner.daemons.lock().await;
        daemons
            .values()
            .filter(|handle| handle.dashboard_label.as_deref() == Some(label.as_str()))
            .max_by_key(|handle| handle.epoch)
            .map(|handle| (DaemonLink::of(handle), Arc::clone(&handle.dashboard_live)))
    };
    let Some((link, node_live)) = target else {
        tier.refuse(
            &mut browser,
            404,
            "There is no dashboard at this address, or its daemon is not connected.",
        )
        .await;
        return;
    };
    let Some(_node_slot) = NodeSlot::take(node_live, tier.max_conns_per_node()) else {
        tier.refuse(
            &mut browser,
            503,
            "This dashboard is busy. Try again shortly.",
        )
        .await;
        return;
    };
    let route = match open_route_on(&inner, link, PEER_HINT_GATEWAY, Some(peer.to_string())).await {
        Ok(route) => route,
        Err(error) => {
            let (status, message) = match error {
                OpenError::NoSuchNode => (404, "This dashboard's daemon is not connected."),
                OpenError::RateLimited => (429, "Too many new connections. Try again shortly."),
                OpenError::Busy | OpenError::NotAccepted => {
                    (503, "This dashboard is unavailable. Try again shortly.")
                }
            };
            tier.refuse(&mut browser, status, message).await;
            return;
        }
    };
    let (daemon, _route) = route.split();
    splice(browser, daemon, inner.idle_timeout).await;
}

/// Copy both ways until either side ends, nothing has moved either way for
/// `idle`, or one side has not taken a write for `idle`; then close both,
/// giving each close [`REFUSAL_WRITE_BUDGET`].
///
/// Every write is flushed before the next read (see [`write_flushed`]): the
/// browser side is a TLS stream, which can hold the end of a response in its
/// own buffer until something flushes it.
///
/// The write is bounded on its own because the idle timer is not polled while
/// a write waits: without it, a browser that asks for a page and then stops
/// reading would hold its dashboard and route slots past `idle`, and enough of
/// them would make the dashboard busy for everyone else. The close is bounded
/// for the same browser: closing a TLS stream sends `close_notify`, which has
/// to get past whatever the browser left unread.
async fn splice<A, B>(a: A, b: B, idle: Duration)
where
    A: AsyncRead + AsyncWrite + Unpin,
    B: AsyncRead + AsyncWrite + Unpin,
{
    let (mut a_read, mut a_write) = tokio::io::split(a);
    let (mut b_read, mut b_write) = tokio::io::split(b);
    let mut a_buf = vec![0u8; 16 * 1024];
    let mut b_buf = vec![0u8; 16 * 1024];
    let mut deadline = Instant::now() + idle;
    loop {
        tokio::select! {
            () = tokio::time::sleep_until(deadline) => break,
            read = a_read.read(&mut a_buf) => match read {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if !wrote_within(idle, write_flushed(&mut b_write, &a_buf[..n])).await {
                        break;
                    }
                    deadline = Instant::now() + idle;
                }
            },
            read = b_read.read(&mut b_buf) => match read {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if !wrote_within(idle, write_flushed(&mut a_write, &b_buf[..n])).await {
                        break;
                    }
                    deadline = Instant::now() + idle;
                }
            },
        }
    }
    let _ = tokio::time::timeout(REFUSAL_WRITE_BUDGET, a_write.shutdown()).await;
    let _ = tokio::time::timeout(REFUSAL_WRITE_BUDGET, b_write.shutdown()).await;
}

/// Write all of `bytes`, then flush them.
///
/// `write_all` alone is not enough on a TLS stream: it returns once the
/// stream has taken the plaintext, which may still be in the TLS session's
/// own send buffer (up to 64 KiB with rustls) when the socket was full. Only
/// a flush, or the next write, sends the rest. A keep-alive connection has no
/// next write once a response is done, so without this the browser is left
/// waiting on the end of that response until the connection closes, and the
/// close flushes it. The frontdoor's keep-alive loop uses it for the same
/// reason.
pub(crate) async fn write_flushed<W>(writer: &mut W, bytes: &[u8]) -> std::io::Result<()>
where
    W: AsyncWrite + Unpin,
{
    writer.write_all(bytes).await?;
    writer.flush().await
}

/// Whether `write` finished, successfully, within `idle`.
async fn wrote_within(
    idle: Duration,
    write: impl std::future::Future<Output = std::io::Result<()>>,
) -> bool {
    matches!(tokio::time::timeout(idle, write).await, Ok(Ok(())))
}

/// Answer a connection the relay will not route: read what the browser sent
/// (so closing does not reset the connection before it reads the answer), send
/// a fixed page and close. Sending and closing share [`REFUSAL_WRITE_BUDGET`],
/// so a browser that never reads gives up its slots with the rest.
async fn refuse<S>(browser: &mut S, status: u16, message: &str)
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut head = [0u8; 16 * 1024];
    let mut read = 0;
    let _ = tokio::time::timeout(REFUSAL_READ_BUDGET, async {
        while read < head.len() {
            match browser.read(&mut head[read..]).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    read += n;
                    if head[..read].windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
            }
        }
    })
    .await;
    let reason = match status {
        404 => "Not Found",
        429 => "Too Many Requests",
        _ => "Service Unavailable",
    };
    let body = format!(
        "<!doctype html><meta charset=\"utf-8\"><title>{reason}</title>\
         <body style=\"font-family:system-ui;margin:3rem\"><h1>{reason}</h1><p>{message}</p>\n"
    );
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\ncontent-type: text/html; charset=utf-8\r\n\
         content-length: {}\r\ncache-control: no-store\r\nx-content-type-options: nosniff\r\n\
         connection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = tokio::time::timeout(REFUSAL_WRITE_BUDGET, async {
        let _ = browser.write_all(response.as_bytes()).await;
        let _ = browser.shutdown().await;
    })
    .await;
}

/// A TLS client and server over an in-memory socket that holds `capacity`
/// bytes each way, for tests that need a real TLS writer: one that keeps what
/// the socket can't take in its own buffer until it is flushed.
#[cfg(test)]
pub(crate) async fn tls_over_duplex(
    capacity: usize,
) -> (
    tokio_rustls::client::TlsStream<tokio::io::DuplexStream>,
    tokio_rustls::server::TlsStream<tokio::io::DuplexStream>,
) {
    let (ca_cert, ca_key) = zeroclaw_tls::testing::gen_ca();
    let (cert, key) =
        zeroclaw_tls::testing::gen_server_cert(&ca_cert, &ca_key, &["abc.relay.test".to_string()]);
    let pem_certs = |pem: &str| {
        rustls_pemfile::certs(&mut pem.as_bytes())
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    };
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let server = rustls::ServerConfig::builder_with_provider(Arc::clone(&provider))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            pem_certs(&cert),
            rustls_pemfile::private_key(&mut key.as_bytes())
                .unwrap()
                .unwrap(),
        )
        .unwrap();
    let mut roots = rustls::RootCertStore::empty();
    for ca in pem_certs(&ca_cert) {
        roots.add(ca).unwrap();
    }
    let client = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();

    let (client_io, server_io) = tokio::io::duplex(capacity);
    let accepting = tokio::spawn(async move {
        tokio_rustls::TlsAcceptor::from(Arc::new(server))
            .accept(server_io)
            .await
            .unwrap()
    });
    let name = rustls::pki_types::ServerName::try_from("abc.relay.test").unwrap();
    let client = tokio_rustls::TlsConnector::from(Arc::new(client))
        .connect(name, client_io)
        .await
        .unwrap();
    (client, accepting.await.unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::pin::Pin;
    use std::task::{Context, Poll};
    use tokio::io::ReadBuf;

    /// A stream whose close never completes, as a TLS stream's doesn't when
    /// its `close_notify` cannot get past what the browser left unread.
    struct NeverCloses<S>(S);

    impl<S: AsyncRead + Unpin> AsyncRead for NeverCloses<S> {
        fn poll_read(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<std::io::Result<()>> {
            Pin::new(&mut self.0).poll_read(cx, buf)
        }
    }

    impl<S: AsyncWrite + Unpin> AsyncWrite for NeverCloses<S> {
        fn poll_write(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<std::io::Result<usize>> {
            Pin::new(&mut self.0).poll_write(cx, buf)
        }

        fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Pin::new(&mut self.0).poll_flush(cx)
        }

        fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Poll::Pending
        }
    }

    fn cfg() -> DashboardConfig {
        DashboardConfig {
            domain: "relay.example.com".into(),
            allow: HashSet::from(["ab".repeat(32)]),
            max_conns_per_node: 2,
            max_conns_per_ip: 2,
        }
    }

    #[test]
    fn an_empty_allowlist_refuses_to_start() {
        check_dashboard_startup(&cfg()).unwrap();
        let mut open = cfg();
        open.allow.clear();
        let err = check_dashboard_startup(&open).unwrap_err().to_string();
        assert!(err.contains("allow is empty"), "{err}");
        for domain in [
            "",
            "localhost",
            "Relay.Example.com",
            "203.0.113.7",
            "relay.example.com.",
        ] {
            let mut bad = cfg();
            bad.domain = domain.into();
            assert!(check_dashboard_startup(&bad).is_err(), "{domain:?}");
        }
    }

    #[test]
    fn only_names_one_label_under_the_domain_are_dashboard_hosts() {
        let tier = DashboardTier::new(cfg());
        let label = "a".repeat(26);
        assert_eq!(
            tier.classify(&format!("{label}.relay.example.com")),
            Some(DashboardHost::Label(label.clone()))
        );
        assert_eq!(
            tier.classify(&format!("{}.RELAY.example.com", label.to_uppercase())),
            Some(DashboardHost::Label(label.clone())),
            "server names are case-insensitive"
        );
        for unknown in [
            "short.relay.example.com",
            "x.y.relay.example.com",
            ".relay.example.com",
        ] {
            assert_eq!(
                tier.classify(unknown),
                Some(DashboardHost::Unknown),
                "{unknown}"
            );
        }
        for elsewhere in [
            "relay.example.com",
            "evil.example",
            &format!("{label}.relay.example.com.evil.example"),
            &format!("{label}xrelay.example.com"),
        ] {
            assert_eq!(tier.classify(elsewhere), None, "{elsewhere}");
        }
    }

    #[test]
    fn connection_slots_are_capped_per_address_and_per_node() {
        let tier = Arc::new(DashboardTier::new(cfg()));
        let ip: IpAddr = "203.0.113.7".parse().unwrap();
        let first = tier.take_ip_slot(ip).unwrap();
        let _second = tier.take_ip_slot(ip).unwrap();
        assert!(tier.take_ip_slot(ip).is_none());
        assert!(tier.take_ip_slot("198.51.100.4".parse().unwrap()).is_some());
        drop(first);
        assert!(tier.take_ip_slot(ip).is_some());

        let live = Arc::new(AtomicUsize::new(0));
        let a = NodeSlot::take(Arc::clone(&live), 2).unwrap();
        let _b = NodeSlot::take(Arc::clone(&live), 2).unwrap();
        assert!(NodeSlot::take(Arc::clone(&live), 2).is_none());
        assert_eq!(live.load(Ordering::Relaxed), 2);
        drop(a);
        assert!(NodeSlot::take(Arc::clone(&live), 2).is_some());
    }

    #[tokio::test]
    async fn a_refusal_reads_the_request_and_answers_with_a_fixed_page() {
        let (mut browser, mut relay) = tokio::io::duplex(64 * 1024);
        browser
            .write_all(b"GET / HTTP/1.1\r\nHost: x\r\n\r\n")
            .await
            .unwrap();
        refuse(&mut relay, 404, "There is no dashboard at this address.").await;
        let mut answer = String::new();
        browser.read_to_string(&mut answer).await.unwrap();
        assert!(answer.starts_with("HTTP/1.1 404 Not Found\r\n"), "{answer}");
        assert!(answer.contains("connection: close"), "{answer}");
        assert!(answer.contains("There is no dashboard at this address."));
    }

    #[tokio::test]
    async fn a_browser_that_stops_reading_is_let_go_after_the_idle_timeout() {
        // The browser has asked for a page and reads nothing more; the daemon
        // keeps answering until the relay's write to the browser blocks.
        let (browser, relay_browser_side) = tokio::io::duplex(1024);
        let (relay_daemon_side, mut daemon) = tokio::io::duplex(64 * 1024);
        let answering = tokio::spawn(async move {
            let chunk = vec![b'x'; 16 * 1024];
            while daemon.write_all(&chunk).await.is_ok() {}
        });
        let idle = Duration::from_millis(200);
        let started = Instant::now();
        tokio::time::timeout(
            Duration::from_secs(10),
            splice(relay_browser_side, relay_daemon_side, idle),
        )
        .await
        .expect("a browser that stops reading must not hold its slots past the idle timeout");
        assert!(started.elapsed() >= idle);
        drop(browser);
        answering.abort();
    }

    // The two tests below run on a paused clock: tokio moves it on whenever
    // every task is parked, so the budgets pass instantly, and an unbounded
    // close leaves only the test's own guard to fire.

    #[tokio::test(start_paused = true)]
    async fn a_browser_that_stops_reading_cannot_hold_the_close_either() {
        let (browser, relay_browser_side) = tokio::io::duplex(1024);
        let (relay_daemon_side, mut daemon) = tokio::io::duplex(64 * 1024);
        let answering = tokio::spawn(async move {
            let chunk = vec![b'x'; 16 * 1024];
            while daemon.write_all(&chunk).await.is_ok() {}
        });
        let idle = Duration::from_secs(30);
        tokio::time::timeout(
            idle + REFUSAL_WRITE_BUDGET * 4,
            splice(NeverCloses(relay_browser_side), relay_daemon_side, idle),
        )
        .await
        .expect("closing a browser that never reads must not hold its slots");
        drop(browser);
        answering.abort();
    }

    /// A browser on a real TLS stream gets the whole of a response while the
    /// daemon keeps the connection open for its next request (keep-alive).
    ///
    /// The TLS writer takes plaintext into its own send buffer and can return
    /// from a write before that data is on the socket. The socket here holds
    /// far less than one TLS record, as a browser's does once its receive
    /// window is full, so the last record is always still in that buffer when
    /// the daemon has nothing more to send. Unflushed, it stays there until the
    /// idle timeout, and the browser waits on the end of the response.
    #[tokio::test]
    async fn a_tls_browser_gets_the_whole_response_while_the_connection_stays_open() {
        let (mut browser, relay_browser_side) = tls_over_duplex(4 * 1024).await;
        let (relay_daemon_side, mut daemon) = tokio::io::duplex(64 * 1024);
        let splicing = tokio::spawn(splice(
            relay_browser_side,
            relay_daemon_side,
            Duration::from_secs(60),
        ));

        // One response of whole TLS records, then nothing: the daemon holds the
        // connection open, so no close flushes the end of it.
        let response = vec![b'x'; 16 * 16 * 1024];
        let answering = {
            let response = response.clone();
            tokio::spawn(async move {
                daemon.write_all(&response).await.unwrap();
                daemon
            })
        };
        let mut got = vec![0u8; response.len()];
        let read = tokio::time::timeout(Duration::from_secs(5), browser.read_exact(&mut got)).await;
        assert!(
            matches!(read, Ok(Ok(_))),
            "the browser must get the whole response while the connection stays open"
        );
        assert_eq!(got, response);
        let _daemon = answering.await.unwrap();
        splicing.abort();
    }

    /// Serve one browser that sends a request, and return what it was sent.
    async fn served(inner: &Arc<crate::Inner>, host: DashboardHost, peer: SocketAddr) -> String {
        let (mut browser, relay_side) = tokio::io::duplex(64 * 1024);
        browser
            .write_all(b"GET / HTTP/1.1\r\nHost: x\r\n\r\n")
            .await
            .unwrap();
        tokio::time::timeout(
            Duration::from_secs(2),
            serve(Arc::clone(inner), relay_side, host, peer),
        )
        .await
        .expect("a refusal with nothing left to wait for returns at once");
        let mut answer = String::new();
        browser.read_to_string(&mut answer).await.unwrap();
        answer
    }

    #[tokio::test]
    async fn refusals_past_their_pool_are_shed_without_touching_the_handshake_pool() {
        let server = crate::RelayServer::new(crate::RelayConfig {
            dashboard: Some(cfg()),
            ..crate::RelayConfig::default()
        });
        let inner = Arc::clone(&server.inner);
        let tier = inner.dashboard.clone().expect("the dashboard tier");
        // An address already at its connection cap: its refusals hold no slot.
        let crowded: SocketAddr = "203.0.113.7:51000".parse().unwrap();
        let _slots = [
            tier.take_ip_slot(crowded.ip()).unwrap(),
            tier.take_ip_slot(crowded.ip()).unwrap(),
        ];
        let label = DashboardHost::Label("a".repeat(26));
        // Browsers that send nothing, half from that address and half to an
        // unknown host from as many others: every refusal waits out its read
        // budget holding a refusal permit.
        let mut silent = Vec::new();
        for n in 0..MAX_DASHBOARD_REFUSALS {
            let (browser, relay_side) = tokio::io::duplex(1024);
            let (host, peer) = if n % 2 == 0 {
                (label.clone(), crowded)
            } else {
                let peer = SocketAddr::from(([10, 0, (n / 250) as u8, (n % 250) as u8 + 1], 40000));
                (DashboardHost::Unknown, peer)
            };
            tokio::spawn(serve(Arc::clone(&inner), relay_side, host, peer));
            silent.push(browser);
        }
        tokio::time::timeout(Duration::from_secs(2), async {
            while tier.refusals.available_permits() > 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the silent refusals take the whole pool");

        // Past the pool, both kinds of refusal are shed unanswered: an unknown
        // host, and an address already at its connection cap.
        let other: SocketAddr = "203.0.113.8:51000".parse().unwrap();
        assert_eq!(served(&inner, DashboardHost::Unknown, other).await, "");
        assert_eq!(served(&inner, label.clone(), crowded).await, "");
        // None of it holds a pre-admission permit: a native client or daemon
        // still gets one, as the accept loop takes it.
        assert_eq!(
            inner.handshake_permits.available_permits(),
            crate::RelayConfig::default().max_pending_handshakes
        );
        let native = Arc::clone(&inner.handshake_permits)
            .try_acquire_owned()
            .expect("a native client is admitted while the refusal pool is full");
        drop(native);

        // Once the silent browsers go, refusals are answered again.
        drop(silent);
        tokio::time::timeout(Duration::from_secs(2), async {
            while tier.refusals.available_permits() < MAX_DASHBOARD_REFUSALS {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the refusals give their permits back");
        let unknown = served(&inner, DashboardHost::Unknown, other).await;
        assert!(unknown.starts_with("HTTP/1.1 404 "), "{unknown}");
        let over_cap = served(&inner, label, crowded).await;
        assert!(over_cap.starts_with("HTTP/1.1 429 "), "{over_cap}");
        assert_eq!(tier.refusals.available_permits(), MAX_DASHBOARD_REFUSALS);
    }

    #[tokio::test(start_paused = true)]
    async fn a_refusal_to_a_browser_that_never_reads_gives_up() {
        // Far too small a link for the page, and a browser that sends nothing
        // and reads nothing: `browser` is held to keep the link open.
        let (browser, relay) = tokio::io::duplex(16);
        let mut relay = NeverCloses(relay);
        tokio::time::timeout(
            REFUSAL_READ_BUDGET + REFUSAL_WRITE_BUDGET * 4,
            refuse(&mut relay, 404, "There is no dashboard at this address."),
        )
        .await
        .expect("a refusal must not hold its slots for a browser that never reads");
        drop(browser);
    }
}
