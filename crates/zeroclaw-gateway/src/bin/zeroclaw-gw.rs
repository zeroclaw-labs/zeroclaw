//! Preview of the gateway as its own process. See
//! [`zeroclaw_gateway::preview`].

use std::process::ExitCode;

use zeroclaw_gateway::preview::{Invocation, USAGE, parse_args, serve};

#[tokio::main]
async fn main() -> ExitCode {
    let invocation = parse_args(
        std::env::args().skip(1),
        std::env::var("ZEROCLAW_SOCKET").ok(),
    );
    match invocation {
        Ok(Invocation::Help) => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        Ok(Invocation::Version) => {
            println!("zeroclaw-gw {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Ok(Invocation::Serve(bootstrap)) => match serve(bootstrap).await {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("zeroclaw-gw: {error:#}");
                ExitCode::FAILURE
            }
        },
        Err(message) => {
            eprintln!("zeroclaw-gw: {message}");
            ExitCode::from(2)
        }
    }
}
