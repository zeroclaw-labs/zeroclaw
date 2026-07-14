use clap::{Parser, Subcommand};

/// `cargo generate` - maintainer surface generators. Surfaces are derived from
/// the canonical spec; install.sh@HEAD is the behavioral reference.
#[derive(Parser)]
#[command(name = "generate", about = "ZeroClaw maintainer surface generation")]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Render install surfaces (setup.bat, ...) from the canonical spec.
    Installers {
        /// Surface(s) to render. Omit (with no --check) to render all.
        targets: Vec<String>,
        /// Regenerate to memory and diff against on-disk; nonzero on drift.
        /// Writes nothing. This is the CI drift gate.
        #[arg(long)]
        check: bool,
    },
    /// Print the resolved feature list for a build selection, comma-joined.
    /// Surfaces and CI consume this instead of hardcoding feature names.
    Features {
        /// Selection id from the canonical menu (e.g. `dist`, `all`, `minimal`).
        #[arg(long)]
        selection: String,
        /// Build target triple used to apply canonical distribution exclusions.
        #[arg(long)]
        target: Option<String>,
        /// Additional caller-requested exclusions applied after target policy.
        /// Repeat the flag or pass a comma-separated list.
        #[arg(long, value_delimiter = ',')]
        exclude: Vec<String>,
    },
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Cmd::Installers { targets, check } => xtask::generate::run(&targets, check),
        Cmd::Features {
            selection,
            target,
            exclude,
        } => xtask::generate::features(&selection, target.as_deref(), &exclude),
    }
}
