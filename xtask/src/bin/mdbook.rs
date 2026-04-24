use xtask::cmd;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "mdbook", about = "ZeroClaw documentation tooling")]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Serve docs locally with live-reload
    Serve {
        #[arg(long, default_value = "en")]
        locale: String,
    },
    /// Static build of all locales into docs/book/book/
    Build,
    /// Regenerate cli.md, config.md, and rustdoc API reference
    Refs,
    /// Sync .po files and AI-fill translation delta
    Sync {
        #[arg(long)]
        locale: Option<String>,
        /// Re-translate all entries (quality pass, costs more)
        #[arg(long)]
        force: bool,
        /// Provider name from [providers.models.<name>] in config.toml
        #[arg(long)]
        provider: Option<String>,
    },
    /// Show translation statistics per locale
    Stats,
    /// Validate .po file format for all locales
    Check,
    /// Print space-separated locale codes from locales.toml (for CI use)
    Locales,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Cmd::Serve { locale }       => cmd::mdbook::serve::run(&locale),
        Cmd::Build                  => cmd::mdbook::build::run(),
        Cmd::Refs                   => cmd::mdbook::refs::run(),
        Cmd::Sync { locale, force, provider } => cmd::mdbook::sync::run(locale.as_deref(), force, provider.as_deref()),
        Cmd::Stats                  => cmd::mdbook::stats::run(),
        Cmd::Check                  => cmd::mdbook::check::run(),
        Cmd::Locales                => { cmd::mdbook::build::print_locales(); Ok(()) },
    }
}
