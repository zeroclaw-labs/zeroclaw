use clap::{Parser, Subcommand};
use xtask::cmd;

#[derive(Parser)]
#[command(name = "mdbook", about = "ZeroClaw documentation tooling")]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Serve docs locally with live-reload. Without --locale, builds all
    /// locales from locales.toml; with --locale, builds and watches only that one.
    Serve {
        #[arg(long)]
        locale: Option<String>,
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
        /// ModelProvider name from [providers.models.<name>] in config.toml
        #[arg(long)]
        model_provider: Option<String>,
        /// Entries per API call (default: 50)
        #[arg(long)]
        batch: Option<usize>,
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
        Cmd::Serve { locale } => cmd::mdbook::serve::run(locale.as_deref()),
        Cmd::Build => cmd::mdbook::build::run(),
        Cmd::Refs => cmd::mdbook::refs::run(),
        Cmd::Sync {
            locale,
            force,
            model_provider,
            batch,
        } => cmd::mdbook::sync::run(locale.as_deref(), force, model_provider.as_deref(), batch),
        Cmd::Stats => cmd::mdbook::stats::run(),
        Cmd::Check => cmd::mdbook::check::run(),
        Cmd::Locales => {
            cmd::mdbook::build::print_locales();
            Ok(())
        }
    }
}
