use clap::{Parser, Subcommand};
use xtask::cmd;

#[derive(Parser)]
#[command(name = "mdbook", about = "ZeroClaw documentation tooling")]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
    /// Optional tag for versioned docs output (e.g. v0.7.5). Falls back to TAG env var.
    #[arg(long)]
    tag: Option<String>,
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
        /// Provider alias from [providers.models.<kind>.<alias>] in config.toml
        #[arg(long)]
        model_provider: Option<String>,
        /// Config directory holding config.toml and .secret-key (default:
        /// ~/.zeroclaw). Mirrors `zeroclaw --config-dir`.
        #[arg(long)]
        config_dir: Option<String>,
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
    /// Extract shared chrome layer into _shared directory
    ExtractChrome {
        version_dir: String,
        shared_dir: String,
    },
    /// Generate versions.json list of deployed documentation versions
    GenVersions,
    /// Regenerate pc-themes.css + switcher list from the dashboard theme registry
    Themes,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let tag_owned = cli.tag.or_else(|| std::env::var("TAG").ok());
    let tag = tag_owned.as_deref();
    match cli.command {
        Cmd::Serve { locale } => cmd::mdbook::serve::run(locale.as_deref(), tag),
        Cmd::Build => cmd::mdbook::build::run(tag),
        Cmd::Refs => cmd::mdbook::refs::run(tag),
        Cmd::Sync {
            locale,
            force,
            model_provider,
            config_dir,
            batch,
        } => cmd::mdbook::sync::run(
            locale.as_deref(),
            force,
            model_provider.as_deref(),
            config_dir.as_deref(),
            batch,
        ),
        Cmd::Stats => cmd::mdbook::stats::run(),
        Cmd::Check => cmd::mdbook::check::run(),
        Cmd::Locales => {
            cmd::mdbook::build::print_locales();
            Ok(())
        }
        Cmd::ExtractChrome {
            version_dir,
            shared_dir,
        } => cmd::mdbook::build::extract_shared_chrome(
            std::path::Path::new(&version_dir),
            std::path::Path::new(&shared_dir),
        ),
        Cmd::GenVersions => cmd::mdbook::versions::run(),
        Cmd::Themes => cmd::mdbook::themes::run(&xtask::util::repo_root()),
    }
}
