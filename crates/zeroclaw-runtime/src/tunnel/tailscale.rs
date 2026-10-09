use super::{
    PublishedTcpService, SharedProcess, TcpService, Tunnel, TunnelProcess, kill_shared,
    new_shared_process,
};
use anyhow::{Context, Result, bail};
use std::collections::BTreeSet;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::process::{Child, Command};
use tokio::sync::{Mutex, oneshot};
use tokio::task::JoinHandle;
use zeroclaw_config::schema::{EnrollConfig, TunnelConfig, WssConfig};

/// Bounds for bringing one `tailscale serve --tcp` forwarder up.
#[derive(Debug, Clone, Copy)]
struct ForwarderStartup {
    /// How long a running forwarder has to appear in the serve config.
    apply_timeout: Duration,
    /// Poll interval while waiting for it to appear.
    poll: Duration,
    /// Spawn attempts when tailscaled rejects a concurrent serve-config write.
    conflict_attempts: u32,
    /// Backoff before conflict retry `n` is `n * conflict_backoff`.
    conflict_backoff: Duration,
}

const FORWARDER_STARTUP: ForwarderStartup = ForwarderStartup {
    apply_timeout: Duration::from_secs(5),
    poll: Duration::from_millis(100),
    conflict_attempts: 4,
    conflict_backoff: Duration::from_millis(250),
};

/// Upper bound on one `tailscale serve status --json` probe.
const SERVE_STATUS_TIMEOUT: Duration = Duration::from_secs(2);

/// Why a forwarder was not published.
#[derive(Debug)]
enum ForwarderStartError {
    /// The process exited before its forward appeared.
    Exited {
        status: String,
        stderr: String,
        attempts: u32,
    },
    /// It kept running but never appeared in the serve config (for example
    /// blocked on an interactive prompt); it was killed.
    NotApplied { waited: Duration },
    /// It could not be spawned or polled.
    Io(std::io::Error),
}

impl ForwarderStartError {
    fn attrs(&self) -> serde_json::Value {
        match self {
            Self::Exited {
                status,
                stderr,
                attempts,
            } => serde_json::json!({"status": status, "stderr": stderr, "attempts": attempts}),
            Self::NotApplied { waited } => {
                serde_json::json!({"error": "not applied to the serve config", "waited_ms": waited.as_millis()})
            }
            Self::Io(e) => serde_json::json!({"error": e.to_string()}),
        }
    }
}

/// `tailscale serve` applies its config with an etag precondition, so
/// concurrent writers (the gateway's own serve, other forwarders) can lose
/// the race and exit with this error. Retrying is safe; other failures are not
/// retried.
fn is_serve_config_conflict(stderr: &str) -> bool {
    stderr.contains("etag mismatch")
        || stderr.contains("Another client is changing the serve config")
}

/// Foreground serve sessions in `tailscale serve status --json` output that
/// raw-forward `target`'s port to `target`, or `None` if the output is
/// unreadable.
///
/// Only an exact match counts: another handler on the port (HTTPS, a forward
/// elsewhere, TLS terminated by tailscaled) is not the requested forward. The
/// background config is ignored because the tunnel publishes in the
/// foreground, so a background entry is never one of its forwarders.
fn serve_sessions_forwarding(json: &[u8], target: SocketAddr) -> Option<BTreeSet<String>> {
    let config: serde_json::Value = serde_json::from_slice(json).ok()?;
    let port = target.port().to_string();
    // Tailscale stores a `tcp://` target as its URL host: `SocketAddr`'s
    // display form, with brackets for IPv6.
    let target = target.to_string();
    let forwards = |session: &serde_json::Value| {
        let handler = &session["TCP"][&port];
        handler["TCPForward"].as_str() == Some(target.as_str())
            && handler["TerminateTLS"].as_str().is_none_or(str::is_empty)
    };
    Some(
        config["Foreground"]
            .as_object()
            .into_iter()
            .flatten()
            .filter(|(_, session)| forwards(session))
            .map(|(id, _)| id.clone())
            .collect(),
    )
}

/// Ask tailscaled which foreground sessions raw-forward to `target`; `None`
/// if it could not be asked.
async fn serve_sessions_for(target: SocketAddr) -> Option<BTreeSet<String>> {
    let output = tokio::time::timeout(
        SERVE_STATUS_TIMEOUT,
        Command::new("tailscale")
            .args(["serve", "status", "--json"])
            .kill_on_drop(true)
            .output(),
    )
    .await;
    match output {
        Ok(Ok(out)) if out.status.success() => serve_sessions_forwarding(&out.stdout, target),
        _ => None,
    }
}

/// Bring one forwarder up and return it only once its own forward is in the
/// serve config. A process that is merely still running is not proof:
/// `tailscale serve` can block before applying anything. Nor is a matching
/// forward that was already there: a forward from another session makes this
/// one fail, so only a session that appears after the spawn counts, and the
/// process must still be running once it has been seen.
/// Concurrent-write conflicts are retried with backoff; anything else fails at
/// once.
async fn start_forwarder<S, P, PF>(
    startup: ForwarderStartup,
    mut spawn: S,
    mut probe: P,
) -> std::result::Result<Child, ForwarderStartError>
where
    S: FnMut() -> std::io::Result<Child>,
    P: FnMut() -> PF,
    PF: std::future::Future<Output = Option<BTreeSet<String>>>,
{
    let mut attempt = 1;
    loop {
        // If this probe fails, every matching session will look new; the
        // post-probe process check still applies.
        let existing = probe().await.unwrap_or_default();
        let mut child = spawn().map_err(ForwarderStartError::Io)?;
        let deadline = tokio::time::Instant::now() + startup.apply_timeout;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => {}
                Err(e) => {
                    child.kill().await.ok();
                    return Err(ForwarderStartError::Io(e));
                }
            }
            let applied = probe()
                .await
                .is_some_and(|sessions| !sessions.is_subset(&existing));
            if applied {
                // The forward can vanish with its process while the probe ran.
                match child.try_wait() {
                    Ok(None) => return Ok(child),
                    Ok(Some(status)) => break status,
                    Err(e) => {
                        child.kill().await.ok();
                        return Err(ForwarderStartError::Io(e));
                    }
                }
            }
            if tokio::time::Instant::now() >= deadline {
                child.kill().await.ok();
                child.wait().await.ok();
                return Err(ForwarderStartError::NotApplied {
                    waited: startup.apply_timeout,
                });
            }
            tokio::time::sleep(startup.poll).await;
        };
        let mut stderr = String::new();
        if let Some(pipe) = child.stderr.take() {
            pipe.take(FORWARDER_STDERR_CAP)
                .read_to_string(&mut stderr)
                .await
                .ok();
        }
        if is_serve_config_conflict(&stderr) && attempt < startup.conflict_attempts {
            tokio::time::sleep(startup.conflict_backoff * attempt).await;
            attempt += 1;
            continue;
        }
        return Err(ForwarderStartError::Exited {
            status: status.to_string(),
            stderr: stderr.trim().to_string(),
            attempts: attempt,
        });
    }
}

/// Tailscale Tunnel — uses `tailscale serve` (tailnet-only) or
/// `tailscale funnel` (public internet).
/// Requires Tailscale installed and authenticated (`tailscale up`).
pub struct TailscaleTunnel {
    funnel: bool,
    hostname: Option<String>,
    proc: SharedProcess,
    /// Foreground `tailscale serve --tcp` forwarders for the daemon's
    /// self-TLS listeners. Foreground serve config lives exactly as long as
    /// the process, so ending these withdraws the forwards.
    tcp_forwarders: Arc<Mutex<Vec<TcpForwarder>>>,
}

/// Cap on the forwarder stderr kept for an unexpected-exit report.
const FORWARDER_STDERR_CAP: u64 = 8 * 1024;

/// A published forwarder, owned by its watcher task.
struct TcpForwarder {
    /// Sending, or dropping it, ends the forwarder (see `watch_forwarder`).
    stop: Option<oneshot::Sender<()>>,
    task: JoinHandle<()>,
}

impl TcpForwarder {
    fn spawn(service: TcpService, child: Child) -> Self {
        let (stop, stop_rx) = oneshot::channel();
        let task = zeroclaw_spawn::spawn!(watch_forwarder(service, child, stop_rx));
        Self {
            stop: Some(stop),
            task,
        }
    }

    fn is_running(&self) -> bool {
        !self.task.is_finished()
    }

    /// Withdraw the forward and wait for the process to be reaped.
    async fn shutdown(mut self) {
        if let Some(stop) = self.stop.take() {
            stop.send(()).ok();
        }
        self.task.await.ok();
    }
}

/// Own one forwarder until it exits or is told to stop. A forwarder that
/// exits on its own has silently taken its service off the tailnet, so that
/// is reported; a stop request (or the tunnel being dropped, which drops the
/// sender) kills it.
async fn watch_forwarder(service: TcpService, mut child: Child, stop: oneshot::Receiver<()>) {
    let stderr = child.stderr.take();
    tokio::select! {
        status = child.wait() => {
            let mut captured = String::new();
            if let Some(stderr) = stderr {
                stderr
                    .take(FORWARDER_STDERR_CAP)
                    .read_to_string(&mut captured)
                    .await
                    .ok();
            }
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "service": service.name,
                        "target": service.target.to_string(),
                        "status": match status {
                            Ok(status) => status.to_string(),
                            Err(e) => e.to_string(),
                        },
                        "stderr": captured.trim(),
                    })),
                "tailscale serve --tcp exited after publication; the service is no longer \
                 reachable on the tailnet until the gateway restarts"
            );
        }
        _ = stop => {
            child.kill().await.ok();
            child.wait().await.ok();
        }
    }
}

impl TailscaleTunnel {
    pub fn new(funnel: bool, hostname: Option<String>) -> Self {
        Self {
            funnel,
            hostname,
            proc: new_shared_process(),
            tcp_forwarders: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// The configured hostname override, else this node's MagicDNS name.
    async fn resolve_hostname(&self) -> Result<String> {
        if let Some(ref h) = self.hostname {
            return Ok(h.clone());
        }
        Ok(query_tailscale_self()
            .await?
            .dns_name
            .unwrap_or_else(|| "localhost".to_string()))
    }
}

// ── Tailnet identity ─────────────────────────────────────────────

/// Upper bound on `tailscale status`, so a wedged tailscaled cannot stall a
/// listener's startup.
const STATUS_TIMEOUT: Duration = Duration::from_secs(5);

/// This node's tailnet identity, as reported by `tailscale status --json`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TailscaleSelf {
    /// MagicDNS FQDN without the trailing dot (`node.tailnet.ts.net`).
    pub dns_name: Option<String>,
    /// The node's tailnet addresses.
    pub ips: Vec<IpAddr>,
    /// `Self.InNetworkMap`: whether tailscaled has received this node's
    /// network map. `None` when the field is absent (older releases).
    pub in_network_map: Option<bool>,
}

impl TailscaleSelf {
    /// Whether this is a usable tailnet identity. `tailscale status --json`
    /// succeeds before tailscaled has its network map, reporting
    /// `InNetworkMap: false` with an empty `DNSName` and no `TailscaleIPs`;
    /// that is "not known yet", not "this node has no tailnet names".
    pub fn has_identity(&self) -> bool {
        self.in_network_map != Some(false) && (self.dns_name.is_some() || !self.ips.is_empty())
    }
}

/// Ask the local tailscaled who this node is.
pub async fn query_tailscale_self() -> Result<TailscaleSelf> {
    let output = tokio::time::timeout(
        STATUS_TIMEOUT,
        Command::new("tailscale")
            .args(["status", "--json"])
            .kill_on_drop(true)
            .output(),
    )
    .await
    .context("tailscale status timed out")??;

    if !output.status.success() {
        bail!(
            "tailscale status failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(parse_tailscale_self(&output.stdout))
}

/// Extract the node identity from `tailscale status --json`. Missing or
/// malformed fields yield an empty identity rather than an error; callers
/// that need a real identity check [`TailscaleSelf::has_identity`].
pub fn parse_tailscale_self(json: &[u8]) -> TailscaleSelf {
    let status: serde_json::Value = serde_json::from_slice(json).unwrap_or_default();
    let node = &status["Self"];
    let dns_name = node["DNSName"]
        .as_str()
        .map(|n| n.trim().trim_end_matches('.').to_string())
        .filter(|n| !n.is_empty());
    let ips = node["TailscaleIPs"]
        .as_array()
        .map(|ips| {
            ips.iter()
                .filter_map(|ip| ip.as_str()?.parse().ok())
                .collect()
        })
        .unwrap_or_default();
    let in_network_map = node["InNetworkMap"].as_bool();
    TailscaleSelf {
        dns_name,
        ips,
        in_network_map,
    }
}

/// Whether `ip` is in Tailscale's address space: the CGNAT range
/// `100.64.0.0/10` or the tailnet ULA prefix `fd7a:115c:a1e0::/48`.
pub fn is_tailscale_ip(ip: IpAddr) -> bool {
    const TS_V4_NET: Ipv4Addr = Ipv4Addr::new(100, 64, 0, 0);
    const TS_V6_NET: Ipv6Addr = Ipv6Addr::new(0xfd7a, 0x115c, 0xa1e0, 0, 0, 0, 0, 0);
    match ip {
        IpAddr::V4(v4) => u32::from(v4) & 0xffc0_0000 == u32::from(TS_V4_NET),
        IpAddr::V6(v6) => v6.segments()[..3] == TS_V6_NET.segments()[..3],
    }
}

/// Whether the daemon's self-TLS listeners are reached over the tailnet: the
/// Tailscale tunnel publishes them, or one is bound directly to a tailnet
/// address. Only then does the server certificate need tailnet names.
fn tailnet_reaches_listeners(
    tunnel: &TunnelConfig,
    wss: &WssConfig,
    enroll: &EnrollConfig,
) -> bool {
    if !wss.enabled {
        return false;
    }
    let bound_to_tailnet = |bind: &str| bind.trim().parse::<IpAddr>().is_ok_and(is_tailscale_ip);
    tunnel.tunnel_provider == "tailscale"
        || bound_to_tailnet(&wss.bind)
        || (enroll.enabled && bound_to_tailnet(&enroll.bind))
}

/// The names a tailnet client uses to reach this node: the configured
/// hostname override, the MagicDNS FQDN and its short name, and the node's
/// tailnet IPs. Deduplicated, case-insensitively, in that order.
fn tailnet_names(hostname_override: Option<&str>, node: &TailscaleSelf) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    let mut push = |name: &str| {
        let name = name.trim().trim_end_matches('.');
        if !name.is_empty() && !names.iter().any(|n| n.eq_ignore_ascii_case(name)) {
            names.push(name.to_string());
        }
    };
    if let Some(h) = hostname_override {
        push(h);
    }
    if let Some(fqdn) = node.dns_name.as_deref() {
        push(fqdn);
        if let Some((short, _)) = fqdn.split_once('.') {
            push(short);
        }
    }
    for ip in &node.ips {
        push(&ip.to_string());
    }
    names
}

/// Outcome of resolving the tailnet names the daemon's server certificate
/// should carry. `Unavailable` is deliberately distinct from `NotApplicable`:
/// a failed query says nothing about the names, so callers must not treat it
/// as "this node has no tailnet names" and drop names a leaf already carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TailnetSans {
    /// The listeners are not reached over the tailnet; no tailnet names apply.
    NotApplicable,
    /// The node's tailnet names, in `tailnet_names` order.
    Resolved(Vec<String>),
    /// The listeners are reached over the tailnet but tailscaled could not be
    /// queried. Carries any configured `[tunnel.tailscale].hostname` override,
    /// which is known without tailscaled.
    Unavailable { hostname_override: Option<String> },
}

/// Resolve the tailnet names for the daemon's self-TLS listeners (WSS,
/// enrollment). Queries tailscaled only when the listeners are reached over
/// the tailnet. Callers that start several listeners must resolve once and
/// share the outcome, so every listener presents the same identity.
pub async fn tailscale_server_sans(
    tunnel: &TunnelConfig,
    wss: &WssConfig,
    enroll: &EnrollConfig,
) -> TailnetSans {
    if !tailnet_reaches_listeners(tunnel, wss, enroll) {
        return TailnetSans::NotApplicable;
    }
    let hostname_override = tunnel.tailscale.as_ref().and_then(|ts| ts.hostname.clone());
    tailnet_sans_from_status(query_tailscale_self().await, hostname_override)
}

/// Classify a `tailscale status` outcome. Only a usable identity
/// ([`TailscaleSelf::has_identity`]) is `Resolved`. A failed or timed-out
/// query, malformed output, and a successful status reported before
/// tailscaled has its network map are all `Unavailable`, so the existing
/// certificate keeps its names instead of being regenerated without them.
pub fn tailnet_sans_from_status(
    status: Result<TailscaleSelf>,
    hostname_override: Option<String>,
) -> TailnetSans {
    let reason = match status {
        Ok(node) if node.has_identity() => {
            return TailnetSans::Resolved(tailnet_names(hostname_override.as_deref(), &node));
        }
        Ok(node) => format!(
            "tailscaled reported no node identity yet (InNetworkMap: {})",
            node.in_network_map
                .map_or_else(|| "absent".to_string(), |v| v.to_string())
        ),
        Err(e) => e.to_string(),
    };
    ::zeroclaw_log::record!(
        WARN,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
            .with_attrs(::serde_json::json!({"error": reason})),
        "could not read this node's tailnet identity; the WSS server certificate \
         keeps the names it already carries until the next successful start"
    );
    TailnetSans::Unavailable { hostname_override }
}

/// Arguments for a raw TCP passthrough of `service` on the same tailnet port.
///
/// Always `serve` (tailnet-only), never `funnel`: Funnel only listens on
/// 443/8443/10000, so these ports cannot be published publicly as-is, and
/// widening a mutually authenticated plane to the internet must be an
/// explicit operator decision rather than a side effect of gateway funnel.
fn tcp_serve_args(service: &TcpService) -> Vec<String> {
    vec![
        "serve".into(),
        "--tcp".into(),
        service.target.port().to_string(),
        format!("tcp://{}", service.target),
    ]
}

fn tcp_endpoint(hostname: &str, service: &TcpService) -> String {
    format!("{}://{hostname}:{}", service.scheme, service.target.port())
}

#[async_trait::async_trait]
impl Tunnel for TailscaleTunnel {
    fn name(&self) -> &str {
        "tailscale"
    }

    async fn start(&self, _local_host: &str, local_port: u16) -> Result<String> {
        let subcommand = if self.funnel { "funnel" } else { "serve" };

        // Get the tailscale hostname for URL construction
        let hostname = self.resolve_hostname().await?;

        // tailscale serve|funnel <port>
        let child = Command::new("tailscale")
            .args([subcommand, &local_port.to_string()])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()?;

        let public_url = format!("https://{hostname}:{local_port}");

        let mut guard = self.proc.lock().await;
        *guard = Some(TunnelProcess {
            child,
            public_url: public_url.clone(),
        });

        Ok(public_url)
    }

    async fn publish_tcp_services(
        &self,
        services: &[TcpService],
    ) -> Result<Vec<PublishedTcpService>> {
        if services.is_empty() {
            return Ok(Vec::new());
        }
        if self.funnel {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(::serde_json::json!({
                        "services": services.iter().map(|s| s.name).collect::<Vec<_>>(),
                    })),
                "tailscale funnel publishes the gateway publicly; WSS and enrollment are \
                 published tailnet-only via `tailscale serve --tcp`"
            );
        }
        let hostname = self.resolve_hostname().await?;

        // One at a time: `tailscale serve` writers race on the serve-config
        // etag, so concurrent spawns make all but one fail.
        let mut published = Vec::with_capacity(services.len());
        for service in services {
            let started = start_forwarder(
                FORWARDER_STARTUP,
                || {
                    Command::new("tailscale")
                        .args(tcp_serve_args(service))
                        .stdout(std::process::Stdio::null())
                        .stderr(std::process::Stdio::piped())
                        .kill_on_drop(true)
                        .spawn()
                },
                || serve_sessions_for(service.target),
            )
            .await;
            match started {
                Ok(child) => {
                    published.push(PublishedTcpService {
                        service: *service,
                        endpoint: tcp_endpoint(&hostname, service),
                    });
                    self.tcp_forwarders
                        .lock()
                        .await
                        .push(TcpForwarder::spawn(*service, child));
                }
                Err(e) => {
                    let mut attrs = e.attrs();
                    attrs["service"] = service.name.into();
                    attrs["target"] = service.target.to_string().into();
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(attrs),
                        "tailscale serve --tcp did not publish the service on the tailnet"
                    );
                }
            }
        }
        Ok(published)
    }

    /// Withdraw what this tunnel published by ending the processes it started:
    /// the gateway's `serve`/`funnel` and each TCP forwarder. All of them run
    /// in the foreground, so tailscaled drops their config when they exit.
    /// The node's serve config is never reset: the operator's own `--bg`
    /// entries and other processes' sessions live in it too.
    async fn stop(&self) -> Result<()> {
        let forwarders: Vec<TcpForwarder> = self.tcp_forwarders.lock().await.drain(..).collect();
        for forwarder in forwarders {
            forwarder.shutdown().await;
        }
        kill_shared(&self.proc).await
    }

    /// Healthy while the gateway serve process and every published TCP
    /// forwarder are still running.
    async fn health_check(&self) -> bool {
        let gateway_up = {
            let guard = self.proc.lock().await;
            guard.as_ref().is_some_and(|tp| tp.child.id().is_some())
        };
        gateway_up
            && self
                .tcp_forwarders
                .lock()
                .await
                .iter()
                .all(TcpForwarder::is_running)
    }

    fn public_url(&self) -> Option<String> {
        self.proc
            .try_lock()
            .ok()
            .and_then(|g| g.as_ref().map(|tp| tp.public_url.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn service(name: &'static str, scheme: &'static str, target: &str) -> TcpService {
        TcpService {
            name,
            scheme,
            target: target.parse().unwrap(),
        }
    }

    #[test]
    fn constructor_stores_hostname_and_mode() {
        let tunnel = TailscaleTunnel::new(true, Some("myhost.tailnet.ts.net".into()));
        assert!(tunnel.funnel);
        assert_eq!(tunnel.hostname.as_deref(), Some("myhost.tailnet.ts.net"));
    }

    #[test]
    fn public_url_is_none_before_start() {
        let tunnel = TailscaleTunnel::new(false, None);
        assert!(tunnel.public_url().is_none());
    }

    #[tokio::test]
    async fn health_check_is_false_before_start() {
        let tunnel = TailscaleTunnel::new(false, None);
        assert!(!tunnel.health_check().await);
    }

    #[tokio::test]
    async fn stop_without_started_process_is_ok() {
        let tunnel = TailscaleTunnel::new(false, None);
        let result = tunnel.stop().await;
        assert!(result.is_ok());
    }

    fn process_exists(pid: u32) -> bool {
        std::path::Path::new(&format!("/proc/{pid}")).exists()
    }

    #[tokio::test]
    async fn stop_ends_and_reaps_only_the_processes_this_tunnel_started() {
        // Foreground serve config lives as long as its process, so ending the
        // gateway serve and each forwarder withdraws exactly what this tunnel
        // published. Reaped, not just signalled: stop returns only once the
        // processes are gone.
        let tunnel = TailscaleTunnel::new(false, Some("node.tailnet.ts.net".into()));
        let gateway = spawn_stand_in("sleep 30");
        let forwarder = spawn_stand_in("sleep 30");
        let pids = [gateway.id().unwrap(), forwarder.id().unwrap()];
        *tunnel.proc.lock().await = Some(TunnelProcess {
            child: gateway,
            public_url: "https://node.tailnet.ts.net".into(),
        });
        tunnel
            .tcp_forwarders
            .lock()
            .await
            .push(TcpForwarder::spawn(forwarder_service(), forwarder));

        tokio::time::timeout(Duration::from_secs(5), tunnel.stop())
            .await
            .expect("stop should kill its processes, not wait out their sleep")
            .unwrap();

        for pid in pids {
            assert!(!process_exists(pid), "pid {pid} left behind by stop");
        }
        assert!(tunnel.proc.lock().await.is_none());
        assert!(tunnel.tcp_forwarders.lock().await.is_empty());
        assert!(!tunnel.health_check().await);
    }

    const STATUS_JSON: &str = r#"{
        "BackendState": "Running",
        "Self": {
            "HostName": "zcnode",
            "DNSName": "zcnode.tail1234.ts.net.",
            "TailscaleIPs": ["100.101.102.103", "fd7a:115c:a1e0::1234", "not-an-ip"]
        }
    }"#;

    fn tailscale_tunnel_cfg(hostname: Option<&str>) -> TunnelConfig {
        TunnelConfig {
            tunnel_provider: "tailscale".into(),
            tailscale: Some(zeroclaw_config::schema::TailscaleTunnelConfig {
                funnel: false,
                hostname: hostname.map(Into::into),
            }),
            ..TunnelConfig::default()
        }
    }

    fn wss_enabled(bind: &str) -> WssConfig {
        WssConfig {
            enabled: true,
            bind: bind.into(),
            ..WssConfig::default()
        }
    }

    #[test]
    fn parse_tailscale_self_reads_dns_name_and_ips() {
        let node = parse_tailscale_self(STATUS_JSON.as_bytes());
        assert_eq!(node.dns_name.as_deref(), Some("zcnode.tail1234.ts.net"));
        assert_eq!(
            node.ips,
            vec![
                "100.101.102.103".parse::<IpAddr>().unwrap(),
                "fd7a:115c:a1e0::1234".parse::<IpAddr>().unwrap(),
            ]
        );
    }

    /// `tailscale status --json` before tailscaled has its network map
    /// (shape from ipnlocal's status builder: OS hostname only, empty
    /// DNSName, null TailscaleIPs, InNetworkMap false). Exit status is 0.
    const NOT_READY_STATUS_JSON: &str = r#"{
        "BackendState": "Starting",
        "Self": {
            "HostName": "zcnode",
            "DNSName": "",
            "TailscaleIPs": null,
            "InNetworkMap": false,
            "Online": false
        }
    }"#;

    fn unavailable(hostname_override: Option<&str>) -> TailnetSans {
        TailnetSans::Unavailable {
            hostname_override: hostname_override.map(Into::into),
        }
    }

    #[test]
    fn status_before_network_map_is_unavailable_not_empty() {
        let node = parse_tailscale_self(NOT_READY_STATUS_JSON.as_bytes());
        assert!(!node.has_identity());
        assert_eq!(tailnet_sans_from_status(Ok(node), None), unavailable(None));
        // The override is known without tailscaled and must survive.
        let node = parse_tailscale_self(NOT_READY_STATUS_JSON.as_bytes());
        assert_eq!(
            tailnet_sans_from_status(Ok(node), Some("zero.tail1234.ts.net".into())),
            unavailable(Some("zero.tail1234.ts.net"))
        );
    }

    #[test]
    fn malformed_status_is_unavailable() {
        for raw in [&b"not json"[..], b"{}", br#"{"Self":null}"#] {
            let node = parse_tailscale_self(raw);
            assert_eq!(tailnet_sans_from_status(Ok(node), None), unavailable(None));
        }
    }

    #[test]
    fn failed_status_query_is_unavailable() {
        let err = anyhow::Error::msg("tailscale status timed out");
        assert_eq!(
            tailnet_sans_from_status(Err(err), Some("zero".into())),
            unavailable(Some("zero"))
        );
    }

    #[test]
    fn stale_identity_outside_the_network_map_is_unavailable() {
        // Names without the map are not trusted as current.
        let node = parse_tailscale_self(
            br#"{"Self":{"DNSName":"old.tail1234.ts.net.","TailscaleIPs":["100.101.102.103"],"InNetworkMap":false}}"#,
        );
        assert_eq!(tailnet_sans_from_status(Ok(node), None), unavailable(None));
    }

    #[test]
    fn ready_status_resolves_and_older_releases_without_the_flag_still_resolve() {
        let node = parse_tailscale_self(STATUS_JSON.as_bytes());
        assert!(matches!(
            tailnet_sans_from_status(Ok(node), None),
            TailnetSans::Resolved(names) if names.contains(&"zcnode.tail1234.ts.net".to_string())
        ));
        // STATUS_JSON has no InNetworkMap field: an identity alone suffices.
        assert_eq!(
            parse_tailscale_self(STATUS_JSON.as_bytes()).in_network_map,
            None
        );
        let ready = parse_tailscale_self(
            br#"{"Self":{"DNSName":"zcnode.tail1234.ts.net.","TailscaleIPs":[],"InNetworkMap":true}}"#,
        );
        assert!(ready.has_identity());
    }

    #[test]
    fn parse_tailscale_self_tolerates_garbage() {
        assert_eq!(parse_tailscale_self(b"not json"), TailscaleSelf::default());
        assert_eq!(
            parse_tailscale_self(br#"{"Self":{"DNSName":""}}"#),
            TailscaleSelf::default()
        );
    }

    #[test]
    fn is_tailscale_ip_matches_cgnat_and_tailnet_ula_only() {
        for ip in [
            "100.64.0.0",
            "100.101.102.103",
            "100.127.255.255",
            "fd7a:115c:a1e0::1",
        ] {
            assert!(is_tailscale_ip(ip.parse().unwrap()), "{ip}");
        }
        for ip in [
            "100.63.255.255",
            "100.128.0.0",
            "127.0.0.1",
            "0.0.0.0",
            "192.168.1.1",
            "fd7a:115c:a1e1::1",
            "::1",
        ] {
            assert!(!is_tailscale_ip(ip.parse().unwrap()), "{ip}");
        }
    }

    #[test]
    fn tailnet_reach_requires_wss() {
        let enroll = EnrollConfig::default();
        assert!(!tailnet_reaches_listeners(
            &tailscale_tunnel_cfg(None),
            &WssConfig::default(),
            &enroll
        ));
    }

    #[test]
    fn tailnet_reach_via_tailscale_tunnel() {
        assert!(tailnet_reaches_listeners(
            &tailscale_tunnel_cfg(None),
            &wss_enabled("0.0.0.0"),
            &EnrollConfig::default()
        ));
    }

    #[test]
    fn tailnet_reach_via_direct_tailnet_bind_without_tunnel() {
        let no_tunnel = TunnelConfig::default();
        assert!(tailnet_reaches_listeners(
            &no_tunnel,
            &wss_enabled("100.101.102.103"),
            &EnrollConfig::default()
        ));
        let enroll = EnrollConfig {
            enabled: true,
            bind: "fd7a:115c:a1e0::1234".into(),
            ..EnrollConfig::default()
        };
        assert!(tailnet_reaches_listeners(
            &no_tunnel,
            &wss_enabled("127.0.0.1"),
            &enroll
        ));
    }

    #[test]
    fn tailnet_reach_false_for_lan_only_daemon() {
        // A disabled enroll bound to a tailnet IP does not count: nothing listens.
        let enroll = EnrollConfig {
            enabled: false,
            bind: "100.101.102.103".into(),
            ..EnrollConfig::default()
        };
        assert!(!tailnet_reaches_listeners(
            &TunnelConfig::default(),
            &wss_enabled("0.0.0.0"),
            &enroll
        ));
    }

    #[test]
    fn tailnet_names_lists_override_fqdn_short_name_and_ips() {
        let node = parse_tailscale_self(STATUS_JSON.as_bytes());
        assert_eq!(
            tailnet_names(Some("zero.example.ts.net."), &node),
            vec![
                "zero.example.ts.net",
                "zcnode.tail1234.ts.net",
                "zcnode",
                "100.101.102.103",
                "fd7a:115c:a1e0::1234",
            ]
        );
    }

    #[test]
    fn tailnet_names_dedupes_override_matching_magicdns() {
        let node = parse_tailscale_self(STATUS_JSON.as_bytes());
        let names = tailnet_names(Some("ZCNODE.tail1234.ts.net"), &node);
        assert_eq!(names[0], "ZCNODE.tail1234.ts.net");
        assert_eq!(names[1], "zcnode");
        assert_eq!(names.len(), 4);
    }

    #[test]
    fn tailnet_names_empty_without_identity() {
        assert!(tailnet_names(None, &TailscaleSelf::default()).is_empty());
    }

    #[tokio::test]
    async fn tailscale_server_sans_skips_query_when_tailnet_unused() {
        // No tunnel, LAN bind: returns before ever invoking `tailscale`.
        let sans = tailscale_server_sans(
            &TunnelConfig::default(),
            &wss_enabled("0.0.0.0"),
            &EnrollConfig::default(),
        )
        .await;
        assert_eq!(sans, TailnetSans::NotApplicable);
    }

    fn forwarder_service() -> TcpService {
        service("wss", "wss", "127.0.0.1:9781")
    }

    /// Stands in for `tailscale serve --tcp`: the watcher only cares whether
    /// the process is alive.
    fn spawn_stand_in(script: &str) -> Child {
        Command::new("sh")
            .args(["-c", script])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .expect("spawn stand-in forwarder")
    }

    async fn wait_until(mut done: impl AsyncFnMut() -> bool) {
        tokio::time::timeout(Duration::from_secs(5), async {
            while !done().await {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("condition not reached within 5s");
    }

    #[tokio::test]
    async fn forwarder_exit_after_publication_is_detected() {
        // A forwarder that exits after the startup check must be observed.
        let forwarder =
            TcpForwarder::spawn(forwarder_service(), spawn_stand_in("echo gone >&2; exit 3"));
        wait_until(async || !forwarder.is_running()).await;
    }

    #[tokio::test]
    async fn health_check_fails_when_a_forwarder_has_exited() {
        let tunnel = TailscaleTunnel::new(false, Some("node.tailnet.ts.net".into()));
        // Gateway serve process stand-in, alive for the whole test.
        *tunnel.proc.lock().await = Some(TunnelProcess {
            child: spawn_stand_in("sleep 30"),
            public_url: "https://node.tailnet.ts.net".into(),
        });
        tunnel.tcp_forwarders.lock().await.push(TcpForwarder::spawn(
            forwarder_service(),
            spawn_stand_in("sleep 30"),
        ));
        assert!(tunnel.health_check().await);

        tunnel.tcp_forwarders.lock().await.push(TcpForwarder::spawn(
            forwarder_service(),
            spawn_stand_in("exit 1"),
        ));
        wait_until(async || !tunnel.health_check().await).await;

        kill_shared(&tunnel.proc).await.ok();
        let forwarders: Vec<TcpForwarder> = tunnel.tcp_forwarders.lock().await.drain(..).collect();
        for forwarder in forwarders {
            forwarder.shutdown().await;
        }
    }

    #[tokio::test]
    async fn forwarder_shutdown_kills_and_reaps_the_process() {
        let forwarder = TcpForwarder::spawn(forwarder_service(), spawn_stand_in("sleep 30"));
        assert!(forwarder.is_running());
        tokio::time::timeout(Duration::from_secs(5), forwarder.shutdown())
            .await
            .expect("shutdown should kill the forwarder, not wait out its sleep");
    }

    /// stderr of a forwarder that lost the serve-config race (Tailscale 1.102.4).
    const CONFLICT_STDERR: &str = "Another client is changing the serve config; please try again.\nsending serve config: Preconditions failed: etag mismatch";

    fn quick_startup() -> ForwarderStartup {
        ForwarderStartup {
            apply_timeout: Duration::from_millis(300),
            poll: Duration::from_millis(10),
            conflict_attempts: 3,
            conflict_backoff: Duration::from_millis(1),
        }
    }

    fn exit_with_stderr(stderr: &str) -> Child {
        spawn_stand_in(&format!("printf '%s' '{stderr}' >&2; exit 1"))
    }

    fn exit_with_stderr_after(delay_secs: &str, stderr: &str) -> Child {
        spawn_stand_in(&format!(
            "sleep {delay_secs}; printf '%s' '{stderr}' >&2; exit 1"
        ))
    }

    #[test]
    fn serve_config_conflict_is_recognised() {
        assert!(is_serve_config_conflict(CONFLICT_STDERR));
        assert!(!is_serve_config_conflict(
            "Access denied: serve config denied"
        ));
        assert!(!is_serve_config_conflict(""));
    }

    fn sessions(ids: &[&str]) -> BTreeSet<String> {
        ids.iter().map(|id| (*id).to_string()).collect()
    }

    fn addr(s: &str) -> SocketAddr {
        s.parse().unwrap()
    }

    #[test]
    fn serve_sessions_match_only_a_raw_forward_to_the_requested_target() {
        // Foreground sessions, shape from a live 1.102.4 node.
        let fg = br#"{"Foreground":{"a7e5":{"TCP":{"19782":{"TCPForward":"127.0.0.1:19782"}}},"ecea":{"TCP":{"443":{"HTTPS":true}}}}}"#;
        assert_eq!(
            serve_sessions_forwarding(fg, addr("127.0.0.1:19782")),
            Some(sessions(&["a7e5"]))
        );
        // Something else on the port is not the requested forward: an HTTPS
        // handler, a forward to another target, or TLS terminated by tailscaled.
        assert_eq!(
            serve_sessions_forwarding(fg, addr("127.0.0.1:443")),
            Some(sessions(&[]))
        );
        let elsewhere =
            br#"{"Foreground":{"b1":{"TCP":{"19781":{"TCPForward":"127.0.0.1:8080"}}}}}"#;
        assert_eq!(
            serve_sessions_forwarding(elsewhere, addr("127.0.0.1:19781")),
            Some(sessions(&[]))
        );
        let terminated = br#"{"Foreground":{"c1":{"TCP":{"19781":{"TCPForward":"127.0.0.1:19781","TerminateTLS":"node.tailnet.ts.net"}}}}}"#;
        assert_eq!(
            serve_sessions_forwarding(terminated, addr("127.0.0.1:19781")),
            Some(sessions(&[]))
        );
        // The background config is never this process's foreground session.
        let bg = br#"{"TCP":{"19781":{"TCPForward":"127.0.0.1:19781"}}}"#;
        assert_eq!(
            serve_sessions_forwarding(bg, addr("127.0.0.1:19781")),
            Some(sessions(&[]))
        );
        // `tcp://[::1]:p` is stored as the URL host, brackets included.
        let v6 = br#"{"Foreground":{"d1":{"TCP":{"9782":{"TCPForward":"[::1]:9782"}}}}}"#;
        assert_eq!(
            serve_sessions_forwarding(v6, addr("[::1]:9782")),
            Some(sessions(&["d1"]))
        );
        assert_eq!(
            serve_sessions_forwarding(b"{}", addr("127.0.0.1:19781")),
            Some(sessions(&[]))
        );
        assert_eq!(
            serve_sessions_forwarding(b"not json", addr("127.0.0.1:19781")),
            None
        );
    }

    /// A probe that reports `before` until `spawns` reaches `applied_at`, then
    /// `after`.
    fn probe_after<'a>(
        spawns: &'a std::sync::atomic::AtomicUsize,
        applied_at: usize,
        before: &'a [&'a str],
        after: &'a [&'a str],
    ) -> impl FnMut() -> std::future::Ready<Option<BTreeSet<String>>> + 'a {
        move || {
            let n = spawns.load(std::sync::atomic::Ordering::SeqCst);
            std::future::ready(Some(sessions(if n >= applied_at { after } else { before })))
        }
    }

    #[tokio::test]
    async fn start_forwarder_does_not_take_an_existing_forward_as_its_own() {
        // Another session already forwards the port to the same target, so the
        // new forwarder is rejected. Its exit must be reported, not hidden
        // behind the forward that was there before it started.
        let spawns = std::sync::atomic::AtomicUsize::new(0);
        let err = start_forwarder(
            quick_startup(),
            || {
                spawns.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(spawn_stand_in(
                    "sleep 0.1; echo 'port 9781 already in use' >&2; exit 1",
                ))
            },
            probe_after(&spawns, 0, &["other"], &["other"]),
        )
        .await
        .expect_err("a pre-existing forward is not this forwarder's publication");
        assert!(
            matches!(err, ForwarderStartError::Exited { ref stderr, .. } if stderr.contains("already in use")),
            "{err:?}"
        );
    }

    #[tokio::test]
    async fn start_forwarder_accepts_its_new_session_beside_an_existing_one() {
        let spawns = std::sync::atomic::AtomicUsize::new(0);
        let child = start_forwarder(
            quick_startup(),
            || {
                spawns.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(spawn_stand_in("sleep 30"))
            },
            probe_after(&spawns, 1, &["other"], &["other", "mine"]),
        )
        .await
        .expect("the new session is this forwarder's");
        TcpForwarder::spawn(forwarder_service(), child)
            .shutdown()
            .await;
    }

    #[tokio::test]
    async fn start_forwarder_rechecks_the_process_after_the_probe() {
        // The process is alive when polled, then exits while the status probe
        // is in flight. A forward seen by that probe is not proof the process
        // still holds it.
        let probes = std::sync::atomic::AtomicUsize::new(0);
        let err = start_forwarder(
            quick_startup(),
            || Ok(exit_with_stderr_after("0.05", "rejected")),
            || {
                let first = probes.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0;
                async move {
                    if first {
                        return Some(sessions(&[]));
                    }
                    tokio::time::sleep(Duration::from_millis(200)).await;
                    Some(sessions(&["mine"]))
                }
            },
        )
        .await
        .expect_err("a process that exited during the probe is not published");
        assert!(
            matches!(err, ForwarderStartError::Exited { ref stderr, .. } if stderr.contains("rejected")),
            "{err:?}"
        );
    }

    #[tokio::test]
    async fn start_forwarder_retries_a_serve_config_conflict() {
        // The first attempt loses the etag race; the retry is applied.
        let spawns = std::sync::atomic::AtomicUsize::new(0);
        let child = start_forwarder(
            quick_startup(),
            || {
                Ok(
                    match spawns.fetch_add(1, std::sync::atomic::Ordering::SeqCst) {
                        0 => exit_with_stderr(CONFLICT_STDERR),
                        _ => spawn_stand_in("sleep 30"),
                    },
                )
            },
            probe_after(&spawns, 2, &[], &["retry"]),
        )
        .await
        .expect("retry should publish");
        assert_eq!(spawns.load(std::sync::atomic::Ordering::SeqCst), 2);
        TcpForwarder::spawn(forwarder_service(), child)
            .shutdown()
            .await;
    }

    #[tokio::test]
    async fn start_forwarder_does_not_retry_other_failures() {
        let spawns = std::sync::atomic::AtomicUsize::new(0);
        let err = start_forwarder(
            quick_startup(),
            || {
                spawns.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(exit_with_stderr("Access denied: serve config denied"))
            },
            || async { Some(sessions(&[])) },
        )
        .await
        .expect_err("a non-conflict exit fails");
        assert_eq!(spawns.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert!(
            matches!(err, ForwarderStartError::Exited { attempts: 1, ref stderr, .. } if stderr.contains("Access denied")),
            "{err:?}"
        );
    }

    #[tokio::test]
    async fn start_forwarder_gives_up_after_bounded_conflicts() {
        let spawns = std::sync::atomic::AtomicUsize::new(0);
        let err = start_forwarder(
            quick_startup(),
            || {
                spawns.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(exit_with_stderr(CONFLICT_STDERR))
            },
            || async { Some(sessions(&[])) },
        )
        .await
        .expect_err("persistent conflicts fail");
        assert_eq!(spawns.load(std::sync::atomic::Ordering::SeqCst), 3);
        assert!(
            matches!(err, ForwarderStartError::Exited { attempts: 3, .. }),
            "{err:?}"
        );
    }

    #[tokio::test]
    async fn start_forwarder_kills_a_process_that_never_applies() {
        // Alive is not published: a forward that never shows up in the serve
        // config is reported and its process is not left behind.
        let pid = std::sync::atomic::AtomicU32::new(0);
        let err = start_forwarder(
            quick_startup(),
            || {
                let child = spawn_stand_in("sleep 30");
                pid.store(child.id().unwrap_or(0), std::sync::atomic::Ordering::SeqCst);
                Ok(child)
            },
            || async { Some(sessions(&[])) },
        )
        .await
        .expect_err("never applied");
        assert!(
            matches!(err, ForwarderStartError::NotApplied { .. }),
            "{err:?}"
        );
        let pid = pid.load(std::sync::atomic::Ordering::SeqCst);
        assert!(
            !std::path::Path::new(&format!("/proc/{pid}")).exists(),
            "pid {pid} left running"
        );
    }

    #[test]
    fn tcp_serve_args_are_raw_passthrough_on_the_same_port() {
        let wss = service("wss", "wss", "127.0.0.1:9781");
        assert_eq!(
            tcp_serve_args(&wss),
            vec!["serve", "--tcp", "9781", "tcp://127.0.0.1:9781"]
        );
    }

    #[test]
    fn tcp_serve_args_bracket_ipv6_targets() {
        let enroll = service("enroll", "https", "[::1]:9782");
        assert_eq!(
            tcp_serve_args(&enroll),
            vec!["serve", "--tcp", "9782", "tcp://[::1]:9782"]
        );
    }

    #[test]
    fn tcp_endpoint_uses_service_scheme_and_port() {
        let wss = service("wss", "wss", "127.0.0.1:9781");
        let enroll = service("enroll", "https", "127.0.0.1:9782");
        assert_eq!(
            tcp_endpoint("node.tailnet.ts.net", &wss),
            "wss://node.tailnet.ts.net:9781"
        );
        assert_eq!(
            tcp_endpoint("node.tailnet.ts.net", &enroll),
            "https://node.tailnet.ts.net:9782"
        );
    }

    #[tokio::test]
    async fn publish_no_services_spawns_nothing() {
        // No hostname override: any `tailscale` invocation would be attempted,
        // so an empty result proves the early return.
        let tunnel = TailscaleTunnel::new(false, None);
        let published = tunnel.publish_tcp_services(&[]).await.unwrap();
        assert!(published.is_empty());
        assert!(tunnel.tcp_forwarders.lock().await.is_empty());
    }
}
