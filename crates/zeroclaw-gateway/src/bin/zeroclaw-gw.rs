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
            // i18n-exempt: `--version` output is the program name and version, read by scripts
            println!("zeroclaw-gw {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Ok(Invocation::Serve(bootstrap)) => match serve(bootstrap).await {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                // i18n-exempt: a program-name prefix on the error's own text, no prose of its own
                eprintln!("zeroclaw-gw: {error:#}");
                ExitCode::FAILURE
            }
        },
        Err(message) => {
            // i18n-exempt: a program-name prefix on the parse error's own text, no prose of its own
            eprintln!("zeroclaw-gw: {message}");
            ExitCode::from(2)
        }
    }
}
