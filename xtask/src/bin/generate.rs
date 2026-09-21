use clap::{Parser, Subcommand};

/// `cargo generate` - maintainer surface generators. Each subcommand owns a
/// typed source and deterministic tracked outputs with a focused drift check.
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
    /// Render tracked PR-review policy documentation from its canonical spec.
    ReviewDocs {
        /// Regenerate to memory and diff against on-disk; nonzero on drift.
        /// Writes nothing. This is the focused review-docs drift gate.
        #[arg(long)]
        check: bool,
    },
    /// Render the source-backed SOP syntax reference.
    SopSyntax {
        /// Regenerate to memory and diff against on-disk; nonzero on drift.
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
        Cmd::ReviewDocs { check } => xtask::generate::review_docs::run(check),
        Cmd::SopSyntax { check } => xtask::generate::sop_syntax::run(check),
        Cmd::Features {
            selection,
            target,
            exclude,
        } => xtask::generate::features(&selection, target.as_deref(), &exclude),
    }
}
