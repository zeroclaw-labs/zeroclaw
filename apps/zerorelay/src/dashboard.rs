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

use crate::Inner;
use crate::enroll_route::{DaemonLink, OpenError, open_route_on};
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
}

impl DashboardTier {
    pub(crate) fn new(cfg: DashboardConfig) -> Self {
        Self {
            cfg,
            per_ip: std::sync::Mutex::new(HashMap::new()),
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
        refuse(
            &mut browser,
            429,
            "Too many connections from this address. Try again shortly.",
        )
        .await;
        return;
    };
    let DashboardHost::Label(label) = host else {
        refuse(&mut browser, 404, "There is no dashboard at this address.").await;
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
        refuse(
            &mut browser,
            404,
            "There is no dashboard at this address, or its daemon is not connected.",
        )
        .await;
        return;
    };
    let Some(_node_slot) = NodeSlot::take(node_live, tier.max_conns_per_node()) else {
        refuse(
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
            refuse(&mut browser, status, message).await;
            return;
        }
    };
    let (daemon, _route) = route.split();
    splice(browser, daemon, inner.idle_timeout).await;
}

/// Copy both ways until either side ends, nothing has moved either way for
/// `idle`, or one side has not taken a write for `idle`.
///
/// The write is bounded on its own because the idle timer is not polled while
/// a write waits: without it, a browser that asks for a page and then stops
/// reading would hold its dashboard and route slots past `idle`, and enough of
/// them would make the dashboard busy for everyone else.
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
                    if !wrote_within(idle, b_write.write_all(&a_buf[..n])).await {
                        break;
                    }
                    deadline = Instant::now() + idle;
                }
            },
            read = b_read.read(&mut b_buf) => match read {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if !wrote_within(idle, a_write.write_all(&b_buf[..n])).await {
                        break;
                    }
                    deadline = Instant::now() + idle;
                }
            },
        }
    }
    let _ = a_write.shutdown().await;
    let _ = b_write.shutdown().await;
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
/// a fixed page and close.
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
    let _ = browser.write_all(response.as_bytes()).await;
    let _ = browser.shutdown().await;
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
