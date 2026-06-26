//! `zerorelay` - the ZeroClaw nominated relay (blind forwarder).
//!
//! Runs a public rendezvous: daemons behind NAT register over an outer TLS +
//! WebSocket session and clients reach them by an opaque `node_id`. The relay
//! pipes the inner client<->daemon mTLS as ciphertext and never terminates it.

use std::fs::File;
use std::io::BufReader;
use std::path::PathBuf;
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

    /// PEM certificate for the relay's own outer TLS identity (chain). When this
    /// and --tls-key are omitted, the relay SELF-PROVISIONS a cert (no openssl).
    #[arg(long)]
    tls_cert: Option<String>,

    /// PEM private key for `--tls-cert`.
    #[arg(long)]
    tls_key: Option<String>,

    /// Directory for the self-provisioned TLS material (CA + server cert, written
    /// on first run when --tls-cert/--tls-key are not given, reused after).
    /// Default: $ZERORELAY_DATA_DIR or $HOME/.zerorelay, under tls/.
    #[arg(long)]
    tls_dir: Option<String>,

    /// Extra Subject Alternative Name(s) for the self-provisioned cert (the relay's
    /// public hostname / IP). localhost + 127.0.0.1 are always included. Repeatable.
    #[arg(long = "tls-san")]
    tls_san: Vec<String>,

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

    let acceptor = match (cli.tls_cert, cli.tls_key) {
        // Bring-your-own (e.g. a public-CA cert for the relay's hostname).
        (Some(cert), Some(key)) => build_tls_acceptor(&cert, &key)
            .with_context(|| format!("loading relay TLS material from {cert} / {key}"))?,
        // Self-provision a cert on first run - no openssl needed.
        (None, None) => provision_tls_acceptor(cli.tls_dir.as_deref(), &cli.tls_san)?,
        _ => anyhow::bail!(
            "--tls-cert and --tls-key must be given together (or neither, to self-provision)"
        ),
    };

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

/// Self-provision the relay's outer TLS cert (CA + server leaf with SANs) on first
/// run, reusing the daemon's `zeroclaw-tls` machinery so no openssl is needed.
/// Reused on later runs. Prints the CA path daemons/clients should trust.
fn provision_tls_acceptor(tls_dir: Option<&str>, extra_sans: &[String]) -> Result<TlsAcceptor> {
    let dir = tls_dir.map(PathBuf::from).unwrap_or_else(default_tls_dir);
    let mut sans = vec!["localhost".to_string(), "127.0.0.1".to_string()];
    for s in extra_sans {
        if !s.is_empty() && !sans.contains(s) {
            sans.push(s.clone());
        }
    }
    let mats = zeroclaw_tls::ensure_server_materials(&dir, &sans)
        .with_context(|| format!("self-provisioning relay TLS in {}", dir.display()))?;
    let acceptor = build_tls_acceptor(
        &mats.server_cert_path.to_string_lossy(),
        &mats.server_key_path.to_string_lossy(),
    )?;
    eprintln!("zerorelay: self-provisioned outer TLS in {}", dir.display());
    eprintln!("  SANs: {}", sans.join(", "));
    eprintln!("  Trust this relay on daemons/clients with its CA:");
    eprintln!(
        "    daemon  [relay] relay_ca_path = \"{}\"",
        mats.ca_cert_path.display()
    );
    eprintln!("    zerocode  --relay-ca {}", mats.ca_cert_path.display());
    Ok(acceptor)
}

/// Default location for self-provisioned relay TLS material.
fn default_tls_dir() -> PathBuf {
    std::env::var_os("ZERORELAY_DATA_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".zerorelay")))
        .unwrap_or_else(|| PathBuf::from("./zerorelay"))
        .join("tls")
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
