//! `zeroclaw-bootstrap` — the four bootstrap operations.
//!
//! Deliberately absent from this surface: any URL, install root, release asset
//! name, shell command, or config path. Adding one would be the whole point of
//! the crate undone, so the argument types below are the enforcement.

use std::process::ExitCode;

use clap::{Parser, Subcommand};

use zeroclaw_bootstrap::error::BootstrapError;
use zeroclaw_bootstrap::fetch::HttpFetcher;
use zeroclaw_bootstrap::handoff;
use zeroclaw_bootstrap::install;
use zeroclaw_bootstrap::origin::{ReleaseTag, default_release_tag};
use zeroclaw_bootstrap::plan::{HostEnv, InstallPlan};
use zeroclaw_bootstrap::status;
use zeroclaw_bootstrap::target::HOST_TARGET;

#[derive(Parser)]
#[command(
    name = "zeroclaw-bootstrap",
    about = "Identify, verify, and install ZeroClaw, then hand off to `zeroclaw control --mcp`.",
    long_about = "Bootstrap install launcher for ZeroClaw.\n\n\
        Accepts no download URL, install root, release asset name, or shell command, and never \
        reads or writes config.toml. Artifacts come only from the pinned official release \
        origin and are digest-verified before installation.\n\n\
        Installation requires `--approve <plan-digest>`, the token printed by `plan`. That token \
        is a hash of the exact plan, so a model cannot satisfy the decision by asserting \
        approval — a human has to copy the value across.",
    version
)]
struct Cli {
    #[command(subcommand)]
    command: Operation,
}

#[derive(Subcommand)]
enum Operation {
    /// Detect the platform, an existing binary, and its verified version.
    Status,
    /// Select one supported artifact and show everything an approval covers.
    Plan {
        /// Release tag to plan against. Defaults to this launcher's version.
        #[arg(long, value_name = "TAG")]
        tag: Option<String>,
    },
    /// Download and install exactly the approved immutable artifact.
    Install {
        /// Plan digest printed by `plan`. Required: this is the human decision.
        #[arg(long, value_name = "PLAN_DIGEST")]
        approve: Option<String>,
        /// Release tag the approved plan was produced for.
        #[arg(long, value_name = "TAG")]
        tag: Option<String>,
    },
    /// Verify the installed control server's identity, then exec it.
    Handoff {
        /// Verify and report without replacing this process with the server.
        #[arg(long)]
        verify_only: bool,
        /// Digest the installed binary must have, as printed by `install`.
        #[arg(long, value_name = "SHA256")]
        expect_binary_sha256: Option<String>,
    },
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("zeroclaw-bootstrap: {err}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<(), BootstrapError> {
    let env = HostEnv::from_process();

    match cli.command {
        Operation::Status => {
            print!("{}", status::status(&env, HOST_TARGET).render());
            Ok(())
        }
        Operation::Plan { tag } => {
            let fetcher = HttpFetcher::new()?;
            let plan = InstallPlan::resolve(&fetcher, &env, HOST_TARGET, resolve_tag(tag)?)?;
            print!("{}", plan.render());
            Ok(())
        }
        Operation::Install { approve, tag } => {
            // The human decision is checked for presence before anything is
            // fetched, so an unapproved install never touches the network.
            let approve = approve.ok_or(BootstrapError::ApprovalMissing)?;
            let fetcher = HttpFetcher::new()?;
            let plan = InstallPlan::resolve(&fetcher, &env, HOST_TARGET, resolve_tag(tag)?)?;
            let outcome = install::install(&fetcher, &plan, Some(&approve))?;
            println!("Installed {}", outcome.binary_path.display());
            println!("  artifact sha256   {}", outcome.artifact_digest);
            println!("  binary sha256     {}", outcome.binary_digest);
            println!(
                "\nHand off with:\n  zeroclaw-bootstrap handoff --expect-binary-sha256 {}",
                outcome.binary_digest
            );
            Ok(())
        }
        Operation::Handoff {
            verify_only,
            expect_binary_sha256,
        } => {
            let target = zeroclaw_bootstrap::target::resolve(HOST_TARGET)?;
            let binary_path = env.install_dir(target.family)?.join(target.binary_name);
            let verified = handoff::verify(&binary_path, expect_binary_sha256.as_deref())?;
            eprint!("{}", verified.render());
            if verify_only {
                return Ok(());
            }
            handoff::exec_control_server(&binary_path).map(|_| ())
        }
    }
}

fn resolve_tag(tag: Option<String>) -> Result<ReleaseTag, BootstrapError> {
    match tag {
        Some(raw) => ReleaseTag::parse(&raw),
        None => Ok(default_release_tag()),
    }
}
