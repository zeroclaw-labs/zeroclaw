mod config;

use anyhow::Result;
use tracing_subscriber;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    println!("zeroclaw-bridge v0.1.0");

    Ok(())
}
