//! The in-process handoff from the relay bridge to the gateway for a
//! dashboard that a relay serves (`[relay.dashboard]`).
//!
//! A browser that opens this daemon's dashboard through a relay ends its TLS
//! at the relay, which forwards the plaintext HTTP over the daemon's own
//! outer relay link. The bridge must not hand that stream to the gateway's
//! TCP listener: a connection from `127.0.0.1` is a loopback peer, and the
//! gateway gives loopback peers standing - the admin routes, the limiter
//! exemptions, SOP cancellation without pairing - that a browser on the
//! internet must never get. The bridge passes the stream over in process
//! instead, as a [`DashboardConn`] carrying the browser's address, which is
//! never loopback by construction, and the one origin the dashboard is
//! served at.
//!
//! [`DashboardIngress`] is the meeting point. The gateway generation that is
//! serving attaches to it and receives connections; the bridge delivers to
//! whichever generation is attached and refuses the route when none is.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex, PoisonError};

use anyhow::{Result, bail};
use tokio::io::DuplexStream;
use tokio::sync::mpsc;

/// Connections that may wait for the attached gateway's accept loop. Past
/// it the bridge refuses the route rather than queueing without bound.
const QUEUE: usize = 32;

/// Where the relay serves this daemon's dashboard: `https://<host>[:port]`.
///
/// The gateway answers relay connections only for this authority and builds
/// every URL it hands a browser from it, never from the request's `Host`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DashboardOrigin {
    host: String,
    port: u16,
}

impl DashboardOrigin {
    /// A dashboard origin at DNS name `host` on HTTPS port `port`.
    ///
    /// `host` must be a DNS name by [`zeroclaw_config::schema::is_dns_host_name`]
    /// once lower-cased; it is stored in lower case, as browsers write it in
    /// `Origin`.
    pub fn new(host: &str, port: u16) -> Result<Self> {
        let host = host.trim().to_ascii_lowercase();
        if !zeroclaw_config::schema::is_dns_host_name(&host) {
            bail!("dashboard host `{host}` is not a DNS name");
        }
        if port == 0 {
            bail!("dashboard port must not be 0");
        }
        Ok(Self { host, port })
    }

    /// The DNS name, in lower case.
    pub fn host(&self) -> &str {
        &self.host
    }

    /// The HTTPS port browsers use.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// `host`, or `host:port` when the port is not 443: the authority a
    /// browser puts in `Host` and `Origin`.
    pub fn authority(&self) -> String {
        if self.port == 443 {
            self.host.clone()
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }

    /// `https://` and the authority, exactly as a browser sends `Origin`.
    pub fn origin(&self) -> String {
        format!("https://{}", self.authority())
    }

    /// Whether a request's `Host` names this dashboard. Case does not
    /// matter, and on port 443 the explicit `host:443` counts too.
    pub fn is_named_by(&self, host_header: &str) -> bool {
        let host_header = host_header.trim();
        host_header.eq_ignore_ascii_case(&self.authority())
            || (self.port == 443 && host_header.eq_ignore_ascii_case(&format!("{}:443", self.host)))
    }
}

/// The address a dashboard connection is served under: the browser's, as
/// the relay reported it, with an IPv4-mapped IPv6 address unwrapped.
///
/// Refuses a loopback or unspecified address. The gateway treats a loopback
/// peer as someone on its own machine, and a relay-forwarded browser never
/// is, whatever the relay says.
pub fn dashboard_peer(addr: SocketAddr) -> Result<SocketAddr> {
    let ip = addr.ip().to_canonical();
    if ip.is_loopback() || ip.is_unspecified() {
        bail!("a relay-forwarded browser cannot have the address {ip}");
    }
    Ok(SocketAddr::new(ip, addr.port()))
}

/// One browser connection a relay forwarded for the dashboard.
pub struct DashboardConn {
    stream: DuplexStream,
    client_addr: SocketAddr,
    origin: DashboardOrigin,
}

impl DashboardConn {
    /// Wrap the bridge's end of a forwarded browser connection. Refuses a
    /// client address [`dashboard_peer`] refuses.
    pub fn new(
        stream: DuplexStream,
        client_addr: SocketAddr,
        origin: DashboardOrigin,
    ) -> Result<Self> {
        Ok(Self {
            stream,
            client_addr: dashboard_peer(client_addr)?,
            origin,
        })
    }

    /// The browser's address. Never loopback or unspecified.
    pub fn client_addr(&self) -> SocketAddr {
        self.client_addr
    }

    /// The origin the dashboard is served at for this connection.
    pub fn origin(&self) -> &DashboardOrigin {
        &self.origin
    }

    /// The stream, the browser's address and the origin.
    pub fn into_parts(self) -> (DuplexStream, SocketAddr, DashboardOrigin) {
        (self.stream, self.client_addr, self.origin)
    }
}

/// Why a dashboard connection could not be handed to the gateway. The text
/// is the relay `Close` reason.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeliverError {
    /// No gateway generation is attached: the gateway is off, or between
    /// generations.
    NoGateway,
    /// The attached gateway's queue of connections it has not taken yet is full.
    Busy,
}

impl DeliverError {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NoGateway => "gateway_unavailable",
            Self::Busy => "busy",
        }
    }
}

#[derive(Default)]
struct Slot {
    generation: u64,
    tx: Option<mpsc::Sender<DashboardConn>>,
}

/// Where the relay bridge hands dashboard connections to the gateway.
///
/// One per daemon, shared by the bridge and every gateway generation. At
/// most one generation is attached at a time; attaching replaces the one
/// before, so a reload's new generation takes over without a gap.
#[derive(Clone, Default)]
pub struct DashboardIngress {
    slot: Arc<Mutex<Slot>>,
}

impl DashboardIngress {
    pub fn new() -> Self {
        Self::default()
    }

    /// Attach a gateway generation. Connections delivered from now on go to
    /// the returned receiver, until another generation attaches or the
    /// receiver is dropped.
    pub fn attach(&self) -> DashboardIngressReceiver {
        let (tx, rx) = mpsc::channel(QUEUE);
        let mut slot = self.slot.lock().unwrap_or_else(PoisonError::into_inner);
        slot.generation = slot.generation.wrapping_add(1);
        slot.tx = Some(tx);
        DashboardIngressReceiver {
            rx,
            slot: Arc::clone(&self.slot),
            generation: slot.generation,
        }
    }

    /// Whether a gateway generation is attached.
    pub fn is_attached(&self) -> bool {
        self.slot
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .tx
            .as_ref()
            .is_some_and(|tx| !tx.is_closed())
    }

    /// Hand `conn` to the attached gateway without waiting. On an error the
    /// connection is dropped, which closes the bridge's side of it.
    pub fn deliver(&self, conn: DashboardConn) -> std::result::Result<(), DeliverError> {
        let tx = self
            .slot
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .tx
            .clone();
        let Some(tx) = tx else {
            return Err(DeliverError::NoGateway);
        };
        tx.try_send(conn).map_err(|error| match error {
            mpsc::error::TrySendError::Full(_) => DeliverError::Busy,
            mpsc::error::TrySendError::Closed(_) => DeliverError::NoGateway,
        })
    }
}

/// A gateway generation's end of the [`DashboardIngress`]. Dropping it
/// detaches that generation, unless a later one has attached since.
pub struct DashboardIngressReceiver {
    rx: mpsc::Receiver<DashboardConn>,
    slot: Arc<Mutex<Slot>>,
    generation: u64,
}

impl DashboardIngressReceiver {
    /// The next delivered connection. `None` once a later generation has
    /// replaced this one and every connection queued here has been taken.
    pub async fn recv(&mut self) -> Option<DashboardConn> {
        self.rx.recv().await
    }
}

impl Drop for DashboardIngressReceiver {
    fn drop(&mut self) {
        let mut slot = self.slot.lock().unwrap_or_else(PoisonError::into_inner);
        if slot.generation == self.generation {
            slot.tx = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    fn origin() -> DashboardOrigin {
        DashboardOrigin::new("abc.relay.example", 443).unwrap()
    }

    fn conn(addr: &str) -> Result<DashboardConn> {
        let (stream, _other) = tokio::io::duplex(64);
        DashboardConn::new(stream, addr.parse().unwrap(), origin())
    }

    #[test]
    fn an_origin_is_a_lower_case_dns_name_with_an_https_port() {
        let on_443 = DashboardOrigin::new(" ABC.Relay.Example ", 443).unwrap();
        assert_eq!(on_443.host(), "abc.relay.example");
        assert_eq!(on_443.authority(), "abc.relay.example");
        assert_eq!(on_443.origin(), "https://abc.relay.example");
        let on_8443 = DashboardOrigin::new("abc.relay.example", 8443).unwrap();
        assert_eq!(on_8443.authority(), "abc.relay.example:8443");
        assert_eq!(on_8443.origin(), "https://abc.relay.example:8443");
        for refused in [
            "",
            "localhost",
            "abc..example",
            "-abc.example",
            "abc-.example",
            "abc.example/x",
            "abc.example:443",
            "user@abc.example",
            "abc_def.example",
            "127.0.0.1",
        ] {
            assert!(DashboardOrigin::new(refused, 443).is_err(), "{refused:?}");
        }
        assert!(DashboardOrigin::new(&format!("{}.example", "a".repeat(64)), 443).is_err());
        assert!(DashboardOrigin::new("abc.relay.example", 0).is_err());
    }

    #[test]
    fn only_the_dashboard_authority_names_the_dashboard() {
        let on_443 = origin();
        for named in [
            "abc.relay.example",
            "ABC.relay.EXAMPLE",
            "abc.relay.example:443",
        ] {
            assert!(on_443.is_named_by(named), "{named}");
        }
        for other in [
            "",
            "evil.example",
            "abc.relay.example.evil.example",
            "xabc.relay.example",
            "abc.relay.example:8443",
            "abc.relay.example@evil.example",
        ] {
            assert!(!on_443.is_named_by(other), "{other}");
        }
        let on_8443 = DashboardOrigin::new("abc.relay.example", 8443).unwrap();
        assert!(on_8443.is_named_by("abc.relay.example:8443"));
        assert!(!on_8443.is_named_by("abc.relay.example"));
        assert!(!on_8443.is_named_by("abc.relay.example:443"));
    }

    #[test]
    fn a_relay_forwarded_browser_is_never_loopback() {
        for refused in [
            "127.0.0.1:5000",
            "127.9.9.9:5000",
            "[::1]:5000",
            "[::ffff:127.0.0.1]:5000",
            "0.0.0.0:5000",
            "[::]:5000",
        ] {
            assert!(conn(refused).is_err(), "{refused}");
        }
        let accepted = conn("203.0.113.7:5000").unwrap();
        assert_eq!(accepted.client_addr(), "203.0.113.7:5000".parse().unwrap());
        // A mapped address is served as the IPv4 address it is, so it keys
        // the same limiters as that address arriving natively.
        let mapped = conn("[::ffff:203.0.113.7]:5000").unwrap();
        assert_eq!(
            mapped.client_addr().ip(),
            IpAddr::V4(Ipv4Addr::new(203, 0, 113, 7))
        );
        let v6 = conn("[2001:db8::1]:5000").unwrap();
        assert_eq!(
            v6.client_addr().ip(),
            IpAddr::V6("2001:db8::1".parse::<Ipv6Addr>().unwrap())
        );
    }

    #[tokio::test]
    async fn delivery_needs_an_attached_gateway() {
        let ingress = DashboardIngress::new();
        assert!(!ingress.is_attached());
        assert_eq!(
            ingress.deliver(conn("203.0.113.7:1").unwrap()).unwrap_err(),
            DeliverError::NoGateway
        );
        let mut receiver = ingress.attach();
        assert!(ingress.is_attached());
        ingress.deliver(conn("203.0.113.7:2").unwrap()).unwrap();
        let delivered = receiver.recv().await.unwrap();
        assert_eq!(delivered.client_addr().port(), 2);
        drop(receiver);
        assert!(!ingress.is_attached());
        assert_eq!(
            ingress.deliver(conn("203.0.113.7:3").unwrap()).unwrap_err(),
            DeliverError::NoGateway
        );
    }

    #[tokio::test]
    async fn a_new_generation_takes_over_and_the_old_one_detaching_leaves_it() {
        let ingress = DashboardIngress::new();
        let old = ingress.attach();
        let mut new = ingress.attach();
        drop(old);
        assert!(
            ingress.is_attached(),
            "the old generation's drop must not detach the new one"
        );
        ingress.deliver(conn("203.0.113.7:4").unwrap()).unwrap();
        assert_eq!(new.recv().await.unwrap().client_addr().port(), 4);
    }

    #[tokio::test]
    async fn a_gateway_that_stops_taking_connections_is_busy_not_unbounded() {
        let ingress = DashboardIngress::new();
        let _receiver = ingress.attach();
        for port in 0..QUEUE as u16 {
            ingress
                .deliver(conn(&format!("203.0.113.7:{}", port + 1)).unwrap())
                .unwrap();
        }
        assert_eq!(
            ingress
                .deliver(conn("203.0.113.7:999").unwrap())
                .unwrap_err(),
            DeliverError::Busy
        );
    }
}
