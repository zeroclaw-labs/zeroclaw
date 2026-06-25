//! `zerorelay` - the ZeroClaw nominated relay (blind forwarder).
//!
//! Runs a public rendezvous: daemons behind NAT register over an outer TLS +
//! WebSocket session and clients reach them by an opaque `node_id`. The relay
//! pipes the inner client<->daemon mTLS as ciphertext and never terminates it.

use std::fs::File;
use std::io::BufReader;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio_rustls::TlsAcceptor;
use zerorelay::{Admission, RelayConfig, RelayServer};

/// Build-time version: `git describe` (tag + commits-since + short hash, `-dirty`
/// when modified), or the crate version when git is unavailable. Set by build.rs.
const VERSION: &str = env!("ZERORELAY_VERSION");

#[derive(Parser, Debug)]
#[command(
    name = "zerorelay",
    about = "ZeroClaw nominated relay (blind forwarder)",
    version = VERSION
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    /// Address to listen on for daemon and client connections.
    #[arg(long, default_value = "0.0.0.0:8443")]
    bind: String,

    /// PEM certificate for the relay's own outer TLS identity (chain).
    #[arg(long)]
    tls_cert: Option<String>,

    /// PEM private key for `--tls-cert`.
    #[arg(long)]
    tls_key: Option<String>,

    /// Admission mode: `open` (any signed daemon may register) or `allowlist`.
    #[arg(long, default_value = "open")]
    registration_mode: String,

    /// Allowed daemon pubkey fingerprints (sha256 hex), allowlist mode. Repeatable.
    #[arg(long = "allow")]
    allow: Vec<String>,

    /// Denied daemon pubkey fingerprints (always rejected). Repeatable.
    #[arg(long = "deny")]
    deny: Vec<String>,

    /// Optional shared-secret gate a daemon must present in its Hello.
    #[arg(long)]
    relay_token: Option<String>,

    /// Cap on simultaneously-open client connections per node-id.
    #[arg(long, default_value_t = 256)]
    max_conns_per_node: usize,

    /// Drop a client connection after this many seconds of inactivity.
    #[arg(long, default_value_t = 300)]
    idle_timeout_secs: u64,

    /// Lease TTL (seconds) advertised to daemons at registration.
    #[arg(long, default_value_t = 300)]
    lease_ttl_secs: u64,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// TCP-connect to a running relay and exit 0 if reachable. For container
    /// HEALTHCHECK on shell-less images.
    Healthcheck {
        /// Address to probe.
        #[arg(long, default_value = "127.0.0.1:8443")]
        addr: String,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    if let Some(Command::Healthcheck { addr }) = &cli.command {
        tokio::net::TcpStream::connect(addr)
            .await
            .with_context(|| format!("relay not reachable at {addr}"))?;
        return Ok(());
    }

    let registration_mode = match cli.registration_mode.as_str() {
        "open" => Admission::Open,
        "allowlist" => Admission::Allowlist,
        other => anyhow::bail!("invalid --registration-mode '{other}' (open|allowlist)"),
    };

    let tls_cert = cli
        .tls_cert
        .context("--tls-cert is required (the relay's own outer TLS certificate)")?;
    let tls_key = cli
        .tls_key
        .context("--tls-key is required (the key for --tls-cert)")?;
    let acceptor = build_tls_acceptor(&tls_cert, &tls_key)
        .with_context(|| format!("loading relay TLS material from {tls_cert} / {tls_key}"))?;

    let cfg = RelayConfig {
        registration_mode,
        allow: cli.allow.into_iter().collect(),
        deny: cli.deny.into_iter().collect(),
        relay_token: cli.relay_token,
        lease_ttl: Duration::from_secs(cli.lease_ttl_secs),
        max_conns_per_node: cli.max_conns_per_node,
        idle_timeout: Duration::from_secs(cli.idle_timeout_secs),
    };

    let listener = tokio::net::TcpListener::bind(&cli.bind)
        .await
        .with_context(|| format!("binding relay on {}", cli.bind))?;
    let addr = listener.local_addr()?;
    eprintln!(
        "zerorelay listening on {addr} (outer TLS, mode: {:?})",
        cfg.registration_mode
    );

    RelayServer::new(cfg).serve(listener, acceptor).await
}

/// Build the outer TLS acceptor from the relay's own server cert + key. Outer TLS
/// is server-authenticated only: clients/daemons verify the relay; the relay does
/// not require a client certificate on the outer layer (the inner mTLS is the RPC
/// security boundary).
fn build_tls_acceptor(cert_path: &str, key_path: &str) -> Result<TlsAcceptor> {
    let certs = load_certs(cert_path)?;
    let key = load_key(key_path)?;
    let config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .context("ring provider supports the default protocol versions")?
    .with_no_client_auth()
    .with_single_cert(certs, key)
    .context("relay cert/key are not a valid pair")?;
    Ok(TlsAcceptor::from(Arc::new(config)))
}

fn load_certs(path: &str) -> Result<Vec<CertificateDer<'static>>> {
    let mut rd = BufReader::new(File::open(path).with_context(|| format!("opening {path}"))?);
    let certs: Vec<_> = rustls_pemfile::certs(&mut rd).collect::<Result<_, _>>()?;
    if certs.is_empty() {
        anyhow::bail!("no certificates found in {path}");
    }
    Ok(certs)
}

fn load_key(path: &str) -> Result<PrivateKeyDer<'static>> {
    let mut rd = BufReader::new(File::open(path).with_context(|| format!("opening {path}"))?);
    rustls_pemfile::private_key(&mut rd)?.with_context(|| format!("no private key in {path}"))
}
