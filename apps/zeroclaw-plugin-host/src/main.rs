//! `zeroclaw-plugin-host`: out-of-process WASM plugin execution sidecar.
//!
//! Reads one JSON request per line on stdin, executes it through the same
//! `zeroclaw_plugins::execution` path the in-process build uses (this binary
//! is built WITH wasmtime/Cranelift), and writes one JSON response per line
//! on stdout. Spawned per call by the main binary's subprocess backend; it
//! exits when stdin closes. All the JIT weight lives here so the main dist
//! binary carries none of it.

use anyhow::Result;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use zeroclaw_plugins::subprocess::{self, Request, Response};

#[tokio::main]
async fn main() -> Result<()> {
    let stdin = BufReader::new(tokio::io::stdin());
    let mut stdout = tokio::io::stdout();
    let mut lines = stdin.lines();

    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<Request>(&line) {
            Ok(request) => subprocess::handle(request).await,
            Err(e) => Response {
                protocol_version: subprocess::PROTOCOL_VERSION,
                result: None,
                error: Some(format!("malformed request: {e}")),
            },
        };
        let mut out = serde_json::to_string(&response)?;
        out.push('\n');
        stdout.write_all(out.as_bytes()).await?;
        stdout.flush().await?;
    }
    Ok(())
}
