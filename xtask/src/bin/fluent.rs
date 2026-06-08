use clap::{Parser, Subcommand};
use xtask::cmd;

#[derive(Parser)]
#[command(name = "fluent", about = "ZeroClaw Fluent app UI translation")]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Scan Rust source for user-facing strings and report en.ftl coverage
    Scan {
        /// Restrict to one catalogue family (`runtime` or `zerocode`).
        /// Defaults to all catalogues.
        #[arg(long)]
        catalog: Option<String>,
    },
    /// AI-fill missing translations in non-English .ftl files
    Fill {
        #[arg(long)]
        locale: Option<String>,
        /// Re-translate all entries (quality pass, costs more)
        #[arg(long)]
        force: bool,
        /// Provider alias from [providers.models.<kind>.<alias>] in config.toml.
        /// Pass a bare alias (e.g. `lineation`) or a `kind.alias` qualifier
        /// (e.g. `anthropic.lineation`) when the alias is ambiguous.
        #[arg(long)]
        model_provider: Option<String>,
        /// Config directory holding config.toml and .secret-key (default:
        /// ~/.zeroclaw). Mirrors `zeroclaw --config-dir`.
        #[arg(long)]
        config_dir: Option<String>,
        /// Restrict to one catalogue family (`runtime` or `zerocode`).
        /// Defaults to all catalogues.
        #[arg(long)]
        catalog: Option<String>,
        /// Entries per API call (default: 50). Lower if the model truncates large JSON responses.
        #[arg(long)]
        batch: Option<usize>,
    },
    /// Show translation coverage per locale
    Stats {
        /// Restrict to one catalogue family (`runtime` or `zerocode`).
        /// Defaults to all catalogues.
        #[arg(long)]
        catalog: Option<String>,
    },
    /// Validate .ftl syntax for all locales
    Check {
        /// Restrict to one catalogue family (`runtime` or `zerocode`).
        /// Defaults to all catalogues.
        #[arg(long)]
        catalog: Option<String>,
    },
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Cmd::Scan { catalog } => cmd::fluent::scan::run(catalog.as_deref()),
        Cmd::Fill {
            locale,
            force,
            model_provider,
            config_dir,
            catalog,
            batch,
        } => cmd::fluent::fill::run(
            locale.as_deref(),
            force,
            model_provider.as_deref(),
            config_dir.as_deref(),
            catalog.as_deref(),
            batch,
        ),
        Cmd::Stats { catalog } => cmd::fluent::stats::run(catalog.as_deref()),
        Cmd::Check { catalog } => cmd::fluent::check::run(catalog.as_deref()),
    }
}
