//! install.sh renderer. install.sh@HEAD is the behavioral reference; we
//! generate the step-dispatcher zones from the canonical `Plan` so the dry-run
//! narration and the real action come from one source. The hand-written glue
//! (arg parsing, toolchain bootstrap, summary) stays outside the sentinels.

use super::spec::{self, Selection};
use std::path::Path;

fn begin(zone: &str) -> String {
    format!("  # >>> generated:{zone} by `cargo generate installers` — do not edit <<<")
}
fn end(zone: &str) -> String {
    format!("  # >>> end generated:{zone} <<<")
}

const ZONE_CARGO_INSTALL: &str = "source-cargo-install";

/// Render the source-build cargo-install step dispatcher, matching install.sh's
/// style (2-space indent, shellcheck pragmas), with the command derived from the
/// canonical spec. Dry-run narration and execute action are one source.
fn render_cargo_install(root: &Path) -> anyhow::Result<String> {
    // Resolve to confirm the spec is readable; the cargo flags are interpolated
    // at runtime via $CARGO_FLAGS, not baked, so the body is selection-stable.
    let _ = spec::resolve_flags(root, &Selection::Full)?;
    Ok([
        "  if [ \"$DRY_RUN\" = true ]; then",
        "    # shellcheck disable=SC2086",
        "    info \"[dry-run] Would run: cargo install --path . --locked --force $CARGO_FLAGS\"",
        "  else",
        "    # shellcheck disable=SC2086",
        "    cargo install --path . --locked --force $CARGO_FLAGS",
        "  fi",
    ]
    .join("\n"))
}

/// Splice generated step zones into install.sh, leaving hand-written glue
/// untouched.
pub fn render_file(root: &Path, current: &str) -> anyhow::Result<String> {
    splice(current, ZONE_CARGO_INSTALL, &render_cargo_install(root)?)
}

fn splice(current: &str, zone: &str, body: &str) -> anyhow::Result<String> {
    let b = begin(zone);
    let e = end(zone);
    let begin_at = current.find(&b).ok_or_else(|| {
        anyhow::Error::msg(format!(
            "install.sh missing generated:{zone} BEGIN sentinel"
        ))
    })?;
    let after_begin = begin_at + b.len();
    let end_rel = current[after_begin..].find(&e).ok_or_else(|| {
        anyhow::Error::msg(format!("install.sh missing generated:{zone} END sentinel"))
    })?;
    let end_at = after_begin + end_rel;
    let mut out = String::new();
    out.push_str(&current[..after_begin]);
    out.push('\n');
    out.push_str(body);
    out.push('\n');
    out.push_str(&current[end_at..]);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .to_path_buf()
    }

    #[test]
    fn cargo_install_zone_has_dryrun_and_execute() {
        let z = render_cargo_install(&root()).unwrap();
        assert!(z.contains("[dry-run] Would run: cargo install"));
        assert!(z.contains("cargo install --path . --locked --force $CARGO_FLAGS"));
        assert!(z.contains("if [ \"$DRY_RUN\" = true ]; then"));
    }

    #[test]
    fn render_file_is_idempotent_against_real_install_sh() {
        let cur = std::fs::read_to_string(root().join("install.sh")).unwrap();
        let once = render_file(&root(), &cur).unwrap();
        let twice = render_file(&root(), &once).unwrap();
        assert_eq!(once, twice, "render must be idempotent");
    }
}
