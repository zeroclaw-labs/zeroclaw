//! `zerorelay` - the ZeroClaw nominated relay (blind forwarder).
//!
//! Run a rendezvous point that daemons behind NAT register with and clients
//! connect through. The relay routes on an opaque node-id and pipes ciphertext;
//! it never terminates the inner client<->daemon mTLS and holds no keys.

use std::collections::HashSet;

use anyhow::{Context, Result};
use clap::Parser;
use zerorelay::{Admission, RelayConfig, RelayServer};

#[derive(Parser, Debug)]
#[command(
    name = "zerorelay",
    about = "ZeroClaw nominated relay (blind forwarder)"
)]
struct Cli {
    /// Address to listen on for daemon and client connections.
    #[arg(long, default_value = "0.0.0.0:8443")]
    bind: String,

    /// Admission mode: `open` (any daemon may register) or `allowlist`.
    #[arg(long, default_value = "open")]
    registration_mode: String,

    /// Allowed relay tokens (allowlist mode). Repeatable.
    #[arg(long = "allow")]
    allow: Vec<String>,

    /// Denied relay tokens (always rejected). Repeatable.
    #[arg(long = "deny")]
    deny: Vec<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    let registration_mode = match cli.registration_mode.as_str() {
        "open" => Admission::Open,
        "allowlist" => Admission::Allowlist,
        other => anyhow::bail!("invalid --registration-mode '{other}' (open|allowlist)"),
    };

    let cfg = RelayConfig {
        registration_mode,
        allow: cli.allow.into_iter().collect::<HashSet<_>>(),
        deny: cli.deny.into_iter().collect::<HashSet<_>>(),
    };

    let listener = tokio::net::TcpListener::bind(&cli.bind)
        .await
        .with_context(|| format!("binding relay on {}", cli.bind))?;
    let addr = listener.local_addr()?;
    eprintln!(
        "zerorelay listening on {addr} (mode: {:?})",
        cfg.registration_mode
    );

    RelayServer::new(cfg).serve(listener).await
}
