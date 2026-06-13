//! `cargo generate installers` — render install surfaces (install.sh,
//! setup.bat, ...) from the canonical spec. install.sh@HEAD is the behavioral
//! reference. The spec is the single source of truth; surfaces are derived and
//! drift-checked.

pub mod spec;

use std::path::PathBuf;

/// Surfaces this command can render. Each maps to a renderer that rewrites only
/// the sentinel-delimited generated region of its file.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Target {
    SetupBat,
}

impl Target {
    fn parse(s: &str) -> anyhow::Result<Target> {
        match s {
            "setup-bat" | "setup.bat" => Ok(Target::SetupBat),
            other => anyhow::bail!("unknown convert target `{other}` (known: setup-bat)"),
        }
    }

    fn all() -> Vec<Target> {
        vec![Target::SetupBat]
    }

    fn name(self) -> &'static str {
        match self {
            Target::SetupBat => "setup-bat",
        }
    }
}

/// Workspace root (where the canonical Cargo.toml lives).
fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn run(targets: &[String], check: bool) -> anyhow::Result<()> {
    // `cargo generate installers` with no targets renders every surface; the
    // plural subcommand means "all installers" by default.
    let selected: Vec<Target> = if targets.is_empty() {
        Target::all()
    } else {
        targets
            .iter()
            .map(|t| Target::parse(t))
            .collect::<anyhow::Result<_>>()?
    };

    let root = workspace_root();
    let mut drift = false;

    for t in selected {
        match t {
            Target::SetupBat => {
                let rendered = render_setup_bat(&root)?;
                let path = root.join("setup.bat");
                if check {
                    let current = std::fs::read_to_string(&path).unwrap_or_default();
                    if current != rendered {
                        eprintln!("DRIFT: {} is out of sync with the spec", t.name());
                        drift = true;
                    } else {
                        println!("ok: {} in sync", t.name());
                    }
                } else {
                    std::fs::write(&path, rendered)?;
                    println!("wrote {}", path.display());
                }
            }
        }
    }

    if check && drift {
        anyhow::bail!("one or more installers drifted; run `cargo generate installers`");
    }
    Ok(())
}

/// Placeholder until the renderer + template land. Returns the rendered
/// setup.bat string from the canonical spec.
fn render_setup_bat(_root: &std::path::Path) -> anyhow::Result<String> {
    anyhow::bail!("render_setup_bat not yet implemented")
}
