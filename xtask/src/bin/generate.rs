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
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Cmd::Installers { targets, check } => xtask::generate::run(&targets, check),
    }
}
