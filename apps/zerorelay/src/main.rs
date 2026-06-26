//! `zerorelay` - the ZeroClaw nominated relay (blind forwarder).
//!
//! Runs a public rendezvous: daemons behind NAT register over an outer TLS +
//! WebSocket session and clients reach them by an opaque `node_id`. The relay
//! pipes the inner client<->daemon mTLS as ciphertext and never terminates it.
//!
//! `zerorelay` is a standalone networking app (not daemon-path code), so bare
//! `tokio::spawn` is the right primitive here; the `zeroclaw_spawn::spawn!` rule
//! is for in-daemon tasks. Mirrors the `apps/zerocode` exemption (and lib.rs).
#![allow(clippy::disallowed_methods)]

use std::fs::File;
use std::io::BufReader;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use std::collections::HashSet;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use serde::Deserialize;
use tokio_rustls::TlsAcceptor;
use zerorelay::{Admission, AdmissionPolicy, RelayConfig, RelayServer, RelayStatus};

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

    /// Path to a relay.toml ([bind]/[tls]/[admission]/[limits]). Values it sets
    /// are the base config; any CLI flag below overrides the file. The
    /// [admission] section hot-reloads on SIGHUP. See relay.example.toml.
    #[arg(long)]
    config: Option<String>,

    /// Address to listen on for daemon and client connections. [default: 0.0.0.0:8443]
    #[arg(long)]
    bind: Option<String>,

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
    /// [default: open]
    #[arg(long)]
    registration_mode: Option<String>,

    /// Allowed daemon pubkey fingerprints (sha256 hex), allowlist mode. Unioned
    /// with the file's [admission] allow list. Repeatable.
    #[arg(long = "allow")]
    allow: Vec<String>,

    /// Denied daemon pubkey fingerprints (always rejected). Unioned with the
    /// file's deny list. Repeatable.
    #[arg(long = "deny")]
    deny: Vec<String>,

    /// Optional shared-secret gate a daemon must present in its Hello.
    #[arg(long)]
    relay_token: Option<String>,

    /// Cap on simultaneously-open client connections per node-id. [default: 256]
    #[arg(long)]
    max_conns_per_node: Option<usize>,

    /// Drop a client connection after this many seconds of inactivity. [default: 300]
    #[arg(long)]
    idle_timeout_secs: Option<u64>,

    /// Lease TTL (seconds) advertised to daemons at registration. [default: 300]
    #[arg(long)]
    lease_ttl_secs: Option<u64>,

    /// Write a per-node metrics snapshot (JSON) to this path, refreshed on a timer
    /// and on SIGUSR1. Read it back with `zerorelay status --file <path>`.
    #[arg(long)]
    status_file: Option<String>,
}

/// A `relay.toml`: every value optional, CLI flags override. The `[admission]`
/// slice is what SIGHUP re-reads and swaps live.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileConfig {
    bind: Option<String>,
    #[serde(default)]
    tls: TlsFile,
    #[serde(default)]
    admission: AdmissionFile,
    #[serde(default)]
    limits: LimitsFile,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct TlsFile {
    cert: Option<String>,
    key: Option<String>,
    dir: Option<String>,
    #[serde(default)]
    sans: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct AdmissionFile {
    /// "open" | "allowlist".
    mode: Option<String>,
    #[serde(default)]
    allow: Vec<String>,
    #[serde(default)]
    deny: Vec<String>,
    relay_token: Option<String>,
    /// Outer-mTLS variant (additive admission on the OUTER TLS): "off" (default),
    /// "optional", or "required". When on, `outer_client_ca` verifies the peer's
    /// outer client cert. The inner mTLS is unaffected.
    outer_client_auth: Option<String>,
    /// PEM CA verifying outer client certs (required when outer_client_auth is on).
    outer_client_ca: Option<String>,
    /// Route to the node-id named by the outer client cert's CN, falling back to
    /// the `Connect` frame (default false; only meaningful with outer client auth).
    route_by_client_cert: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct LimitsFile {
    max_conns_per_node: Option<usize>,
    idle_timeout_secs: Option<u64>,
    lease_ttl_secs: Option<u64>,
    /// Per-source-IP connection-handshake rate cap (A6).
    accept_burst_per_ip: Option<u32>,
    accept_rate_per_ip: Option<f64>,
    /// Per-node-id client-connect rate cap (A6).
    connect_burst_per_node: Option<u32>,
    connect_rate_per_node: Option<f64>,
}

/// The CLI admission overrides, captured so SIGHUP can re-apply them on top of a
/// freshly re-read file without re-parsing argv.
#[derive(Clone)]
struct AdmissionOverlay {
    mode: Option<String>,
    allow: Vec<String>,
    deny: Vec<String>,
    relay_token: Option<String>,
}

/// Resolve the admission policy: file `[admission]` as the base, CLI overlay on
/// top. Scalars (mode, relay_token) take the CLI value when present; the allow /
/// deny lists are the UNION of file + CLI. Deny always wins at admission time.
fn resolve_admission(file: &AdmissionFile, overlay: &AdmissionOverlay) -> Result<AdmissionPolicy> {
    let mode_str = overlay.mode.clone().or_else(|| file.mode.clone());
    let registration_mode = match mode_str.as_deref() {
        None | Some("open") => Admission::Open,
        Some("allowlist") => Admission::Allowlist,
        Some(other) => anyhow::bail!("invalid admission mode '{other}' (open|allowlist)"),
    };
    let mut allow: HashSet<String> = file.allow.iter().cloned().collect();
    allow.extend(overlay.allow.iter().cloned());
    let mut deny: HashSet<String> = file.deny.iter().cloned().collect();
    deny.extend(overlay.deny.iter().cloned());
    let relay_token = overlay
        .relay_token
        .clone()
        .or_else(|| file.relay_token.clone());
    Ok(AdmissionPolicy {
        registration_mode,
        allow,
        deny,
        relay_token,
    })
}

/// Load and parse a relay.toml.
fn load_file_config(path: &str) -> Result<FileConfig> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading relay config {path}"))?;
    toml::from_str(&text).with_context(|| format!("parsing relay config {path}"))
}

/// Read a relay `--status-file` snapshot and print it as a per-node table.
fn print_status(path: &str) -> Result<()> {
    let text = std::fs::read_to_string(path).with_context(|| {
        format!("reading status file {path} (is --status-file set on the relay?)")
    })?;
    let status: RelayStatus =
        serde_json::from_str(text.trim()).with_context(|| format!("parsing status file {path}"))?;
    if status.nodes.is_empty() {
        println!("no registered nodes");
        return Ok(());
    }
    println!(
        "{:<34}  {:>5}  {:>6}  {:>8}  {:>8}",
        "node_id", "live", "total", "frames", "rejected"
    );
    for n in &status.nodes {
        println!(
            "{:<34}  {:>5}  {:>6}  {:>8}  {:>8}",
            n.node_id, n.conns_live, n.conns_total, n.frames_relayed, n.connects_rejected
        );
    }
    Ok(())
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
    /// Print the running relay's per-node metrics from its --status-file snapshot
    /// (counts only, never payloads). Send the relay SIGUSR1 first to refresh it.
    Status {
        /// Path to the relay's --status-file.
        #[arg(long)]
        file: String,
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

    if let Some(Command::Status { file }) = &cli.command {
        return print_status(file);
    }

    // relay.toml is the base; CLI flags override. Absent --config => an empty
    // file config, so the CLI + builtin defaults reproduce the prior behavior.
    let file = match cli.config.as_deref() {
        Some(path) => load_file_config(path)?,
        None => FileConfig::default(),
    };

    // The CLI admission overlay is captured so SIGHUP can re-apply it onto a
    // freshly re-read file.
    let overlay = AdmissionOverlay {
        mode: cli.registration_mode.clone(),
        allow: cli.allow.clone(),
        deny: cli.deny.clone(),
        relay_token: cli.relay_token.clone(),
    };
    let admission = resolve_admission(&file.admission, &overlay)?;

    // Outer-mTLS variant (additive): optionally require/accept an outer client
    // cert, verified against outer_client_ca. The inner mTLS is untouched.
    let outer_verifier = build_outer_client_verifier(&file.admission)?;
    let route_by_client_cert = file.admission.route_by_client_cert.unwrap_or(false);

    // TLS material: CLI flag -> file [tls] -> self-provision.
    let tls_cert = cli.tls_cert.clone().or_else(|| file.tls.cert.clone());
    let tls_key = cli.tls_key.clone().or_else(|| file.tls.key.clone());
    let tls_dir = cli.tls_dir.clone().or_else(|| file.tls.dir.clone());
    let mut tls_sans = file.tls.sans.clone();
    tls_sans.extend(cli.tls_san.iter().cloned());
    let acceptor = match (tls_cert, tls_key) {
        // Bring-your-own (e.g. a public-CA cert for the relay's hostname).
        (Some(cert), Some(key)) => build_tls_acceptor(&cert, &key, outer_verifier.clone())
            .with_context(|| format!("loading relay TLS material from {cert} / {key}"))?,
        // Self-provision a cert on first run - no openssl needed.
        (None, None) => {
            provision_tls_acceptor(tls_dir.as_deref(), &tls_sans, outer_verifier.clone())?
        }
        _ => {
            anyhow::bail!("tls cert and key must be given together (or neither, to self-provision)")
        }
    };

    let bind = cli
        .bind
        .clone()
        .or_else(|| file.bind.clone())
        .unwrap_or_else(|| "0.0.0.0:8443".to_string());
    let cfg = RelayConfig {
        registration_mode: admission.registration_mode.clone(),
        allow: admission.allow.clone(),
        deny: admission.deny.clone(),
        relay_token: admission.relay_token.clone(),
        lease_ttl: Duration::from_secs(
            cli.lease_ttl_secs
                .or(file.limits.lease_ttl_secs)
                .unwrap_or(300),
        ),
        max_conns_per_node: cli
            .max_conns_per_node
            .or(file.limits.max_conns_per_node)
            .unwrap_or(256),
        idle_timeout: Duration::from_secs(
            cli.idle_timeout_secs
                .or(file.limits.idle_timeout_secs)
                .unwrap_or(300),
        ),
        accept_burst_per_ip: file.limits.accept_burst_per_ip.unwrap_or(30),
        accept_rate_per_ip: file.limits.accept_rate_per_ip.unwrap_or(10.0),
        connect_burst_per_node: file.limits.connect_burst_per_node.unwrap_or(60),
        connect_rate_per_node: file.limits.connect_rate_per_node.unwrap_or(20.0),
        route_by_client_cert,
    };

    let listener = tokio::net::TcpListener::bind(&bind)
        .await
        .with_context(|| format!("binding relay on {bind}"))?;
    let addr = listener.local_addr()?;
    eprintln!(
        "zerorelay listening on {addr} (outer TLS, mode: {:?})",
        cfg.registration_mode
    );

    let server = RelayServer::new(cfg);
    spawn_sighup_reloader(server.clone(), cli.config.clone(), overlay);
    spawn_status_dumper(server.clone(), cli.status_file.clone());
    server.serve(listener, acceptor).await
}

/// On SIGUSR1, snapshot per-node metrics to stderr (and to `--status-file` when
/// set) - a read-only operational surface for a shell-less/stateless relay. Also
/// refreshes the status file on a slow timer so `zerorelay status --file` stays
/// reasonably current. No-op for the signal half on non-unix.
#[cfg(unix)]
fn spawn_status_dumper(server: RelayServer, status_file: Option<String>) {
    use std::time::Duration as StdDuration;
    tokio::spawn(async move {
        let mut usr1 =
            match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::user_defined1()) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("zerorelay: cannot install SIGUSR1 handler ({e}); status dump off");
                    return;
                }
            };
        let mut tick = tokio::time::interval(StdDuration::from_secs(15));
        loop {
            let on_signal = tokio::select! {
                _ = usr1.recv() => true,
                _ = tick.tick() => false,
            };
            let status = server.status().await;
            let json = serde_json::to_string(&status).unwrap_or_else(|_| "{}".to_string());
            if let Some(path) = status_file.as_deref() {
                let _ = std::fs::write(path, format!("{json}\n"));
            }
            if on_signal {
                eprintln!("zerorelay status: {json}");
            }
        }
    });
}

#[cfg(not(unix))]
fn spawn_status_dumper(server: RelayServer, status_file: Option<String>) {
    use std::time::Duration as StdDuration;
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(StdDuration::from_secs(15));
        loop {
            tick.tick().await;
            if let Some(path) = status_file.as_deref() {
                let status = server.status().await;
                let json = serde_json::to_string(&status).unwrap_or_else(|_| "{}".to_string());
                let _ = std::fs::write(path, format!("{json}\n"));
            }
        }
    });
}

/// On SIGHUP, re-read the config file's `[admission]` section and swap the live
/// admission policy (allow/deny/mode/token), re-applying the startup CLI overlay.
/// Live connections are untouched. No-op when there is no `--config` file.
#[cfg(unix)]
fn spawn_sighup_reloader(
    server: RelayServer,
    config_path: Option<String>,
    overlay: AdmissionOverlay,
) {
    tokio::spawn(async move {
        let mut sighup = match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())
        {
            Ok(s) => s,
            Err(e) => {
                eprintln!("zerorelay: cannot install SIGHUP handler ({e}); admission reload off");
                return;
            }
        };
        while sighup.recv().await.is_some() {
            let Some(path) = config_path.as_deref() else {
                eprintln!("zerorelay: SIGHUP ignored (no --config to reload)");
                continue;
            };
            match load_file_config(path).and_then(|f| resolve_admission(&f.admission, &overlay)) {
                Ok(policy) => {
                    server.reload_admission(policy);
                    eprintln!("zerorelay: reloaded admission from {path} (SIGHUP)");
                }
                Err(e) => {
                    eprintln!("zerorelay: SIGHUP reload failed, keeping current policy ({e:#})");
                }
            }
        }
    });
}

#[cfg(not(unix))]
fn spawn_sighup_reloader(
    _server: RelayServer,
    _config_path: Option<String>,
    _overlay: AdmissionOverlay,
) {
}

/// Build an outer client-cert verifier for the outer-mTLS variant, or `None` when
/// `outer_client_auth` is off. "optional" accepts unauthenticated peers too;
/// "required" rejects a peer without a valid outer client cert.
fn build_outer_client_verifier(
    admission: &AdmissionFile,
) -> Result<Option<Arc<dyn rustls::server::danger::ClientCertVerifier>>> {
    let mode = admission.outer_client_auth.as_deref().unwrap_or("off");
    match mode {
        "off" => Ok(None),
        "optional" | "required" => {
            let ca = admission.outer_client_ca.clone().ok_or_else(|| {
                anyhow::Error::msg(format!(
                    "[admission].outer_client_auth = {mode} requires [admission].outer_client_ca"
                ))
            })?;
            let verifier = zeroclaw_tls::build_client_verifier(&zeroclaw_tls::ClientAuthParams {
                ca_cert_path: ca,
                require_client_cert: mode == "required",
                pinned_certs: vec![],
            })?;
            Ok(Some(verifier))
        }
        other => {
            anyhow::bail!("invalid [admission].outer_client_auth '{other}' (off|optional|required)")
        }
    }
}

/// Build the outer TLS acceptor from the relay's own server cert + key. Outer TLS
/// is server-authenticated; `client_verifier` adds the optional outer-mTLS variant
/// (additive admission), but the inner mTLS stays the real RPC security boundary.
fn build_tls_acceptor(
    cert_path: &str,
    key_path: &str,
    client_verifier: Option<Arc<dyn rustls::server::danger::ClientCertVerifier>>,
) -> Result<TlsAcceptor> {
    let certs = load_certs(cert_path)?;
    let key = load_key(key_path)?;
    let builder = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .context("ring provider supports the default protocol versions")?;
    let config = match client_verifier {
        Some(v) => builder.with_client_cert_verifier(v),
        None => builder.with_no_client_auth(),
    }
    .with_single_cert(certs, key)
    .context("relay cert/key are not a valid pair")?;
    Ok(TlsAcceptor::from(Arc::new(config)))
}

/// Self-provision the relay's outer TLS cert (CA + server leaf with SANs) on first
/// run, reusing the daemon's `zeroclaw-tls` machinery so no openssl is needed.
/// Reused on later runs. Prints the CA path daemons/clients should trust.
fn provision_tls_acceptor(
    tls_dir: Option<&str>,
    extra_sans: &[String],
    client_verifier: Option<Arc<dyn rustls::server::danger::ClientCertVerifier>>,
) -> Result<TlsAcceptor> {
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
        client_verifier,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn overlay(mode: Option<&str>, allow: &[&str], token: Option<&str>) -> AdmissionOverlay {
        AdmissionOverlay {
            mode: mode.map(str::to_string),
            allow: allow.iter().map(|s| s.to_string()).collect(),
            deny: vec![],
            relay_token: token.map(str::to_string),
        }
    }

    #[test]
    fn shipped_example_config_parses() {
        // The relay.example.toml we ship (and bake into the image) must parse with
        // `deny_unknown_fields` on, so a stale key never silently no-ops.
        let text = include_str!("../relay.example.toml");
        let file: FileConfig = toml::from_str(text).expect("example relay.toml parses");
        assert_eq!(file.bind.as_deref(), Some("0.0.0.0:8443"));
        assert_eq!(file.tls.dir.as_deref(), Some("/data/tls"));
        assert_eq!(file.admission.mode.as_deref(), Some("open"));
        assert_eq!(file.limits.max_conns_per_node, Some(256));
    }

    #[test]
    fn cli_overrides_file_scalars_and_unions_lists() {
        let file = AdmissionFile {
            mode: Some("open".into()),
            allow: vec!["from_file".into()],
            deny: vec!["bad_file".into()],
            relay_token: Some("file_tok".into()),
            ..Default::default()
        };
        // CLI flips the mode + token and adds an allow entry.
        let pol = resolve_admission(
            &file,
            &overlay(Some("allowlist"), &["from_cli"], Some("cli_tok")),
        )
        .unwrap();
        assert_eq!(pol.registration_mode, Admission::Allowlist); // CLI wins
        assert_eq!(pol.relay_token.as_deref(), Some("cli_tok")); // CLI wins
        assert!(pol.allow.contains("from_file") && pol.allow.contains("from_cli")); // union
        assert!(pol.deny.contains("bad_file"));
    }

    #[test]
    fn file_only_admission_resolves() {
        let file = AdmissionFile {
            mode: Some("allowlist".into()),
            allow: vec!["only_file".into()],
            ..Default::default()
        };
        let pol = resolve_admission(&file, &overlay(None, &[], None)).unwrap();
        assert_eq!(pol.registration_mode, Admission::Allowlist);
        assert!(pol.allow.contains("only_file"));
        assert!(pol.relay_token.is_none());
    }

    #[test]
    fn missing_mode_defaults_to_open_and_bad_mode_errors() {
        let empty = AdmissionFile::default();
        let pol = resolve_admission(&empty, &overlay(None, &[], None)).unwrap();
        assert_eq!(pol.registration_mode, Admission::Open);
        assert!(resolve_admission(&empty, &overlay(Some("nonsense"), &[], None)).is_err());
    }

    #[test]
    fn unknown_config_key_is_rejected() {
        // deny_unknown_fields: a typo'd key fails loudly instead of silently
        // running stale defaults.
        let bad = "bind = \"0.0.0.0:1\"\n[admission]\nmodee = \"open\"\n";
        assert!(toml::from_str::<FileConfig>(bad).is_err());
    }
}
